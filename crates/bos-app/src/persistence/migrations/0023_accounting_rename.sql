-- accounting slice: provider-neutral cache names (was qbo_views). The same
-- snapshot tables now back any accounting provider (QuickBooks, Invoice
-- Ninja, …). qbo_credentials intentionally keeps its name: realm-bound
-- Intuit OAuth is QBO-specific; other providers configure via env and store
-- no credential row.
ALTER TABLE qbo_sync_cursors RENAME TO accounting_sync_cursors;

ALTER TABLE qbo_invoice_snapshots RENAME TO accounting_invoice_snapshots;
ALTER TABLE accounting_invoice_snapshots RENAME COLUMN qbo_invoice_id TO provider_invoice_id;
ALTER TABLE accounting_invoice_snapshots RENAME COLUMN qbo_updated_at TO provider_updated_at;

ALTER TABLE qbo_customer_snapshots RENAME TO accounting_customer_snapshots;
ALTER TABLE accounting_customer_snapshots RENAME COLUMN qbo_customer_id TO provider_customer_id;
ALTER TABLE accounting_customer_snapshots RENAME COLUMN qbo_updated_at TO provider_updated_at;

ALTER TABLE qbo_pnl_snapshots RENAME TO accounting_pnl_snapshots;

-- SQLite keeps the old index names through RENAME TABLE; recreate them.
DROP INDEX qbo_invoice_snapshots_open;
DROP INDEX qbo_invoice_snapshots_txn;
CREATE INDEX accounting_invoice_snapshots_open
    ON accounting_invoice_snapshots (client_id, balance_cents, due_date);
CREATE INDEX accounting_invoice_snapshots_txn
    ON accounting_invoice_snapshots (client_id, txn_date);
