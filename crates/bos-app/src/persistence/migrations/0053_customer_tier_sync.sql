-- QBO customer tiers -> Shopify customer tier sync staging.
-- Preview/stage is a receipted local mutation; approval enqueues the Shopify
-- outbox job in the same transaction. Live provider writes remain gate-bound.

CREATE TABLE customer_tier_sync_runs (
    client_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    plan_json TEXT NOT NULL,
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, run_id)
);

CREATE INDEX customer_tier_sync_runs_recent
    ON customer_tier_sync_runs (client_id, created_at_ms DESC);
