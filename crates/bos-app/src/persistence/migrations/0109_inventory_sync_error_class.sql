-- Stamp when a Stockforge cursor last failed so the refresh banner can
-- show the error class (rate limited / auth / timeout / error) without
-- treating a later successful walk as stale, and without storing 409
-- cooldown as an error.
ALTER TABLE stockforge_sync_cursors
ADD COLUMN last_error_class TEXT;

ALTER TABLE stockforge_sync_cursors
ADD COLUMN last_error_at_ms INTEGER;
