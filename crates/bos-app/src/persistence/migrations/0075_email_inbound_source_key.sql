-- Preserve per-mailbox Gmail provenance by separating the BusinessOS source
-- identity from the raw Gmail message id. Legacy rows keep source_key = message_id.

CREATE TABLE email_inbound_messages_new (
    client_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id TEXT,
    internal_date_ms INTEGER,
    from_addr TEXT,
    to_addr TEXT,
    subject TEXT,
    body_excerpt TEXT NOT NULL DEFAULT '',
    labels_json TEXT NOT NULL DEFAULT '[]',
    resolved_category TEXT NOT NULL,
    matched_rule_id TEXT,
    ingested_at_ms INTEGER NOT NULL,
    ai_triage_status TEXT,
    ai_triage_rationale TEXT,
    ai_triaged_at_ms INTEGER,
    ai_triage_generation INTEGER NOT NULL DEFAULT 0,
    source_user_id TEXT,
    body_full TEXT NOT NULL DEFAULT '',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (client_id, source_key)
);

INSERT INTO email_inbound_messages_new (
    client_id, source_key, message_id, thread_id, internal_date_ms, from_addr, to_addr,
    subject, body_excerpt, labels_json, resolved_category, matched_rule_id, ingested_at_ms,
    ai_triage_status, ai_triage_rationale, ai_triaged_at_ms, ai_triage_generation,
    source_user_id, body_full, attachments_json
)
SELECT
    client_id, message_id, message_id, thread_id, internal_date_ms, from_addr, to_addr,
    subject, body_excerpt, labels_json, resolved_category, matched_rule_id, ingested_at_ms,
    ai_triage_status, ai_triage_rationale, ai_triaged_at_ms, ai_triage_generation,
    source_user_id, body_full, attachments_json
FROM email_inbound_messages;

DROP TABLE email_inbound_messages;
ALTER TABLE email_inbound_messages_new RENAME TO email_inbound_messages;

CREATE INDEX email_inbound_messages_recent
    ON email_inbound_messages (client_id, internal_date_ms DESC, source_key);

CREATE INDEX email_inbound_messages_gmail_id
    ON email_inbound_messages (client_id, message_id, source_user_id);
