-- Append-only evidence records produced by deterministic grounding reads.
-- These rows are audit material only: ProduceFlavor::stage remains the only
-- draft writer, and stage never reads this table.
--
-- grounding_mode is intentionally constrained to deterministic for now. The
-- agentic Smart Draft path continues to use packet_proposal_run_evidence.

CREATE TABLE grounding_evidence (
    client_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    draft_id TEXT,
    packet_kind TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    grounding_mode TEXT NOT NULL CHECK (grounding_mode IN ('deterministic')),
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_args_json TEXT NOT NULL,
    result_ref TEXT NOT NULL,
    result_excerpt TEXT NOT NULL,
    scope_label TEXT NOT NULL CHECK (scope_label IN ('all', 'user')),
    scope_user_id TEXT,
    actor_id TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    correlation_id TEXT,
    causation_id TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, evidence_id),
    FOREIGN KEY (client_id, work_item_id)
        REFERENCES work_items (client_id, item_id)
        ON DELETE CASCADE,
    CHECK (
        (scope_label = 'all' AND scope_user_id IS NULL)
        OR (scope_label = 'user' AND scope_user_id IS NOT NULL)
    )
);

CREATE INDEX grounding_evidence_work_item
    ON grounding_evidence (
        client_id,
        work_item_id,
        packet_kind,
        attempt,
        created_at_ms DESC,
        evidence_id
    );

CREATE INDEX grounding_evidence_draft
    ON grounding_evidence (client_id, draft_id, created_at_ms DESC, evidence_id)
    WHERE draft_id IS NOT NULL;
