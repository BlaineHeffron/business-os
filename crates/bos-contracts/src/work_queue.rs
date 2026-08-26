//! Work queue contracts: the ONE operator feed (views are filters over it).
//!
//! Pipeline position: ingest → classify (email_triage) → **work item** →
//! accept → produce (future) → approve → write (future). A work item is the
//! operator-visible suggestion that a classified input deserves work; the
//! per-category policy decides which inputs generate items and which packet
//! kinds they suggest.

use serde::{Deserialize, Serialize};

use crate::email_identity::AttentionLevel;
use crate::email_triage::EmailTriageGmailCategory;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemStatus {
    Open,
    Accepted,
    Dismissed,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemAcceptActor {
    Operator,
    System,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// Deterministic id derived from the source (e.g. "wi_email_<message_id>").
    pub item_id: String,
    /// Source family ("email" today; call logs, webhooks, notes later).
    pub source_kind: String,
    /// BusinessOS-stable source reference. For email this is the inbound
    /// message source key, not necessarily the raw Gmail message id.
    pub source_ref: String,
    pub category_id: String,
    pub title: String,
    pub summary: String,
    /// Packet kinds the policy suggests producing for this item.
    pub packet_kinds: Vec<String>,
    pub status: WorkItemStatus,
    /// Queryable actor class that accepted this item. Null means the item is
    /// not accepted; accepted rows are operator-accepted or system-accepted.
    #[serde(default)]
    pub accept_actor: Option<WorkItemAcceptActor>,
    /// True when the item was suggested by the AI triage pass rather than a
    /// deterministic category policy. Accepting it approves the AI's read.
    #[serde(default)]
    pub ai_suggested: bool,
    /// The AI's one-line rationale (empty for policy-emitted items).
    #[serde(default)]
    pub rationale: String,
    /// Operator-authored guidance that rides into the produce-stage LLM input.
    /// It can steer tone, priorities, missing details, or specific output
    /// shape, but it never bypasses each packet kind's grounding/approval gate.
    #[serde(default)]
    pub produce_guidance: String,
    /// Operator user whose account/input sourced this item (the connected
    /// mailbox for email; the author for notes). Null = legacy rows or
    /// env-credential single-account ingestion.
    #[serde(default)]
    pub source_user_id: Option<String>,
    /// Operator user currently responsible for the item. Assignment is local
    /// queue state and never changes the provider/source owner above.
    #[serde(default)]
    pub assignee_user_id: Option<String>,
    /// Named operators who can see and mutate this work item. Empty means no
    /// explicit visibility rows; shared/all-scope operators can still see it.
    #[serde(default)]
    pub visible_to_user_ids: Vec<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemFailureNotification {
    pub notification_id: String,
    pub source: String,
    #[serde(default)]
    pub packet_kind: Option<String>,
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub diagnostic_id: Option<String>,
    #[serde(default)]
    pub diagnostic_href: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub occurred_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemAttentionSummary {
    pub level: AttentionLevel,
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemWithRevision {
    pub item: WorkItem,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    /// Packet kinds with a STAGED draft awaiting an operator decision on this
    /// item — the "needs you" signal for accepted items.
    #[serde(default)]
    pub staged_draft_kinds: Vec<String>,
    /// Packet kinds the auto-produce pump will draft for this item (accepted,
    /// opted-in category, no draft yet) — rendered as "drafting…" so the item
    /// stays visible while the operator waits instead of vanishing.
    #[serde(default)]
    pub pending_produce_kinds: Vec<String>,
    /// Internal task failures tied to this work item. This keeps an accepted
    /// item visible in the "needs you" lane with a consistent recovery/debug
    /// signal instead of silently stranding it.
    #[serde(default)]
    pub failure_notifications: Vec<WorkItemFailureNotification>,
    /// Strongest source-level attention signal, when an inbound parser supplied
    /// one. Generic queue code renders this without knowing parser reason codes.
    #[serde(default)]
    pub attention: Option<WorkItemAttentionSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueueResponse {
    pub items: Vec<WorkItemWithRevision>,
}

/// Replace the packet kinds suggested on a work item — the operator tunes
/// what gets produced (e.g. keep the calendar event, drop the CRM note)
/// before drafting starts. Kinds must exist in the platform catalog.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemKindsUpdateRequest {
    pub packet_kinds: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Replace the operator guidance attached to a work item. The produce spine
/// injects this into every packet-kind LLM request for the item.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemGuidanceUpdateRequest {
    pub produce_guidance: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// The full source behind a work item (the email or note it came from),
/// rendered inline in the queue so a decision never requires navigation.
/// Non-email sources are served through the same synthesized message view
/// the produce stage consumes.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemSourceBodyFormat {
    PlainText,
    Html,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemSourceResponse {
    pub source_kind: String,
    pub message: crate::email_triage::InboundMessageRecord,
    /// Full display body for the source route. This intentionally lives on the
    /// source response, not InboundMessageRecord, so inbox/feed rows stay small.
    #[serde(default)]
    pub source_body: String,
    #[serde(default = "default_source_body_format")]
    pub source_body_format: WorkItemSourceBodyFormat,
}

fn default_source_body_format() -> WorkItemSourceBodyFormat {
    WorkItemSourceBodyFormat::PlainText
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemActionKind {
    Accept,
    Dismiss,
    Reopen,
    Trash,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemActionRequest {
    pub action: WorkItemActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemAssignActionKind {
    AssignToMe,
    AssignToUser,
    Unassign,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemAssignRequest {
    pub action: WorkItemAssignActionKind,
    #[serde(default)]
    pub assignee_user_id: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Sentinel value in [`WorkQueuePolicy::ai_suggestible_packet_kinds`] meaning
/// "the AI triage pass may suggest any enabled packet kind it judges
/// appropriate" — it chooses, rather than the operator pre-selecting a set.
pub const AI_SUGGEST_ALL_SENTINEL: &str = "*";

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkQueueAiGmailScope {
    /// Back-compatible fallback: AI triage only examines Primary + Updates
    /// fallback mail unless a deterministic rule matched the message.
    #[default]
    Default,
    /// AI triage may examine every fallback message, including mail without
    /// Gmail category labels.
    All,
    /// AI triage may examine only the listed Gmail categories.
    Selected,
}

/// Per-category policy: does a classified input of this category generate a
/// work item, and which packet kinds does it suggest? Categories with no
/// policy row generate nothing (quiet by default).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueuePolicy {
    pub category_id: String,
    pub create_work_item: bool,
    pub packet_kinds: Vec<String>,
    /// Packet kinds the AI triage pass may add when a specific email warrants
    /// extra work (not attached by deterministic policy). The single sentinel
    /// [`AI_SUGGEST_ALL_SENTINEL`] means "any enabled kind — the AI chooses";
    /// the UI drives this all-or-nothing. A specific id list is still honored
    /// for back-compat with policies configured before the toggle.
    #[serde(default)]
    pub ai_suggestible_packet_kinds: Vec<String>,
    /// Scope mode for AI triage on the fallback `inbound_email` policy.
    #[serde(default)]
    pub ai_suggestible_gmail_scope: WorkQueueAiGmailScope,
    /// Gmail-tab scope for AI triage on the fallback `inbound_email` policy.
    /// Used only when `ai_suggestible_gmail_scope` is `selected`; legacy
    /// `default` rows are canonicalized to Primary + Updates on write.
    #[serde(default)]
    pub ai_suggestible_gmail_categories: Vec<EmailTriageGmailCategory>,
    /// When true (and the auto-produce pump is enabled), accepting an item in
    /// this category produces its drafts automatically — an LLM-cost decision,
    /// so it is opt-in per category. Manual "Draft X" buttons always remain.
    #[serde(default)]
    pub auto_produce: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueuePoliciesResponse {
    pub policies: Vec<WorkQueuePolicy>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueuePolicyUpsertRequest {
    pub policy: WorkQueuePolicy,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// A packet kind is a PLATFORM-DEFINED typed transform: the output schema the
/// produce stage (AI or deterministic) must fill, and eventually the write
/// binding for the approved output. Operators SELECT kinds from this catalog
/// per category — they cannot invent them, because a kind without produce/
/// write code behind it does nothing. Served by GET /api/work-queue/packet-kinds.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketKindRecord {
    pub kind_id: String,
    pub title: String,
    /// Operator-facing meaning; also feeds the produce-stage prompt.
    pub description: String,
    /// False until the produce slice implements this kind end-to-end —
    /// selectable now so policies can be configured ahead of the build.
    pub produce_available: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketKindsResponse {
    pub kinds: Vec<PacketKindRecord>,
}

/// Launch a Agent Monitor agent session seeded with a work item's context.
/// Operator-only power tool gated by `BOS_AGENT_LAUNCH_ENABLED` — it is not a
/// client-facing feature. `context` is the operator's optional free-text notes
/// appended to the item/source context the server assembles.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgentRequest {
    #[serde(default)]
    pub context: String,
    /// Optional per-launch working directory override. Empty/null means the
    /// server uses the item's category default, then its built-in fallback.
    #[serde(default)]
    pub work_dir: Option<String>,
    /// Attachment ids from the source email to stage into the selected workdir
    /// before launching the agent. Metadata is still included for every
    /// attachment; bytes are fetched only for these selected ids.
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    pub idempotency_key: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgentResponse {
    pub session_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub monitor_url: Option<String>,
    #[serde(default)]
    pub staged_evidence_paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_round_trips() {
        let item = WorkItem {
            item_id: "wi_email_m1".into(),
            source_kind: "email".into(),
            source_ref: "m1".into(),
            category_id: "billing".into(),
            title: "Dominion Energy bill".into(),
            summary: "Your bill is now available".into(),
            packet_kinds: vec!["invoice_reconciliation".into()],
            status: WorkItemStatus::Open,
            accept_actor: None,
            ai_suggested: false,
            rationale: String::new(),
            produce_guidance: String::new(),
            source_user_id: None,
            assignee_user_id: None,
            visible_to_user_ids: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let back: WorkItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(item, back);
    }
}
