//! CRM drafts slice: the produce → approve → provider-write vertical for the
//! `crm_activity` packet kind: answering-service summaries and customer
//! emails logged into the CRM as notes. BOS_CRM_PROVIDER picks the provider
//! (hubspot | espocrm — the self-hosted open-source arm of the
//! provider model); approval enqueues that provider's note-create outbox
//! job, and each write-gated client dry-runs until its BOS_*_WRITE_ENABLED
//! is flipped in an attended session. Notes-only posture on both (proven in
//! agent_monitor): no associations/parent-record API — the contact reference rides
//! in the note text.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "crm_drafts",
    title: "CRM note drafts",
    summary: "Produce + approval vertical for crm_activity work items: typed fill stages a provenance'd CRM note (occurred-at grounded from the source email's date); approval enqueues an outbox job delivered through the write-gated CRM client — BOS_CRM_PROVIDER selects hubspot or espocrm (dry-run while the gate is closed).",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/crm-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-drafts/produce",
            summary: "Produce a CRM note draft from an accepted work item (typed fill; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-drafts/{draft_id}/action",
            summary: "Approve (stages the CRM write as an outbox job for the configured provider) or reject a staged draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-drafts/{draft_id}/update",
            summary: "Edit a staged draft's AI-filled note fields (body/contact) before approval",
        },
    ],
    tables: &["crm_note_drafts"],
    env_vars: &[
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_ESPOCRM_API_KEY,
        &env_registry::BOS_ESPOCRM_BASE_URL,
        &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
        &env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
        &env_registry::BOS_HUBSPOT_WRITE_ENABLED,
    ],
    read_models: &["crm_drafts"],
};
