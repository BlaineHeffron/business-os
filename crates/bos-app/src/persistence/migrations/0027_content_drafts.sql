-- Content drafts (port #5 part 2, slice content_drafts): grounded drafting
-- over the local Drive corpus. Evidence + claims + the deterministic
-- citation-gate verdict are persisted with the draft so the audit trail of
-- what the model grounded survives. DRAFT-ONLY: approval has no provider
-- write; publish stays manual.

CREATE TABLE content_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',  -- staged | approved | rejected
    title TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    target_query TEXT,
    meta_description TEXT,
    claims_json TEXT NOT NULL DEFAULT '[]',
    evidence_json TEXT NOT NULL DEFAULT '[]',
    gate_passed INTEGER NOT NULL DEFAULT 0,
    gate_json TEXT NOT NULL DEFAULT '{}',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX content_drafts_active_item
    ON content_drafts (client_id, item_id)
    WHERE status != 'rejected';
