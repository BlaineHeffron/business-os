//! Email drafts slice: the produce → approve → provider-write vertical for
//! the `email_draft_reply` packet kind (Demo workflow-map W10 `draft.email`,
//! DRAFT→approver posture). Produce fills the reply BODY and initializes
//! destination/thread fields from the source. Blank composition stages
//! operator-authored typed fields. All reviewable fields may be edited. Approval
//! enqueues a Gmail DRAFT-create outbox job (NEVER send: even with
//! BOS_GMAIL_WRITE_ENABLED open, the human sends from Gmail). Needs the
//! gmail.compose scope — operators who connected earlier see the reconnect
//! prompt.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "email_drafts",
    title: "Email reply drafts",
    summary: "Produce/manual-stage + approval vertical for email_draft_reply work items: typed fields remain operator-editable and an optional bounded AI rewrite changes only the body. Approval enqueues an outbox job that creates a Gmail DRAFT (never sends) through the write-gated client (dry-run while the gate is closed).",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/email-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/manual",
            summary: "Stage an operator-authored typed email draft for an accepted work item without a model call",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/produce",
            summary: "Produce a reply draft from an accepted work item (typed fill; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/{draft_id}/action",
            summary: "Approve (stages the Gmail draft-create as an outbox job) or reject a staged draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/{draft_id}/update",
            summary: "Edit a staged draft's recipients, subject, and body before approval",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/{draft_id}/rewrite",
            summary: "Rewrite a staged exact-revision email body with the configured bounded typed LLM route",
        },
        RouteSpec {
            method: "GET",
            path: "/api/email-drafts/follow-ups",
            summary: "List outbound email follow-up workflow summaries for task decoration and debug",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/follow-ups/{follow_up_id}/check",
            summary: "Manually reconcile a Gmail thread for an outbound follow-up workflow",
        },
        RouteSpec {
            method: "POST",
            path: "/api/email-drafts/follow-ups/{follow_up_id}/draft",
            summary: "Create an accepted email_draft_reply work item for an overdue follow-up",
        },
    ],
    tables: &["email_reply_drafts", "email_outbound_follow_ups"],
    env_vars: &[&env_registry::BOS_GMAIL_WRITE_ENABLED],
    read_models: &["email_drafts"],
};
