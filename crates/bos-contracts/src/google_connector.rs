//! Google connector status wire contract.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorStatus {
    pub service: String,
    pub connected: bool,
    /// Where the credential came from: "env" or "stored".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Scopes the platform needs but the stored credential lacks. Non-empty =
    /// the operator should reconnect to re-consent (e.g. calendar.events or
    /// calendar.calendarlist.readonly added after the original connect).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_scopes: Vec<String>,
    /// Present when not connected and the OAuth app is configured — and also
    /// when connected with missing_scopes (reconnect to grant them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_url: Option<String>,
    /// Present when not connected and the OAuth app is NOT configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleDriveFolderOption {
    pub folder_id: String,
    pub name: String,
    #[serde(default)]
    pub parent_folder_ids: Vec<String>,
    #[serde(default)]
    pub web_view_link: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleDriveFolderOptionsResponse {
    pub folders: Vec<GoogleDriveFolderOption>,
}
