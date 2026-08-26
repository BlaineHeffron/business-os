-- crm_sales_intent slice (packet kind `crm_sales_intent`): staged pipeline
-- intent proposals kept separate from CRM account/contact record creation.

CREATE TABLE crm_sales_intent_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    source_user_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('staged', 'approved', 'rejected')),
    company_name TEXT,
    contact_name TEXT,
    contact_email TEXT,
    lead_title TEXT NOT NULL,
    intent_summary TEXT NOT NULL,
    rationale TEXT NOT NULL,
    qualification_status TEXT NOT NULL,
    next_step_text TEXT NOT NULL,
    follow_up_due_date TEXT,
    provider_target TEXT NOT NULL CHECK (provider_target IN ('lead', 'deal', 'task_only')),
    create_businessos_task INTEGER NOT NULL DEFAULT 0,
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL,
    confidence TEXT NOT NULL,
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

CREATE UNIQUE INDEX crm_sales_intent_drafts_active_item
    ON crm_sales_intent_drafts (client_id, item_id)
    WHERE status IN ('staged', 'approved');

CREATE INDEX crm_sales_intent_drafts_item
    ON crm_sales_intent_drafts (client_id, item_id, created_at_ms DESC);
