//! Per-client company background + owner/operator voice, used to ground
//! outward-facing LLM tasks (seeded from the client overlay, read at produce).

use serde::{Deserialize, Serialize};

/// A client's background profile. Every field is optional: a blank deployment
/// simply grounds nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientProfile {
    pub client_id: String,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub industry: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    /// Owner/operator voice line — the persona outward-facing drafts speak in.
    #[serde(default)]
    pub persona: Option<String>,
}
