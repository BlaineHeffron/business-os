//! Release notes slice: operator-facing notes created by the fleet when it
//! observes a deployment build change.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "release_notes",
    title: "Release notes",
    summary: "Operator-facing deployment notes created by the fleet and dismissed per operator.",
    routes: &[
        RouteSpec {
            method: "POST",
            path: "/api/webhooks/release-notes",
            summary: "Create a release note from the fleet; webhook-token gated and idempotent by release note id",
        },
        RouteSpec {
            method: "GET",
            path: "/api/release-notes/latest",
            summary: "Latest release note not dismissed by this operator",
        },
        RouteSpec {
            method: "GET",
            path: "/api/release-notes",
            summary: "Recent release notes",
        },
        RouteSpec {
            method: "POST",
            path: "/api/release-notes/{id}/dismiss",
            summary: "Dismiss a release note for this operator",
        },
    ],
    tables: &["release_notes", "release_note_dismissals"],
    env_vars: &[&env_registry::BOS_RELEASE_NOTES_WEBHOOK_SECRET],
    read_models: &["release_notes"],
};
