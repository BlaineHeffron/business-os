CREATE TABLE release_note_dismissals (
    client_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    release_note_id TEXT NOT NULL,
    dismissed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, user_id, release_note_id),
    FOREIGN KEY (client_id, release_note_id)
        REFERENCES release_notes (client_id, release_note_id)
        ON DELETE CASCADE
);

CREATE INDEX release_note_dismissals_by_note
    ON release_note_dismissals (client_id, release_note_id);
