CREATE TABLE call_input_drive_settings (
    client_id TEXT PRIMARY KEY NOT NULL,
    credential_user_id TEXT,
    drive_folder_id TEXT,
    drive_folder_name TEXT,
    ingestion_enabled INTEGER NOT NULL DEFAULT 1,
    interval_secs INTEGER,
    updated_at_ms INTEGER NOT NULL
);
