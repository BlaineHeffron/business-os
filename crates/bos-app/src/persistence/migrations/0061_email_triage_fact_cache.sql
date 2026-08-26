CREATE TABLE IF NOT EXISTS email_triage_fact_cache (
    client_id TEXT NOT NULL,
    fact_key TEXT NOT NULL,
    fact_json TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    provider TEXT,
    fetched_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    last_error TEXT,
    PRIMARY KEY (client_id, fact_key)
);

CREATE INDEX IF NOT EXISTS email_triage_fact_cache_expiry
    ON email_triage_fact_cache (client_id, expires_at_ms);
