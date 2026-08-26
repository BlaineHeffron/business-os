//! Wire form of mutation outcomes (shared by every slice's mutation routes).

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOutcomeKind {
    Applied,
    ReplayedIdempotent,
    RevisionConflict,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationResponse {
    pub outcome: MutationOutcomeKind,
    pub receipt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_response_round_trips() {
        let response = MutationResponse {
            outcome: MutationOutcomeKind::Applied,
            receipt_id: "rcpt_1".into(),
            revision: Some(2),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let back: MutationResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(response, back);
    }
}
