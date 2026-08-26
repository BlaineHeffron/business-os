//! Operator user contracts: named operators (Jordan, Casey, …) with personal
//! bearer tokens, so every mutation receipt records WHO acted and per-user
//! provider credentials become possible. Tokens are write-only on the wire:
//! returned exactly once at create/rotate, never readable afterward.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUser {
    /// Stable id ("user_jordan") — the actor_id stamped on receipts.
    pub user_id: String,
    pub display_name: String,
    pub active: bool,
    /// Soft-deleted users stay auditable but are hidden from the default
    /// operator list and cannot authenticate.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub archived_at_ms: Option<u64>,
    /// Calendar this user's approved event drafts target when the draft
    /// doesn't pick one. Null = BOS_GOOGLE_CALENDAR_ID, then "primary".
    #[serde(default)]
    pub default_calendar_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated_at_ms: u64,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUsersResponse {
    pub users: Vec<OperatorUser>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserCreateRequest {
    pub display_name: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserCreateResponse {
    pub user: OperatorUser,
    /// Personal bearer token — shown ONCE; store it now.
    pub token: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorUserActionKind {
    Disable,
    Enable,
    Archive,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserActionRequest {
    pub action: OperatorUserActionKind,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// Set (or clear, with null) the user's default calendar.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserDefaultCalendarRequest {
    #[serde(default)]
    pub calendar_id: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_revision: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserRotateTokenRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorUserRotateTokenResponse {
    /// The replacement token — shown ONCE; the old token stops working.
    pub token: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorSessionLoginRequest {
    /// Existing shared/personal operator token. Exchanged once for an
    /// HttpOnly browser session cookie; browser JavaScript does not store it.
    pub token: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorSessionResponse {
    pub ok: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorSessionVisibilityResponse {
    /// Slice ids enabled for this client after applying the authenticated
    /// operator's UI visibility policy.
    pub visible_slices: Vec<String>,
}

/// Who the presented bearer token authenticates as.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoAmIResponse {
    /// "operator" for the shared/env token or open dev mode; the user_id for
    /// personal tokens.
    pub actor_id: String,
    pub display_name: String,
}
