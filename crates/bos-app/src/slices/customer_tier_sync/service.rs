//! Deterministic QBO-customer-tier to Shopify-target planning and delivery.
//! There is no AI and no inference from forms: only cached QBO tier values and
//! explicit env/config mapping can produce Shopify actions.

use std::collections::BTreeMap;

use bos_contracts::customer_tier_sync::{
    CustomerTierSyncAction, CustomerTierSyncPlan, CustomerTierSyncRun,
    CustomerTierSyncSkippedCustomer, ShopifyTierTarget,
};
use bos_integrations::shopify::{
    shopify_execution_client, ShopifyApprovalMetadata, ShopifyTierSyncAction,
    ShopifyTierSyncOutboxPayload, ShopifyWriteConfig, ShopifyWriteError,
};

use crate::env_registry;
use crate::outbox::{
    provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob,
};

pub const PROVIDER_SHOPIFY: &str = "shopify";
pub const CAPABILITY_SYNC_CUSTOMER_TIERS: &str = "sync_customer_tiers";

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TierMappingTarget {
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub metafield_namespace: Option<String>,
    #[serde(default)]
    pub metafield_key: Option<String>,
    #[serde(default)]
    pub metafield_value: Option<String>,
    #[serde(default)]
    pub segment_query: Option<String>,
}

impl TierMappingTarget {
    fn to_contract(&self) -> ShopifyTierTarget {
        ShopifyTierTarget {
            tag: clean_opt(self.tag.as_deref()),
            exclusive_tag_prefix: None,
            metafield_namespace: clean_opt(self.metafield_namespace.as_deref()),
            metafield_key: clean_opt(self.metafield_key.as_deref()),
            metafield_value: clean_opt(self.metafield_value.as_deref()),
            segment_query: clean_opt(self.segment_query.as_deref()),
        }
    }

