ALTER TABLE email_reply_drafts ADD COLUMN reply_message_id TEXT;
ALTER TABLE email_reply_drafts ADD COLUMN reference_message_ids_json TEXT NOT NULL DEFAULT '[]';
