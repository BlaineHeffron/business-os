-- Web fact evidence gathered for content drafts before the drafting LLM.
-- The content draft still persists the exact evidence bundle it saw; this
-- table is the store_core-backed, retry-reloadable record of web snippets
-- produced by an enrichment run.

CREATE TABLE content_web_facts (
    client_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    run_id TEXT NOT NULL,
    snippet_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    doc_title TEXT NOT NULL,
    heading_path_json TEXT NOT NULL DEFAULT '[]',
    text TEXT NOT NULL,
    web_view_link TEXT,
    rank INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, target_id, snippet_id)
);

CREATE INDEX content_web_facts_run
    ON content_web_facts (client_id, run_id, rank);

CREATE INDEX content_web_facts_target
    ON content_web_facts (client_id, target_id, rank);
