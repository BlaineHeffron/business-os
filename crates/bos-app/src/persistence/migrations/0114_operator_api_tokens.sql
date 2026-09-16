-- Machine operator credentials with an explicit capability list. Secrets are
-- stored as a domain-separated SHA-256 of the bearer (never the bearer
-- itself). Unscoped env/personal tokens stay in BOS_OPERATOR_TOKEN /
-- operator_users; this table is additive.
CREATE TABLE operator_api_tokens (
    client_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    label TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    capabilities_json TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    created_by TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    revoked_at_ms INTEGER,
    PRIMARY KEY (client_id, token_id)
);

CREATE UNIQUE INDEX idx_operator_api_tokens_hash
    ON operator_api_tokens (client_id, token_hash);
