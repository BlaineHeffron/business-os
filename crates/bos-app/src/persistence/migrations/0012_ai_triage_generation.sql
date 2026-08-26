-- AI re-triage reset support: a per-message generation counter. Resetting
-- bumps the generation, and the AI-triage result write keys its idempotency
-- on (message, generation) — so a reset message can receive a NEW verdict
-- while replays within one generation stay quiet.

ALTER TABLE email_inbound_messages
    ADD COLUMN ai_triage_generation INTEGER NOT NULL DEFAULT 0;
