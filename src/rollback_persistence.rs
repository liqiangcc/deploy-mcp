//! Durable explicit-rollback persistence.
//!
//! This adapter uses the same SQLite database as deployment persistence but is
//! kept behind its own application-owned port.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Transaction};

use crate::domain::{
    ApplicationId, DeploymentId, EnvironmentId, RollbackOperation, RollbackOperationId,
    RollbackOperationState, RollbackReference, RollbackReferenceState,
};
use crate::ports::{
    RepositoryError, RepositoryResult, RollbackRepository, RollbackRetentionRepository,
};

const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");
const RETENTION_MIGRATION: &str = include_str!("../migrations/005_rollback_reference_retention.sql");

type ReferenceRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);
type OperationRow = (String, String, String, String, String);

pub struct SqliteRollbackRepository {
    connection: Connection,
}

impl SqliteRollbackRepository {
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
            .execute_batch(RETENTION_MIGRATION)
            .map_err(storage_error)?;
        Ok(Self { connection })
    }
}

impl RollbackRepository for SqliteRollbackRepository {
    fn record_reference(&mut self, reference: &RollbackReference) -> RepositoryResult<()> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let (application, environment, state, source_rowid) =
            source_deployment(&transaction, reference.deployment_id())?.ok_or_else(|| {
                RepositoryError::NotFound(reference.deployment_id().as_str().to_owned())
            })?;

        if application != reference.application().as_str()
            || environment != reference.environment().as_str()
        {
            return Err(RepositoryError::CorruptData(
                "rollback reference does not match source deployment identity".into(),
            ));
        }
        if !matches!(state.as_str(), "succeeded" | "rollback_failed") {
            return Err(RepositoryError::RollbackUnavailable(format!(
                "source deployment {} is not eligible for explicit rollback",
                reference.deployment_id().as_str()
            )));
        }
        ensure_source_is_latest(&transaction, &application, &environment, source_rowid)?;

        let existing: Option<String> = transaction
            .query_row(
                "SELECT state FROM rollback_references WHERE deployment_id = ?1",
                params![reference.deployment_id().as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if let Some(existing) = existing {
            return match existing.as_str() {
                "active" => Ok(()),
                _ => Err(RepositoryError::RollbackUnavailable(format!(
                    "rollback reference for {} is already {existing}",
                    reference.deployment_id().as_str()
                ))),
            };
        }

        transaction
            .execute(
                "UPDATE rollback_references
                 SET state = 'superseded', updated_at_unix_ms = ?1
                 WHERE application_id = ?2 AND environment_id = ?3 AND state = 'active'",
                params![
                    now_unix_ms(),
                    reference.application().as_str(),
                    reference.environment().as_str()
                ],
            )
            .map_err(storage_error)?;

        let now = now_unix_ms();
        transaction
            .execute(
                "INSERT INTO rollback_references (
                    deployment_id, application_id, environment_id,
                    target, backup_path, install_path,
                    rollback_task, restart_task, health_check_task,
                    state, created_at_unix_ms, updated_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?10)",
                params![
                    reference.deployment_id().as_str(),
                    reference.application().as_str(),
                    reference.environment().as_str(),
                    reference.target(),
                    reference.backup_path(),
                    reference.install_path(),
                    reference.rollback_task(),
                    reference.restart_task(),
                    reference.health_check_task(),
                    now,
                ],
            )
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    fn get_reference(
        &self,
        deployment_id: &DeploymentId,
    ) -> RepositoryResult<Option<RollbackReference>> {
        self.connection
            .query_row(
                "SELECT deployment_id, application_id, environment_id,
                        target, backup_path, install_path,
                        rollback_task, restart_task, health_check_task, state
                 FROM rollback_references rr
                 WHERE deployment_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM rollback_reference_retention retention
                       WHERE retention.deployment_id = rr.deployment_id
                   )",
                params![deployment_id.as_str()],
                reference_row,
            )
            .optional()
            .map_err(storage_error)?
            .map(decode_reference)
            .transpose()
    }

    fn begin_operation(
        &mut self,
        operation: &RollbackOperation,
    ) -> RepositoryResult<RollbackReference> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let row = transaction
            .query_row(
                "SELECT deployment_id, application_id, environment_id,
                        target, backup_path, install_path,
                        rollback_task, restart_task, health_check_task, state
                 FROM rollback_references
                 WHERE deployment_id = ?1 AND state = 'active'",
                params![operation.source_deployment_id().as_str()],
                reference_row,
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| {
                RepositoryError::RollbackUnavailable(format!(
                    "no active rollback reference for {}",
                    operation.source_deployment_id().as_str()
                ))
            })?;
        let reference = decode_reference(row)?;

