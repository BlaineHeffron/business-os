//! Claim packet contracts (port #6, packet kind `claim_draft`): provider-
//! neutral shipping damage packet drafting. Shipment, order, and evidence
//! fields are DETERMINISTIC (cached source data — never model-chosen); the
//! model writes only the damage narrative + item description, grounded on
//! the damage report. A completeness gate blocks approval of packets
//! missing evidence. Approval stages a Gmail draft addressed to the filing
//! mailbox and creates a follow-up task to track the claim; no provider
//! submission is performed.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimDraftStatus {
    Staged,
    Approved,
    Rejected,
}

/// Evidence assembled from local shipment/order caches — URLs, never bytes.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimEvidence {
    /// Damage photos from the damage report (customer or pack station).
    pub damage_photo_urls: Vec<String>,
    /// Pack-time photos of the pack-station container (when fetched).
    pub pack_photo_urls: Vec<String>,
    /// The order board's pack-photo count — packing proof may exist in the
    /// source system even when the URLs above are not (yet) cached.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub pack_photo_count: i64,
}

/// The harvested completeness rule over provider-neutral evidence roles.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimPacketGate {
    /// True only when every required role is present — approval requires it.
    pub ready: bool,
    /// Missing roles: order_reference | packing_proof | tracking_reference |
    /// damage_photo.
    pub missing_roles: Vec<String>,
}

/// Provider-neutral shipment references supplied by StockForge. Legacy scalar
/// fields stay on the draft for compatibility; this envelope carries parcel,
/// LTL, and shipping-platform identifiers without BOS knowing provider internals.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimShipmentRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_shipment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier_service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pro_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bol_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_url: Option<String>,
    #[serde(default)]
    pub document_refs: Vec<ClaimShipmentDocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_api_supported: Option<bool>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimShipmentDocumentRef {
    pub kind: String,
    pub url: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraft {
    /// "clm_<item_id>_<attempt>" — one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub status: ClaimDraftStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_number: Option<String>,
    /// Local shipment/damage source that supplied this context, currently
    /// "stockforge" for the shipped adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_context_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_refs: Option<ClaimShipmentRefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_number: Option<String>,
    /// Order system from the cached order card, for example "shopify".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_platform: Option<String>,
    /// Provider-native order id when known, for example the Shopify order id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub order_total_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_date: Option<String>,
    pub damage_type: String,
    pub damage_severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_reported_at: Option<String>,
    /// Claimed amount: the damage report's amount when stated, else the
    /// order total — grounded, never model-invented. Editable (the human is
    /// the grounding when they change it).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub claim_amount_cents: i64,
    /// Model-written claim narrative (grounded on the damage report).
    pub damage_narrative: String,
    /// Model-written description of the damaged item(s).
    pub item_description: String,
    pub evidence: ClaimEvidence,
    pub packet: ClaimPacketGate,
    pub provenance: Vec<DraftFieldProvenance>,
    pub model: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftWithRevision {
    pub draft: ClaimDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftsResponse {
    pub drafts: Vec<ClaimDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftProduceResponse {
    pub draft: ClaimDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimDraftActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftActionRequest {
    pub action: ClaimDraftActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Operator edit of a STAGED draft's model-filled/judgment fields. Shipment
/// and evidence fields are immutable — they are the cached provider truth.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDraftUpdateRequest {
    pub damage_narrative: String,
    pub item_description: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub claim_amount_cents: i64,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
