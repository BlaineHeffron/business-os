-- email_triage slice: operator-managed classification rules.
-- Revision tracking and audit live in the receipt spine (0001), not here.

CREATE TABLE email_triage_rules (
    client_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    rule_json TEXT NOT NULL,
    priority INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    deleted INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, rule_id)
);

CREATE INDEX email_triage_rules_active
    ON email_triage_rules (client_id, deleted, priority, rule_id);
