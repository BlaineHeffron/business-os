-- Provider-neutral shipment/order context for shipping damage claim drafts.
-- Existing Stockforge-backed cache tables stay as the current read adapter;
-- these nullable columns let the draft contract carry Shopify/order-source
-- metadata without introducing provider writes or claim submission.

ALTER TABLE claim_drafts ADD COLUMN shipment_context_source TEXT;
ALTER TABLE claim_drafts ADD COLUMN order_platform TEXT;
ALTER TABLE claim_drafts ADD COLUMN external_order_id TEXT;
