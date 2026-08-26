-- Stockforge replaced decorative item types with explicit stock behavior.
-- Nullable on purpose: pre-migration cache rows are not assumed alertable;
-- the next material sync supplies authoritative behavior.
ALTER TABLE stockforge_material_snapshots
    ADD COLUMN is_purchasable INTEGER;

ALTER TABLE stockforge_material_snapshots
    ADD COLUMN replenishment_policy TEXT;

ALTER TABLE stockforge_material_snapshots
    ADD COLUMN sale_depletion_policy TEXT;

CREATE INDEX idx_stockforge_materials_replenishment
    ON stockforge_material_snapshots
       (client_id, is_active, is_purchasable, replenishment_policy);
