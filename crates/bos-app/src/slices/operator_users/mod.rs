//! Operator users slice: named operators with personal bearer tokens.
//! Authentication accepts the shared BOS_OPERATOR_TOKEN (actor "operator")
//! OR a personal token (actor = user_id) — every mutation receipt then says
//! WHO acted, which the Demo agreement's approval rules require (Jordan and Casey
//! both approve, separately). Per-user provider credentials (0e-2) key off
//! these user ids. Tokens are returned exactly once at create/rotate and are
//! never readable or receipted.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "operator_users",
    title: "Operator users",
    summary: "Named operators with personal bearer tokens: authentication resolves WHO acts (receipts stamp the user id), enabling per-user approvals and, next, per-user provider credentials.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/me",
            summary: "Who the presented token authenticates as",
        },
        RouteSpec {
            method: "GET",
            path: "/api/users",
            summary: "List operator users",
        },
        RouteSpec {
            method: "POST",
            path: "/api/users",
            summary: "Create an operator user (returns the personal token ONCE)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/users/{user_id}/action",
            summary: "Enable, disable, or archive a user (disable/archive invalidate the token immediately)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/users/{user_id}/rotate-token",
            summary: "Replace the user's token (returned ONCE; the old token stops working)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/users/{user_id}/default-calendar",
            summary: "Set or clear the calendar the user's approved event drafts default to",
        },
    ],
    tables: &["operator_users"],
    env_vars: &[],
    read_models: &["operator_users"],
};
