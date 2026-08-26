-- Drive RAG corpus (port #5, slice drive_corpus): local document snapshots,
-- deterministic chunks, and an FTS5 (BM25) lexical index over the configured
-- Google Drive folders. The browser and the content_drafts query path only
-- ever read these tables — Drive itself is touched solely by the
-- request-budgeted sync pump.

CREATE TABLE drive_doc_snapshots (
    client_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL DEFAULT '',
    modified_time TEXT NOT NULL DEFAULT '',
    version TEXT,
    parent_folder_ids_json TEXT NOT NULL DEFAULT '[]',
    web_view_link TEXT,
    -- stale: metadata seen, text not yet (re)indexed
    -- indexed | skipped (no supported text form) | error | removed
    status TEXT NOT NULL DEFAULT 'stale',
    content_hash TEXT NOT NULL DEFAULT '',
    chunk_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    first_seen_at_ms INTEGER NOT NULL,
    last_synced_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, file_id)
);

CREATE INDEX drive_doc_snapshots_by_status
    ON drive_doc_snapshots (client_id, status);

CREATE TABLE drive_chunks (
    client_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,            -- "<file_id>:<seq>"
    file_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    heading_path_json TEXT NOT NULL DEFAULT '[]',
    start_offset INTEGER NOT NULL,
    end_offset INTEGER NOT NULL,
    text TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, chunk_id)
);

CREATE INDEX drive_chunks_by_file ON drive_chunks (client_id, file_id, seq);

-- Lexical index. doc_title/heading_path columns carry the document context
-- (the deterministic 80% of Anthropic's contextual retrieval); BM25 column
-- weights are applied at query time. Rows are maintained by the slice store
-- in the same transaction as drive_chunks.
CREATE VIRTUAL TABLE drive_chunks_fts USING fts5(
    client_id UNINDEXED,
    chunk_id UNINDEXED,
    file_id UNINDEXED,
    doc_title,
    heading_path,
    text
);

CREATE TABLE drive_sync_cursors (
    client_id TEXT NOT NULL,
    corpus_id TEXT NOT NULL DEFAULT 'default',
    -- Hash of the corpus pointer config; a change resets the backfill walk
    -- and locally re-evaluates stored docs against the new rules.
    config_hash TEXT NOT NULL DEFAULT '',
    start_page_token TEXT,
    -- Mid-walk continuation inside the changes feed (large change sets).
    pending_page_token TEXT,
    backfill_folder_index INTEGER NOT NULL DEFAULT 0,
    backfill_page_token TEXT,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    PRIMARY KEY (client_id, corpus_id)
);
