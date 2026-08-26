CREATE TABLE crm_contact_snapshots (
    client_id TEXT NOT NULL,
    provider_contact_id TEXT NOT NULL,
    email TEXT,
    full_name TEXT,
    company TEXT,
    phone TEXT,
    lifecycle_stage TEXT,
    owner TEXT,
    last_activity_at TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, provider_contact_id)
);

CREATE INDEX crm_contact_snapshots_email_idx
    ON crm_contact_snapshots (client_id, email);

CREATE INDEX crm_contact_snapshots_company_idx
    ON crm_contact_snapshots (client_id, company COLLATE NOCASE);

CREATE TABLE crm_deal_snapshots (
    client_id TEXT NOT NULL,
    provider_deal_id TEXT NOT NULL,
    deal_name TEXT,
    stage TEXT,
    amount_cents INTEGER,
    currency TEXT,
    pipeline TEXT,
    close_date TEXT,
    associated_contact_email TEXT,
    associated_company TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, provider_deal_id)
);

CREATE INDEX crm_deal_snapshots_contact_email_idx
    ON crm_deal_snapshots (client_id, associated_contact_email);

CREATE TABLE crm_cache_sync_cursors (
    client_id TEXT NOT NULL,
    entity TEXT NOT NULL,
    next_after_cursor TEXT,
    high_water_modified_at TEXT,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    sync_started_at_ms INTEGER,
    rate_limited_until_ms INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    last_advanced_at_ms INTEGER,
    PRIMARY KEY (client_id, entity)
);
