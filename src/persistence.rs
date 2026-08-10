//! Durable deployment persistence adapters.
//!
//! SQLite implements the application-owned `DeploymentRepository` port. No
//! deployment workflow logic lives here; this module is responsible only for
//! durable state, transactional state/history updates, and rehydration.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, TransactionBehavior};

use crate::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentMechanismKind, DeploymentState,
    DeploymentStep, EnvironmentId, MechanismContractFingerprint, ReleaseIdentity,
    RollbackMechanismSnapshot, RollbackReference, RollbackReferenceState,
};
use crate::ports::{
    DeploymentRepository, DeploymentReservation, DeploymentTransition, RepositoryError,
    RepositoryResult, StepAttemptId, StepAttemptRecord, StepAttemptStatus,
};

const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");
const IDENTITY_MIGRATION: &str = include_str!("../migrations/004_deployment_identity.sql");
const GENERIC_MODEL_MIGRATION: &str = include_str!("../migrations/006_v0_2_generic_model.sql");
const ROLLBACK_CAPTURE_MIGRATION: &str =
    include_str!("../migrations/007_deployment_rollback_capture.sql");

type DeploymentRow = (
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
);

pub struct SqliteDeploymentRepository {
    connection: Connection,
}

impl SqliteDeploymentRepository {
    pub fn open(path: impl AsRef<Path>) -> RepositoryResult<Self> {
        let connection = Connection::open(path).map_err(storage_error)?;
        Self::from_connection(connection)
    }

    pub fn in_memory() -> RepositoryResult<Self> {
        let connection = Connection::open_in_memory().map_err(storage_error)?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> RepositoryResult<Self> {
        connection
            .execute_batch(INITIAL_MIGRATION)
            .map_err(storage_error)?;
        connection
            .execute_batch(IDENTITY_MIGRATION)
            .map_err(storage_error)?;
        connection
            .execute_batch(GENERIC_MODEL_MIGRATION)
            .map_err(storage_error)?;
        connection
            .execute_batch(ROLLBACK_CAPTURE_MIGRATION)
            .map_err(storage_error)?;
        Ok(Self { connection })
    }
}

impl DeploymentRepository for SqliteDeploymentRepository {
    fn create(&mut self, deployment: &Deployment) -> RepositoryResult<()> {
        let projection = legacy_storage_projection(deployment)?;
        let now = now_unix_ms();
        let transaction = self.connection.transaction().map_err(storage_error)?;

        let result = transaction.execute(
            "INSERT INTO deployments (
                id, application_id, environment_id,
                artifact_version, artifact_size_bytes, artifact_sha256,
                state, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                deployment.id().as_str(),
                deployment.application().as_str(),
                deployment.environment().as_str(),
                projection.version,
                projection.size_bytes,
                projection.sha256,
                state_name(deployment.state()),
                now,
            ],
        );

