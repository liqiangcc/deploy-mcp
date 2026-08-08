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
