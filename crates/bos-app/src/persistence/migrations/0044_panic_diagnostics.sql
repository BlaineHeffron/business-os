CREATE TABLE panic_diagnostics (
    diagnostic_id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    message TEXT NOT NULL,
    location TEXT,
    backtrace TEXT NOT NULL,
    thread_name TEXT,
    occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX idx_panic_diagnostics_client_time
    ON panic_diagnostics(client_id, occurred_at_ms DESC, diagnostic_id DESC);
