CREATE TABLE email_inbound_enrichments (
    client_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    parser_id TEXT NOT NULL,
    parsed_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, source_key, parser_id)
);

CREATE INDEX email_inbound_enrichments_source
    ON email_inbound_enrichments (client_id, source_key);
