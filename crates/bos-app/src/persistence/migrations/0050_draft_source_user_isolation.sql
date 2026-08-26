ALTER TABLE calendar_event_drafts ADD COLUMN source_user_id TEXT;
ALTER TABLE email_reply_drafts    ADD COLUMN source_user_id TEXT;
ALTER TABLE crm_note_drafts       ADD COLUMN source_user_id TEXT;
