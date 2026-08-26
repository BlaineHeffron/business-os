-- qbo_views slice: read-only QuickBooks Online connector + snapshot caches.
-- These tables are PROVIDER CACHES (read models of QBO data), never
-- source-of-truth — QBO stays the accounting system of record.

-- ONE credential per client: QBO connects a company (realm), not an operator.
-- The refresh token ROTATES on every refresh; access_token is a short-lived
-- cache so steady cycles skip the token endpoint. Tokens never enter receipts.
CREATE TABLE qbo_credentials (
    client_id TEXT NOT NULL,
    realm_id TEXT NOT NULL,
    environment TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    refresh_token_expires_at_ms INTEGER NOT NULL,
    access_token TEXT,
    access_token_expires_at_ms INTEGER NOT NULL DEFAULT 0,
    connected_by_user_id TEXT NOT NULL,
    connected_at_ms INTEGER NOT NULL,
    last_refreshed_at_ms INTEGER,
    PRIMARY KEY (client_id)
);

-- Per-entity incremental sync cursor. A walk = one pass over
-- "updated since <walk_since>" (NULL = initial backfill), paged by
-- next_start_position; completing it promotes walk_max_updated_at to
-- high_water_updated_at. Cursors only advance after their page's rows commit,
-- so a failed cycle resumes exactly where it stopped.
CREATE TABLE qbo_sync_cursors (
    client_id TEXT NOT NULL,
    entity TEXT NOT NULL,
    high_water_updated_at TEXT,
    walk_since TEXT,
    walk_max_updated_at TEXT,
    next_start_position INTEGER NOT NULL DEFAULT 1,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    PRIMARY KEY (client_id, entity)
);

CREATE TABLE qbo_invoice_snapshots (
    client_id TEXT NOT NULL,
    qbo_invoice_id TEXT NOT NULL,
    doc_number TEXT,
    customer_id TEXT,
    customer_name TEXT,
    txn_date TEXT,
    due_date TEXT,
    total_amt_cents INTEGER NOT NULL DEFAULT 0,
    balance_cents INTEGER NOT NULL DEFAULT 0,
    voided INTEGER NOT NULL DEFAULT 0,
    qbo_updated_at TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, qbo_invoice_id)
);

CREATE INDEX qbo_invoice_snapshots_open
    ON qbo_invoice_snapshots (client_id, balance_cents, due_date);
CREATE INDEX qbo_invoice_snapshots_txn
    ON qbo_invoice_snapshots (client_id, txn_date);

CREATE TABLE qbo_customer_snapshots (
    client_id TEXT NOT NULL,
    qbo_customer_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    company_name TEXT,
    email TEXT,
    phone TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    tier TEXT,
    tier_source TEXT NOT NULL DEFAULT 'not_provided',
    qbo_updated_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, qbo_customer_id)
);
