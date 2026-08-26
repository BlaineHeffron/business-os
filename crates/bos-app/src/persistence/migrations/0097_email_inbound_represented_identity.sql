ALTER TABLE email_inbound_messages
    ADD COLUMN represented_email TEXT;

ALTER TABLE email_inbound_messages
    ADD COLUMN represented_domain TEXT;

CREATE INDEX email_inbound_messages_represented_identity_idx
    ON email_inbound_messages (
        client_id,
        represented_domain
    );
