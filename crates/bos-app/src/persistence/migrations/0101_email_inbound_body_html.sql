ALTER TABLE email_inbound_messages
ADD COLUMN body_html TEXT NOT NULL DEFAULT '';
