//! SQLite adapter for startup recovery.
//!
//! Recovery is intentionally persisted separately from normal deployment and
//! rollback workflow repositories. The adapter atomically terminates stale
//! orchestration records and records whether the environment is safe to reuse.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, EnvironmentId,
    RecoveryDisposition, RecoveryIncident, RecoverySubjectKind, RollbackOperation,
    RollbackOperationId, RollbackOperationState,
};
use crate::ports::{RecoveryRepository, RepositoryError, RepositoryResult};

const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");
const RECOVERY_MIGRATION: &str = include_str!("../migrations/002_startup_recovery.sql");

type DeploymentRow = (String, String, String, String, i64, String, String);
type RollbackRow = (String, String, String, String);
type IncidentRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
);

pub struct SqliteRecoveryRepository {
    connection: Connection,
}

impl SqliteRecoveryRepository {
    pub fn open(path: impl AsRef<Path>) -> RepositoryResult<Self> {
        Self::from_connection(Connection::open(path).map_err(storage_error)?)
    }

    pub fn in_memory() -> RepositoryResult<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(storage_error)?)
    }

    fn from_connection(connection: Connection) -> RepositoryResult<Self> {
        connection
            .execute_batch(INITIAL_MIGRATION)
            .map_err(storage_error)?;
        connection
            .execute_batch(RECOVERY_MIGRATION)
            .map_err(storage_error)?;
        Ok(Self { connection })
    }
}

