-- email_drafts slice: staged reply drafts (typed fill from accepted work
-- items), awaiting operator approval. Approval enqueues a Gmail
-- DRAFT-create (never send) as an outbox job in the same transaction.

CREATE TABLE email_reply_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    to_addr TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    thread_id TEXT,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX email_reply_drafts_active_item
    ON email_reply_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX email_reply_drafts_item
    ON email_reply_drafts (client_id, item_id, created_at_ms DESC);
