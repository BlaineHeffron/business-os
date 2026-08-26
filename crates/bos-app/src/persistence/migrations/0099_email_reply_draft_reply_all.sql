ALTER TABLE email_reply_drafts ADD COLUMN cc_addrs_json TEXT NOT NULL DEFAULT '[]';
