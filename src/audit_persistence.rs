//! Read-only SQLite projection for structured deployment audit/history.
//!
//! The audit model intentionally does not duplicate lifecycle writes into a
//! second event store. Deployment transitions/step attempts, rollback records,
//! and recovery/acknowledgement rows are already durable authoritative facts.
//! This adapter projects those facts into one bounded deployment timeline.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::domain::DeploymentId;
use crate::ports::{
    AuditEvent, AuditEventKind, AuditRepository, AuditSubjectKind, RepositoryError,
    RepositoryResult,
};

const INITIAL_MIGRATION: &str = include_str!("../migrations/001_initial.sql");
const RECOVERY_MIGRATION: &str = include_str!("../migrations/002_startup_recovery.sql");
const ACKNOWLEDGEMENT_MIGRATION: &str =
    include_str!("../migrations/003_operator_reconciliation.sql");
const IDENTITY_MIGRATION: &str = include_str!("../migrations/004_deployment_identity.sql");

const HISTORY_QUERY: &str = r#"
WITH audit_history AS (
    SELECT
        d.created_at_unix_ms AS occurred_at_unix_ms,
        10 AS sort_rank,
        0 AS source_id,
        d.application_id AS application_id,
        d.environment_id AS environment_id,
        'deployment' AS subject_kind,
        d.id AS subject_id,
        'deployment_created' AS event_kind,
        json_object(
            'version', d.artifact_version,
            'size_bytes', d.artifact_size_bytes,
            'sha256', d.artifact_sha256
        ) AS attributes_json
    FROM deployments d
    WHERE d.id = ?1

    UNION ALL

    SELECT
        t.occurred_at_unix_ms,
        20,
        t.id,
        d.application_id,
        d.environment_id,
        'deployment',
        t.deployment_id,
        'deployment_transition',
        json_object('from_state', t.from_state, 'to_state', t.to_state)
    FROM deployment_transitions t
    JOIN deployments d ON d.id = t.deployment_id
    WHERE t.deployment_id = ?1

    UNION ALL

    SELECT
        s.started_at_unix_ms,
        30,
        s.id,
        d.application_id,
        d.environment_id,
        'deployment',
        s.deployment_id,
        'deployment_step_started',
        json_object('attempt_id', s.id, 'step', s.step)
    FROM deployment_step_attempts s
    JOIN deployments d ON d.id = s.deployment_id
    WHERE s.deployment_id = ?1

    UNION ALL

    SELECT
        s.finished_at_unix_ms,
        40,
        s.id,
        d.application_id,
        d.environment_id,
        'deployment',
        s.deployment_id,
        CASE s.status
            WHEN 'succeeded' THEN 'deployment_step_succeeded'
            ELSE 'deployment_step_failed'
        END,
        json_object('attempt_id', s.id, 'step', s.step, 'error', s.error)
    FROM deployment_step_attempts s
    JOIN deployments d ON d.id = s.deployment_id
    WHERE s.deployment_id = ?1
      AND s.status IN ('succeeded', 'failed')
      AND s.finished_at_unix_ms IS NOT NULL

    UNION ALL

    SELECT
        rr.created_at_unix_ms,
        50,
        d.rowid,
        rr.application_id,
        rr.environment_id,
        'rollback_reference',
        rr.deployment_id,
        'rollback_reference_recorded',
        json_object()
    FROM rollback_references rr
    JOIN deployments d ON d.id = rr.deployment_id
    WHERE rr.deployment_id = ?1

    UNION ALL

    SELECT
        rr.updated_at_unix_ms,
        60,
        d.rowid,
        rr.application_id,
        rr.environment_id,
        'rollback_reference',
        rr.deployment_id,
        CASE rr.state
            WHEN 'superseded' THEN 'rollback_reference_superseded'
            ELSE 'rollback_reference_consumed'
        END,
        json_object()
    FROM rollback_references rr
    JOIN deployments d ON d.id = rr.deployment_id
    WHERE rr.deployment_id = ?1
      AND rr.state IN ('superseded', 'consumed')

    UNION ALL

    SELECT
        ro.created_at_unix_ms,
        70,
        ro.rowid,
        ro.application_id,
        ro.environment_id,
        'rollback_operation',
        ro.id,
        'rollback_operation_started',
        json_object('source_deployment_id', ro.source_deployment_id)
    FROM rollback_operations ro
    WHERE ro.source_deployment_id = ?1

    UNION ALL

    SELECT
        ro.finished_at_unix_ms,
        80,
        ro.rowid,
        ro.application_id,
        ro.environment_id,
        'rollback_operation',
        ro.id,
        CASE ro.state
            WHEN 'succeeded' THEN 'rollback_operation_succeeded'
            ELSE 'rollback_operation_failed'
        END,
        json_object('source_deployment_id', ro.source_deployment_id, 'error', ro.error)
    FROM rollback_operations ro
    WHERE ro.source_deployment_id = ?1
      AND ro.state IN ('succeeded', 'failed')
      AND ro.finished_at_unix_ms IS NOT NULL

    UNION ALL

    SELECT
        ri.created_at_unix_ms,
        90,
        ri.id,
        ri.application_id,
        ri.environment_id,
        'recovery_incident',
        CAST(ri.id AS TEXT),
        'recovery_incident_recorded',
        json_object(
            'recovery_subject_kind', ri.subject_kind,
            'recovery_subject_id', ri.subject_id,
            'previous_state', ri.previous_state,
            'disposition', ri.disposition,
            'reason', ri.reason
        )
    FROM recovery_incidents ri
    WHERE (ri.subject_kind = 'deployment' AND ri.subject_id = ?1)
       OR (
            ri.subject_kind = 'rollback_operation'
            AND EXISTS (
                SELECT 1
                FROM rollback_operations ro
                WHERE ro.id = ri.subject_id
                  AND ro.source_deployment_id = ?1
            )
       )

    UNION ALL

    SELECT
        ra.acknowledged_at_unix_ms,
        100,
        ra.incident_id,
        ra.application_id,
        ra.environment_id,
        'recovery_incident',
        CAST(ra.incident_id AS TEXT),
        'recovery_acknowledged',
        json_object('operator', ra.operator)
    FROM recovery_acknowledgements ra
    JOIN recovery_incidents ri ON ri.id = ra.incident_id
    WHERE (ri.subject_kind = 'deployment' AND ri.subject_id = ?1)
       OR (
            ri.subject_kind = 'rollback_operation'
            AND EXISTS (
                SELECT 1
                FROM rollback_operations ro
                WHERE ro.id = ri.subject_id
                  AND ro.source_deployment_id = ?1
            )
       )
)
SELECT
    application_id,
    environment_id,
    subject_kind,
    subject_id,
    event_kind,
    attributes_json,
    occurred_at_unix_ms
