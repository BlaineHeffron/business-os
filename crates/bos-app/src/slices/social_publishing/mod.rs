//! Approval-gated social publishing: canonical blog URL → editable per-channel
//! proposal → exact-revision operator approval → independent Buffer outbox jobs.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "social_publishing",
    title: "Social publishing",
    summary: "Published-content ingress and a bounded typed transform produce editable platform-specific proposals. Operator approval snapshots the exact current revision and atomically enqueues one independently retryable Buffer job per channel; live writes default off.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/social-publishing/proposals",
            summary: "Published-source generation state, recent proposals, configured Buffer channels, and per-channel delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/social-publishing/proposals",
            summary: "Stage one editable proposal covering every configured Buffer channel",
        },
        RouteSpec {
            method: "POST",
            path: "/api/social-publishing/proposals/{proposal_id}/update",
            summary: "Replace a staged proposal's exact channel text, image, UTM, and schedule snapshot",
        },
        RouteSpec {
            method: "POST",
            path: "/api/social-publishing/proposals/{proposal_id}/action",
            summary: "Approve the exact current revision and fan out channel jobs atomically, or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/social-publishing/sources/{source_id}/generate",
            summary: "Kick off one bounded typed transform that drafts grounded per-channel proposals from published content",
        },
        RouteSpec {
            method: "POST",
            path: "/api/social-publishing/drafts/{draft_id}/generate-preview",
            summary: "Draft grounded social variants from an exact editable article revision and operator-previewed canonical URL",
        },
    ],
    tables: &["social_published_sources", "social_post_proposals", "outbox_jobs"],
    env_vars: &[
        &env_registry::BOS_BUFFER_ACCESS_TOKEN,
        &env_registry::BOS_BUFFER_API_URL,
        &env_registry::BOS_BUFFER_CHANNELS_JSON,
        &env_registry::BOS_BUFFER_WRITE_ENABLED,
    ],
    read_models: &["social_published_sources", "social_post_proposals"],
};
