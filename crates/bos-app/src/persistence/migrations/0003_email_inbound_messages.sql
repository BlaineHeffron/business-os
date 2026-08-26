-- email_triage slice: classified inbound messages from the ingestion pump.
-- One row per Gmail message; body is stored as a bounded excerpt only.
-- Every insert flows through store_core, so ingestion is receipt-audited.

CREATE TABLE email_inbound_messages (
    client_id TEXT NOT NULL,
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
    PRIMARY KEY (client_id, message_id)
);

CREATE INDEX email_inbound_messages_recent
    ON email_inbound_messages (client_id, internal_date_ms DESC, message_id);
