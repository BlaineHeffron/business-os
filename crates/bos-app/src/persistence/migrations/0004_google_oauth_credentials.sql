-- google_connector slice: refresh tokens obtained via the operator
-- "connect" flow. One row per (client, google service). The receipt spine
-- audits connect/disconnect, but receipts NEVER contain the token itself.

CREATE TABLE google_oauth_credentials (
    client_id TEXT NOT NULL,
    service TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    scopes_json TEXT NOT NULL DEFAULT '[]',
    connected_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, service)
);
