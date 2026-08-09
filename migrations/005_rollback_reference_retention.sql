CREATE TABLE IF NOT EXISTS rollback_reference_retention (
    deployment_id TEXT PRIMARY KEY,
    snapshot_pruned_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES rollback_references(deployment_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_rollback_reference_retention_pruned_at
    ON rollback_reference_retention(snapshot_pruned_at_unix_ms, deployment_id);
