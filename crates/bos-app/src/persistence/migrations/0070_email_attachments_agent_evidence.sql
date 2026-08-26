ALTER TABLE email_inbound_messages
ADD COLUMN attachments_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE agent_evidence_files (
    client_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    item_id TEXT,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    attachment_id TEXT NOT NULL,
    path TEXT NOT NULL,
    filename TEXT NOT NULL,
    mime_type TEXT,
    size_bytes INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    last_used_at_ms INTEGER NOT NULL,
    retention_until_ms INTEGER NOT NULL,
    deleted_at_ms INTEGER,
    PRIMARY KEY (client_id, evidence_id)
);

CREATE INDEX agent_evidence_files_cleanup
    ON agent_evidence_files (client_id, retention_until_ms, deleted_at_ms);

CREATE INDEX agent_evidence_files_session
    ON agent_evidence_files (client_id, session_id, created_at_ms DESC);
