-- ledger_drafts slice: staged "record received payment" drafts (typed fill
-- from accepted work items — e.g. Stripe receipt emails), awaiting operator
-- approval. Approval enqueues the accounting-provider record_receipt write
-- as an outbox job in the same transaction.

CREATE TABLE ledger_entry_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    payer_name TEXT NOT NULL,
    payer_email TEXT,
    amount_cents INTEGER NOT NULL,
    paid_date TEXT NOT NULL,                -- YYYY-MM-DD, grounded
    description TEXT NOT NULL DEFAULT '',
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX ledger_entry_drafts_active_item
    ON ledger_entry_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX ledger_entry_drafts_item
    ON ledger_entry_drafts (client_id, item_id, created_at_ms DESC);
