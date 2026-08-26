//! Generic outbox operator actions. Outbox job summaries are embedded in draft
//! read models; these requests cover recovery actions on the shared delivery
//! spine.

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRetryRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub actor_id: Option<String>,
}
