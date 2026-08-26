//! Call input contracts: consent-gated call logs, transcripts, and selected
//! recordings staged as auditable source inputs before normal queue work.

use crate::source::EvidenceRecord;
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallInputSourceKind {
    DriveTranscript,
    DriveRecording,
    FolderTranscript,
    FolderRecording,
    RubySummary,
    ManualTranscript,
    Other,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputSourceConfig {
    pub source_id: String,
    pub display_name: String,
    pub kind: CallInputSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_hint: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_basis: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputsRouting {
    #[serde(default)]
    pub packet_kinds: Vec<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputsStatusResponse {
    pub configured: bool,
    pub enabled_sources: usize,
    pub pending_sources: usize,
    pub sources: Vec<CallInputSourceConfig>,
    pub routing: CallInputsRouting,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputsDriveSettingsResponse {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
    #[serde(default)]
    pub credential_user_id: Option<String>,
    #[serde(default)]
    pub drive_folder_id: Option<String>,
    #[serde(default)]
    pub drive_folder_name: Option<String>,
    #[serde(default)]
    pub ingestion_enabled: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub interval_secs: Option<u64>,
    pub credential_connected: bool,
    #[serde(default)]
    pub drive_scope_granted: Option<bool>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputsDriveSettingsUpdateRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub drive_folder_id: Option<String>,
    #[serde(default)]
    pub drive_folder_name: Option<String>,
    #[serde(default)]
    pub ingestion_enabled: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub interval_secs: Option<u64>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallInputKind {
    CallLog,
    Transcript,
    Recording,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallInputStatus {
    Staged,
    Accepted,
    Rejected,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputItem {
    pub call_input_id: String,
    pub source_id: String,
    pub source_ref: String,
    pub input_kind: CallInputKind,
    pub status: CallInputStatus,
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_email: Option<String>,
    pub transcript_text: String,
    pub recording_ref: EvidenceRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_meta: Option<CallInputTranscriptionMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub occurred_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub captured_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputTranscriptionMeta {
    pub engine: String,
    pub executable: String,
    pub executable_version: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_hash: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub audio_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub runtime_ms: u64,
    pub exit_status: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputWithRevision {
    pub input: CallInputItem,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputsResponse {
    pub inputs: Vec<CallInputWithRevision>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputStageRequest {
    pub source_id: String,
    pub source_ref: String,
    pub input_kind: CallInputKind,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub caller_name: Option<String>,
    #[serde(default)]
    pub caller_phone: Option<String>,
    #[serde(default)]
    pub caller_email: Option<String>,
    pub transcript_text: String,
    pub recording_ref: EvidenceRecord,
    #[serde(default)]
    pub transcription_meta: Option<CallInputTranscriptionMeta>,
    #[serde(default)]
    pub occurred_at_ms: Option<u64>,
    #[serde(default)]
    pub captured_at_ms: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallInputActionKind {
    Accept,
    Reject,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInputActionRequest {
    pub action: CallInputActionKind,
    #[serde(default)]
    pub packet_kinds: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
