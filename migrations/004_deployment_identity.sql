CREATE TABLE IF NOT EXISTS deployment_idempotency (
    idempotency_key TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL UNIQUE,
    created_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_deployment_version_identity
    ON deployments(application_id, environment_id, artifact_version);
