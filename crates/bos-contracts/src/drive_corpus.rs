//! Drive RAG corpus contracts (drive_corpus slice): corpus/sync status and
//! lexical search over the local chunk index. The browser never talks to
//! Google Drive — only to the local snapshot/FTS tables the sync pump fills.

use serde::{Deserialize, Serialize};

use crate::mutation::MutationOutcomeKind;

/// Corpus configuration + sync state, for the operator status surface.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCorpusStatus {
    /// True when folder ids or include-file ids are configured.
    pub configured: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
    #[serde(default)]
    pub credential_user_id: Option<String>,
    pub folder_ids: Vec<String>,
    #[serde(default)]
    pub folder_names: Vec<DriveCorpusFolderName>,
    pub include_file_ids: Vec<String>,
    /// True when BOS_DRIVE_CORPUS_FOLDER_IDS pins the active folders. Overlay
    /// folder ids are defaults; the settings UI can override them.
    #[serde(default)]
    pub folder_selection_pinned: bool,
    /// The background pump's env gate (manual sync works regardless).
    pub sync_enabled: bool,
    /// A Google credential resolves for the corpus reader.
    pub credential_connected: bool,
    /// drive.readonly present on the resolved credential. None = scope list
    /// unknown (legacy env credential).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_scope_granted: Option<bool>,
    pub in_flight: bool,
    /// False until the initial folder walk completes.
    pub backfill_complete: bool,
    pub doc_counts: DriveCorpusDocCounts,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub chunk_count: u64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_attempt_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_sync_allowed_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCorpusFolderName {
    pub folder_id: String,
    pub name: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCorpusSettingsUpdateRequest {
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
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCorpusSettingsUpdateResponse {
    pub outcome: MutationOutcomeKind,
    pub receipt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
    pub sync_started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_refusal_reason: Option<String>,
}

/// Document counts by index status.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCorpusDocCounts {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub indexed: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stale: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub skipped: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub error: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub removed: u64,
}

/// One BM25 hit from the local chunk index.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriveSearchHit {
    pub chunk_id: String,
    pub file_id: String,
    pub doc_title: String,
    pub heading_path: Vec<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_view_link: Option<String>,
    /// bm25() rank — lower is better (SQLite returns negative scores).
    pub score: f64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriveSearchResponse {
    pub hits: Vec<DriveSearchHit>,
}

/// 202/409 envelope for the manual Sync-now kick.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveSyncNowResponse {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub next_allowed_at_ms: u64,
}