        match result {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                return Err(RepositoryError::AlreadyExists(
                    deployment.id().as_str().to_owned(),
                ));
            }
            Err(error) => return Err(storage_error(error)),
        }
        insert_deployment_model_metadata(&transaction, deployment)?;
        transaction.commit().map_err(storage_error)
    }

    fn reserve(
        &mut self,
        deployment: &Deployment,
        idempotency_key: Option<&str>,
    ) -> RepositoryResult<DeploymentReservation> {
        let projection = legacy_storage_projection(deployment)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;

        if let Some(key) = idempotency_key {
            let existing = transaction
                .query_row(
                    "SELECT d.id, d.application_id, d.environment_id,
                            d.artifact_version, d.artifact_size_bytes, d.artifact_sha256, d.state,
                            m.mechanism_kind, m.release_identity_json
                     FROM deployment_idempotency i
                     JOIN deployments d ON d.id = i.deployment_id
                     LEFT JOIN deployment_model_metadata m ON m.deployment_id = d.id
                     WHERE i.idempotency_key = ?1",
                    params![key],
                    deployment_row,
                )
                .optional()
                .map_err(storage_error)?;
            if let Some(existing) = existing {
                let existing = decode_deployment(existing)?;
                if same_request_identity(&existing, deployment) {
                    return Ok(DeploymentReservation::Reused(existing));
                }
                return Err(RepositoryError::IdempotencyConflict(key.to_owned()));
            }
        }

        {
            let mut statement = transaction
                .prepare(
                    "SELECT d.id, d.application_id, d.environment_id,
                            d.artifact_version, d.artifact_size_bytes, d.artifact_sha256, d.state,
                            m.mechanism_kind, m.release_identity_json
                     FROM deployments d
                     LEFT JOIN deployment_model_metadata m ON m.deployment_id = d.id
                     WHERE d.application_id = ?1
                       AND d.environment_id = ?2
                       AND d.artifact_version = ?3
                     ORDER BY d.rowid",
                )
                .map_err(storage_error)?;
            let rows = statement
                .query_map(
                    params![
                        deployment.application().as_str(),
                        deployment.environment().as_str(),
                        deployment.release_identity().version(),
                    ],
                    deployment_row,
                )
                .map_err(storage_error)?;
            for row in rows {
                let existing = decode_deployment(row.map_err(storage_error)?)?;
                if existing.mechanism_kind() != deployment.mechanism_kind()
                    || existing.release_identity() != deployment.release_identity()
                {
                    return Err(RepositoryError::ArtifactVersionConflict {
                        application: deployment.application().as_str().to_owned(),
                        environment: deployment.environment().as_str().to_owned(),
                        version: deployment.release_identity().version().to_owned(),
                        existing_sha256: release_identity_fingerprint(existing.release_identity()),
                        requested_sha256: release_identity_fingerprint(
                            deployment.release_identity(),
                        ),
                    });
                }
            }
        }

        let now = now_unix_ms();
        transaction
            .execute(
                "INSERT INTO deployments (
                    id, application_id, environment_id,
                    artifact_version, artifact_size_bytes, artifact_sha256,
                    state, created_at_unix_ms, updated_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    deployment.id().as_str(),
                    deployment.application().as_str(),
                    deployment.environment().as_str(),
                    projection.version,
                    projection.size_bytes,
                    projection.sha256,
                    state_name(deployment.state()),
                    now,
                ],
            )
            .map_err(|error| reservation_insert_error(error, deployment))?;
        insert_deployment_model_metadata(&transaction, deployment)?;

        if let Some(key) = idempotency_key {
            transaction
                .execute(
                    "INSERT INTO deployment_idempotency (
                        idempotency_key, deployment_id, created_at_unix_ms
                     ) VALUES (?1, ?2, ?3)",
                    params![key, deployment.id().as_str(), now],
                )
                .map_err(|error| idempotency_insert_error(error, key))?;
        }

        transaction.commit().map_err(storage_error)?;
        Ok(DeploymentReservation::Created)
    }

    fn get(&self, id: &DeploymentId) -> RepositoryResult<Option<Deployment>> {
        let row = self
            .connection
            .query_row(
                "SELECT d.id, d.application_id, d.environment_id,
                        d.artifact_version, d.artifact_size_bytes, d.artifact_sha256, d.state,
                        m.mechanism_kind, m.release_identity_json
                 FROM deployments d
                 LEFT JOIN deployment_model_metadata m ON m.deployment_id = d.id
                 WHERE d.id = ?1",
                params![id.as_str()],
                deployment_row,
            )
            .optional()
            .map_err(storage_error)?;

        row.map(decode_deployment).transpose()
    }

    fn list(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> RepositoryResult<Vec<Deployment>> {
        let limit = i64::try_from(limit)
            .map_err(|_| RepositoryError::Storage("deployment list limit is too large".into()))?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT d.id, d.application_id, d.environment_id,
                        d.artifact_version, d.artifact_size_bytes, d.artifact_sha256, d.state,
                        m.mechanism_kind, m.release_identity_json
                 FROM deployments d
                 LEFT JOIN deployment_model_metadata m ON m.deployment_id = d.id
                 WHERE (?1 IS NULL OR d.application_id = ?1)
                   AND (?2 IS NULL OR d.environment_id = ?2)
                 ORDER BY d.created_at_unix_ms DESC, d.id DESC
                 LIMIT ?3",
            )
            .map_err(storage_error)?;

        let rows = statement
            .query_map(params![application, environment, limit], deployment_row)
            .map_err(storage_error)?;

        rows.map(|row| row.map_err(storage_error).and_then(decode_deployment))
            .collect()
    }

    fn list_non_terminal(&self) -> RepositoryResult<Vec<Deployment>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT d.id, d.application_id, d.environment_id,
                        d.artifact_version, d.artifact_size_bytes, d.artifact_sha256, d.state,
                        m.mechanism_kind, m.release_identity_json
                 FROM deployments d
                 LEFT JOIN deployment_model_metadata m ON m.deployment_id = d.id
                 WHERE d.state NOT IN ('succeeded', 'failed', 'rolled_back', 'rollback_failed')
                 ORDER BY d.created_at_unix_ms, d.id",
            )
            .map_err(storage_error)?;

        let rows = statement
            .query_map([], deployment_row)
            .map_err(storage_error)?;

        rows.map(|row| row.map_err(storage_error).and_then(decode_deployment))
            .collect()
    }

    fn persist_transition(
        &mut self,
        id: &DeploymentId,
        expected_from: DeploymentState,
        to: DeploymentState,
    ) -> RepositoryResult<()> {
        let now = now_unix_ms();
        let transaction = self.connection.transaction().map_err(storage_error)?;

        let updated = transaction
            .execute(
                "UPDATE deployments
                 SET state = ?1, updated_at_unix_ms = ?2
                 WHERE id = ?3 AND state = ?4",
                params![state_name(to), now, id.as_str(), state_name(expected_from)],
            )
            .map_err(storage_error)?;

        if updated != 1 {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM deployments WHERE id = ?1)",
                    params![id.as_str()],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;

            return if exists {
                Err(RepositoryError::StateConflict {
                    deployment_id: id.as_str().to_owned(),
                    expected: expected_from,
                })
            } else {
                Err(RepositoryError::NotFound(id.as_str().to_owned()))
            };
        }

        transaction
            .execute(
                "INSERT INTO deployment_transitions (
                    deployment_id, from_state, to_state, occurred_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![id.as_str(), state_name(expected_from), state_name(to), now],
            )
            .map_err(storage_error)?;

        transaction.commit().map_err(storage_error)
    }

    fn transitions(&self, id: &DeploymentId) -> RepositoryResult<Vec<DeploymentTransition>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT from_state, to_state, occurred_at_unix_ms
                 FROM deployment_transitions
                 WHERE deployment_id = ?1
                 ORDER BY id",
            )
            .map_err(storage_error)?;

        let rows = statement
            .query_map(params![id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage_error)?;

        rows.map(|row| {
            let (from, to, occurred_at_unix_ms) = row.map_err(storage_error)?;
            Ok(DeploymentTransition {
                from: parse_state(&from)?,
                to: parse_state(&to)?,
                occurred_at_unix_ms,
            })
        })
        .collect()
    }

    fn start_step(
        &mut self,
        id: &DeploymentId,
        step: DeploymentStep,
    ) -> RepositoryResult<StepAttemptId> {
        let now = now_unix_ms();
        let result = self.connection.execute(
            "INSERT INTO deployment_step_attempts (
                deployment_id, step, status, started_at_unix_ms
             ) VALUES (?1, ?2, 'started', ?3)",
            params![id.as_str(), step_name(step), now],
        );

        match result {
            Ok(_) => {
                let row_id = self.connection.last_insert_rowid();
                let id = u64::try_from(row_id)
                    .map_err(|_| RepositoryError::Storage("invalid SQLite row id".into()))?;
                Ok(StepAttemptId::new(id))
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                Err(RepositoryError::NotFound(id.as_str().to_owned()))
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    fn finish_step(
        &mut self,
        attempt_id: StepAttemptId,
        status: StepAttemptStatus,
        error: Option<&str>,
    ) -> RepositoryResult<()> {
        match (status, error) {
            (StepAttemptStatus::Succeeded, None) | (StepAttemptStatus::Failed, Some(_)) => {}
            _ => return Err(RepositoryError::InvalidStepAttemptCompletion),
        }

        let row_id = i64::try_from(attempt_id.get())
            .map_err(|_| RepositoryError::InvalidStepAttemptCompletion)?;
        let updated = self
            .connection
            .execute(
                "UPDATE deployment_step_attempts
                 SET status = ?1, error = ?2, finished_at_unix_ms = ?3
                 WHERE id = ?4 AND status = 'started'",
                params![status_name(status), error, now_unix_ms(), row_id],
            )
            .map_err(storage_error)?;

        if updated == 1 {
            Ok(())
        } else {
            Err(RepositoryError::InvalidStepAttemptCompletion)
        }
    }

    fn step_attempts(&self, id: &DeploymentId) -> RepositoryResult<Vec<StepAttemptRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, step, status, error, started_at_unix_ms, finished_at_unix_ms
                 FROM deployment_step_attempts
                 WHERE deployment_id = ?1
                 ORDER BY id",
            )
            .map_err(storage_error)?;

        let rows = statement
            .query_map(params![id.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })
            .map_err(storage_error)?;

        rows.map(|row| {
            let (row_id, step, status, error, started_at_unix_ms, finished_at_unix_ms) =
                row.map_err(storage_error)?;
            let row_id = u64::try_from(row_id)
                .map_err(|_| RepositoryError::CorruptData("negative step-attempt id".into()))?;
            Ok(StepAttemptRecord {
                id: StepAttemptId::new(row_id),
                step: parse_step(&step)?,
                status: parse_status(&status)?,
                error,
                started_at_unix_ms,
                finished_at_unix_ms,
            })
        })
        .collect()
    }

    fn persist_rollback_capture(&mut self, reference: &RollbackReference) -> RepositoryResult<()> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let source = transaction
            .query_row(
                "SELECT application_id, environment_id FROM deployments WHERE id = ?1",
                params![reference.deployment_id().as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| {
                RepositoryError::NotFound(reference.deployment_id().as_str().to_owned())
            })?;
        if source.0 != reference.application().as_str()
            || source.1 != reference.environment().as_str()
        {
            return Err(RepositoryError::CorruptData(
                "rollback capture does not match source deployment identity".into(),
            ));
        }

        let existing = transaction
            .query_row(
                "SELECT d.id, d.application_id, d.environment_id, c.mechanism_kind, c.target,
                        c.contract_fingerprint, c.mechanism_snapshot_json
                 FROM deployment_rollback_captures c
                 JOIN deployments d ON d.id = c.deployment_id
                 WHERE c.deployment_id = ?1",
                params![reference.deployment_id().as_str()],
                rollback_capture_row,
            )
            .optional()
            .map_err(storage_error)?
            .map(decode_rollback_capture)
            .transpose()?;
        if let Some(existing) = existing {
            return if &existing == reference {
                Ok(())
            } else {
                Err(RepositoryError::CorruptData(
                    "deployment rollback capture changed after it was persisted".into(),
                ))
            };
        }

        let snapshot_json =
            serde_json::to_string(reference.mechanism_snapshot()).map_err(|error| {
                RepositoryError::Storage(format!("serialize deployment rollback capture: {error}"))
            })?;
        transaction
            .execute(
                "INSERT INTO deployment_rollback_captures (
                    deployment_id, mechanism_kind, target, contract_fingerprint,
                    mechanism_snapshot_json, captured_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    reference.deployment_id().as_str(),
                    reference.mechanism_kind().as_str(),
                    reference.target(),
                    reference.contract_fingerprint().as_str(),
                    snapshot_json,
                    now_unix_ms(),
                ],
            )
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    fn get_rollback_capture(
        &self,
        deployment_id: &DeploymentId,
    ) -> RepositoryResult<Option<RollbackReference>> {
        self.connection
            .query_row(
                "SELECT d.id, d.application_id, d.environment_id, c.mechanism_kind, c.target,
                        c.contract_fingerprint, c.mechanism_snapshot_json
                 FROM deployment_rollback_captures c
                 JOIN deployments d ON d.id = c.deployment_id
                 WHERE c.deployment_id = ?1",
                params![deployment_id.as_str()],
                rollback_capture_row,
            )
            .optional()
            .map_err(storage_error)?
            .map(decode_rollback_capture)
            .transpose()
    }

    fn clear_rollback_capture(&mut self, deployment_id: &DeploymentId) -> RepositoryResult<()> {
        self.connection
            .execute(
                "DELETE FROM deployment_rollback_captures WHERE deployment_id = ?1",
                params![deployment_id.as_str()],
            )
            .map_err(storage_error)?;
        Ok(())
    }
}

