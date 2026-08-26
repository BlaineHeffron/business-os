-- Claim packets (port #6, slice claim_drafts): shipping damage-claim drafting.
-- Stockforge stores pack/damage evidence; BusinessOS caches damage events
-- locally (the claims pump), assembles the packet deterministically, and
-- gates approval on agent_monitor's four required evidence roles. Approval stages a
-- Gmail draft (manual carrier/platform filing) + a follow-up tracking task.

-- Order cards gain the shipment linkage the claim packet joins on.
ALTER TABLE stockforge_order_snapshots ADD COLUMN shipment_id TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN ship_date TEXT;
ALTER TABLE stockforge_order_snapshots ADD COLUMN photo_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stockforge_order_snapshots ADD COLUMN pack_station_container_id TEXT;

CREATE INDEX stockforge_order_snapshots_by_shipment
    ON stockforge_order_snapshots (client_id, shipment_id);

-- Damage events cached from GET /api/v1/damage (claimStatus=OPEN).
CREATE TABLE stockforge_damage_snapshots (
    client_id TEXT NOT NULL,
    damage_event_id TEXT NOT NULL,
    shipment_id TEXT NOT NULL,
    reported_at TEXT,
    reported_by TEXT NOT NULL DEFAULT 'INTERNAL',
    severity TEXT NOT NULL DEFAULT 'MEDIUM',
    damage_type TEXT NOT NULL DEFAULT '',
    photos_json TEXT NOT NULL DEFAULT '[]',
    description TEXT,
    claim_status TEXT NOT NULL DEFAULT 'OPEN',
    claim_amount_cents INTEGER,
    shipment_number TEXT,
    carrier TEXT,
    tracking_number TEXT,
    shipment_status TEXT,
    -- Pack-station photos fetched separately (urls); empty until fetched.
    pack_photos_json TEXT NOT NULL DEFAULT '[]',
    pack_photos_fetched INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT NOT NULL DEFAULT '',
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, damage_event_id)
);

CREATE INDEX stockforge_damage_snapshots_by_status
    ON stockforge_damage_snapshots (client_id, claim_status);

-- Claims pump state (rate-limit standdown + last error surface).
CREATE TABLE claims_sync_cursors (
    client_id TEXT NOT NULL,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    PRIMARY KEY (client_id)
);

CREATE TABLE claim_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    -- Deterministic shipment/order grounding (never model-chosen).
    tracking_number TEXT,
    carrier TEXT,
    shipment_number TEXT,
    order_number TEXT,
    customer_name TEXT,
    order_total_cents INTEGER,
    ship_date TEXT,
    damage_type TEXT NOT NULL DEFAULT '',
    damage_severity TEXT NOT NULL DEFAULT '',
    damage_reported_at TEXT,
    claim_amount_cents INTEGER NOT NULL,
    -- The one model-filled field pair (grounded, editable until approval).
    damage_narrative TEXT NOT NULL,
    item_description TEXT NOT NULL DEFAULT '',
    evidence_json TEXT NOT NULL DEFAULT '{}',
    packet_ready INTEGER NOT NULL DEFAULT 0,
    packet_json TEXT NOT NULL DEFAULT '{}',
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    follow_up_task_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX claim_drafts_active_item
    ON claim_drafts (client_id, item_id)
    WHERE status != 'rejected';
