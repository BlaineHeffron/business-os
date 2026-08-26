CREATE TABLE IF NOT EXISTS home_dashboard_preferences (
  client_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  widgets_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (client_id, user_id)
);
