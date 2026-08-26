//! CRM sales-intent drafts (packet kind `crm_sales_intent`): pipeline intent is
//! staged separately from CRM account/contact record creation. Approval can
//! create a provider lead/deal only when the configured CRM supports that
//! target; unsupported provider/target combinations fail before mutation.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "crm_sales_intent",
    title: "CRM sales-intent drafts",
    summary: "Produce + approval vertical for crm_sales_intent work items: a typed fill stages pipeline intent (lead title, rationale, qualification, next step, optional follow-up date) separately from address-book CRM records. Approval writes an EspoCRM Lead behind the CRM write gate; unsupported providers or targets fail gracefully before mutation.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/crm-sales-intent",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-sales-intent/produce",
            summary: "Produce a sales-intent draft from an accepted work item",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-sales-intent/{draft_id}/action",
            summary: "Approve (stages a provider lead write when supported) or reject",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-sales-intent/{draft_id}/update",
            summary: "Edit a staged sales-intent draft before approval",
        },
    ],
    tables: &["crm_sales_intent_drafts"],
    env_vars: &[
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_ESPOCRM_API_KEY,
        &env_registry::BOS_ESPOCRM_BASE_URL,
        &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
    ],
    read_models: &["crm_sales_intent"],
};
