//! Operator notes slice: manually logged notes (phone calls the operator
//! took directly, walk-ins, reminders) — the second work-item source family
//! after email (Demo workflow-map W9/W11 `operator_note`). Creating a note
//! emits its work item immediately (operator-initiated input skips the
//! quiet-by-default policy gate); produce kinds then run over the note text
//! via the same shared produce flow as email sources.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "operator_notes",
    title: "Operator notes",
    summary: "Manually logged notes as a work-item source: creating a note emits a work item (category operator_note; policy supplies packet kinds, defaulting to CRM note + follow-up task), and produce kinds run over the note text.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/operator-notes",
            summary: "Recent notes, newest first",
        },
        RouteSpec {
            method: "POST",
            path: "/api/operator-notes",
            summary: "Log a note; emits its work item in the same request (idempotent on the key)",
        },
    ],
    tables: &["operator_notes"],
    env_vars: &[],
    read_models: &["operator_notes"],
};
