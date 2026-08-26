//! Admin runtime settings visibility and curated overrides.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "admin_settings",
    title: "Admin settings",
    summary: "Full runtime configuration visibility plus curated per-client overrides for resolver-wired behavior switches.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/admin/settings",
            summary: "Read grouped runtime configuration with secrets redacted",
        },
        RouteSpec {
            method: "POST",
            path: "/api/admin/settings/{var_name}",
            summary: "Set a resolver-wired runtime setting override",
        },
        RouteSpec {
            method: "DELETE",
            path: "/api/admin/settings/{var_name}",
            summary: "Clear a resolver-wired runtime setting override",
        },
    ],
    tables: &["runtime_setting_overrides"],
    env_vars: &[],
    read_models: &["admin_settings"],
};
