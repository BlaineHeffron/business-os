-- Append-only evidence records produced by packet proposal tool-loop turns.
-- Evidence is audit material only; draft staging still flows through each
-- packet kind's ProduceFlavor::stage() with its own prepared context.

CREATE TABLE packet_proposal_run_evidence (
    client_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    turn_index INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    tool_args_json TEXT NOT NULL,
    result_ref TEXT NOT NULL,
    result_excerpt TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, evidence_id)
);

CREATE INDEX packet_proposal_run_evidence_run
    ON packet_proposal_run_evidence (client_id, run_id, turn_index, evidence_id);
