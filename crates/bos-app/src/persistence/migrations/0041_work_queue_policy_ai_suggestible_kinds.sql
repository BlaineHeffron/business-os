-- Optional per-category packet kinds the AI triage pass may suggest for
-- specific emails. Existing packet_kinds_json remains the deterministic
-- always-on policy set.
ALTER TABLE work_queue_policies
    ADD COLUMN ai_suggestible_packet_kinds_json TEXT NOT NULL DEFAULT '[]';
