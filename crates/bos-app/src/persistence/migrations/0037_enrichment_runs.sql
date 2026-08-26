CREATE TABLE enrichment_runs (
  client_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  slice_id TEXT NOT NULL,
  draft_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  subject TEXT NOT NULL,
  status TEXT NOT NULL,
  started_at_ms INTEGER NOT NULL,
  finished_at_ms INTEGER,
  plan_json TEXT NOT NULL,
  diagnostics_json TEXT NOT NULL,
  proposals_json TEXT NOT NULL,
  cost_micros INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL,
  PRIMARY KEY (client_id, run_id)
);

CREATE INDEX idx_enrichment_runs_draft
  ON enrichment_runs (client_id, slice_id, draft_id, started_at_ms DESC);

CREATE INDEX idx_enrichment_runs_item
  ON enrichment_runs (client_id, item_id, started_at_ms DESC);
