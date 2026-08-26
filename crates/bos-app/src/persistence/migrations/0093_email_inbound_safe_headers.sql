ALTER TABLE email_inbound_messages
    ADD COLUMN headers_json TEXT NOT NULL DEFAULT '[]';
