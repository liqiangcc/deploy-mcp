CREATE TABLE IF NOT EXISTS deployment_model_metadata (
    deployment_id TEXT PRIMARY KEY,
    mechanism_kind TEXT NOT NULL,
    release_identity_json TEXT NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS rollback_reference_model (
    deployment_id TEXT PRIMARY KEY,
    mechanism_kind TEXT NOT NULL,
    contract_fingerprint TEXT NOT NULL CHECK (length(contract_fingerprint) = 64),
    mechanism_snapshot_json TEXT NOT NULL,
    FOREIGN KEY (deployment_id) REFERENCES rollback_references(deployment_id) ON DELETE CASCADE
);
