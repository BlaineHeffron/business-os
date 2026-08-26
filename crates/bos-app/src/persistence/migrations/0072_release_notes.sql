CREATE TABLE release_notes (
    client_id TEXT NOT NULL,
    release_note_id TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    body TEXT,
    build_sha TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, release_note_id)
);

CREATE INDEX release_notes_recent
    ON release_notes (client_id, created_at_ms DESC, release_note_id DESC);
