CREATE TABLE lead_findings (
    client_id TEXT NOT NULL,
    finding_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    status TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    contact_hint TEXT,
    company_hint TEXT,
    matched_terms_json TEXT NOT NULL DEFAULT '[]',
    provenance_json TEXT NOT NULL,
    work_item_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, finding_id),
    CHECK (status IN ('staged', 'accepted', 'rejected'))
);

CREATE INDEX idx_lead_findings_client_status_updated
    ON lead_findings (client_id, status, updated_at_ms DESC);
