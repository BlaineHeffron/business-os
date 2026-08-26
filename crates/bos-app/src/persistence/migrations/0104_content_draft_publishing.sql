-- Explicit publish requests for approved content drafts. The referenced job
-- carries the client-specific external effect through the shared outbox.

ALTER TABLE content_drafts ADD COLUMN publish_outbox_job_id TEXT;

