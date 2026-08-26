//! Shopify sales cache service helpers: config, visibility policy, connector
//! status, and wire row assembly. Views are assembled from local snapshots
//! only.

use bos_contracts::shopify_sales::{
    ShopifyCustomerSnapshotRow as ShopifyCustomerDto, ShopifyOrderLineItemSummary,
    ShopifyOrderSnapshotRow as ShopifyOrderDto, ShopifySalesConnectorStatus,
};
use bos_integrations::shopify_oauth::{fetch_access_token, ShopifyOAuthApp};
use bos_integrations::shopify_sales_read::ShopifySalesReadConfig;
use rusqlite::Connection;

use super::store::{ShopifyCustomerSnapshotRow, ShopifyOrderSnapshotRow};
use crate::env_registry;
use crate::http::OperatorScope;
use crate::store_core::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifySalesConnectorConfig {
    pub shop_domain: String,
    pub access_token: String,
    pub api_version: String,
}

impl ShopifySalesConnectorConfig {
    pub fn to_read_config(&self) -> ShopifySalesReadConfig {
        ShopifySalesReadConfig {
            shop_domain: Some(self.shop_domain.clone()),
            access_token: Some(self.access_token.clone()),
            api_version: self.api_version.clone(),
        }
    }
}

pub fn connector_config_from_env() -> Option<ShopifySalesConnectorConfig> {
    let shop_domain = env_registry::string(&env_registry::BOS_SHOPIFY_SHOP_DOMAIN)?
        .trim()
        .to_string();
    let access_token = shopify_access_token_from_env(&shop_domain)?;
    if shop_domain.is_empty() || access_token.is_empty() {
        return None;
    }
    Some(ShopifySalesConnectorConfig {
        shop_domain,
        access_token,
        api_version: env_registry::string(&env_registry::BOS_SHOPIFY_API_VERSION)
            .unwrap_or_else(|| "2026-01".to_string()),
    })
}

pub fn connector_config_present_from_env() -> bool {
    let Some(shop_domain) = env_registry::string(&env_registry::BOS_SHOPIFY_SHOP_DOMAIN) else {
        return false;
    };
    !shop_domain.trim().is_empty() && shopify_access_config_present_from_env()
}

pub fn shopify_access_config_present_from_env() -> bool {
    if env_registry::string(&env_registry::BOS_SHOPIFY_ACCESS_TOKEN)
        .is_some_and(|token| !token.trim().is_empty())
    {
        return true;
    }
    env_registry::string(&env_registry::BOS_SHOPIFY_CLIENT_ID)
        .is_some_and(|value| !value.trim().is_empty())
        && env_registry::string(&env_registry::BOS_SHOPIFY_CLIENT_SECRET)
            .is_some_and(|value| !value.trim().is_empty())
}

pub fn shopify_access_token_from_env(shop_domain: &str) -> Option<String> {
    if let Some(token) = env_registry::string(&env_registry::BOS_SHOPIFY_ACCESS_TOKEN)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
    {
        return Some(token);
    }
    let client_id = env_registry::string(&env_registry::BOS_SHOPIFY_CLIENT_ID)?
        .trim()
        .to_string();
    let client_secret = env_registry::string(&env_registry::BOS_SHOPIFY_CLIENT_SECRET)?
        .trim()
        .to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return None;
    }
    match fetch_access_token(&ShopifyOAuthApp {
        shop_domain: shop_domain.to_string(),
        client_id,
        client_secret,
        token_url: None,
    }) {
        Ok(token) => Some(token),
        Err(err) => {
            tracing::warn!(error = %err, "shopify client-credential token exchange failed");
            None
        }
    }
}

