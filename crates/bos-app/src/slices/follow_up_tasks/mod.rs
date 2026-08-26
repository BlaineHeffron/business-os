//! Follow-up tasks slice: the produce → approve → LOCAL write vertical for
//! the `follow_up_task` packet kind, plus the operator's task list itself.
//! Approval inserts the tasks row in the same receipted transaction that
//! flips the draft — no provider, no outbox, no write gate.

pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "follow_up_tasks",
    title: "Follow-up tasks",
    summary: "Produce/manual-stage + approval vertical for follow_up_task work items: typed fields are validated at one chokepoint; optional AI produce stages a provenance'd draft. Approval writes the local tasks row in the same receipted transaction. Serves the operator task list (complete/reopen).",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/follow-up-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/follow-up-drafts/manual",
            summary: "Stage an operator-authored typed follow-up draft for an accepted work item without a model call",
        },
        RouteSpec {
            method: "POST",
            path: "/api/follow-up-drafts/produce",
            summary: "Produce a draft from an accepted work item (typed fill; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/follow-up-drafts/{draft_id}/action",
            summary: "Approve (creates the local task in the same transaction) or reject a staged draft",
        },
        RouteSpec {
            method: "POST",
            path: "/api/follow-up-drafts/{draft_id}/update",
            summary: "Edit a staged draft's AI-filled task fields (title/due date/context) before approval",
        },
        RouteSpec {
            method: "GET",
            path: "/api/tasks",
            summary: "Operator task list, open-first by due date (?status=open|done; ?today=YYYY-MM-DD decorates open tasks with watchdog escalation lanes: overdue/due-today/upcoming, missed->escalated->critical)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/tasks/{task_id}/action",
            summary: "Complete or reopen a task",
        },
    ],
    tables: &["follow_up_task_drafts", "tasks"],
    env_vars: &[],
    read_models: &["follow_up_drafts", "tasks"],
};
