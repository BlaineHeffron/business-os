//! Shopify customer tier write client. Payloads are assembled by BusinessOS
//! from QBO customer snapshots; this crate only validates and applies the
//! approved plan. The dry-run client is the default until the caller opens
//! its explicit write gate.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ShopifyWriteConfig {
    pub shop_domain: Option<String>,
    pub access_token: Option<String>,
    pub api_version: String,
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShopifyWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

fn permanent(code: &str, message: impl Into<String>) -> ShopifyWriteError {
    ShopifyWriteError::Permanent {
        code: code.to_string(),
        message: message.into(),
    }
}

fn retryable(code: &str, message: impl Into<String>) -> ShopifyWriteError {
    ShopifyWriteError::Retryable {
        code: code.to_string(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl ShopifyApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "shopify", capability =
/// "sync_customer_tiers"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyTierSyncOutboxPayload {
    pub idempotency_key: String,
    pub approval: ShopifyApprovalMetadata,
    pub run_id: String,
    pub mapping_version: String,
    pub actions: Vec<ShopifyTierSyncAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyTierSyncAction {
    pub qbo_customer_id: String,
    pub display_name: String,
    pub email: String,
    pub qbo_tier: String,
    pub target: ShopifyTierTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyTierTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_tag_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metafield_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyTierSyncResponse {
    pub status: ShopifyExecutionStatus,
    pub action_count: usize,
    pub customer_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShopifyCustomerMatch {
    gid: String,
    tags: Vec<String>,
}

pub trait ShopifyExecutionClient: Send + Sync {
    fn sync_customer_tiers(
        &self,
        payload: &ShopifyTierSyncOutboxPayload,
    ) -> Result<ShopifyTierSyncResponse, ShopifyWriteError>;
}

pub fn validate_tier_sync_payload(
    payload: &ShopifyTierSyncOutboxPayload,
) -> Result<(), ShopifyWriteError> {
    if !payload.approval.is_complete() {
        return Err(permanent(
            "shopify_approval_missing",
            "approval metadata is incomplete",
        ));
    }
    if payload.idempotency_key.trim().is_empty() || payload.run_id.trim().is_empty() {
        return Err(permanent(
            "shopify_idempotency_missing",
            "idempotency key and run id are required",
        ));
    }
    if payload.actions.is_empty() {
        return Err(permanent(
            "shopify_tier_sync_empty",
            "at least one customer tier action is required",
        ));
    }
    for action in &payload.actions {
        if action.qbo_customer_id.trim().is_empty()
            || action.email.trim().is_empty()
            || !action.email.contains('@')
            || action.qbo_tier.trim().is_empty()
        {
            return Err(permanent(
                "shopify_customer_tier_not_grounded",
                "each action requires QBO customer id, email, and tier",
            ));
        }
        let target = &action.target;
        let has_tag = target.tag.as_deref().is_some_and(|v| !v.trim().is_empty());
        let has_metafield = target
            .metafield_namespace
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
            && target
                .metafield_key
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty())
            && target
                .metafield_value
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty());
        if !(has_tag || has_metafield) {
            return Err(permanent(
                "shopify_customer_tier_target_missing",
                "each tier action requires a tag or metafield target; segment queries describe Shopify segments but do not mutate customers by themselves",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunShopifyClient;

impl ShopifyExecutionClient for DryRunShopifyClient {
    fn sync_customer_tiers(
        &self,
        payload: &ShopifyTierSyncOutboxPayload,
    ) -> Result<ShopifyTierSyncResponse, ShopifyWriteError> {
        validate_tier_sync_payload(payload)?;
        Ok(ShopifyTierSyncResponse {
            status: ShopifyExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("shopify_write_disabled_dry_run".to_string()),
            },
            action_count: payload.actions.len(),
            customer_ids: vec![],
        })
    }
}

#[derive(Clone)]
pub struct LiveShopifyClient {
    http: reqwest::blocking::Client,
    shop_domain: String,
    access_token: Arc<str>,
    api_version: String,
}

impl LiveShopifyClient {
    pub fn new(config: &ShopifyWriteConfig) -> Result<Self, ShopifyWriteError> {
        let shop_domain = config
            .shop_domain
            .as_deref()
            .map(normalize_shop_domain)
            .filter(|domain| !domain.is_empty())
            .ok_or_else(|| {
                permanent(
                    "shopify_shop_domain_missing",
                    "Shopify shop domain is required for live writes",
                )
            })?;
        let access_token = config
            .access_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                permanent(
                    "shopify_access_token_missing",
                    "Shopify access token is required for live writes",
                )
            })?;
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|err| retryable("shopify_http_client", err.to_string()))?;
        Ok(Self {
            http,
            shop_domain,
            access_token: Arc::from(access_token.to_string()),
            api_version: config.api_version.clone(),
        })
    }

    fn graph_url(&self) -> String {
        format!(
            "https://{}/admin/api/{}/graphql.json",
            self.shop_domain, self.api_version
        )
    }

    fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, ShopifyWriteError> {
        let response = self
            .http
            .post(self.graph_url())
            .header("X-Shopify-Access-Token", self.access_token.as_ref())
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .map_err(|err| retryable("shopify_http_request", err.to_string()))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .map_err(|err| retryable("shopify_http_response", err.to_string()))?;
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(retryable("shopify_http_retryable", body.to_string()));
        }
        if !status.is_success() {
            return Err(permanent("shopify_http_rejected", body.to_string()));
        }
        if body.get("errors").is_some() {
            return Err(retryable("shopify_graphql_errors", body.to_string()));
        }
        Ok(body)
    }

    fn find_customer(
        &self,
        email: &str,
    ) -> Result<Option<ShopifyCustomerMatch>, ShopifyWriteError> {
        let body = self.graphql(
            "query($query:String!){ customers(first:1, query:$query){ edges{ node{ id email tags } } } }",
            json!({ "query": format!("email:{email}") }),
        )?;
        Ok(body["data"]["customers"]["edges"]
            .as_array()
            .and_then(|edges| edges.first())
            .and_then(|edge| {
                let node = &edge["node"];
                let gid = node["id"].as_str()?.to_string();
                let tags = node["tags"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ShopifyCustomerMatch { gid, tags })
            }))
    }

    fn apply_action(
        &self,
        customer: &ShopifyCustomerMatch,
        action: &ShopifyTierSyncAction,
    ) -> Result<(), ShopifyWriteError> {
        if let Some(tag) = action
            .target
            .tag
            .as_deref()
            .filter(|tag| !tag.trim().is_empty())
        {
            let stale_tags = stale_exclusive_tags(
                &customer.tags,
                tag,
                action.target.exclusive_tag_prefix.as_deref(),
            );
            if !stale_tags.is_empty() {
                let body = self.graphql(
                    "mutation($id:ID!,$tags:[String!]!){ tagsRemove(id:$id,tags:$tags){ userErrors{ field message } } }",
                    json!({ "id": customer.gid, "tags": stale_tags }),
                )?;
                reject_user_errors(&body, "tagsRemove")?;
            }
            let body = self.graphql(
                "mutation($id:ID!,$tags:[String!]!){ tagsAdd(id:$id,tags:$tags){ userErrors{ field message } } }",
                json!({ "id": customer.gid, "tags": [tag] }),
            )?;
            reject_user_errors(&body, "tagsAdd")?;
        }
        if let (Some(namespace), Some(key), Some(value)) = (
            action.target.metafield_namespace.as_deref(),
            action.target.metafield_key.as_deref(),
            action.target.metafield_value.as_deref(),
        ) {
            let body = self.graphql(
                "mutation($metafields:[MetafieldsSetInput!]!){ metafieldsSet(metafields:$metafields){ userErrors{ field message } } }",
                json!({
                    "metafields": [{
                        "ownerId": customer.gid,
                        "namespace": namespace,
                        "key": key,
                        "type": "single_line_text_field",
                        "value": value,
                    }]
                }),
            )?;
            reject_user_errors(&body, "metafieldsSet")?;
        }
        Ok(())
    }
}

impl ShopifyExecutionClient for LiveShopifyClient {
    fn sync_customer_tiers(
        &self,
        payload: &ShopifyTierSyncOutboxPayload,
    ) -> Result<ShopifyTierSyncResponse, ShopifyWriteError> {
        validate_tier_sync_payload(payload)?;
        let mut ids = Vec::new();
        for action in &payload.actions {
            let Some(customer) = self.find_customer(&action.email)? else {
                return Err(permanent(
                    "shopify_customer_not_found",
                    format!("no Shopify customer found for {}", action.email),
                ));
            };
            self.apply_action(&customer, action)?;
            ids.push(customer.gid);
        }
        Ok(ShopifyTierSyncResponse {
            status: ShopifyExecutionStatus {
                executed: true,
                dry_run: false,
                reason: None,
            },
            action_count: payload.actions.len(),
            customer_ids: ids,
        })
    }
}

fn normalize_shop_domain(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn stale_exclusive_tags(
    existing_tags: &[String],
    desired_tag: &str,
    exclusive_prefix: Option<&str>,
) -> Vec<String> {
    let Some(prefix) = exclusive_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    else {
        return Vec::new();
    };
    existing_tags
        .iter()
        .filter(|tag| tag.starts_with(prefix) && tag.as_str() != desired_tag)
        .cloned()
        .collect()
}

fn reject_user_errors(body: &serde_json::Value, mutation: &str) -> Result<(), ShopifyWriteError> {
    let errors = body["data"][mutation]["userErrors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if errors.is_empty() {
        return Ok(());
    }
    Err(permanent(
        "shopify_user_errors",
        serde_json::Value::Array(errors).to_string(),
    ))
}

pub fn shopify_execution_client(
    config: &ShopifyWriteConfig,
) -> Result<Box<dyn ShopifyExecutionClient>, ShopifyWriteError> {
    if config.write_enabled {
        Ok(Box::new(LiveShopifyClient::new(config)?))
    } else {
        Ok(Box::new(DryRunShopifyClient))
    }
}

#[cfg(test)]
mod tests {
    use super::stale_exclusive_tags;

    #[test]
    fn exclusive_tier_tags_remove_old_prefixed_values_only() {
        let existing = vec![
            "tier:Contractor".to_string(),
            "tier:Distributor".to_string(),
            "VIP".to_string(),
            "Tier:Retail".to_string(),
        ];

        let stale = stale_exclusive_tags(&existing, "tier:Distributor", Some("tier:"));

        assert_eq!(stale, vec!["tier:Contractor".to_string()]);
    }

    #[test]
    fn ordinary_tags_remain_additive_without_exclusive_prefix() {
        let existing = vec!["Wholesale".to_string(), "Retail".to_string()];

        let stale = stale_exclusive_tags(&existing, "Wholesale", None);

        assert!(stale.is_empty());
    }
}
