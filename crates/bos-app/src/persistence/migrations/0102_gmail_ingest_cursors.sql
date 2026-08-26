CREATE TABLE gmail_ingest_cursors (
    client_id TEXT NOT NULL,
    account_ref TEXT NOT NULL,
    query_hash TEXT NOT NULL,
    next_page_token TEXT,
    last_advanced_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, account_ref)
);
