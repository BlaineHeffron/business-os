CREATE INDEX receipts_by_correlation
    ON receipts (client_id, correlation_id, created_at_ms DESC)
    WHERE correlation_id IS NOT NULL;

CREATE INDEX outbox_jobs_by_correlation
    ON outbox_jobs (client_id, correlation_id, created_at_ms DESC)
    WHERE correlation_id IS NOT NULL;
