-- Discovered Google Search Console properties and one local selected property.
-- Env/overlay property_url remains the highest-priority override.

CREATE TABLE search_console_properties (
    client_id TEXT NOT NULL,
    site_url TEXT NOT NULL,
    permission_level TEXT NOT NULL,
    discovered_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, site_url)
);

CREATE TABLE search_console_property_selection (
    client_id TEXT NOT NULL PRIMARY KEY,
    site_url TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
