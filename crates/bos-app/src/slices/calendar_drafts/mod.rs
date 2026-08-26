//! Calendar drafts slice: the produce → approve → provider-write vertical for
//! the `calendar_event_draft` packet kind. An accepted work item is produced
//! into a staged, provenance'd event draft (typed Extract over the source
//! email); operator approval enqueues the Google Calendar write as an outbox
//! job; the delivery pump executes it through the write-gated client (dry-run
//! until BOS_GOOGLE_CALENDAR_WRITE_ENABLED is flipped in an attended session).

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "calendar_drafts",
    title: "Calendar event drafts",
    summary: "Produce + approval vertical for calendar_event_draft work items: typed Extract stages a provenance'd event draft; approval enqueues an outbox job delivered through the write-gated Google Calendar client (dry-run while the gate is closed). Owns the outbox delivery pump.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/calendar-drafts",
            summary: "Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state",
        },
        RouteSpec {
            method: "POST",
            path: "/api/calendar-drafts/produce",
            summary: "Produce a draft from an accepted work item (typed Extract; returns the existing active draft when one exists)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/calendar-drafts/{draft_id}/action",
            summary: "Approve (stages the provider write as an outbox job) or reject a staged draft",
        },
        RouteSpec {
            method: "GET",
            path: "/api/calendar-drafts/calendars",
            summary: "Writable calendars of the connected account (the event-draft calendar picker)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/calendar-drafts/{draft_id}/update",
            summary: "Edit a staged draft's event fields, attendees, invitation choice, and calendar before approval",
        },
    ],
    tables: &["calendar_event_drafts", "outbox_jobs"],
    env_vars: &[
        &env_registry::BOS_GOOGLE_CALENDAR_ID,
        &env_registry::BOS_GOOGLE_CALENDAR_WRITE_ENABLED,
        &env_registry::BOS_OUTBOX_DELIVERY_ENABLED,
        &env_registry::BOS_OUTBOX_DELIVERY_INTERVAL_SECS,
    ],
    read_models: &["calendar_drafts"],
};
