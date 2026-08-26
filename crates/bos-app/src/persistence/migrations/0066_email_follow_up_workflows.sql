-- email_drafts slice: outbound email follow-up workflows attached to an
-- approved Gmail DRAFT. The linked local task remains in follow_up_tasks.

CREATE TABLE email_outbound_follow_ups (
    client_id TEXT NOT NULL,
    follow_up_id TEXT NOT NULL,
    email_draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    thread_id TEXT,
    source_user_id TEXT,
    gmail_draft_outbox_job_id TEXT,
    follow_up_task_id TEXT,
    status TEXT NOT NULL DEFAULT 'active', -- active | resolved | cancelled | stale
    thread_state TEXT NOT NULL DEFAULT 'draft_created',
    due_date TEXT NOT NULL,
    follow_up_title TEXT NOT NULL,
    follow_up_context TEXT NOT NULL DEFAULT '',
    create_follow_up_draft INTEGER NOT NULL DEFAULT 0,
    sent_message_id TEXT,
    sent_at_ms INTEGER,
    reply_message_id TEXT,
    reply_at_ms INTEGER,
    resolution_reason TEXT,
    last_checked_at_ms INTEGER,
    last_check_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, follow_up_id)
);

CREATE UNIQUE INDEX email_outbound_follow_ups_active_draft
    ON email_outbound_follow_ups (client_id, email_draft_id)
    WHERE status NOT IN ('resolved', 'cancelled');

CREATE INDEX email_outbound_follow_ups_due
    ON email_outbound_follow_ups (client_id, status, due_date);

CREATE INDEX email_outbound_follow_ups_thread_user
    ON email_outbound_follow_ups (client_id, thread_id, source_user_id);

CREATE INDEX email_outbound_follow_ups_task
    ON email_outbound_follow_ups (client_id, follow_up_task_id);
