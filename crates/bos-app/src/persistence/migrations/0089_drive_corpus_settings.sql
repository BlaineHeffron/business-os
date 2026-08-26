-- Operator-selected Drive corpus folders. Overlay folder ids remain supported
-- as defaults that Settings can override; BOS_DRIVE_CORPUS_FOLDER_IDS is the
-- deployment pin that overrides/disables UI folder selection.

CREATE TABLE drive_corpus_settings (
    client_id TEXT NOT NULL,
    corpus_id TEXT NOT NULL DEFAULT 'default',
    credential_user_id TEXT,
    folder_ids_json TEXT NOT NULL DEFAULT '[]',
    folder_names_json TEXT NOT NULL DEFAULT '{}',
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, corpus_id)
);
