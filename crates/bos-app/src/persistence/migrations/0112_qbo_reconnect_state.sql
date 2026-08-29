-- Keep a safe, durable signal when Intuit rejects the stored rotating token.
-- The UI can request reconnection without exposing token data.
ALTER TABLE qbo_credentials
    ADD COLUMN reconnect_required INTEGER NOT NULL DEFAULT 0;

ALTER TABLE qbo_credentials
    ADD COLUMN connection_error_code TEXT;

ALTER TABLE qbo_credentials
    ADD COLUMN connection_error_at_ms INTEGER;
