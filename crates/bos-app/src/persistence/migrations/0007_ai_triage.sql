-- AI triage pass (tier 2): runs only on fallback mail no rule matched.
-- Status on the message row prevents re-spending LLM calls; AI-suggested
-- work items are flagged and carry the model's rationale.

ALTER TABLE email_inbound_messages ADD COLUMN ai_triage_status TEXT;
ALTER TABLE email_inbound_messages ADD COLUMN ai_triage_rationale TEXT;
ALTER TABLE email_inbound_messages ADD COLUMN ai_triaged_at_ms INTEGER;

ALTER TABLE work_items ADD COLUMN ai_suggested INTEGER NOT NULL DEFAULT 0;
ALTER TABLE work_items ADD COLUMN rationale TEXT NOT NULL DEFAULT '';
