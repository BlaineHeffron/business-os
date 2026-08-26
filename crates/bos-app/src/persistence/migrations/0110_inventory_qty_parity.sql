-- Reserved and incoming come from the Stockforge material payload
-- (reservedQty / onOrderQty). Nullable so a missing field stays unknown
-- instead of being invented as zero. Available is computed at read time.
ALTER TABLE stockforge_material_snapshots
    ADD COLUMN reserved_qty REAL;

ALTER TABLE stockforge_material_snapshots
    ADD COLUMN incoming_qty REAL;