        if reference.application() != operation.application()
            || reference.environment() != operation.environment()
        {
            return Err(RepositoryError::CorruptData(
                "rollback operation does not match rollback reference identity".into(),
            ));
        }

        let (_, _, state, source_rowid) =
            source_deployment(&transaction, operation.source_deployment_id())?.ok_or_else(
                || RepositoryError::NotFound(operation.source_deployment_id().as_str().to_owned()),
            )?;
        if !matches!(state.as_str(), "succeeded" | "rollback_failed") {
            return Err(RepositoryError::RollbackUnavailable(format!(
                "source deployment {} is not eligible for explicit rollback",
                operation.source_deployment_id().as_str()
            )));
        }
        ensure_source_is_latest(
            &transaction,
            operation.application().as_str(),
            operation.environment().as_str(),
            source_rowid,
        )?;

        let insert = transaction.execute(
            "INSERT INTO rollback_operations (
                id, source_deployment_id, application_id, environment_id,
                state, error, created_at_unix_ms, finished_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, 'started', NULL, ?5, NULL)",
            params![
                operation.id().as_str(),
                operation.source_deployment_id().as_str(),
                operation.application().as_str(),
                operation.environment().as_str(),
                now_unix_ms(),
            ],
        );
        match insert {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                return Err(RepositoryError::MutationConflict {
                    application: operation.application().as_str().to_owned(),
                    environment: operation.environment().as_str().to_owned(),
                });
            }
            Err(error) => return Err(storage_error(error)),
        }

        transaction.commit().map_err(storage_error)?;
        Ok(reference)
    }

    fn finish_operation(
        &mut self,
        operation_id: &RollbackOperationId,
        source_deployment_id: &DeploymentId,
        state: RollbackOperationState,
        error: Option<&str>,
    ) -> RepositoryResult<()> {
        if state == RollbackOperationState::Started
            || (state == RollbackOperationState::Succeeded && error.is_some())
            || (state == RollbackOperationState::Failed && error.is_none())
        {
            return Err(RepositoryError::RollbackOperationConflict(
                operation_id.as_str().to_owned(),
            ));
        }

        let transaction = self.connection.transaction().map_err(storage_error)?;
        let updated = transaction
            .execute(
                "UPDATE rollback_operations
                 SET state = ?1, error = ?2, finished_at_unix_ms = ?3
                 WHERE id = ?4 AND source_deployment_id = ?5 AND state = 'started'",
                params![
                    operation_state_name(state),
                    error,
                    now_unix_ms(),
                    operation_id.as_str(),
                    source_deployment_id.as_str(),
                ],
            )
            .map_err(storage_error)?;
        if updated != 1 {
            return Err(RepositoryError::RollbackOperationConflict(
                operation_id.as_str().to_owned(),
            ));
        }

        if state == RollbackOperationState::Succeeded {
            let consumed = transaction
                .execute(
                    "UPDATE rollback_references
                     SET state = 'consumed', updated_at_unix_ms = ?1
                     WHERE deployment_id = ?2 AND state = 'active'",
                    params![now_unix_ms(), source_deployment_id.as_str()],
                )
                .map_err(storage_error)?;
            if consumed != 1 {
                return Err(RepositoryError::RollbackUnavailable(format!(
                    "active rollback reference disappeared for {}",
                    source_deployment_id.as_str()
                )));
            }
        }
        transaction.commit().map_err(storage_error)
    }

    fn get_operation(
        &self,
        operation_id: &RollbackOperationId,
    ) -> RepositoryResult<Option<RollbackOperation>> {
        self.connection
            .query_row(
                "SELECT id, source_deployment_id, application_id, environment_id, state
                 FROM rollback_operations WHERE id = ?1",
                params![operation_id.as_str()],
                operation_row,
            )
            .optional()
            .map_err(storage_error)?
            .map(decode_operation)
            .transpose()
    }
}

