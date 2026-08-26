ALTER TABLE work_queue_policies
    ADD COLUMN ai_suggestible_gmail_categories_json TEXT NOT NULL DEFAULT '[]';