pub fn connector_status(has_synced: bool) -> ShopifySalesConnectorStatus {
    let shop_domain = env_registry::string(&env_registry::BOS_SHOPIFY_SHOP_DOMAIN)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match (shop_domain, shopify_access_config_present_from_env()) {
        (Some(shop_domain), true) => ShopifySalesConnectorStatus {
            configured: true,
            shop_domain: Some(shop_domain),
            has_synced,
            blocked_reason: None,
        },
        (shop_domain, false) => ShopifySalesConnectorStatus {
            configured: false,
            shop_domain,
            has_synced,
            blocked_reason: Some(
                "shopify_unconfigured: set BOS_SHOPIFY_SHOP_DOMAIN and either BOS_SHOPIFY_ACCESS_TOKEN or BOS_SHOPIFY_CLIENT_ID/BOS_SHOPIFY_CLIENT_SECRET"
                    .to_string(),
            ),
        },
        (None, true) => ShopifySalesConnectorStatus {
            configured: false,
            shop_domain: None,
            has_synced,
            blocked_reason: Some(
                "shopify_unconfigured: set BOS_SHOPIFY_SHOP_DOMAIN and either BOS_SHOPIFY_ACCESS_TOKEN or BOS_SHOPIFY_CLIENT_ID/BOS_SHOPIFY_CLIENT_SECRET"
                    .to_string(),
            ),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShopifySalesVisibilityPolicy {
    Shared,
    AdminOnly,
    AuthorizerOnly,
}

impl ShopifySalesVisibilityPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shared" => Some(Self::Shared),
            "admin_only" => Some(Self::AdminOnly),
            "" | "authorizer_only" => Some(Self::AuthorizerOnly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::AdminOnly => "admin_only",
            Self::AuthorizerOnly => "authorizer_only",
        }
    }
}

pub fn visibility_policy_from_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<ShopifySalesVisibilityPolicy, StoreError> {
    match crate::slices::admin_settings::service::value(
        conn,
        client_id,
        &env_registry::BOS_SHOPIFY_SALES_VISIBILITY_POLICY,
    )? {
        Some(raw) if !raw.trim().is_empty() => ShopifySalesVisibilityPolicy::parse(&raw)
            .ok_or_else(|| {
                StoreError::Domain(format!(
                    "unknown BOS_SHOPIFY_SALES_VISIBILITY_POLICY: {}",
                    raw.trim()
                ))
            }),
        _ => Ok(ShopifySalesVisibilityPolicy::AuthorizerOnly),
    }
}

pub fn financial_visible(scope: &OperatorScope, policy: ShopifySalesVisibilityPolicy) -> bool {
    match policy {
        ShopifySalesVisibilityPolicy::Shared => true,
        ShopifySalesVisibilityPolicy::AdminOnly | ShopifySalesVisibilityPolicy::AuthorizerOnly => {
            matches!(scope, OperatorScope::All)
        }
    }
}

pub fn recent_orders(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    limit: usize,
) -> Result<Vec<ShopifyOrderDto>, StoreError> {
    let policy = visibility_policy_from_settings(conn, client_id)?;
    let visible = financial_visible(scope, policy);
    super::store::list_recent_orders(conn, client_id, scope, visible, limit)
        .map(|rows| rows.iter().map(order_row).collect())
}

pub fn orders_for_customer(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    email: &str,
    limit: usize,
) -> Result<Vec<ShopifyOrderDto>, StoreError> {
    let policy = visibility_policy_from_settings(conn, client_id)?;
    let visible = financial_visible(scope, policy);
    super::store::orders_by_customer(conn, client_id, scope, visible, email, limit)
        .map(|rows| rows.iter().map(order_row).collect())
}

pub fn customers_for_email(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    email: &str,
    limit: usize,
) -> Result<Vec<ShopifyCustomerDto>, StoreError> {
    let policy = visibility_policy_from_settings(conn, client_id)?;
    let visible = financial_visible(scope, policy);
    super::store::customers_by_email(conn, client_id, scope, visible, email, limit)
        .map(|rows| rows.iter().map(customer_row).collect())
}

pub fn shop_domain_fingerprint(shop_domain: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalized = normalize_shop_domain(shop_domain);
    let digest = Sha256::digest(normalized.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn normalize_shop_domain(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

pub fn order_row(row: &ShopifyOrderSnapshotRow) -> ShopifyOrderDto {
    ShopifyOrderDto {
        order_id: row.order_id.clone(),
        order_number: row.order_number.clone(),
        customer_email: row.customer_email.clone(),
        customer_name: row.customer_name.clone(),
        total_cents: row.total_cents,
        currency: row.currency.clone(),
        financial_status: row.financial_status.clone(),
        fulfillment_status: row.fulfillment_status.clone(),
        tracking_number: row.tracking_number.clone(),
        carrier: row.tracking_carrier.clone(),
        line_items_summary: row.line_items_summary.clone(),
        line_items: parse_line_items(&row.line_items_json),
        created_at: row.created_at.clone(),
    }
}

pub fn customer_row(row: &ShopifyCustomerSnapshotRow) -> ShopifyCustomerDto {
    ShopifyCustomerDto {
        customer_id: row.customer_id.clone(),
        email: row.email.clone(),
        name: row.name.clone(),
        phone: row.phone.clone(),
        total_spent_cents: row.total_spent_cents,
        orders_count: row.orders_count,
        tags: row.tags.split(',').filter_map(clean).collect::<Vec<_>>(),
        tier: row.tier.clone(),
    }
}

fn parse_line_items(raw: &str) -> Vec<ShopifyOrderLineItemSummary> {
    serde_json::from_str::<Vec<bos_integrations::shopify_sales_read::ShopifyLineItemRecord>>(raw)
        .unwrap_or_default()
        .into_iter()
        .map(|item| ShopifyOrderLineItemSummary {
            title: item.title,
            sku: item.sku,
            quantity: item.quantity,
        })
        .collect()
}

fn clean(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorizer_only_collapses_to_admin_only_without_connecting_user() {
        assert!(financial_visible(
            &OperatorScope::All,
            ShopifySalesVisibilityPolicy::AuthorizerOnly
        ));
        assert!(!financial_visible(
            &OperatorScope::User("casey".to_string()),
            ShopifySalesVisibilityPolicy::AuthorizerOnly
        ));
    }

    #[test]
    fn shared_visibility_allows_named_operator() {
        assert!(financial_visible(
            &OperatorScope::User("casey".to_string()),
            ShopifySalesVisibilityPolicy::Shared
        ));
    }
}
