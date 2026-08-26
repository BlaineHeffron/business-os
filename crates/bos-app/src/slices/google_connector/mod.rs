//! Google account connect flow: the operator clicks "connect", consents in
//! the browser, and the resulting refresh token is stored per client +
//! operator user + service (the connect binds to whoever's token initiated
//! it). The ingest pump polls every connected account; writes resolve the
//! acting user's credential.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "google_connector",
    title: "Google account connector",
    summary: "OAuth connect flow for Google services, per operator user: consent URL bound to the connecting user, code-exchange callback, stored refresh tokens (audited, never in receipts), per-user status + disconnect.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/connectors/google/status",
            summary: "Connection status; includes connect_url when disconnected",
        },
        RouteSpec {
            method: "GET",
            path: "/api/connectors/google/connect",
            summary: "Redirect to the Google consent screen",
        },
        RouteSpec {
            method: "GET",
            path: "/api/connectors/google/callback",
            summary: "OAuth redirect target; exchanges the code and stores the refresh token",
        },
        RouteSpec {
            method: "POST",
            path: "/api/connectors/google/disconnect",
            summary: "Remove the stored credential",
        },
        RouteSpec {
            method: "GET",
            path: "/api/connectors/google/drive/folders",
            summary: "Search Google Drive folders available to the connected credential",
        },
    ],
    tables: &["google_oauth_credentials", "connector_oauth_states"],
    env_vars: &[
        &env_registry::BOS_GMAIL_OAUTH_CLIENT_ID,
        &env_registry::BOS_GMAIL_OAUTH_CLIENT_SECRET,
        &env_registry::BOS_PUBLIC_BASE_URL,
    ],
    read_models: &["google_connector_status"],
};

/// Service identifier for the Gmail read credential.
pub const SERVICE_GMAIL: &str = "gmail";
