//! Durable deployment persistence adapters.
//!
//! SQLite implements the application-owned `DeploymentRepository` port. No
//! deployment workflow logic lives here; this module is responsible only for
//! durable state, transactional state/history updates, and rehydration.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use crate::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, DeploymentStep,
    EnvironmentId,
};
use crate::ports::{
    DeploymentRepository, DeploymentTransition, RepositoryError, RepositoryResult, StepAttemptId,
    StepAttemptRecord, StepAttemptStatus,
};

const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");

type DeploymentRow = (String, String, String, String, i64, String, String);

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
        Ok(Self { connection })
    }
}

impl DeploymentRepository for SqliteDeploymentRepository {
    fn create(&mut self, deployment: &Deployment) -> RepositoryResult<()> {
        let now = now_unix_ms();
        let size = i64::try_from(deployment.artifact().size_bytes()).map_err(|_| {
            RepositoryError::Storage("artifact size does not fit SQLite INTEGER".into())
        })?;

        let result = self.connection.execute(
            "INSERT INTO deployments (
                id, application_id, environment_id,
                artifact_version, artifact_size_bytes, artifact_sha256,
                state, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                deployment.id().as_str(),
                deployment.application().as_str(),
                deployment.environment().as_str(),
                deployment.artifact().version(),
                size,
                deployment.artifact().sha256(),
                state_name(deployment.state()),
                now,
            ],
        );

        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                Err(RepositoryError::AlreadyExists(
                    deployment.id().as_str().to_owned(),
                ))
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    fn get(&self, id: &DeploymentId) -> RepositoryResult<Option<Deployment>> {
        let row = self
            .connection
            .query_row(
                "SELECT id, application_id, environment_id,
                        artifact_version, artifact_size_bytes, artifact_sha256, state
                 FROM deployments WHERE id = ?1",
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
                "SELECT id, application_id, environment_id,
                        artifact_version, artifact_size_bytes, artifact_sha256, state
                 FROM deployments
                 WHERE (?1 IS NULL OR application_id = ?1)
                   AND (?2 IS NULL OR environment_id = ?2)
                 ORDER BY created_at_unix_ms DESC, id DESC
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
    let id = DeploymentId::new(id).map_err(corrupt_domain)?;
    let application = ApplicationId::new(application).map_err(corrupt_domain)?;
    let environment = EnvironmentId::new(environment).map_err(corrupt_domain)?;
    let artifact = Artifact::new(version, size, sha256).map_err(corrupt_domain)?;
    let state = parse_state(&state)?;
    Ok(Deployment::rehydrate(
        id,
        application,
        environment,
        artifact,
        state,
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

    fn deployment(id: &str) -> Deployment {
        Deployment::new(
            DeploymentId::new(id).unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 42, SHA256).unwrap(),
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
        repository.create(&Deployment::new(
            DeploymentId::new("d2").unwrap(),
            ApplicationId::new("other").unwrap(),
            EnvironmentId::new("prod").unwrap(),
            Artifact::new("2.0.0", 42, SHA256).unwrap(),
        )).unwrap();

        assert_eq!(repository.list(Some("demo"), None, 50).unwrap().len(), 1);
        assert_eq!(repository.list(None, None, 1).unwrap().len(), 1);
        assert_eq!(
            repository
                .list(Some("other"), Some("prod"), 50)
                .unwrap()[0]
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
}
