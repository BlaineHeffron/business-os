-- Client profile: per-client company background and owner/operator voice
-- used to ground outward-facing LLM tasks at produce time. Seeded from the
-- client overlay; one row per client.

CREATE TABLE client_profile (
    client_id TEXT NOT NULL PRIMARY KEY,
    company_name TEXT,
    bio TEXT,
    industry TEXT,
    website TEXT,
    persona TEXT,
    updated_at_ms INTEGER NOT NULL
);
