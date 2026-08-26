CREATE TABLE IF NOT EXISTS shopify_order_snapshots (
    client_id TEXT NOT NULL,
    provider_order_id TEXT NOT NULL,
    order_number TEXT NOT NULL,
    customer_email TEXT,
    customer_name TEXT,
    total_cents INTEGER NOT NULL,
    currency TEXT,
    financial_status TEXT,
    fulfillment_status TEXT,
    tracking_number TEXT,
    tracking_carrier TEXT,
    tracking_url TEXT,
    line_items_summary TEXT NOT NULL DEFAULT '',
    line_items_json TEXT NOT NULL DEFAULT '[]',
    order_created_at TEXT,
    provider_updated_at TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, provider_order_id)
);

CREATE INDEX IF NOT EXISTS idx_shopify_order_snapshots_customer_email
    ON shopify_order_snapshots(client_id, customer_email);

CREATE INDEX IF NOT EXISTS idx_shopify_order_snapshots_created
    ON shopify_order_snapshots(client_id, order_created_at DESC);

CREATE TABLE IF NOT EXISTS shopify_customer_snapshots (
    client_id TEXT NOT NULL,
    provider_customer_id TEXT NOT NULL,
    email TEXT,
    name TEXT,
    phone TEXT,
    total_spent_cents INTEGER NOT NULL,
    currency TEXT,
    orders_count INTEGER NOT NULL,
    tags TEXT NOT NULL DEFAULT '',
    tier TEXT,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, provider_customer_id)
);

CREATE INDEX IF NOT EXISTS idx_shopify_customer_snapshots_email
    ON shopify_customer_snapshots(client_id, email);

CREATE TABLE IF NOT EXISTS shopify_sales_sync_state (
    client_id TEXT PRIMARY KEY,
    shop_domain_fingerprint TEXT,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    order_backfill_complete INTEGER NOT NULL DEFAULT 0,
    customer_backfill_complete INTEGER NOT NULL DEFAULT 0,
    order_backfill_cursor TEXT,
    customer_backfill_cursor TEXT,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    last_order_count INTEGER NOT NULL DEFAULT 0,
    last_customer_count INTEGER NOT NULL DEFAULT 0
);
