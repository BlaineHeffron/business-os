ALTER TABLE calendar_event_drafts
ADD COLUMN attendees_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE calendar_event_drafts
ADD COLUMN send_invitations INTEGER NOT NULL DEFAULT 0;
