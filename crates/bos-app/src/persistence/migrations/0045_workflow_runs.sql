CREATE TABLE workflow_runs (
    client_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    workflow TEXT NOT NULL,
    version TEXT NOT NULL,
    build_sha TEXT,
    status TEXT NOT NULL,
    input_snapshot_json TEXT NOT NULL,
    terminal_json TEXT,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, run_id)
);

CREATE INDEX workflow_runs_recent
    ON workflow_runs (client_id, updated_at_ms DESC);
