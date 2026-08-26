//! Home dashboard slice: per-operator dashboard preferences plus a server-side
//! authorized aggregate of the small-business widgets shown on the landing tab.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::{
    env_registry,
    slices::{RouteSpec, SliceSpec},
};

pub const SLICE: SliceSpec = SliceSpec {
    id: "home_dashboard",
    title: "Home dashboard",
    summary: "Configurable per-operator landing dashboard assembled from existing authorized read models: tasks, inbox/work queue, inventory, and financial widgets.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/home-dashboard",
            summary: "Current operator's dashboard preferences and authorized widget data",
        },
        RouteSpec {
            method: "POST",
            path: "/api/home-dashboard/preferences",
            summary: "Replace the current operator's dashboard widget order and visibility",
        },
        RouteSpec {
            method: "GET",
            path: "/api/home-dashboard/hubspot-deals/discovery",
            summary: "Read-only HubSpot deal pipelines, stages, and date properties for dashboard setup",
        },
        RouteSpec {
            method: "GET",
            path: "/api/home-dashboard/hubspot-deals/mapping",
            summary: "Current saved HubSpot deal pipeline mapping for the Home sales widget",
        },
        RouteSpec {
            method: "POST",
            path: "/api/home-dashboard/hubspot-deals/mapping",
            summary: "Save the HubSpot deal pipeline mapping used by the Home sales widget",
        },
    ],
    tables: &[
        "home_dashboard_preferences",
        "home_dashboard_hubspot_deal_mapping",
    ],
    env_vars: &[&env_registry::BOS_HUBSPOT_ACCESS_TOKEN],
    read_models: &["home_dashboard"],
};
