CREATE TABLE runtime_setting_overrides (
    client_id TEXT NOT NULL,
    var_name TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, var_name)
);
