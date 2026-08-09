CREATE TABLE IF NOT EXISTS recovery_incidents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('deployment', 'rollback_operation')),
    subject_id TEXT NOT NULL,
    application_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    previous_state TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('auto_resolved', 'manual_reconciliation_required')),
    reason TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    resolved_at_unix_ms INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_recovery_incident_subject
    ON recovery_incidents(subject_kind, subject_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_recovery_incident_unresolved_environment
    ON recovery_incidents(application_id, environment_id)
    WHERE resolved_at_unix_ms IS NULL;

CREATE TRIGGER IF NOT EXISTS trg_deployment_blocked_by_recovery
BEFORE INSERT ON deployments
WHEN EXISTS (
    SELECT 1 FROM recovery_incidents
    WHERE application_id = NEW.application_id
      AND environment_id = NEW.environment_id
      AND resolved_at_unix_ms IS NULL
)
BEGIN
    SELECT RAISE(ABORT, 'recovery_required');
END;

CREATE TRIGGER IF NOT EXISTS trg_rollback_blocked_by_recovery
BEFORE INSERT ON rollback_operations
WHEN EXISTS (
    SELECT 1 FROM recovery_incidents
    WHERE application_id = NEW.application_id
      AND environment_id = NEW.environment_id
      AND resolved_at_unix_ms IS NULL
)
BEGIN
    SELECT RAISE(ABORT, 'recovery_required');
END;
