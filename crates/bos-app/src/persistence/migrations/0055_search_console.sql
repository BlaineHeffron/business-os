-- search_console slice: read-only Google Search Console snapshots for owner
-- reporting and dashboard traffic surfaces.

CREATE TABLE search_console_sync_cursors (
    client_id TEXT NOT NULL,
    property_url TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    synced_start_date TEXT,
    synced_end_date TEXT,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_synced_at_ms INTEGER,
    PRIMARY KEY (client_id, property_url)
);

CREATE TABLE search_console_daily_metrics (
    client_id TEXT NOT NULL,
    property_url TEXT NOT NULL,
    date TEXT NOT NULL,
    clicks INTEGER NOT NULL,
    impressions INTEGER NOT NULL,
    ctr_micros INTEGER NOT NULL,
    position_micros INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, property_url, date)
);

CREATE TABLE search_console_dimension_metrics (
    client_id TEXT NOT NULL,
    property_url TEXT NOT NULL,
    date TEXT NOT NULL,
    dimension_type TEXT NOT NULL,
    dimension_value TEXT NOT NULL,
    is_branded INTEGER NOT NULL DEFAULT 0,
    clicks INTEGER NOT NULL,
    impressions INTEGER NOT NULL,
    ctr_micros INTEGER NOT NULL,
    position_micros INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, property_url, date, dimension_type, dimension_value)
);

CREATE INDEX search_console_daily_date
    ON search_console_daily_metrics (client_id, property_url, date DESC);

CREATE INDEX search_console_dimension_lookup
    ON search_console_dimension_metrics
    (client_id, property_url, dimension_type, date DESC, clicks DESC);
