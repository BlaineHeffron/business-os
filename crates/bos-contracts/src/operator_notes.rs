//! Operator note contracts: manually logged notes — the second work-item
//! source family after email (Demo workflow-map W9/W11 `operator_note`).
//! Creating a note immediately emits a work item; produce kinds (CRM note,
//! follow-up task, calendar draft) then run over the note text.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNote {
    pub note_id: String,
    pub body: String,
    /// Work-queue category the note's item lands in.
    pub category_id: String,
    pub created_by: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNotesResponse {
    pub notes: Vec<OperatorNote>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNoteCreateRequest {
    pub body: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    /// Packet kinds the operator chose to spin up from this note (the form's
    /// CRM · Invoice · Follow-up checkboxes). Each is validated against the
    /// packet-kind catalog; selecting one accepts the item and produces that
    /// kind immediately — selection IS the consent to spend the LLM call.
    /// Empty defaults to `["crm_activity"]` server-side.
    #[serde(default)]
    pub actions: Vec<String>,
    /// Defaults to true for the existing Log note flow. The blank Output
    /// Composer sets false when it will stage an operator-authored typed draft
    /// itself; this prevents an unnecessary model call and produce race.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_produce: Option<bool>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNoteCreateResponse {
    pub note: OperatorNote,
    /// Stable accepted work item created for the note. Blank/manual composers
    /// use this to stage the typed draft in its owning slice.
    pub work_item_id: String,
    /// The work item the note emitted (false when an item for this note
    /// already existed — an idempotent replay).
    pub work_item_emitted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_round_trips() {
        let note = OperatorNote {
            note_id: "note_1".into(),
            body: "Dana called — wants the storefront quote by Friday.".into(),
            category_id: "operator_note".into(),
            created_by: "jordan".into(),
            created_at_ms: 1,
        };
        let json = serde_json::to_string(&note).expect("serialize");
        let back: OperatorNote = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(note, back);
    }
}
