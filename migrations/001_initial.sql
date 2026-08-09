PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS deployments (
    id TEXT PRIMARY KEY,
    application_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    artifact_version TEXT NOT NULL,
    artifact_size_bytes INTEGER NOT NULL CHECK (artifact_size_bytes > 0),
    artifact_sha256 TEXT NOT NULL CHECK (length(artifact_sha256) = 64),
    state TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_deployments_active_environment
    ON deployments(application_id, environment_id)
    WHERE state NOT IN ('succeeded', 'failed', 'rolled_back', 'rollback_failed');

CREATE TABLE IF NOT EXISTS deployment_transitions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deployment_id TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    occurred_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_deployment_transitions_deployment
    ON deployment_transitions(deployment_id, id);

CREATE TABLE IF NOT EXISTS deployment_step_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deployment_id TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('started', 'succeeded', 'failed')),
    error TEXT,
    started_at_unix_ms INTEGER NOT NULL,
    finished_at_unix_ms INTEGER,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_deployment_step_attempts_deployment
    ON deployment_step_attempts(deployment_id, id);

CREATE TABLE IF NOT EXISTS rollback_references (
    deployment_id TEXT PRIMARY KEY,
    application_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    target TEXT NOT NULL,
    backup_path TEXT NOT NULL,
    install_path TEXT NOT NULL,
    rollback_task TEXT NOT NULL,
    restart_task TEXT NOT NULL,
    health_check_task TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'superseded', 'consumed')),
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_rollback_reference_active_environment
    ON rollback_references(application_id, environment_id)
    WHERE state = 'active';

CREATE TABLE IF NOT EXISTS rollback_operations (
    id TEXT PRIMARY KEY,
    source_deployment_id TEXT NOT NULL,
    application_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('started', 'succeeded', 'failed')),
    error TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    finished_at_unix_ms INTEGER,
    FOREIGN KEY (source_deployment_id) REFERENCES deployments(id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_rollback_operation_active_environment
    ON rollback_operations(application_id, environment_id)
    WHERE state = 'started';

CREATE TRIGGER IF NOT EXISTS trg_deployment_blocked_by_rollback
BEFORE INSERT ON deployments
WHEN EXISTS (
    SELECT 1 FROM rollback_operations
    WHERE application_id = NEW.application_id
      AND environment_id = NEW.environment_id
      AND state = 'started'
)
BEGIN
    SELECT RAISE(ABORT, 'mutation_conflict');
END;

CREATE TRIGGER IF NOT EXISTS trg_rollback_blocked_by_deployment
BEFORE INSERT ON rollback_operations
WHEN EXISTS (
    SELECT 1 FROM deployments
    WHERE application_id = NEW.application_id
      AND environment_id = NEW.environment_id
      AND state NOT IN ('succeeded', 'failed', 'rolled_back', 'rollback_failed')
)
BEGIN
    SELECT RAISE(ABORT, 'mutation_conflict');
END;
