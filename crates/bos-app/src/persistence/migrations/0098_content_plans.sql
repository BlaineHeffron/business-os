-- Content planning (slice content_plans): local plan items that queue normal
-- content_draft work, plus the inventory tables PR3 will fill for deterministic
-- duplicate/cannibalization warnings. Publishing stays manual; no provider
-- writes are introduced by this migration.

CREATE TABLE content_plan_items (
    client_id TEXT NOT NULL,
    plan_item_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'planned',
    topic TEXT NOT NULL,
    angle TEXT,
    format TEXT,
    target_query TEXT,
    audience TEXT,
    notes TEXT,
    work_item_id TEXT,
    published_url TEXT,
    collision_summary_json TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, plan_item_id),
    CHECK (status IN ('planned', 'queued', 'published', 'cancelled'))
);

CREATE INDEX idx_content_plan_items_client_status_updated
    ON content_plan_items (client_id, status, updated_at_ms DESC);

CREATE INDEX idx_content_plan_items_work_item
    ON content_plan_items (client_id, work_item_id)
    WHERE work_item_id IS NOT NULL;

CREATE TABLE content_inventory_items (
    client_id TEXT NOT NULL,
    inventory_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL,
    title TEXT NOT NULL,
    target_query TEXT,
    url TEXT,
    summary TEXT,
    canonical_key TEXT NOT NULL,
    metrics_json TEXT NOT NULL DEFAULT '{}',
    last_seen_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, inventory_id),
    UNIQUE (client_id, canonical_key),
    CHECK (source_kind IN ('plan_item', 'search_console_page', 'manual')),
    CHECK (status IN ('pipeline', 'published', 'archived'))
);

CREATE INDEX idx_content_inventory_items_client_status_updated
    ON content_inventory_items (client_id, status, updated_at_ms DESC);

CREATE VIRTUAL TABLE content_inventory_fts USING fts5(
    client_id UNINDEXED,
    inventory_id UNINDEXED,
    title,
    target_query,
    url,
    summary
);
