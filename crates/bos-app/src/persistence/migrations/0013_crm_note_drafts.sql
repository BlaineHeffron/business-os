-- crm_drafts slice: staged CRM note drafts (typed fill from accepted work
-- items), awaiting operator approval. Approval enqueues the HubSpot
-- note-create as an outbox job in the same transaction.

CREATE TABLE crm_note_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    note_body TEXT NOT NULL,
    contact_email TEXT,
    occurred_at TEXT NOT NULL,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX crm_note_drafts_active_item
    ON crm_note_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX crm_note_drafts_item
    ON crm_note_drafts (client_id, item_id, created_at_ms DESC);