impl RollbackRetentionRepository for SqliteRollbackRepository {
    fn prune_inactive_reference_snapshots(
        &mut self,
        cutoff_unix_ms: i64,
        limit: usize,
    ) -> RepositoryResult<usize> {
        if limit == 0 {
            return Ok(0);
        }
        let limit = i64::try_from(limit)
            .map_err(|_| RepositoryError::Storage("retention batch size is too large".into()))?;
        let transaction = self.connection.transaction().map_err(storage_error)?;
        let pruned_at_unix_ms = now_unix_ms();
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO rollback_reference_retention (
                     deployment_id, snapshot_pruned_at_unix_ms
                 )
                 SELECT rr.deployment_id, ?1
                 FROM rollback_references rr
                 WHERE rr.state IN ('superseded', 'consumed')
                   AND rr.updated_at_unix_ms <= ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM rollback_reference_retention retention
                       WHERE retention.deployment_id = rr.deployment_id
                   )
                 ORDER BY rr.updated_at_unix_ms ASC, rr.deployment_id ASC
                 LIMIT ?3",
                params![pruned_at_unix_ms, cutoff_unix_ms, limit],
            )
            .map_err(storage_error)?;
        if inserted > 0 {
            transaction
                .execute(
                    "UPDATE rollback_references
                     SET target = '',
                         backup_path = '',
                         install_path = '',
                         rollback_task = '',
                         restart_task = '',
                         health_check_task = ''
                     WHERE deployment_id IN (
                         SELECT deployment_id
                         FROM rollback_reference_retention
                         WHERE snapshot_pruned_at_unix_ms = ?1
                     )
                       AND state IN ('superseded', 'consumed')",
                    params![pruned_at_unix_ms],
                )
                .map_err(storage_error)?;
        }
        transaction.commit().map_err(storage_error)?;
        Ok(inserted)
    }
}

fn source_deployment(
    transaction: &Transaction<'_>,
    deployment_id: &DeploymentId,
) -> RepositoryResult<Option<(String, String, String, i64)>> {
    transaction
        .query_row(
            "SELECT application_id, environment_id, state, rowid
             FROM deployments WHERE id = ?1",
            params![deployment_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(storage_error)
}

fn ensure_source_is_latest(
    transaction: &Transaction<'_>,
    application: &str,
    environment: &str,
    source_rowid: i64,
) -> RepositoryResult<()> {
    let newer_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM deployments
                WHERE application_id = ?1 AND environment_id = ?2 AND rowid > ?3
            )",
            params![application, environment, source_rowid],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if newer_exists {
        Err(RepositoryError::RollbackUnavailable(
            "a newer deployment exists for this application/environment".into(),
        ))
    } else {
        Ok(())
    }
}

fn reference_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReferenceRow> {
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

fn decode_reference(row: ReferenceRow) -> RepositoryResult<RollbackReference> {
    let (
        deployment_id,
        application,
        environment,
        target,
        backup_path,
        install_path,
        rollback_task,
        restart_task,
        health_check_task,
        state,
    ) = row;
    Ok(RollbackReference::rehydrate(
        DeploymentId::new(deployment_id).map_err(corrupt_domain)?,
        ApplicationId::new(application).map_err(corrupt_domain)?,
        EnvironmentId::new(environment).map_err(corrupt_domain)?,
        target,
        backup_path,
        install_path,
        rollback_task,
        restart_task,
        health_check_task,
        parse_reference_state(&state)?,
    ))
}

fn operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_operation(row: OperationRow) -> RepositoryResult<RollbackOperation> {
    let (id, source_deployment_id, application, environment, state) = row;
    Ok(RollbackOperation::rehydrate(
        RollbackOperationId::new(id).map_err(corrupt_rollback)?,
        DeploymentId::new(source_deployment_id).map_err(corrupt_domain)?,
        ApplicationId::new(application).map_err(corrupt_domain)?,
        EnvironmentId::new(environment).map_err(corrupt_domain)?,
        parse_operation_state(&state)?,
    ))
}

fn parse_reference_state(value: &str) -> RepositoryResult<RollbackReferenceState> {
    match value {
        "active" => Ok(RollbackReferenceState::Active),
        "superseded" => Ok(RollbackReferenceState::Superseded),
        "consumed" => Ok(RollbackReferenceState::Consumed),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown rollback reference state: {other}"
        ))),
    }
}

fn operation_state_name(state: RollbackOperationState) -> &'static str {
    match state {
        RollbackOperationState::Started => "started",
        RollbackOperationState::Succeeded => "succeeded",
        RollbackOperationState::Failed => "failed",
    }
}

fn parse_operation_state(value: &str) -> RepositoryResult<RollbackOperationState> {
    match value {
        "started" => Ok(RollbackOperationState::Started),
        "succeeded" => Ok(RollbackOperationState::Succeeded),
        "failed" => Ok(RollbackOperationState::Failed),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown rollback operation state: {other}"
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