impl RecoveryRepository for SqliteRecoveryRepository {
    fn interrupted_deployments(&self) -> RepositoryResult<Vec<Deployment>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, application_id, environment_id,
                        artifact_version, artifact_size_bytes, artifact_sha256, state
                 FROM deployments
                 WHERE state NOT IN ('succeeded', 'failed', 'rolled_back', 'rollback_failed')
                 ORDER BY created_at_unix_ms, id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], deployment_row)
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error).and_then(decode_deployment))
            .collect()
    }

    fn started_rollback_operations(&self) -> RepositoryResult<Vec<RollbackOperation>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, source_deployment_id, application_id, environment_id
                 FROM rollback_operations
                 WHERE state = 'started'
                 ORDER BY created_at_unix_ms, id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], rollback_row)
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error).and_then(decode_rollback))
            .collect()
    }

    fn recover_deployment(
        &mut self,
        deployment: &Deployment,
        disposition: RecoveryDisposition,
        reason: &str,
    ) -> RepositoryResult<Option<RecoveryIncident>> {
        if deployment.state().is_terminal() {
            return Ok(None);
        }

        let now = now_unix_ms();
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let updated = transaction
            .execute(
                "UPDATE deployments
                 SET state = 'failed', updated_at_unix_ms = ?1
                 WHERE id = ?2 AND state = ?3",
                params![
                    now,
                    deployment.id().as_str(),
                    state_name(deployment.state())
                ],
            )
            .map_err(storage_error)?;
        if updated == 0 {
            return Ok(None);
        }

        transaction
            .execute(
                "UPDATE deployment_step_attempts
                 SET status = 'failed', error = ?1, finished_at_unix_ms = ?2
                 WHERE deployment_id = ?3 AND status = 'started'",
                params![reason, now, deployment.id().as_str()],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "INSERT INTO deployment_transitions (
                    deployment_id, from_state, to_state, occurred_at_unix_ms
                 ) VALUES (?1, ?2, 'failed', ?3)",
                params![
                    deployment.id().as_str(),
                    state_name(deployment.state()),
                    now
                ],
            )
            .map_err(storage_error)?;

        let incident = insert_incident(
            &transaction,
            RecoverySubjectKind::Deployment,
            deployment.id().as_str(),
            deployment.application().as_str(),
            deployment.environment().as_str(),
            state_name(deployment.state()),
            disposition,
            reason,
            now,
        )?;
        transaction.commit().map_err(storage_error)?;
        Ok(Some(incident))
    }

    fn recover_rollback_operation(
        &mut self,
        operation: &RollbackOperation,
        reason: &str,
    ) -> RepositoryResult<Option<RecoveryIncident>> {
        let now = now_unix_ms();
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let updated = transaction
            .execute(
                "UPDATE rollback_operations
                 SET state = 'failed', error = ?1, finished_at_unix_ms = ?2
                 WHERE id = ?3 AND state = 'started'",
                params![reason, now, operation.id().as_str()],
            )
            .map_err(storage_error)?;
        if updated == 0 {
            return Ok(None);
        }

        let incident = insert_incident(
            &transaction,
            RecoverySubjectKind::RollbackOperation,
            operation.id().as_str(),
            operation.application().as_str(),
            operation.environment().as_str(),
            "started",
            RecoveryDisposition::ManualReconciliationRequired,
            reason,
            now,
        )?;
        transaction.commit().map_err(storage_error)?;
        Ok(Some(incident))
    }

    fn unresolved_incidents(&self) -> RepositoryResult<Vec<RecoveryIncident>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, subject_kind, subject_id, application_id, environment_id,
                        previous_state, disposition, reason, created_at_unix_ms,
                        resolved_at_unix_ms
                 FROM recovery_incidents
                 WHERE resolved_at_unix_ms IS NULL
                 ORDER BY id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], incident_row)
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error).and_then(decode_incident))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_incident(
    transaction: &Transaction<'_>,
    subject_kind: RecoverySubjectKind,
    subject_id: &str,
    application: &str,
    environment: &str,
    previous_state: &str,
    disposition: RecoveryDisposition,
    reason: &str,
    now: i64,
) -> RepositoryResult<RecoveryIncident> {
    let resolved_at = match disposition {
        RecoveryDisposition::AutoResolved => Some(now),
        RecoveryDisposition::ManualReconciliationRequired => None,
    };
    transaction
        .execute(
            "INSERT INTO recovery_incidents (
                subject_kind, subject_id, application_id, environment_id,
                previous_state, disposition, reason, created_at_unix_ms,
                resolved_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                subject_kind_name(subject_kind),
                subject_id,
                application,
                environment,
                previous_state,
                disposition_name(disposition),
                reason,
                now,
                resolved_at,
            ],
        )
        .map_err(storage_error)?;
    let row_id = transaction.last_insert_rowid();
    let id = u64::try_from(row_id)
        .map_err(|_| RepositoryError::CorruptData("negative recovery incident id".into()))?;
    Ok(RecoveryIncident::rehydrate(
        id,
        subject_kind,
        subject_id.to_owned(),
        application.to_owned(),
        environment.to_owned(),
        previous_state.to_owned(),
        disposition,
        reason.to_owned(),
        now,
        resolved_at,
    ))
}

fn deployment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeploymentRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_deployment(row: DeploymentRow) -> RepositoryResult<Deployment> {
    let (id, application, environment, version, size, sha256, state) = row;
    let size = u64::try_from(size)
        .map_err(|_| RepositoryError::CorruptData("negative artifact size".into()))?;
    Ok(Deployment::rehydrate(
        DeploymentId::new(id).map_err(corrupt_domain)?,
        ApplicationId::new(application).map_err(corrupt_domain)?,
        EnvironmentId::new(environment).map_err(corrupt_domain)?,
        Artifact::new(version, size, sha256).map_err(corrupt_domain)?,
        parse_state(&state)?,
    ))
}

fn rollback_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RollbackRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn decode_rollback(row: RollbackRow) -> RepositoryResult<RollbackOperation> {
    let (id, source_deployment_id, application, environment) = row;
    Ok(RollbackOperation::rehydrate(
        RollbackOperationId::new(id).map_err(corrupt_rollback)?,
        DeploymentId::new(source_deployment_id).map_err(corrupt_domain)?,
        ApplicationId::new(application).map_err(corrupt_domain)?,
        EnvironmentId::new(environment).map_err(corrupt_domain)?,
        RollbackOperationState::Started,
    ))
}

