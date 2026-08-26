-- Preserve full inbound email bodies for server-side AI/produce grounding.
-- body_excerpt remains the bounded UI/list summary.

ALTER TABLE email_inbound_messages
ADD COLUMN body_full TEXT NOT NULL DEFAULT '';
