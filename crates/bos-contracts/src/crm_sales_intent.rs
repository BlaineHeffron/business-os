//! CRM sales-intent draft contracts: a separate produce -> approve workflow for
//! pipeline intent (`crm_sales_intent`). A lead is not a durable address-book
//! contact; approval writes a provider sales object only when the configured CRM
//! supports the selected target.

use serde::{Deserialize, Serialize};

use crate::calendar_drafts::{DraftFieldProvenance, OutboxJobSummary};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmSalesIntentDraftStatus {
    Staged,
    Approved,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmSalesIntentProviderTarget {
    Lead,
    Deal,
    TaskOnly,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentDraft {
    /// "csi_<item_id>_<attempt>" - one active (non-rejected) draft per item.
    pub draft_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user_id: Option<String>,
    pub status: CrmSalesIntentDraftStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    pub lead_title: String,
    pub intent_summary: String,
    pub rationale: String,
    pub qualification_status: String,
    pub next_step_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_due_date: Option<String>,
    pub provider_target: CrmSalesIntentProviderTarget,
    pub create_businessos_task: bool,
    pub provenance: Vec<DraftFieldProvenance>,
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
pub struct CrmSalesIntentDraftWithRevision {
    pub draft: CrmSalesIntentDraft,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_job: Option<OutboxJobSummary>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentDraftsResponse {
    pub drafts: Vec<CrmSalesIntentDraftWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentProduceRequest {
    pub item_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentProduceResponse {
    pub draft: CrmSalesIntentDraftWithRevision,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrmSalesIntentActionKind {
    Approve,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentActionRequest {
    pub action: CrmSalesIntentActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmSalesIntentUpdateRequest {
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub contact_name: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
    pub lead_title: String,
    pub intent_summary: String,
    pub rationale: String,
    pub qualification_status: String,
    pub next_step_text: String,
    #[serde(default)]
    pub follow_up_due_date: Option<String>,
    pub provider_target: CrmSalesIntentProviderTarget,
    pub create_businessos_task: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sales_intent_draft_round_trips() {
        let draft = CrmSalesIntentDraft {
            draft_id: "csi_wi_1_1".into(),
            item_id: "wi_1".into(),
            source_kind: "email".into(),
            source_ref: "msg_1".into(),
            source_user_id: None,
            status: CrmSalesIntentDraftStatus::Staged,
            company_name: Some("Acme".into()),
            contact_name: Some("Sarah".into()),
            contact_email: Some("sarah@example.test".into()),
            lead_title: "Acme wholesale pricing".into(),
            intent_summary: "Sarah asked about wholesale pricing.".into(),
            rationale: "Explicit pricing interest.".into(),
            qualification_status: "qualified".into(),
            next_step_text: "Follow up next Tuesday.".into(),
            follow_up_due_date: Some("2026-06-30".into()),
            provider_target: CrmSalesIntentProviderTarget::Lead,
            create_businessos_task: true,
            provenance: vec![DraftFieldProvenance {
                field: "lead_title".into(),
                quote: "interested in wholesale pricing".into(),
            }],
            model: "test".into(),
            confidence: "high".into(),
            outbox_job_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        let back: CrmSalesIntentDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(draft, back);
    }
}
