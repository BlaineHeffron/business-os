//! Content draft contracts: the produce → citation-gate → approve → publish vertical
//! for the `content_draft` packet kind (port #5 part 2). Grounded drafting
//! over the local Drive corpus: evidence snippets are selected
//! deterministically (BM25 + heading-path scoring, hard budget), the model
//! must cite snippet ids for every claim, and a deterministic citation gate
//! blocks approval-readiness when any claim is uncited or unsupported.
//! Publishing is an explicit second operator action after approval and is
//! delivered through the generic content-publisher outbox adapter.

use crate::calendar_drafts::OutboxJobSummary;
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentDraftStatus {
    Staged,
    Approved,
    Rejected,
}

/// One evidence snippet handed to the drafting transform — exactly what the
/// model saw, persisted with the draft so citations stay auditable.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEvidenceSnippet {
    /// The corpus chunk id ("<file_id>:<seq>") — the citable unit.
    pub snippet_id: String,
    pub file_id: String,
    pub doc_title: String,
    pub heading_path: Vec<String>,
    /// Trimmed to the snippet budget (≤900 chars).
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_view_link: Option<String>,
}

/// Claim-support triad harvested from agent_monitor's content RAG: only Supported
/// claims are approval-ready.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentClaimStatus {
    Supported,
    MissingCitation,
    Unsupported,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentClaim {
    pub claim_id: String,
    pub text: String,
    /// Evidence snippet ids the model cited for this claim.
    pub snippet_ids: Vec<String>,
    pub status: ContentClaimStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Deterministic citation-coverage verdict (computed at stage time, stored).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCitationGate {
    /// True only when EVERY claim is Supported — approval requires it.
    pub passed: bool,
    pub missing_citation_claim_ids: Vec<String>,
    pub unsupported_claim_ids: Vec<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraft {
    /// "cnt_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub status: ContentDraftStatus,
    pub title: String,
    pub body_markdown: String,
    /// SEO essentials cherry-picked from agent_monitor's blog models — the primary
    /// search query the piece targets and the meta description. The full
    /// keyword-cluster machinery did not earn its keep in a draft-only
    /// vertical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta_description: Option<String>,
    pub claims: Vec<ContentClaim>,
    pub evidence: Vec<ContentEvidenceSnippet>,
    pub citation_gate: ContentCitationGate,
    pub model: String,
    pub confidence: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftWithRevision {
    pub draft: ContentDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftsResponse {
    pub drafts: Vec<ContentDraftWithRevision>,
    /// True when this instance has a client-specific publisher adapter.
    pub publishing_available: bool,
    /// The external-write gate. A closed gate accepts a publish request as a
    /// dry run so operators can validate the workflow without changing a site.
    pub publishing_live_enabled: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftProduceResponse {
    pub draft: ContentDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftActionRequest {
    pub action: ContentDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft's text fields. Claims/evidence/gate are
/// NOT editable — they are the audit trail of what the model grounded; an
/// operator who disagrees rejects and re-produces (or approves and edits the
/// published copy in the destination system).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftUpdateRequest {
    pub title: String,
    pub body_markdown: String,
    #[serde(default)]
    pub target_query: Option<String>,
    #[serde(default)]
    pub meta_description: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Explicit operator request to publish an already-approved content draft.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDraftPublishRequest {
    /// Lowercase, hyphen-separated URL slug (without leading/trailing slash).
    pub slug: String,
    /// Site-local publication date in YYYY-MM-DD format.
    pub published_at: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
