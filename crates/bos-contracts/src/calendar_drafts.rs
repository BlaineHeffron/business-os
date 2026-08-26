//! Calendar event draft contracts: the produce → approve → provider-write
//! vertical for the `calendar_event_draft` packet kind.
//!
//! Pipeline position: work item accepted → **produce** (typed Extract stages a
//! draft) → operator approves → outbox job → Google Calendar client (write-
//! gated; dry-run until the operator enables provider writes).

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarDraftStatus {
    Staged,
    Approved,
    Rejected,
}

/// Per-field provenance: the literal source-message quote the extractor based
/// the field on. Empty quote = the model inferred it (UI flags those).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftFieldProvenance {
    pub field: String,
    pub quote: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEventDraft {
    /// "ced_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    /// Operator user this draft is bound to, inherited from the originating
    /// work item. Null = legacy rows / all-scope-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    pub status: CalendarDraftStatus,
    pub title: String,
    /// RFC3339 timestamps (what the Google Calendar API consumes).
    pub start_at: String,
    pub end_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Calendar the approved event writes to. None = the server default
    /// (BOS_GOOGLE_CALENDAR_ID). Operator-editable while staged, picked from
    /// the connected account's writable calendars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_id: Option<String>,
    #[serde(default)]
    pub attendees: Vec<String>,
    #[serde(default)]
    pub send_invitations: bool,
    pub provenance: Vec<DraftFieldProvenance>,
    /// Model that produced the extraction (audit display).
    pub model: String,
    /// Extractor's own confidence: "high" | "medium" | "low".
    pub confidence: String,
    /// Set when approved: the outbox job carrying the provider write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

/// Delivery state of the approved draft's outbox job, surfaced so the operator
/// sees "queued → delivered (dry-run)" without a separate outbox view.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxJobSummary {
    pub job_id: String,
    /// "pending" | "delivered" | "failed_terminal" |
    /// "delivery_outcome_unknown" (manual provider reconciliation required).
    pub status: String,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// True when the delivery executed against the dry-run client (provider
    /// write gate closed) — the event was NOT created on the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    /// Provider object id (Google event id) once delivered for real.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_object_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftWithRevision {
    pub draft: CalendarEventDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftsResponse {
    pub drafts: Vec<CalendarDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftProduceResponse {
    pub draft: CalendarDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftActionRequest {
    pub action: CalendarDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft's AI-filled fields ("AI-produced fields
/// remain editable until accepted"). Full replacement of the editable set;
/// grounded/audit fields (source, provenance, model) are not editable.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDraftUpdateRequest {
    pub title: String,
    pub start_at: String,
    pub end_at: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// None = write to the server default calendar.
    #[serde(default)]
    pub calendar_id: Option<String>,
    pub attendees: Vec<String>,
    pub send_invitations: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// One calendar the connected account can write to (the event-draft picker).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarOption {
    pub id: String,
    pub summary: String,
    pub primary: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarListResponse {
    pub calendars: Vec<CalendarOption>,
    /// The server default target (BOS_GOOGLE_CALENDAR_ID) — what None means.
    pub default_calendar_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_round_trips() {
        let draft = CalendarEventDraft {
            draft_id: "ced_wi_email_m1_1".into(),
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            source_user_id: None,
            status: CalendarDraftStatus::Staged,
            title: "Soccer practice".into(),
            start_at: "2026-06-12T16:00:00-04:00".into(),
            end_at: "2026-06-12T17:00:00-04:00".into(),
            timezone: Some("America/New_York".into()),
            location: Some("Field 3".into()),
            calendar_id: None,
            attendees: vec!["Coach@business-76e9de2c7e.test".into()],
            send_invitations: false,
            description: None,
            provenance: vec![DraftFieldProvenance {
                field: "start_at".into(),
                quote: "Friday June 12 at 4pm".into(),
            }],
            model: "claude-sonnet-4-6".into(),
            confidence: "high".into(),
            outbox_job_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        let back: CalendarEventDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(draft, back);
    }
}
