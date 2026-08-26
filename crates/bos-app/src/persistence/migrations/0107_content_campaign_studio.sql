-- Thin campaign coordination over content_plans, content_drafts, the content
-- publish adapter, and social_publishing. Editable article/social bodies stay
-- in their owning tables. This row is the immutable exact-revision approval
-- snapshot and the blog -> canonical URL -> social dependency state.

ALTER TABLE social_published_sources
    ADD COLUMN source_content_draft_revision INTEGER;

ALTER TABLE social_post_proposals
    ADD COLUMN source_content_draft_revision INTEGER;

CREATE TABLE content_campaign_publications (
    client_id TEXT NOT NULL,
    publication_id TEXT NOT NULL,
    plan_item_id TEXT NOT NULL,
    content_draft_id TEXT NOT NULL,
    content_draft_revision INTEGER NOT NULL,
    social_proposal_id TEXT,
    social_proposal_revision INTEGER,
    expected_canonical_url TEXT NOT NULL,
    actual_canonical_url TEXT,
    launch_mode TEXT NOT NULL CHECK (launch_mode IN ('publish_now', 'schedule')),
    selected_channel_ids_json TEXT NOT NULL DEFAULT '[]',
    approved_social_targets_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL CHECK (
        status IN ('awaiting_blog', 'blog_dry_run', 'social_enqueued', 'completed', 'requires_review')
    ),
    review_reason TEXT,
    approved_by TEXT NOT NULL,
    approved_at_ms INTEGER NOT NULL,
    blog_outbox_job_id TEXT NOT NULL,
    social_outbox_job_ids_json TEXT NOT NULL DEFAULT '[]',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, publication_id)
);

CREATE INDEX content_campaign_publications_plan_recent
    ON content_campaign_publications (client_id, plan_item_id, created_at_ms DESC);

CREATE INDEX content_campaign_publications_reconcile
    ON content_campaign_publications (client_id, status, updated_at_ms);

CREATE UNIQUE INDEX content_campaign_publications_active_proposal
    ON content_campaign_publications (client_id, social_proposal_id)
    WHERE social_proposal_id IS NOT NULL
      AND status IN ('awaiting_blog', 'social_enqueued', 'completed', 'requires_review');

CREATE UNIQUE INDEX content_campaign_publications_active_article
    ON content_campaign_publications (client_id, content_draft_id)
    WHERE status IN ('awaiting_blog', 'social_enqueued', 'completed', 'requires_review');
