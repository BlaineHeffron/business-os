//! CRM record-create draft contracts: the produce → approve → provider-write
//! vertical for the `crm_record_create` packet kind. A note that references a
//! company and/or people who are NOT yet in the CRM yields one or more drafts
//! proposing the missing records; each approval runs one ensure-chain write
//! (account → contact) against the configured CRM. Names are grounded — a
//! record with an invented name is refused. Both providers run the ensure-chain:
//! EspoCRM (Account → Contact) and HubSpot (Company → Contact + default
//! association), each behind its own write gate.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};
use crate::enrichment::EnrichmentConfidence;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmRecordDraftStatus {
    Staged,
    Approved,
    Rejected,
}

/// One field the website-enrichment pass filled, with where it came from — the
/// operator-facing "why this value" record.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmEnrichmentTraceItem {
    /// Draft field the value was applied to (e.g. "company_phone").
    pub field: String,
    /// Previous value when enrichment replaced a weak prefill instead of
    /// filling an empty field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_value: Option<String>,
    pub value: String,
    /// Provenance: "page:<url>" for the deterministic pass, or the literal page
    /// quote the LLM grounded against.
    pub source: String,
    /// "deterministic" (schema.org/OpenGraph/regex) or "ai" (gap-filler).
    pub via: String,
}

/// One search result considered by web-search enrichment before any fetched
/// page text is handed to a typed transform.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSearchTraceResult {
    pub query: String,
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Per-field Tier-4 research provenance surfaced beside the editable draft
/// value. This mirrors the exact span accepted by the engine; the UI does not
/// re-derive confidence, sensitivity, or provenance.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmResearchFieldAnnotation {
    pub field_id: String,
    pub confidence: EnrichmentConfidence,
    pub source_domain: String,
    pub quote: String,
    pub person_sensitive: bool,
}

/// A bounded, temporary record of one enrichment run, shown in the panel so the
/// operator can see exactly what the crawl read and what was fed to the model.
/// Cleared on approval (review aid, not durable state).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmEnrichmentTrace {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub captured_at_ms: u64,
    /// The domain the note named (the crawl seed).
    pub domain: String,
    /// URLs actually fetched (homepage first).
    pub pages: Vec<String>,
    /// Fields the run produced, deterministic + AI, with provenance.
    pub items: Vec<CrmEnrichmentTraceItem>,
    /// Whether the bounded LLM gap-filler ran (false = deterministic covered
    /// everything, or nothing was missing).
    pub llm_ran: bool,
    /// Size of the stripped page text handed to the gap-filler.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub llm_input_chars: u32,
    /// Bounded preview of that text (head), so the operator can see what the
    /// model actually read without storing whole pages.
    pub llm_input_preview: String,
    /// Whether gated web search ran after crawler evidence was insufficient.
    #[serde(default)]
    pub search_ran: bool,
    /// Why search was eligible for this contract/field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_reason: Option<String>,
    /// Purpose-scoped queries issued to the configured search endpoint.
    #[serde(default)]
    pub search_queries: Vec<String>,
    /// Bounded search results retained for diagnostics/provenance.
    #[serde(default)]
    pub search_results: Vec<CrmSearchTraceResult>,
    /// Search/crawl/model failures that were non-fatal to draft production.
    #[serde(default)]
    pub failures: Vec<String>,
    /// Tier-4 research annotations for fields actually surfaced to the draft.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research_annotations: Vec<CrmResearchFieldAnnotation>,
}

/// Provider record ids resolved by the ensure-chain at delivery time (the
/// account is created/found first, then the contact linked to it).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordProviderIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraft {
    /// "crd_<item_id>_<attempt>[_<contact_index>]" — multiple active drafts
    /// are allowed when one source names multiple missing contacts.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub status: CrmRecordDraftStatus,
    /// Propose creating a Company record (false when one already matched).
    pub create_company: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_description: Option<String>,
    /// Propose creating a Contact record (false when one already matched).
    pub create_contact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_title: Option<String>,
    /// Filled on delivery (empty until then).
    pub provider_ids: CrmRecordProviderIds,
    pub provenance: Vec<DraftFieldProvenance>,
    /// Website-enrichment trace (what the crawl read + fed the model). Present
    /// only while staged with an enrichment run; cleared on approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrichment_trace: Option<CrmEnrichmentTrace>,
    /// Tier-4 research provenance keyed by draft field. Empty for standard
    /// enrichment and skipped on the wire to preserve existing DTO bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research_annotations: Vec<CrmResearchFieldAnnotation>,
    pub model: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftWithRevision {
    pub draft: CrmRecordDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftsResponse {
    pub drafts: Vec<CrmRecordDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftProduceResponse {
    pub draft: CrmRecordDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmRecordDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftActionRequest {
    pub action: CrmRecordDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft. The proposed-record set and every field
/// stay editable until approval (AI proposes, the human disposes); the server
/// re-validates that each proposed record carries a non-empty name.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmRecordDraftUpdateRequest {
    pub create_company: bool,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub company_website: Option<String>,
    #[serde(default)]
    pub company_phone: Option<String>,
    #[serde(default)]
    pub company_address: Option<String>,
    #[serde(default)]
    pub company_description: Option<String>,
    pub create_contact: bool,
    #[serde(default)]
    pub contact_first_name: Option<String>,
    #[serde(default)]
    pub contact_last_name: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default)]
    pub contact_phone: Option<String>,
    #[serde(default)]
    pub contact_title: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