fn same_request_identity(existing: &Deployment, requested: &Deployment) -> bool {
    existing.application() == requested.application()
        && existing.environment() == requested.environment()
        && existing.mechanism_kind() == requested.mechanism_kind()
        && existing.release_identity() == requested.release_identity()
}

struct LegacyStorageProjection<'a> {
    version: &'a str,
    size_bytes: i64,
    sha256: String,
}

fn legacy_storage_projection(
    deployment: &Deployment,
) -> RepositoryResult<LegacyStorageProjection<'_>> {
    match deployment.release_identity() {
        ReleaseIdentity::LocalFile(artifact) => Ok(LegacyStorageProjection {
            version: artifact.version(),
            size_bytes: i64::try_from(artifact.size_bytes()).map_err(|_| {
                RepositoryError::Storage("artifact size does not fit SQLite INTEGER".into())
            })?,
            sha256: artifact.sha256().to_owned(),
        }),
        ReleaseIdentity::ContainerImage(image) => Ok(LegacyStorageProjection {
            version: image.version(),
            size_bytes: 1,
            sha256: image.digest().trim_start_matches("sha256:").to_owned(),
        }),
    }
}

fn release_identity_fingerprint(identity: &ReleaseIdentity) -> String {
    match identity {
        ReleaseIdentity::LocalFile(artifact) => artifact.sha256().to_owned(),
        ReleaseIdentity::ContainerImage(image) => {
            format!("{}@{}", image.repository(), image.digest())
        }
    }
}

