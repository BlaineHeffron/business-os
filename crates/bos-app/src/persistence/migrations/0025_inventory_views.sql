-- Inventory cached views (Stockforge connector). No credential table on
-- purpose: the durable credential is the env-provided service-account login
-- (BOS_STOCKFORGE_EMAIL/PASSWORD); session tokens live in memory only.
-- Snapshot tables follow the qbo_views pattern: content_hash for
-- compare-before-write upserts so steady-state sync cycles stay quiet.

-- Per-entity sync state. Stockforge lists paginate by skip/take (no
-- updated-since filter), so the cursor is a plain offset for the in-progress
-- material walk; single-request entities use it for backoff/error state only.
CREATE TABLE stockforge_sync_cursors (
    client_id TEXT NOT NULL,
    entity TEXT NOT NULL,
    next_skip INTEGER NOT NULL DEFAULT 0,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    PRIMARY KEY (client_id, entity)
);

-- Stock on hand per material. Quantities are REAL (fractional gallons);
-- money is integer cents.
CREATE TABLE stockforge_material_snapshots (
    client_id TEXT NOT NULL,
    material_id TEXT NOT NULL,
    name TEXT NOT NULL,
    sku TEXT,
    category TEXT,
    quantity REAL NOT NULL DEFAULT 0,
    unit TEXT,
    warning_threshold REAL,
    critical_threshold REAL,
    threshold_type TEXT,
    unit_cost_cents INTEGER NOT NULL DEFAULT 0,
    lead_time_days INTEGER,
    vendor_name TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    sf_updated_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, material_id)
);

CREATE INDEX idx_stockforge_materials_active
    ON stockforge_material_snapshots (client_id, is_active);

-- ACTIVE low-stock alerts (full-set sync: resolved/acknowledged alerts are
-- pruned each cycle — Stockforge owns the alert lifecycle).
CREATE TABLE stockforge_alert_snapshots (
    client_id TEXT NOT NULL,
    alert_id TEXT NOT NULL,
    material_id TEXT,
    material_name TEXT,
    material_sku TEXT,
    severity TEXT NOT NULL,
    status TEXT NOT NULL,
    quantity REAL,
    threshold_value REAL,
    percentage_remaining REAL,
    message TEXT,
    sf_created_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, alert_id)
);

-- PENDING reorder suggestions (full-set sync, pruned like alerts).
CREATE TABLE stockforge_reorder_snapshots (
    client_id TEXT NOT NULL,
    suggestion_id TEXT NOT NULL,
    material_id TEXT,
    material_name TEXT,
    material_sku TEXT,
    vendor_name TEXT,
    urgency TEXT NOT NULL,
    status TEXT NOT NULL,
    current_quantity REAL,
    suggested_quantity REAL,
    unit TEXT,
    estimated_cost_cents INTEGER NOT NULL DEFAULT 0,
    days_until_stockout REAL,
    lead_time_days INTEGER,
    reasoning TEXT,
    sf_created_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, suggestion_id)
);

-- The cached live-order-board window (orders from the last N days, all
-- pipeline columns). Full-set sync per cycle; orders that leave the window
-- are pruned.
CREATE TABLE stockforge_order_snapshots (
    client_id TEXT NOT NULL,
    order_id TEXT NOT NULL,
    order_number TEXT NOT NULL,
    platform TEXT,
    board_status TEXT NOT NULL,
    raw_status TEXT,
    customer_name TEXT,
    total_amount_cents INTEGER NOT NULL DEFAULT 0,
    currency TEXT,
    order_date TEXT,
    item_count INTEGER NOT NULL DEFAULT 0,
    unit_count INTEGER NOT NULL DEFAULT 0,
    carrier TEXT,
    tracking_number TEXT,
    needs_mapping INTEGER NOT NULL DEFAULT 0,
    blocked INTEGER NOT NULL DEFAULT 0,
    deducted INTEGER NOT NULL DEFAULT 0,
    deduction_failed INTEGER NOT NULL DEFAULT 0,
    label_needed INTEGER NOT NULL DEFAULT 0,
    packed_missing_photo INTEGER NOT NULL DEFAULT 0,
    exception INTEGER NOT NULL DEFAULT 0,
    blocked_reasons_json TEXT NOT NULL DEFAULT '[]',
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, order_id)
);

CREATE INDEX idx_stockforge_orders_status
    ON stockforge_order_snapshots (client_id, board_status);

-- Purchase orders (inbound stock). Upsert-only: status changes flow through
-- (RECEIVED/CANCELLED rows stay, views filter). KNOWN SEAM: a PO hard-deleted
-- in Stockforge lingers here, mirroring the qbo_views delete seam.
CREATE TABLE stockforge_po_snapshots (
    client_id TEXT NOT NULL,
    po_id TEXT NOT NULL,
    vendor_name TEXT,
    status TEXT NOT NULL,
    total_estimated_cost_cents INTEGER NOT NULL DEFAULT 0,
    freight_mode TEXT,
    line_count INTEGER NOT NULL DEFAULT 0,
    sf_created_at TEXT,
    sf_sent_at TEXT,
    sf_received_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, po_id)
);
