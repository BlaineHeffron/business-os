-- One operator-reviewed proposal revision fans out into one shared-outbox job
-- per configured Buffer channel. targets_json stores the exact channel/text/
-- URL/media/UTM/schedule snapshot; credentials remain env-only.

CREATE TABLE social_published_sources (
    client_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    external_id TEXT NOT NULL,
    source_content_draft_id TEXT,
    canonical_url TEXT NOT NULL,
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

CREATE INDEX social_published_sources_recent
    ON social_published_sources (client_id, updated_at_ms DESC);

CREATE TABLE social_post_proposals (
    client_id TEXT NOT NULL,
    proposal_id TEXT NOT NULL,
    source_id TEXT,
    source_content_draft_id TEXT,
    canonical_url TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('staged', 'approved', 'rejected')),
    targets_json TEXT NOT NULL,
    approved_by TEXT,
    approved_revision INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, proposal_id)
);

CREATE INDEX social_post_proposals_recent
    ON social_post_proposals (client_id, created_at_ms DESC);

CREATE INDEX social_post_proposals_source
    ON social_post_proposals (client_id, source_id, source_content_draft_id, created_at_ms DESC);