fn insert_deployment_model_metadata(
    transaction: &rusqlite::Transaction<'_>,
    deployment: &Deployment,
) -> RepositoryResult<()> {
    let release_identity_json =
        serde_json::to_string(deployment.release_identity()).map_err(|error| {
            RepositoryError::Storage(format!("serialize release identity: {error}"))
        })?;
    transaction
        .execute(
            "INSERT INTO deployment_model_metadata (
                 deployment_id, mechanism_kind, release_identity_json
             ) VALUES (?1, ?2, ?3)",
            params![
                deployment.id().as_str(),
                deployment.mechanism_kind().as_str(),
                release_identity_json,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn reservation_insert_error(error: rusqlite::Error, deployment: &Deployment) -> RepositoryError {
    match &error {
        rusqlite::Error::SqliteFailure(sqlite, message)
            if sqlite.code == ErrorCode::ConstraintViolation =>
        {
            let message = message.as_deref().unwrap_or_default();
            if message.contains("mutation_conflict")
                || message.contains("recovery_required")
                || message.contains("deployments.application_id, deployments.environment_id")
            {
                RepositoryError::MutationConflict {
                    application: deployment.application().as_str().to_owned(),
                    environment: deployment.environment().as_str().to_owned(),
                }
            } else if message.contains("deployments.id") {
                RepositoryError::AlreadyExists(deployment.id().as_str().to_owned())
            } else {
                storage_error(error)
            }
        }
        _ => storage_error(error),
    }
}

fn idempotency_insert_error(error: rusqlite::Error, key: &str) -> RepositoryError {
    match &error {
        rusqlite::Error::SqliteFailure(sqlite, _)
            if sqlite.code == ErrorCode::ConstraintViolation =>
        {
            RepositoryError::IdempotencyConflict(key.to_owned())
        }
        _ => storage_error(error),
    }
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
        row.get(7)?,
        row.get(8)?,
    ))
}

