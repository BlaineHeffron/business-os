-- invoice_drafts slice: staged Stripe invoice drafts (typed fill from
-- accepted work items — notes/emails describing billable work), awaiting
-- operator approval. Line amounts are provenance-grounded; totals are
-- recomputed server-side. Approval enqueues the Stripe create-invoice-draft
-- write as an outbox job in the same transaction (the invoice stays a
-- Stripe DRAFT even when the write gate is open — finalize/send is human).

CREATE TABLE invoice_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    customer_name TEXT NOT NULL,
    customer_email TEXT,                    -- required at APPROVAL, not staging
    currency TEXT NOT NULL DEFAULT 'usd',
    line_items_json TEXT NOT NULL DEFAULT '[]',
    subtotal_cents INTEGER NOT NULL DEFAULT 0,
    total_cents INTEGER NOT NULL DEFAULT 0,
    due_date TEXT,                          -- YYYY-MM-DD, only when stated
    memo TEXT NOT NULL DEFAULT '',
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX invoice_drafts_active_item
    ON invoice_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX invoice_drafts_item
    ON invoice_drafts (client_id, item_id, created_at_ms DESC);
