-- 0e-3: each operator can pick the calendar their approved event drafts
-- default to (their own credential writes it). NULL = fall back to
-- BOS_GOOGLE_CALENDAR_ID, then "primary".
ALTER TABLE operator_users ADD COLUMN default_calendar_id TEXT;
