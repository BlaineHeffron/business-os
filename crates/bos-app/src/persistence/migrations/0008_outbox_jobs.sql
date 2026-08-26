-- Outbox spine: every external (provider) effect is a row here, enqueued in
-- the SAME transaction as the domain write that authorized it. A leased
-- delivery worker executes post-commit; attempts and outcomes are receipted
-- through store_core (entity_kind = 'outbox_job').

CREATE TABLE outbox_jobs (
    client_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    capability TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending | delivered | failed_terminal
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_ms INTEGER NOT NULL,
    leased_until_ms INTEGER,
    last_error TEXT,
    -- Sanitized delivery result metadata (provider ids, dry_run flag, etc).
    result_json TEXT,
    source_entity_kind TEXT NOT NULL,
    source_entity_id TEXT NOT NULL,
    correlation_id TEXT,
    causation_id TEXT,
    idempotency_key TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, job_id)
);

CREATE INDEX outbox_jobs_due
    ON outbox_jobs (client_id, status, next_attempt_at_ms);
