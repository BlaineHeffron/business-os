//! Scoped machine operator tokens: a capability list so the Slack bridge and
//! an ingest-only agent can authenticate without holding the shared
//! `BOS_OPERATOR_TOKEN`. Existing unscoped env/personal tokens keep working.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "operator_api_tokens",
    title: "Scoped operator tokens",
    summary: "Machine operator credentials with an explicit capability list. Unscoped env and personal-user tokens keep full access; a scoped token can be minted for social publishing and/or agent MCP ingest only.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/operator-tokens",
            summary: "List scoped API tokens (metadata only; secrets are never readable)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/operator-tokens",
            summary: "Mint a scoped API token (returns the bearer secret ONCE)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/operator-tokens/{token_id}/action",
            summary: "Enable, disable, or revoke a scoped token (disable/revoke stop authentication immediately)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/operator-tokens/{token_id}/rotate-token",
            summary: "Replace the bearer secret (returned ONCE; the old secret stops working)",
        },
    ],
    tables: &["operator_api_tokens"],
    env_vars: &[],
    read_models: &["operator_api_tokens"],
};
