-- Ad-hoc social sources may have no destination URL. Rebuild both social
-- tables so canonical_url is nullable without dropping operator history.

CREATE TABLE social_published_sources_new (
    client_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    external_id TEXT NOT NULL,
    source_content_draft_id TEXT,
    source_content_draft_revision INTEGER,
    canonical_url TEXT,
    title TEXT NOT NULL,
    excerpt TEXT,
    published_at TEXT,
    generation_status TEXT NOT NULL CHECK (
        generation_status IN ('ready', 'generating', 'proposal_staged', 'generation_failed')
    ),
    generation_run_id TEXT,
    generation_error TEXT,
    proposal_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, source_id),
    UNIQUE (client_id, source_kind, external_id)
);

INSERT INTO social_published_sources_new (
    client_id, source_id, source_kind, external_id, source_content_draft_id,
    source_content_draft_revision, canonical_url, title, excerpt, published_at,
    generation_status, generation_run_id, generation_error, proposal_id,
    created_at_ms, updated_at_ms
)
SELECT
    client_id, source_id, source_kind, external_id, source_content_draft_id,
    source_content_draft_revision, canonical_url, title, excerpt, published_at,
    generation_status, generation_run_id, generation_error, proposal_id,
    created_at_ms, updated_at_ms
FROM social_published_sources;

DROP TABLE social_published_sources;
ALTER TABLE social_published_sources_new RENAME TO social_published_sources;

CREATE INDEX social_published_sources_recent
    ON social_published_sources (client_id, updated_at_ms DESC);

CREATE TABLE social_post_proposals_new (
    client_id TEXT NOT NULL,
    proposal_id TEXT NOT NULL,
    source_id TEXT,
    source_content_draft_id TEXT,
    source_content_draft_revision INTEGER,
    canonical_url TEXT,
    status TEXT NOT NULL CHECK (status IN ('staged', 'approved', 'rejected')),
    targets_json TEXT NOT NULL,
    approved_by TEXT,
    approved_revision INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, proposal_id)
);

INSERT INTO social_post_proposals_new (
    client_id, proposal_id, source_id, source_content_draft_id,
    source_content_draft_revision, canonical_url, status, targets_json,
    approved_by, approved_revision, created_at_ms, updated_at_ms
)
SELECT
    client_id, proposal_id, source_id, source_content_draft_id,
    source_content_draft_revision, canonical_url, status, targets_json,
    approved_by, approved_revision, created_at_ms, updated_at_ms
FROM social_post_proposals;

DROP TABLE social_post_proposals;
ALTER TABLE social_post_proposals_new RENAME TO social_post_proposals;

CREATE INDEX social_post_proposals_recent
    ON social_post_proposals (client_id, created_at_ms DESC);

CREATE INDEX social_post_proposals_source
    ON social_post_proposals (client_id, source_id, source_content_draft_id, created_at_ms DESC);
