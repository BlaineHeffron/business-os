-- crm_record_drafts slice (packet kind `crm_record_create`): staged proposals
-- to CREATE the CRM records a note references when they do not already exist —
-- a Company and/or a Contact. ONE draft, ONE approval, ONE ensure-chain
-- provider write (account -> contact). The produce stage runs a bounded LIVE
-- EspoCRM search and proposes ONLY the missing records; names are grounded
-- (a record with an invented name is refused). Approval enqueues the
-- create-records outbox job in the same transaction; provider ids are filled
-- on delivery. Idempotent on redelivery (search-before-create in the chain).

CREATE TABLE crm_record_drafts (
    client_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'staged',   -- staged | approved | rejected
    create_company INTEGER NOT NULL DEFAULT 0,
    company_name TEXT,
    company_website TEXT,
    company_phone TEXT,
    company_address TEXT,
    create_contact INTEGER NOT NULL DEFAULT 0,
    contact_first_name TEXT,
    contact_last_name TEXT,
    contact_email TEXT,
    contact_phone TEXT,
    contact_title TEXT,
    -- {"account_id":...,"contact_id":...} written when the ensure-chain delivers.
    provider_ids_json TEXT NOT NULL DEFAULT '{}',
    provenance_json TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    confidence TEXT NOT NULL DEFAULT '',
    outbox_job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, draft_id)
);

-- One active (non-rejected) draft per work item — the house produce-race guard.
CREATE UNIQUE INDEX crm_record_drafts_active_item
    ON crm_record_drafts (client_id, item_id)
    WHERE status != 'rejected';

CREATE INDEX crm_record_drafts_item
    ON crm_record_drafts (client_id, item_id, created_at_ms DESC);
