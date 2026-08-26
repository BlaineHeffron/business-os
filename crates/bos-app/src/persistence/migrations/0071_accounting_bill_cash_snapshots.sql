-- accounting provider caches for Demo Home Financial Overview.
-- These are provider READ snapshots, not BusinessOS source-of-truth tables.

CREATE TABLE accounting_bill_snapshots (
    client_id TEXT NOT NULL,
    provider_bill_id TEXT NOT NULL,
    vendor_id TEXT,
    vendor_name TEXT,
    txn_date TEXT,
    due_date TEXT,
    total_amt_cents INTEGER NOT NULL DEFAULT 0,
    balance_cents INTEGER NOT NULL DEFAULT 0,
    voided INTEGER NOT NULL DEFAULT 0,
    provider_updated_at TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, provider_bill_id)
);

CREATE INDEX accounting_bill_snapshots_open
    ON accounting_bill_snapshots (client_id, balance_cents, due_date);
CREATE INDEX accounting_bill_snapshots_txn
    ON accounting_bill_snapshots (client_id, txn_date);

CREATE TABLE accounting_balance_sheet_snapshots (
    client_id TEXT NOT NULL,
    as_of_date TEXT NOT NULL,
    cash_on_hand_cents INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, as_of_date)
);
