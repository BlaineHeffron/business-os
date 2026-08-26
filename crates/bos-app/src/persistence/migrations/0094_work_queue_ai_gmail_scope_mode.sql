ALTER TABLE work_queue_policies
    ADD COLUMN ai_suggestible_gmail_scope TEXT NOT NULL DEFAULT 'default';

UPDATE work_queue_policies
SET ai_suggestible_gmail_scope = 'all'
WHERE category_id = 'inbound_email'
  AND ai_suggestible_packet_kinds_json <> '[]'
  AND ai_suggestible_gmail_categories_json = '[]';
