-- GA4 Data API snapshots for website behavior/acquisition reporting. Read-only
-- provider data; rendered from sqlite by owner reports and home dashboard.

CREATE TABLE google_analytics_sync_cursors (
    client_id TEXT NOT NULL,
    property_id TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    synced_start_date TEXT,
    synced_end_date TEXT,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_synced_at_ms INTEGER,
    PRIMARY KEY (client_id, property_id)
);

CREATE TABLE google_analytics_daily_metrics (
    client_id TEXT NOT NULL,
    property_id TEXT NOT NULL,
    date TEXT NOT NULL,
    sessions INTEGER NOT NULL,
    total_users INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    conversions INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, property_id, date)
);

CREATE TABLE google_analytics_dimension_metrics (
    client_id TEXT NOT NULL,
    property_id TEXT NOT NULL,
    date TEXT NOT NULL,
    dimension_type TEXT NOT NULL,
    dimension_value TEXT NOT NULL,
    sessions INTEGER NOT NULL,
    total_users INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    conversions INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, property_id, date, dimension_type, dimension_value)
);

CREATE INDEX google_analytics_daily_date
    ON google_analytics_daily_metrics (client_id, property_id, date DESC);

CREATE INDEX google_analytics_dimension_lookup
    ON google_analytics_dimension_metrics
    (client_id, property_id, dimension_type, date DESC, sessions DESC);
