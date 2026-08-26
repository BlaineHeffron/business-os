-- The receipt spine: every mutation in the system lands here, success or failure.
-- store_core is the only writer.

CREATE TABLE entity_revisions (
    client_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, entity_kind, entity_id)
);

CREATE TABLE receipts (
    receipt_id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    change_kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    outcome TEXT NOT NULL,
    error_class TEXT,
    before_json TEXT,
    after_json TEXT,
    revision_before INTEGER,
    revision_after INTEGER,
    idempotency_key TEXT NOT NULL,
    correlation_id TEXT,
    causation_id TEXT,
    created_at_ms INTEGER NOT NULL
);

-- Idempotency replay lookup: one applied receipt per (client, key).
CREATE UNIQUE INDEX receipts_idempotency
    ON receipts (client_id, idempotency_key)
    WHERE outcome = 'applied';

CREATE INDEX receipts_entity
    ON receipts (client_id, entity_kind, entity_id, created_at_ms);

CREATE INDEX receipts_created
    ON receipts (client_id, created_at_ms);
