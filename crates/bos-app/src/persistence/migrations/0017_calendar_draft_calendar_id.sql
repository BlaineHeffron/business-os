-- Per-draft calendar target: the operator picks which calendar an approved
-- event writes to. NULL = the server default (BOS_GOOGLE_CALENDAR_ID).
ALTER TABLE calendar_event_drafts
    ADD COLUMN calendar_id TEXT;
