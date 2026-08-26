//! Operator-facing release notes created by the fleet when a deployment's
//! running build changes.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNote {
    pub release_note_id: String,
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_sha: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNotesResponse {
    pub notes: Vec<ReleaseNote>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNoteCreateRequest {
    #[serde(default)]
    pub release_note_id: Option<String>,
    pub idempotency_key: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub build_sha: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNoteDismissRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_note_round_trips() {
        let note = ReleaseNote {
            release_note_id: "build_1".into(),
            title: "What's new".into(),
            summary: "The queue is easier to scan.".into(),
            body: Some("- Follow-ups are grouped more clearly.".into()),
            build_sha: Some("abc123".into()),
            created_at_ms: 1,
        };
        let json = serde_json::to_string(&note).expect("serialize");
        let back: ReleaseNote = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(note, back);
    }
}
