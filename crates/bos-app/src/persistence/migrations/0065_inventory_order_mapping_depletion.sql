-- Extra Stockforge order-board projection fields used by BusinessOS to
-- distinguish Shopify order visibility, SKU mapping state, and inventory
-- depletion outcome after mapping.
ALTER TABLE stockforge_order_snapshots ADD COLUMN external_order_id TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN customer_email TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN processed_at TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN mapped_line_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stockforge_order_snapshots ADD COLUMN depletion_total INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stockforge_order_snapshots ADD COLUMN depletion_applied INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stockforge_order_snapshots ADD COLUMN depletion_failed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stockforge_order_snapshots ADD COLUMN depletion_reversed INTEGER NOT NULL DEFAULT 0;
