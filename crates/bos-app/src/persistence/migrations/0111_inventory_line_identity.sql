-- Per-material demand and inbound evidence from the line arrays already
-- embedded in the cached Stockforge order-board and purchase-order GETs.
-- Old rows start incomplete and cannot be called dead stock until refreshed.
ALTER TABLE stockforge_order_snapshots
    ADD COLUMN line_material_ids_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE stockforge_order_snapshots
    ADD COLUMN line_identity_complete INTEGER NOT NULL DEFAULT 0;

ALTER TABLE stockforge_po_snapshots
    ADD COLUMN line_material_ids_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE stockforge_po_snapshots
    ADD COLUMN line_identity_complete INTEGER NOT NULL DEFAULT 0;
