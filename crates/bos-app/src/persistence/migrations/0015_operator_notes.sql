-- operator_notes slice: manually logged notes (call notes, walk-ins,
-- reminders) — the second work-item source family after email. Each note
-- emits a work item on creation; produce kinds run over the note text.

CREATE TABLE operator_notes (
    client_id TEXT NOT NULL,
    note_id TEXT NOT NULL,
    body TEXT NOT NULL,
    category_id TEXT NOT NULL DEFAULT 'operator_note',
    created_by TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, note_id)
);

CREATE INDEX operator_notes_recent
    ON operator_notes (client_id, created_at_ms DESC);