fn decode_deployment(row: DeploymentRow) -> RepositoryResult<Deployment> {
    let (
        id,
        application,
        environment,
        version,
        size,
        sha256,
        state,
        mechanism_kind,
        release_identity_json,
    ) = row;
    let size = u64::try_from(size)
        .map_err(|_| RepositoryError::CorruptData("negative artifact size".into()))?;
    let id = DeploymentId::new(id).map_err(corrupt_domain)?;
    let application = ApplicationId::new(application).map_err(corrupt_domain)?;
    let environment = EnvironmentId::new(environment).map_err(corrupt_domain)?;
    let legacy_artifact = Artifact::new(version, size, sha256).map_err(corrupt_domain)?;
    let (mechanism_kind, release_identity) = match (mechanism_kind, release_identity_json) {
        (None, None) => (
            DeploymentMechanismKind::JarSystemd,
            ReleaseIdentity::LocalFile(legacy_artifact.clone()),
        ),
        (Some(mechanism_kind), Some(release_identity_json)) => {
            let mechanism_kind =
                DeploymentMechanismKind::parse(&mechanism_kind).ok_or_else(|| {
                    RepositoryError::CorruptData(format!(
                        "unknown deployment mechanism kind: {mechanism_kind}"
                    ))
                })?;
            let release_identity: ReleaseIdentity = serde_json::from_str(&release_identity_json)
                .map_err(|error| {
                    RepositoryError::CorruptData(format!(
                        "invalid deployment release identity metadata: {error}"
                    ))
                })?;
            if let Some(local) = release_identity.local_file() {
                if local != &legacy_artifact {
                    return Err(RepositoryError::CorruptData(
                        "generic release identity does not match legacy local-file columns".into(),
                    ));
                }
            }
            (mechanism_kind, release_identity)
        }
        _ => {
            return Err(RepositoryError::CorruptData(
                "deployment model metadata is partially present".into(),
            ));
        }
    };
    let state = parse_state(&state)?;
    Ok(Deployment::rehydrate_with_release(
        id,
        application,
        environment,
        mechanism_kind,
        release_identity,
        state,
    ))
}

type RollbackCaptureRow = (String, String, String, String, String, String, String);

