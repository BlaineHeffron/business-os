-- packet_proposals slice: run ledger for Smart draft proposals. Draft rows
-- remain owned by each packet kind's existing draft table; this table records
-- the planner/filler run and per-kind outcomes only.

ALTER TABLE work_items
    ADD COLUMN accept_actor TEXT;

UPDATE work_items
SET accept_actor = CASE WHEN status = 'accepted' THEN 'operator' ELSE NULL END
WHERE accept_actor IS NULL;

CREATE INDEX work_items_accept_actor
    ON work_items (client_id, status, accept_actor, updated_at_ms DESC, item_id);

CREATE TABLE packet_proposal_runs (
    client_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    item_id TEXT,
    resolved_decision_mode TEXT NOT NULL,
    execution_mode TEXT NOT NULL,
    status TEXT NOT NULL,
    candidate_packet_kinds_json TEXT NOT NULL DEFAULT '[]',
    outcomes_json TEXT NOT NULL DEFAULT '[]',
    model TEXT,
    confidence TEXT,
    error_code TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, run_id)
);

CREATE INDEX packet_proposal_runs_source_status
    ON packet_proposal_runs (
        client_id,
        source_kind,
        source_ref,
        status,
        updated_at_ms DESC,
        run_id
    );

CREATE INDEX packet_proposal_runs_item_status
    ON packet_proposal_runs (client_id, item_id, status, updated_at_ms DESC, run_id)
    WHERE item_id IS NOT NULL
      AND status IN ('running', 'completed', 'failed');
