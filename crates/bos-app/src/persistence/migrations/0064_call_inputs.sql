CREATE TABLE call_inputs (
    client_id TEXT NOT NULL,
    call_input_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    input_kind TEXT NOT NULL,
    status TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    caller_name TEXT,
    caller_phone TEXT,
    caller_email TEXT,
    transcript_text TEXT NOT NULL,
    recording_ref_json TEXT NOT NULL,
    occurred_at_ms INTEGER,
    captured_at_ms INTEGER,
    work_item_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, call_input_id),
    CHECK (input_kind IN ('call_log', 'transcript', 'recording')),
    CHECK (status IN ('staged', 'accepted', 'rejected'))
);

CREATE INDEX idx_call_inputs_client_status_updated
    ON call_inputs (client_id, status, updated_at_ms DESC);

CREATE UNIQUE INDEX idx_call_inputs_client_source_ref
    ON call_inputs (client_id, source_id, source_ref);
