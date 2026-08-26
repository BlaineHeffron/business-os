CREATE TABLE IF NOT EXISTS home_dashboard_hubspot_deal_mapping (
  client_id TEXT PRIMARY KEY,
  mapping_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
