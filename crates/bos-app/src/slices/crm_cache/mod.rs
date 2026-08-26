//! Cached CRM contacts and deals. Provider reads feed local snapshots; future
//! grounding tools read this slice instead of calling CRM live per draft.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "crm_cache",
    title: "CRM cache",
    summary: "Local cached CRM contacts and HubSpot deals for offline grounding; sync is request-budgeted and the browser reads local data only.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/crm-cache/status",
            summary: "CRM cache counts and sync freshness",
        },
        RouteSpec {
            method: "GET",
            path: "/api/crm-cache/contacts",
            summary: "Cached CRM contacts by ?email= or ?company=",
        },
        RouteSpec {
            method: "GET",
            path: "/api/crm-cache/deals",
            summary: "Cached CRM deals by ?contact_email= with amount visibility applied",
        },
        RouteSpec {
            method: "GET",
            path: "/api/crm-cache/context",
            summary: "Source-aware cached CRM context for an inbound message",
        },
        RouteSpec {
            method: "POST",
            path: "/api/crm-cache/sync",
            summary: "Kick one CRM cache sync cycle",
        },
    ],
    tables: &[
        "crm_contact_snapshots",
        "crm_deal_snapshots",
        "crm_cache_sync_cursors",
    ],
    env_vars: &[
        &env_registry::BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS,
        &env_registry::BOS_CRM_DEAL_VISIBILITY_POLICY,
        &env_registry::BOS_CRM_PROVIDER,
        &env_registry::BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_CRM_READ_SYNC_ENABLED,
        &env_registry::BOS_CRM_READ_SYNC_INTERVAL_SECS,
        &env_registry::BOS_ESPOCRM_API_KEY,
        &env_registry::BOS_ESPOCRM_BASE_URL,
        &env_registry::BOS_HUBSPOT_ACCESS_TOKEN,
        &env_registry::BOS_HUBSPOT_PORTAL_ID,
    ],
    read_models: &[
        "crm_cache_status",
        "crm_cache_contacts",
        "crm_cache_deals",
        "crm_cache_context",
    ],
};
