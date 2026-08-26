//! Content drafting vertical (port #5 part 2, packet kind `content_draft`):
//! grounded drafts over the local Drive corpus on the ProduceFlavor spine.
//! Operator brief → deterministic BM25 retrieval + snippet budget → one
//! drafting transform (claims must cite snippet ids) → deterministic
//! citation gate (uncited/unsupported claims block approval) → operator
//! approval. An explicit second operator action may enqueue the approved
//! draft to a client-specific publishing adapter.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "content_drafts",
    title: "Content drafts",
    summary: "Grounded content drafting over the drive_corpus index: brief → BM25 top-k → evidence budget → one typed drafting transform with mandatory snippet citations → deterministic citation gate → operator approval → separately authorized, gated publishing through a client-specific outbox adapter.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/content-drafts",
            summary: "Content drafts, newest first (?item_id= filters)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-drafts/produce",
            summary: "Produce a grounded draft for an accepted work item (202, panel polls)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-drafts/{draft_id}/action",
            summary: "Approve (citation gate must pass; draft-only, no provider write) or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-drafts/{draft_id}/update",
            summary: "Edit a staged draft's title/body/SEO fields (claims and gate are immutable)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/content-drafts/{draft_id}/publish",
            summary: "Publish an approved draft through the configured client adapter",
        },
    ],
    tables: &["content_drafts", "content_web_facts", "outbox_jobs"],
    env_vars: &[
        &env_registry::BOS_CONTENT_PUBLISH_ADAPTER_URL,
        &env_registry::BOS_CONTENT_PUBLISH_ADAPTER_TOKEN,
        &env_registry::BOS_CONTENT_PUBLISH_WRITE_ENABLED,
        &env_registry::BOS_CONTENT_WEB_FACTS_ENABLED,
        &env_registry::BOS_WEB_ENRICHMENT_ENABLED,
    ],
    read_models: &["content_drafts"],
};
