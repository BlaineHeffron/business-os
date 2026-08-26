-- email_triage slice: operator-configurable inbox chrome. v1 stores which
-- Gmail system tabs should be offered in the inbox UI. Revision is tracked by
-- store_core; the missing row default is all built-in Gmail categories visible.
CREATE TABLE email_triage_inbox_settings (
    client_id TEXT PRIMARY KEY,
    visible_gmail_categories_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
