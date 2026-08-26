-- work_queue slice: the operator work-item feed + per-category packet policy.
-- Items are emitted when a classified input's category policy says so; the
-- accept/dismiss lifecycle is revisioned and receipted via the spine.

CREATE TABLE work_items (
    client_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    category_id TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    packet_kinds_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'open',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, item_id)
);

CREATE UNIQUE INDEX work_items_source
    ON work_items (client_id, source_kind, source_ref);

CREATE INDEX work_items_feed
    ON work_items (client_id, status, created_at_ms DESC, item_id);

CREATE TABLE work_queue_policies (
    client_id TEXT NOT NULL,
    category_id TEXT NOT NULL,
    create_work_item INTEGER NOT NULL DEFAULT 0,
    packet_kinds_json TEXT NOT NULL DEFAULT '[]',
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, category_id)
);
