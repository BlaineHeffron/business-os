-- follow_up_tasks slice: staged follow-up-task drafts (typed fill from
-- accepted work items) and the LOCAL tasks table approval writes into.
-- No outbox involvement: approval IS the write, one receipted transaction.

CREATE TABLE follow_up_task_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    title TEXT NOT NULL,
    due_date TEXT,
    context TEXT NOT NULL DEFAULT '',
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    task_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

-- One ACTIVE (staged or approved) draft per work item; rejected drafts are
-- history and a re-produce creates the next attempt.
CREATE UNIQUE INDEX follow_up_task_drafts_active_item
    ON follow_up_task_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX follow_up_task_drafts_item
    ON follow_up_task_drafts (client_id, item_id, created_at_ms DESC);

CREATE TABLE tasks (
    client_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    title TEXT NOT NULL,
    due_date TEXT,
    context TEXT NOT NULL DEFAULT '',
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',  -- open | done
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, task_id)
);

CREATE INDEX tasks_feed
    ON tasks (client_id, status, due_date, created_at_ms DESC);