fn rollback_capture_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RollbackCaptureRow> {
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

fn decode_rollback_capture(row: RollbackCaptureRow) -> RepositoryResult<RollbackReference> {
    let (
        deployment_id,
        application,
        environment,
        mechanism_kind,
        target,
        fingerprint,
        snapshot_json,
    ) = row;
    let mechanism_kind = DeploymentMechanismKind::parse(&mechanism_kind).ok_or_else(|| {
        RepositoryError::CorruptData(format!(
            "unknown rollback capture mechanism kind: {mechanism_kind}"
        ))
    })?;
    let fingerprint = MechanismContractFingerprint::new(fingerprint).map_err(|error| {
        RepositoryError::CorruptData(format!(
            "invalid rollback capture contract fingerprint: {error}"
        ))
    })?;
    let snapshot: RollbackMechanismSnapshot =
        serde_json::from_str(&snapshot_json).map_err(|error| {
            RepositoryError::CorruptData(format!(
                "invalid rollback capture snapshot metadata: {error}"
            ))
        })?;
    RollbackReference::rehydrate_with_snapshot(
        DeploymentId::new(deployment_id).map_err(corrupt_domain)?,
        ApplicationId::new(application).map_err(corrupt_domain)?,
        EnvironmentId::new(environment).map_err(corrupt_domain)?,
        mechanism_kind,
        target,
        fingerprint,
        snapshot,
        RollbackReferenceState::Active,
    )
    .map_err(|error| RepositoryError::CorruptData(format!("invalid rollback capture: {error}")))
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

fn step_name(step: DeploymentStep) -> &'static str {
    match step {
        DeploymentStep::Precheck => "precheck",
        DeploymentStep::StageArtifact => "stage_artifact",
        DeploymentStep::BackupCurrent => "backup_current",
        DeploymentStep::Install => "install",
        DeploymentStep::Restart => "restart",
        DeploymentStep::Verify => "verify",
    }
}

fn parse_step(value: &str) -> RepositoryResult<DeploymentStep> {
    match value {
        "precheck" => Ok(DeploymentStep::Precheck),
        "stage_artifact" => Ok(DeploymentStep::StageArtifact),
        "backup_current" => Ok(DeploymentStep::BackupCurrent),
        "install" => Ok(DeploymentStep::Install),
        "restart" => Ok(DeploymentStep::Restart),
        "verify" => Ok(DeploymentStep::Verify),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown deployment step: {other}"
        ))),
    }
}

fn status_name(status: StepAttemptStatus) -> &'static str {
    match status {
        StepAttemptStatus::Started => "started",
        StepAttemptStatus::Succeeded => "succeeded",
        StepAttemptStatus::Failed => "failed",
    }
}

fn parse_status(value: &str) -> RepositoryResult<StepAttemptStatus> {
    match value {
        "started" => Ok(StepAttemptStatus::Started),
        "succeeded" => Ok(StepAttemptStatus::Succeeded),
        "failed" => Ok(StepAttemptStatus::Failed),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown step-attempt status: {other}"
        ))),
    }
}

fn storage_error(error: rusqlite::Error) -> RepositoryError {
    RepositoryError::Storage(error.to_string())
}

fn corrupt_domain(error: crate::domain::DeploymentError) -> RepositoryError {
    RepositoryError::CorruptData(error.to_string())
}

