-- invoice_drafts slice: operator-configurable invoicing defaults. v1 holds a
-- default due-date term (Net N days) applied at produce-time when the source
-- states no explicit date and no "Net N" term. NULL default_due_days = no
-- default (due date stays blank, as today). Revision is tracked by store_core.
CREATE TABLE invoice_settings (
    client_id TEXT PRIMARY KEY,
    default_due_days INTEGER,
    updated_at_ms INTEGER NOT NULL
);
