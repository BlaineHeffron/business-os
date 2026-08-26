-- owner_reports slice (port #7, W16): deterministic weekly + month-to-date
-- owner digest rows. Metrics are assembled from the LOCAL caches (accounting
-- snapshots, email triage, tasks, Stockforge order/damage snapshots) — money
-- figures are READ, never AI-generated; the narration fields hold the ONE
-- bounded LLM transform's prose. Deterministic report ids
-- (owr_<kind>_<period_start>) make regeneration an upsert, never a duplicate.

CREATE TABLE owner_reports (
    client_id TEXT NOT NULL,
    report_id TEXT NOT NULL,
    -- 'weekly' | 'mtd'
    period_kind TEXT NOT NULL,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    -- The civil date the metrics were assembled for (regenerate when stale).
    as_of_date TEXT NOT NULL,
    -- 'complete' | 'narration_failed' (metrics are always present).
    status TEXT NOT NULL,
    metrics_json TEXT NOT NULL,
    headline TEXT,
    narrative TEXT,
    callouts_json TEXT NOT NULL DEFAULT '[]',
    confidence TEXT,
    model TEXT,
    narration_error TEXT,
    -- Gmail draft delivery of the digest (gated outbox job), when staged.
    outbox_job_id TEXT,
    generated_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, report_id)
);

CREATE INDEX owner_reports_period
    ON owner_reports (client_id, period_kind, period_start DESC);