FROM audit_history
ORDER BY occurred_at_unix_ms ASC, sort_rank ASC, source_id ASC
LIMIT ?2
"#;

type AuditRow = (String, String, String, String, String, String, i64);

pub struct SqliteAuditRepository {
    connection: Mutex<Connection>,
}

impl SqliteAuditRepository {
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
        connection
            .execute_batch(ACKNOWLEDGEMENT_MIGRATION)
            .map_err(storage_error)?;
        connection
            .execute_batch(IDENTITY_MIGRATION)
            .map_err(storage_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

impl AuditRepository for SqliteAuditRepository {
    fn events_for_deployment(
        &self,
        deployment_id: &DeploymentId,
        limit: usize,
    ) -> RepositoryResult<Vec<AuditEvent>> {
        let limit = i64::try_from(limit)
            .map_err(|_| RepositoryError::Storage("audit history limit is too large".into()))?;
        let connection = self.connection.lock().map_err(|_| {
            RepositoryError::Storage("audit repository connection lock poisoned".into())
        })?;
        let mut statement = connection.prepare(HISTORY_QUERY).map_err(storage_error)?;
        let rows = statement
            .query_map(params![deployment_id.as_str(), limit], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .map_err(storage_error)?;

        rows.map(|row| {
            let row = row.map_err(storage_error)?;
            decode_event(deployment_id, row)
        })
        .collect()
    }
}

fn decode_event(deployment_id: &DeploymentId, row: AuditRow) -> RepositoryResult<AuditEvent> {
    let (application, environment, subject_kind, subject_id, kind, attributes, occurred_at) = row;
    let attributes: Value = serde_json::from_str(&attributes).map_err(|error| {
        RepositoryError::CorruptData(format!("invalid audit attributes JSON: {error}"))
    })?;
    if !attributes.is_object() {
        return Err(RepositoryError::CorruptData(
            "audit attributes must be a JSON object".into(),
        ));
    }

    Ok(AuditEvent {
        deployment_id: deployment_id.clone(),
        application,
        environment,
        subject_kind: parse_subject_kind(&subject_kind)?,
        subject_id,
        kind: parse_event_kind(&kind)?,
        attributes,
        occurred_at_unix_ms: occurred_at,
    })
}

fn parse_subject_kind(value: &str) -> RepositoryResult<AuditSubjectKind> {
    match value {
        "deployment" => Ok(AuditSubjectKind::Deployment),
        "rollback_reference" => Ok(AuditSubjectKind::RollbackReference),
        "rollback_operation" => Ok(AuditSubjectKind::RollbackOperation),
        "recovery_incident" => Ok(AuditSubjectKind::RecoveryIncident),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown audit subject kind: {other}"
        ))),
    }
}

fn parse_event_kind(value: &str) -> RepositoryResult<AuditEventKind> {
    match value {
        "deployment_created" => Ok(AuditEventKind::DeploymentCreated),
        "deployment_transition" => Ok(AuditEventKind::DeploymentTransition),
        "deployment_step_started" => Ok(AuditEventKind::DeploymentStepStarted),
        "deployment_step_succeeded" => Ok(AuditEventKind::DeploymentStepSucceeded),
        "deployment_step_failed" => Ok(AuditEventKind::DeploymentStepFailed),
        "rollback_reference_recorded" => Ok(AuditEventKind::RollbackReferenceRecorded),
        "rollback_reference_superseded" => Ok(AuditEventKind::RollbackReferenceSuperseded),
        "rollback_reference_consumed" => Ok(AuditEventKind::RollbackReferenceConsumed),
        "rollback_operation_started" => Ok(AuditEventKind::RollbackOperationStarted),
        "rollback_operation_succeeded" => Ok(AuditEventKind::RollbackOperationSucceeded),
        "rollback_operation_failed" => Ok(AuditEventKind::RollbackOperationFailed),
        "recovery_incident_recorded" => Ok(AuditEventKind::RecoveryIncidentRecorded),
        "recovery_acknowledged" => Ok(AuditEventKind::RecoveryAcknowledged),
        other => Err(RepositoryError::CorruptData(format!(
            "unknown audit event kind: {other}"
        ))),
    }
}

fn storage_error(error: rusqlite::Error) -> RepositoryError {
    RepositoryError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_is_stable_for_unknown_deployment() {
        let repository = SqliteAuditRepository::in_memory().unwrap();
        let id = DeploymentId::new("missing").unwrap();
        assert!(repository
            .events_for_deployment(&id, 100)
            .unwrap()
            .is_empty());
    }
}
