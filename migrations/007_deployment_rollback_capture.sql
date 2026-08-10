CREATE TABLE IF NOT EXISTS deployment_rollback_captures (
    deployment_id TEXT PRIMARY KEY,
    mechanism_kind TEXT NOT NULL,
    target TEXT NOT NULL,
    contract_fingerprint TEXT NOT NULL CHECK (length(contract_fingerprint) = 64),
    mechanism_snapshot_json TEXT NOT NULL,
    captured_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);
