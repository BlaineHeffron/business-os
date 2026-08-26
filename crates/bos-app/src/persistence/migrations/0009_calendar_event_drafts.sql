-- calendar_drafts slice: staged calendar-event drafts produced (typed Extract)
-- from accepted work items, awaiting operator approval. Approval enqueues the
-- provider write as an outbox job in the same transaction.

CREATE TABLE calendar_event_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    title TEXT NOT NULL,
    start_at TEXT NOT NULL,
    end_at TEXT NOT NULL,
    timezone TEXT,
    location TEXT,
    description TEXT,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

-- One ACTIVE (staged or approved) draft per work item; rejected drafts stay
-- as history and a re-produce creates the next attempt.
CREATE UNIQUE INDEX calendar_event_drafts_active_item
    ON calendar_event_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX calendar_event_drafts_item
    ON calendar_event_drafts (client_id, item_id, created_at_ms DESC);