fn now_unix_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_SHA256: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn deployment(id: &str) -> Deployment {
        Deployment::new(
            DeploymentId::new(id).unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 42, SHA256).unwrap(),
        )
    }

    fn container_deployment(id: &str, digest: &str) -> Deployment {
        Deployment::new_with_release(
            DeploymentId::new(id).unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            DeploymentMechanismKind::DockerCompose,
            ReleaseIdentity::ContainerImage(
                crate::domain::ContainerImageReleaseIdentity::new(
                    "2.0.0",
                    "registry.example.com/demo",
                    digest,
                )
                .unwrap(),
            ),
        )
    }

    #[test]
    fn deployment_survives_repository_reopen_in_non_terminal_state() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("deployments.sqlite");
        let id = DeploymentId::new("d-recovery").unwrap();

        {
            let mut repository = SqliteDeploymentRepository::open(&path).unwrap();
            repository.create(&deployment(id.as_str())).unwrap();
            repository
                .persist_transition(&id, DeploymentState::Created, DeploymentState::Prechecking)
                .unwrap();
            repository
                .persist_transition(
                    &id,
                    DeploymentState::Prechecking,
                    DeploymentState::StagingArtifact,
                )
                .unwrap();
            repository
                .persist_transition(
                    &id,
                    DeploymentState::StagingArtifact,
                    DeploymentState::BackingUp,
                )
                .unwrap();
            repository
                .persist_transition(&id, DeploymentState::BackingUp, DeploymentState::Installing)
                .unwrap();
        }

        let repository = SqliteDeploymentRepository::open(&path).unwrap();
        let restored = repository.get(&id).unwrap().unwrap();
        assert_eq!(restored.state(), DeploymentState::Installing);
        assert_eq!(repository.list_non_terminal().unwrap().len(), 1);
        assert_eq!(repository.transitions(&id).unwrap().len(), 4);
    }

    #[test]
    fn list_filters_and_returns_recent_deployments() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        repository.create(&deployment("d1")).unwrap();
        repository
            .create(&Deployment::new(
                DeploymentId::new("d2").unwrap(),
                ApplicationId::new("other").unwrap(),
                EnvironmentId::new("prod").unwrap(),
                Artifact::new("2.0.0", 42, SHA256).unwrap(),
            ))
            .unwrap();

        assert_eq!(repository.list(Some("demo"), None, 50).unwrap().len(), 1);
        assert_eq!(repository.list(None, None, 1).unwrap().len(), 1);
        assert_eq!(
            repository.list(Some("other"), Some("prod"), 50).unwrap()[0]
                .id()
                .as_str(),
            "d2"
        );
    }

    #[test]
    fn state_conflict_rolls_back_state_and_history_together() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        let deployment = deployment("d-conflict");
        let id = deployment.id().clone();
        repository.create(&deployment).unwrap();
        repository
            .persist_transition(&id, DeploymentState::Created, DeploymentState::Prechecking)
            .unwrap();

        let error = repository
            .persist_transition(
                &id,
                DeploymentState::Created,
                DeploymentState::StagingArtifact,
            )
            .unwrap_err();
        assert!(matches!(error, RepositoryError::StateConflict { .. }));
        assert_eq!(
            repository.get(&id).unwrap().unwrap().state(),
            DeploymentState::Prechecking
        );
        assert_eq!(repository.transitions(&id).unwrap().len(), 1);
    }

    #[test]
    fn step_attempts_are_durable_and_cannot_be_completed_twice() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        let deployment = deployment("d-step");
        let id = deployment.id().clone();
        repository.create(&deployment).unwrap();

        let attempt = repository
            .start_step(&id, DeploymentStep::Precheck)
            .unwrap();
        repository
            .finish_step(
                attempt,
                StepAttemptStatus::Failed,
                Some("target unavailable"),
            )
            .unwrap();
        assert_eq!(
            repository
                .finish_step(attempt, StepAttemptStatus::Succeeded, None)
                .unwrap_err(),
            RepositoryError::InvalidStepAttemptCompletion
        );

        let attempts = repository.step_attempts(&id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].step, DeploymentStep::Precheck);
        assert_eq!(attempts[0].status, StepAttemptStatus::Failed);
        assert_eq!(attempts[0].error.as_deref(), Some("target unavailable"));
        assert!(attempts[0].finished_at_unix_ms.is_some());
    }

    #[test]
    fn duplicate_deployment_id_is_rejected_stably() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        let deployment = deployment("d-duplicate");
        repository.create(&deployment).unwrap();
        assert_eq!(
            repository.create(&deployment).unwrap_err(),
            RepositoryError::AlreadyExists("d-duplicate".into())
        );
    }

    #[test]
    fn generic_model_metadata_round_trips_jar_systemd_release_identity() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        let deployment = deployment("d-generic-metadata");
        let id = deployment.id().clone();
        repository.create(&deployment).unwrap();

        let metadata: (String, String) = repository
            .connection
            .query_row(
                "SELECT mechanism_kind, release_identity_json
                 FROM deployment_model_metadata WHERE deployment_id = ?1",
                params![id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(metadata.0, "jar_systemd");
        assert!(metadata.1.contains("local_file"));

        let restored = repository.get(&id).unwrap().unwrap();
        assert_eq!(
            restored.mechanism_kind(),
            DeploymentMechanismKind::JarSystemd
        );
        assert_eq!(restored.release_identity(), deployment.release_identity());
    }

    #[test]
    fn legacy_deployment_rows_without_generic_metadata_rehydrate_as_jar_systemd() {
        let repository = SqliteDeploymentRepository::in_memory().unwrap();
        repository
            .connection
            .execute(
                "INSERT INTO deployments (
                     id, application_id, environment_id, artifact_version,
                     artifact_size_bytes, artifact_sha256, state,
                     created_at_unix_ms, updated_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'succeeded', 1, 1)",
                params!["legacy", "demo", "test", "1.0.0", 42_i64, SHA256],
            )
            .unwrap();

        let restored = repository
            .get(&DeploymentId::new("legacy").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            restored.mechanism_kind(),
            DeploymentMechanismKind::JarSystemd
        );
        assert_eq!(restored.artifact().version(), "1.0.0");
        assert_eq!(restored.artifact().sha256(), SHA256);
    }

    #[test]
    fn idempotency_key_reuses_exact_deployment_across_repository_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("deployments.sqlite");
        let first = deployment("d-first");
        let mut repository = SqliteDeploymentRepository::open(&path).unwrap();
        assert!(matches!(
            repository.reserve(&first, Some("request-1")).unwrap(),
            DeploymentReservation::Created
        ));
        drop(repository);

        let mut reopened = SqliteDeploymentRepository::open(&path).unwrap();
        let proposed = deployment("d-second");
        let reused = reopened.reserve(&proposed, Some("request-1")).unwrap();
        match reused {
            DeploymentReservation::Reused(existing) => {
                assert_eq!(existing.id().as_str(), "d-first");
            }
            DeploymentReservation::Created => panic!("exact replay must reuse deployment"),
        }
        assert_eq!(reopened.list(None, None, 50).unwrap().len(), 1);
    }

    #[test]
    fn idempotency_key_cannot_be_rebound_to_different_intent() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        repository
            .reserve(&deployment("d-first"), Some("request-1"))
            .unwrap();
        let changed = Deployment::new(
            DeploymentId::new("d-changed").unwrap(),
            ApplicationId::new("other").unwrap(),
            EnvironmentId::new("prod").unwrap(),
            Artifact::new("2.0.0", 42, SHA256).unwrap(),
        );
        assert_eq!(
            repository.reserve(&changed, Some("request-1")).unwrap_err(),
            RepositoryError::IdempotencyConflict("request-1".to_owned())
        );
    }

    #[test]
    fn same_version_with_changed_checksum_is_rejected() {
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();
        repository
            .reserve(&deployment("d-first"), Some("request-1"))
            .unwrap();
        let changed = Deployment::new(
            DeploymentId::new("d-changed").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 42, OTHER_SHA256).unwrap(),
        );
        let error = repository.reserve(&changed, Some("request-2")).unwrap_err();
        assert!(matches!(
            error,
            RepositoryError::ArtifactVersionConflict { .. }
        ));
    }

    #[test]
    fn same_container_version_conflict_checks_complete_history() {
        const DIGEST_A: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const DIGEST_B: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut repository = SqliteDeploymentRepository::in_memory().unwrap();

        // `create` intentionally bypasses reservation policy to model a database
        // upgraded from history that already contains conflicting same-version rows.
        repository
            .create(&container_deployment("d-old-b", DIGEST_B))
            .unwrap();
        repository
            .connection
            .execute(
                "UPDATE deployments SET state = 'succeeded' WHERE id = ?1",
                params!["d-old-b"],
            )
            .unwrap();
        repository
            .create(&container_deployment("d-new-a", DIGEST_A))
            .unwrap();
        repository
            .connection
            .execute(
                "UPDATE deployments SET state = 'succeeded' WHERE id = ?1",
                params!["d-new-a"],
            )
            .unwrap();

        let requested = container_deployment("d-request-a", DIGEST_A);
        let error = repository
            .reserve(&requested, Some("container-request"))
            .unwrap_err();
        assert!(matches!(
            error,
            RepositoryError::ArtifactVersionConflict { .. }
        ));
    }

    #[test]
    fn docker_rollback_capture_survives_repository_reopen() {
        const DIGEST: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const PREVIOUS: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let directory = tempdir().unwrap();
        let path = directory.path().join("captures.sqlite");
        let deployment = container_deployment("d-capture", DIGEST);
        let id = deployment.id().clone();
        let snapshot = crate::domain::DockerComposeRollbackSnapshot::new(
            PREVIOUS,
            "registry.example.com/demo",
            "demo",
            "app",
            "compose-rollback",
            "compose-up",
            "compose-health",
        )
        .unwrap();
        let fingerprint = crate::domain::MechanismContractFingerprint::docker_compose(
            "test-server",
            "registry.example.com/demo",
            "demo",
            "app",
            Some("compose-precheck"),
            "compose-prepare",
            "compose-current",
            "compose-apply",
            "compose-up",
            "compose-health",
            "compose-rollback",
        );
        let reference = RollbackReference::new_with_snapshot(
            id.clone(),
            deployment.application().clone(),
            deployment.environment().clone(),
            DeploymentMechanismKind::DockerCompose,
            "test-server",
            fingerprint,
            RollbackMechanismSnapshot::DockerCompose(snapshot),
        )
        .unwrap();

        {
            let mut repository = SqliteDeploymentRepository::open(&path).unwrap();
            repository.create(&deployment).unwrap();
            repository.persist_rollback_capture(&reference).unwrap();
        }

        let mut reopened = SqliteDeploymentRepository::open(&path).unwrap();
        assert_eq!(reopened.get_rollback_capture(&id).unwrap(), Some(reference));
        reopened.clear_rollback_capture(&id).unwrap();
        assert!(reopened.get_rollback_capture(&id).unwrap().is_none());
    }
}