    fn has_target(&self) -> bool {
        self.to_contract().tag.is_some()
            || (self.to_contract().metafield_namespace.is_some()
                && self.to_contract().metafield_key.is_some()
                && self.to_contract().metafield_value.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyTierTarget {
    pub metafield_namespace: Option<String>,
    pub metafield_key: Option<String>,
    pub write_tag: bool,
    pub tag_prefix: Option<String>,
}

impl CopyTierTarget {
    fn from_overlay(overlay: &crate::overlay::CustomerTierSyncOverlay) -> Option<Self> {
        if !overlay.copy_qbo_tier {
            return None;
        }
        Some(Self {
            metafield_namespace: clean_opt(overlay.metafield_namespace.as_deref()),
            metafield_key: clean_opt(overlay.metafield_key.as_deref()),
            write_tag: overlay.write_tag,
            tag_prefix: clean_opt(overlay.tag_prefix.as_deref()),
        })
    }

    fn has_target(&self) -> bool {
        self.write_tag || (self.metafield_namespace.is_some() && self.metafield_key.is_some())
    }

    fn to_contract(&self, tier: &str) -> ShopifyTierTarget {
        let tag_prefix = self.tag_prefix.as_deref().unwrap_or("tier:");
        ShopifyTierTarget {
            tag: self
                .write_tag
                .then(|| format!("{}{}", tag_prefix, tier.trim())),
            exclusive_tag_prefix: self.write_tag.then(|| tag_prefix.to_string()),
            metafield_namespace: self.metafield_namespace.clone(),
            metafield_key: self.metafield_key.clone(),
            metafield_value: self
                .metafield_namespace
                .as_ref()
                .zip(self.metafield_key.as_ref())
                .map(|_| tier.trim().to_string()),
            segment_query: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierTargetResolver {
    explicit_mappings: BTreeMap<String, TierMappingTarget>,
    copy_tier: Option<CopyTierTarget>,
}

impl TierTargetResolver {
    pub fn mapping_version(&self) -> String {
        let json = serde_json::json!({
            "explicit_mappings": self.explicit_mappings,
            "copy_tier": self.copy_tier.as_ref().map(|copy| serde_json::json!({
                "metafield_namespace": copy.metafield_namespace,
                "metafield_key": copy.metafield_key,
                "write_tag": copy.write_tag,
                "tag_prefix": copy.tag_prefix,
            })),
        });
        format!("mapping:{}", short_hash(&json.to_string()))
    }

    fn target_for_tier(&self, tier: &str) -> Option<ShopifyTierTarget> {
        self.explicit_mappings
            .get(&tier.to_ascii_lowercase())
            .map(TierMappingTarget::to_contract)
            .or_else(|| self.copy_tier.as_ref().map(|copy| copy.to_contract(tier)))
    }
}

fn clean_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn mapping_from_env() -> Result<BTreeMap<String, TierMappingTarget>, String> {
    let raw = env_registry::string(&env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON)
        .ok_or_else(|| "shopify_tier_mapping_unconfigured".to_string())?;
    mapping_from_json(&raw)
}

fn mapping_from_json(raw: &str) -> Result<BTreeMap<String, TierMappingTarget>, String> {
    let parsed: BTreeMap<String, TierMappingTarget> =
        serde_json::from_str(raw).map_err(|_err| "shopify_tier_mapping_invalid".to_string())?;
    clean_mapping(parsed)
}

pub fn mapping_from_sources(
    overlay: &crate::overlay::CustomerTierSyncOverlay,
) -> Result<BTreeMap<String, TierMappingTarget>, String> {
    match env_registry::string(&env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON) {
        Some(raw) => mapping_from_json(&raw),
        None => {
            if overlay.tier_mappings.is_empty() {
                return Err("shopify_tier_mapping_unconfigured".to_string());
            }
            let parsed = overlay
                .tier_mappings
                .iter()
                .map(|(tier, target)| {
                    (
                        tier.clone(),
                        TierMappingTarget {
                            tag: target.tag.clone(),
                            metafield_namespace: target.metafield_namespace.clone(),
                            metafield_key: target.metafield_key.clone(),
                            metafield_value: target.metafield_value.clone(),
                            segment_query: target.segment_query.clone(),
                        },
                    )
                })
                .collect();
            clean_mapping(parsed)
        }
    }
}

pub fn target_resolver_from_sources(
    overlay: &crate::overlay::CustomerTierSyncOverlay,
) -> Result<TierTargetResolver, String> {
    match env_registry::string(&env_registry::BOS_SHOPIFY_TIER_MAPPING_JSON) {
        Some(raw) => Ok(TierTargetResolver {
            explicit_mappings: mapping_from_json(&raw)?,
            copy_tier: None,
        }),
        None => {
            let explicit_mappings = if overlay.tier_mappings.is_empty() {
                BTreeMap::new()
            } else {
                let parsed = overlay
                    .tier_mappings
                    .iter()
                    .map(|(tier, target)| {
                        (
                            tier.clone(),
                            TierMappingTarget {
                                tag: target.tag.clone(),
                                metafield_namespace: target.metafield_namespace.clone(),
                                metafield_key: target.metafield_key.clone(),
                                metafield_value: target.metafield_value.clone(),
                                segment_query: target.segment_query.clone(),
                            },
                        )
                    })
                    .collect();
                clean_mapping(parsed)?
            };
            let copy_tier = CopyTierTarget::from_overlay(overlay);
            if explicit_mappings.is_empty()
                && copy_tier.as_ref().is_none_or(|target| !target.has_target())
            {
                return Err("shopify_tier_mapping_unconfigured".to_string());
            }
            Ok(TierTargetResolver {
                explicit_mappings,
                copy_tier,
            })
        }
    }
}

pub fn overlay_mapping_json(overlay: &crate::overlay::CustomerTierSyncOverlay) -> Option<String> {
    if overlay.tier_mappings.is_empty() {
        return None;
    }
    let parsed = overlay
        .tier_mappings
        .iter()
        .map(|(tier, target)| {
            (
                tier.clone(),
                TierMappingTarget {
                    tag: target.tag.clone(),
                    metafield_namespace: target.metafield_namespace.clone(),
                    metafield_key: target.metafield_key.clone(),
                    metafield_value: target.metafield_value.clone(),
                    segment_query: target.segment_query.clone(),
                },
            )
        })
        .collect();
    clean_mapping(parsed)
        .ok()
        .and_then(|mapping| serde_json::to_string(&mapping).ok())
}

fn clean_mapping(
    parsed: BTreeMap<String, TierMappingTarget>,
) -> Result<BTreeMap<String, TierMappingTarget>, String> {
    let mut cleaned = BTreeMap::new();
    for (tier, target) in parsed {
        let tier = tier.trim();
        if tier.is_empty() {
            continue;
        }
        if !target.has_target() {
            return Err("shopify_tier_mapping_target_missing".to_string());
        }
        cleaned.insert(tier.to_ascii_lowercase(), target);
    }
    if cleaned.is_empty() {
        return Err("shopify_tier_mapping_empty".to_string());
    }
    Ok(cleaned)
}

pub fn mapping_version(mapping: &BTreeMap<String, TierMappingTarget>) -> String {
    let json = serde_json::to_string(mapping).unwrap_or_else(|_| "[]".to_string());
    format!("mapping:{}", short_hash(&json))
}

pub fn run_id_for_idempotency_key(idempotency_key: &str) -> String {
    format!("tier_sync_{}", short_hash(idempotency_key))
}

fn short_hash(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw.as_bytes());
    let mut out = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn build_plan(
    customers: &[crate::slices::accounting::store::CustomerSnapshotRow],
    mapping: &BTreeMap<String, TierMappingTarget>,
) -> CustomerTierSyncPlan {
    build_plan_with_resolver(
        customers,
        &TierTargetResolver {
            explicit_mappings: mapping.clone(),
            copy_tier: None,
        },
    )
}

pub fn build_plan_with_resolver(
    customers: &[crate::slices::accounting::store::CustomerSnapshotRow],
    resolver: &TierTargetResolver,
) -> CustomerTierSyncPlan {
    let mut actions = Vec::new();
    let mut skipped = Vec::new();
    for customer in customers {
        let tier = clean_opt(customer.tier.as_deref());
        let Some(tier) = tier else {
            skipped.push(skip(customer, "qbo_tier_missing", None));
            continue;
        };
        if !customer.active {
            skipped.push(skip(customer, "qbo_customer_inactive", Some(tier)));
            continue;
        }
        let Some(email) = clean_opt(customer.email.as_deref()).filter(|value| value.contains('@'))
        else {
            skipped.push(skip(customer, "shopify_match_email_missing", Some(tier)));
            continue;
        };
        let Some(target) = resolver.target_for_tier(&tier) else {
            skipped.push(skip(customer, "qbo_tier_unmapped", Some(tier)));
            continue;
        };
        actions.push(CustomerTierSyncAction {
            qbo_customer_id: customer.customer_id.clone(),
            display_name: customer.display_name.clone(),
            email: Some(email),
            qbo_tier: tier,
            shopify: target,
        });
    }
    CustomerTierSyncPlan {
        source_provider: "qbo".to_string(),
        target_provider: "shopify".to_string(),
        mapping_version: resolver.mapping_version(),
        actions,
        skipped,
    }
}

fn skip(
    customer: &crate::slices::accounting::store::CustomerSnapshotRow,
    reason: &str,
    qbo_tier: Option<String>,
) -> CustomerTierSyncSkippedCustomer {
    CustomerTierSyncSkippedCustomer {
        qbo_customer_id: customer.customer_id.clone(),
        display_name: customer.display_name.clone(),
        reason: reason.to_string(),
        qbo_tier,
    }
}

pub fn build_approval_job(
    run: &CustomerTierSyncRun,
    approved_by: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    if run.plan.actions.is_empty() {
        return Err("customer_tier_sync_no_actions".to_string());
    }
    let payload = ShopifyTierSyncOutboxPayload {
        idempotency_key: format!("shopify_tier_sync:{}", run.run_id),
        approval: ShopifyApprovalMetadata {
            approval_id: format!("approve:{}", run.run_id),
            approved_by: approved_by.to_string(),
            approved_at: now_ms.to_string(),
        },
        run_id: run.run_id.clone(),
        mapping_version: run.plan.mapping_version.clone(),
        actions: run
            .plan
            .actions
            .iter()
            .map(|action| ShopifyTierSyncAction {
                qbo_customer_id: action.qbo_customer_id.clone(),
                display_name: action.display_name.clone(),
                email: action.email.clone().unwrap_or_default(),
                qbo_tier: action.qbo_tier.clone(),
                target: bos_integrations::shopify::ShopifyTierTarget {
                    tag: action.shopify.tag.clone(),
                    exclusive_tag_prefix: action.shopify.exclusive_tag_prefix.clone(),
                    metafield_namespace: action.shopify.metafield_namespace.clone(),
                    metafield_key: action.shopify.metafield_key.clone(),
                    metafield_value: action.shopify.metafield_value.clone(),
                    segment_query: action.shopify.segment_query.clone(),
                },
            })
            .collect(),
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|err| format!("serialize shopify tier sync payload: {err}"))?;
    Ok(NewOutboxJob {
        job_id: format!("shopify_tier_sync_{}", run.run_id),
        provider: PROVIDER_SHOPIFY.to_string(),
        capability: CAPABILITY_SYNC_CUSTOMER_TIERS.to_string(),
        payload_json,
        source_entity_kind: crate::slices::customer_tier_sync::store::RUN_ENTITY_KIND.to_string(),
        source_entity_id: run.run_id.clone(),
        correlation_id: Some(run.run_id.clone()),
        causation_id: None,
        idempotency_key: format!("outbox:shopify_tier_sync:{}", run.run_id),
    })
}

pub fn shopify_config_from_env() -> ShopifyWriteConfig {
    let shop_domain = env_registry::string(&env_registry::BOS_SHOPIFY_SHOP_DOMAIN);
    let access_token = shop_domain
        .as_deref()
        .and_then(crate::slices::shopify_sales::service::shopify_access_token_from_env);
    ShopifyWriteConfig {
        shop_domain,
        access_token,
        api_version: env_registry::string(&env_registry::BOS_SHOPIFY_API_VERSION)
            .unwrap_or_else(|| "2026-01".to_string()),
        write_enabled: false,
    }
}

fn shopify_config_from_settings(state: &crate::http::AppState) -> ShopifyWriteConfig {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_SHOPIFY_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "shopify write gate read failed");
            false
        })
    };
    ShopifyWriteConfig {
        write_enabled,
        ..shopify_config_from_env()
    }
}

pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let config = shopify_config_from_settings(state);
    execute_job(job, &config, now_ms)
}

pub fn execute_job(job: &ClaimedJob, config: &ShopifyWriteConfig, now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_SHOPIFY || job.capability != CAPABILITY_SYNC_CUSTOMER_TIERS {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload: ShopifyTierSyncOutboxPayload = match serde_json::from_str(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("shopify_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = match shopify_execution_client(config) {
        Ok(client) => client,
        Err(err) => return shopify_error_outcome(err, job.attempts, now_ms),
    };
    match client.sync_customer_tiers(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "executed": response.status.executed,
                "reason": response.status.reason,
                "action_count": response.action_count,
                "customer_ids": response.customer_ids,
            })
            .to_string(),
        },
        Err(err) => shopify_error_outcome(err, job.attempts, now_ms),
    }
}

fn shopify_error_outcome(err: ShopifyWriteError, attempts: u32, now_ms: u64) -> AttemptOutcome {
    match err {
        ShopifyWriteError::Permanent { code, message } => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: None,
        },
        ShopifyWriteError::Retryable { code, message } => AttemptOutcome::Retry {
            error: provider_error_detail(&code, &message),
            retry_at_ms: now_ms + retry_backoff_ms(attempts),
        },
    }
}
