CREATE TABLE workflow_steps (
    client_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    node TEXT NOT NULL,
    node_kind TEXT NOT NULL,
    input_hash TEXT,
    output_hash TEXT,
    decision TEXT,
    llm_usage_json TEXT,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    error_code TEXT,
    receipt_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, run_id, step_index)
);

CREATE INDEX workflow_steps_run
    ON workflow_steps (client_id, run_id, step_index);

CREATE TABLE quote_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL,
    customer_name TEXT NOT NULL,
    summary TEXT NOT NULL,
    line_items_json TEXT NOT NULL,
    subtotal_cents INTEGER NOT NULL,
    policy_notes_json TEXT NOT NULL,
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX quote_drafts_active_run
    ON quote_drafts (client_id, run_id)
    WHERE status != 'rejected';
