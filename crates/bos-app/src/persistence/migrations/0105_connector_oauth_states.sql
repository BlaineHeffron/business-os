CREATE TABLE connector_oauth_states (
    client_id      TEXT    NOT NULL,
    connector      TEXT    NOT NULL CHECK (connector <> ''),
    state_hash     TEXT    NOT NULL CHECK (state_hash <> ''),
    user_id        TEXT    NOT NULL CHECK (user_id <> ''),
    issued_at_ms   INTEGER NOT NULL CHECK (issued_at_ms >= 0),
    expires_at_ms  INTEGER NOT NULL CHECK (expires_at_ms > issued_at_ms),
    PRIMARY KEY (client_id, connector, state_hash)
);

CREATE INDEX idx_connector_oauth_states_expiry
    ON connector_oauth_states (expires_at_ms);
