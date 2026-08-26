use serde::{Deserialize, Serialize};

/// Wire form of a mutation receipt. Every mutation in the system produces one,
/// success or failure — this is the audit trail the future central dashboard reads.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptDto {
    pub receipt_id: String,
    pub client_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub change_kind: String,
    pub actor_id: String,
    pub actor_kind: ActorKindDto,
    pub outcome: ReceiptOutcomeDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision_before: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision_after: Option<u64>,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKindDto {
    Operator,
    System,
    Agent,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcomeDto {
    Applied,
    ReplayedIdempotent,
    RevisionConflict,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_round_trips() {
        let receipt = ReceiptDto {
            receipt_id: "rcpt_1".into(),
            client_id: "Example Company".into(),
            entity_kind: "email_triage_rule".into(),
            entity_id: "rule_1".into(),
            change_kind: "upsert".into(),
            actor_id: "op_example".into(),
            actor_kind: ActorKindDto::Operator,
            outcome: ReceiptOutcomeDto::Applied,
            error_class: None,
            revision_before: Some(1),
            revision_after: Some(2),
            idempotency_key: "idem_1".into(),
            correlation_id: Some("corr_1".into()),
            causation_id: None,
            created_at_ms: 1,
        };
        let json = serde_json::to_string(&receipt).expect("serialize");
        let back: ReceiptDto = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(receipt, back);
    }
}