fn incident_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IncidentRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn decode_incident(row: IncidentRow) -> RepositoryResult<RecoveryIncident> {
    let (
        id,
        subject_kind,
        subject_id,
        application,
        environment,
        previous_state,
        disposition,
        reason,
        created_at_unix_ms,
        resolved_at_unix_ms,
    ) = row;
    let id = u64::try_from(id)
        .map_err(|_| RepositoryError::CorruptData("negative recovery incident id".into()))?;
    Ok(RecoveryIncident::rehydrate(
        id,
        parse_subject_kind(&subject_kind)?,
        subject_id,
        application,
        environment,
        previous_state,
        parse_disposition(&disposition)?,
        reason,
        created_at_unix_ms,
        resolved_at_unix_ms,
    ))
}

fn state_name(state: DeploymentState) -> &'static str {
    match state {
        DeploymentState::Created => "created",
        DeploymentState::Prechecking => "prechecking",
        DeploymentState::StagingArtifact => "staging_artifact",
        DeploymentState::BackingUp => "backing_up",
        DeploymentState::Installing => "installing",
        DeploymentState::Restarting => "restarting",
        DeploymentState::Verifying => "verifying",
        DeploymentState::Succeeded => "succeeded",
        DeploymentState::Failed => "failed",
        DeploymentState::RollingBack => "rolling_back",
        DeploymentState::RolledBack => "rolled_back",
        DeploymentState::RollbackFailed => "rollback_failed",
    }
}

fn parse_state(value: &str) -> RepositoryResult<DeploymentState> {
    match value {
        "created" => Ok(DeploymentState::Created),
        "prechecking" => Ok(DeploymentState::Prechecking),
        "staging_artifact" => Ok(DeploymentState::StagingArtifact),
        "backing_up" => Ok(DeploymentState::BackingUp),
        "installing" => Ok(DeploymentState::Installing),
        "restarting" => Ok(DeploymentState::Restarting),
        "verifying" => Ok(DeploymentState::Verifying),
        "succeeded" => Ok(DeploymentState::Succeeded),
        "failed" => Ok(DeploymentState::Failed),
        "rolling_back" => Ok(DeploymentState::RollingBack),
        "rolled_back" => Ok(DeploymentState::RolledBack),
        "rollback_failed" => Ok(DeploymentState::RollbackFailed),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown deployment state: {other}"
        ))),
    }
}

fn subject_kind_name(kind: RecoverySubjectKind) -> &'static str {
    match kind {
        RecoverySubjectKind::Deployment => "deployment",
        RecoverySubjectKind::RollbackOperation => "rollback_operation",
    }
}

fn parse_subject_kind(value: &str) -> RepositoryResult<RecoverySubjectKind> {
    match value {
        "deployment" => Ok(RecoverySubjectKind::Deployment),
        "rollback_operation" => Ok(RecoverySubjectKind::RollbackOperation),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown recovery subject kind: {other}"
        ))),
    }
}

fn disposition_name(disposition: RecoveryDisposition) -> &'static str {
    match disposition {
        RecoveryDisposition::AutoResolved => "auto_resolved",
        RecoveryDisposition::ManualReconciliationRequired => "manual_reconciliation_required",
    }
}

fn parse_disposition(value: &str) -> RepositoryResult<RecoveryDisposition> {
    match value {
        "auto_resolved" => Ok(RecoveryDisposition::AutoResolved),
        "manual_reconciliation_required" => Ok(RecoveryDisposition::ManualReconciliationRequired),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown recovery disposition: {other}"
        ))),
    }
}

fn storage_error(error: rusqlite::Error) -> RepositoryError {
    RepositoryError::Storage(error.to_string())
}

fn corrupt_domain(error: crate::domain::DeploymentError) -> RepositoryError {
    RepositoryError::CorruptData(error.to_string())
}

fn corrupt_rollback(error: crate::domain::RollbackError) -> RepositoryError {
    RepositoryError::CorruptData(error.to_string())
}

fn now_unix_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
