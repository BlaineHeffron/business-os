-- email_triage slice: optional operator-only defaults for work-item agent launches.
-- Empty strings preserve the legacy launch behavior.

ALTER TABLE email_triage_categories
    ADD COLUMN default_agent_dir TEXT NOT NULL DEFAULT '';

ALTER TABLE email_triage_categories
    ADD COLUMN default_agent_context TEXT NOT NULL DEFAULT '';
