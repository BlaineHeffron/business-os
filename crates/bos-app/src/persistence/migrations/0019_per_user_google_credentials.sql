-- 0e-2 multi-user Google accounts: credentials key off the operator user.
-- The pre-existing single credential (connected before logins existed) maps
-- to the shared 'operator' identity, so a deployed instance keeps working.
CREATE TABLE google_oauth_credentials_v2 (
    client_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    service TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    scopes_json TEXT NOT NULL DEFAULT '[]',
    connected_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, user_id, service)
);

INSERT INTO google_oauth_credentials_v2
    (client_id, user_id, service, refresh_token, scopes_json, connected_at_ms)
SELECT client_id, 'operator', service, refresh_token, scopes_json, connected_at_ms
FROM google_oauth_credentials;

DROP TABLE google_oauth_credentials;
ALTER TABLE google_oauth_credentials_v2 RENAME TO google_oauth_credentials;

-- Which connected account an ingested message / work item came from.
-- NULL = legacy rows and env-credential ingestion (single-account mode).
ALTER TABLE email_inbound_messages ADD COLUMN source_user_id TEXT;
ALTER TABLE work_items ADD COLUMN source_user_id TEXT;
