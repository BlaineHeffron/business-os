-- Named operators with personal bearer tokens: every mutation receipt can
-- record WHO acted, and per-user provider credentials (0e-2) key off user_id.
CREATE TABLE operator_users (
    client_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    token TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, user_id)
);

CREATE UNIQUE INDEX idx_operator_users_token
    ON operator_users (client_id, token);
