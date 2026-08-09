CREATE TABLE IF NOT EXISTS recovery_acknowledgements (
    incident_id INTEGER PRIMARY KEY,
    application_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('deployment', 'rollback_operation')),
    subject_id TEXT NOT NULL,
    operator TEXT NOT NULL CHECK (length(trim(operator)) > 0),
    evidence TEXT NOT NULL CHECK (length(trim(evidence)) > 0),
    acknowledged_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (incident_id) REFERENCES recovery_incidents(id)
);

CREATE INDEX IF NOT EXISTS idx_recovery_acknowledgement_environment
    ON recovery_acknowledgements(application_id, environment_id, acknowledged_at_unix_ms);
