-- Provider-neutral shipment reference envelopes for parcel, LTL, and
-- shipping-platform claim workflows. Legacy scalar carrier/tracking fields
-- remain for compatibility and existing UI consumers.

ALTER TABLE stockforge_damage_snapshots ADD COLUMN shipment_refs_json TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN shipment_refs_json TEXT;
ALTER TABLE claim_drafts ADD COLUMN shipment_refs_json TEXT;
