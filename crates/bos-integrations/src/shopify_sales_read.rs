//! Shopify read-only sales client: recent orders and customer snapshots from
//! the Admin GraphQL API. No write operations live in this module.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub const SHOPIFY_MAX_PAGE_SIZE: u32 = 250;

#[derive(Debug, Clone)]
pub struct ShopifySalesReadConfig {
    pub shop_domain: Option<String>,
    pub access_token: Option<String>,
    pub api_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShopifySalesReadError {
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    AuthRejected {
        message: String,
    },
    Retryable {
        code: String,
        message: String,
    },
    Permanent {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for ShopifySalesReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthRejected { message } => write!(formatter, "auth_rejected: {message}"),
            Self::Retryable { code, message } => write!(formatter, "{code}: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

pub trait ShopifyHttp: Send + Sync {
    fn post_graphql(
        &self,
        url: &str,
        access_token: &str,
        body: Value,
    ) -> Result<ShopifyHttpResponse, ShopifySalesReadError>;
}

pub struct ShopifyHttpResponse {
    pub status: u16,
    pub body: Value,
    pub retry_after_secs: Option<u64>,
}

pub struct ReqwestShopifyHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestShopifyHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl ShopifyHttp for ReqwestShopifyHttpClient {
    fn post_graphql(
        &self,
        url: &str,
        access_token: &str,
        body: Value,
    ) -> Result<ShopifyHttpResponse, ShopifySalesReadError> {
        let response = self
            .client
            .post(url)
            .header("X-Shopify-Access-Token", access_token)
            .json(&body)
            .send()
            .map_err(|err| ShopifySalesReadError::Retryable {
                code: "shopify_request_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(ShopifyHttpResponse {
            status,
            body,
            retry_after_secs,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyMoney {
    pub cents: i64,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyLineItemRecord {
    pub title: String,
    pub sku: Option<String>,
    pub quantity: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyOrderRecord {
    pub order_id: String,
    pub order_number: String,
    pub customer_email: Option<String>,
    pub customer_name: Option<String>,
    pub total: ShopifyMoney,
    pub financial_status: Option<String>,
    pub fulfillment_status: Option<String>,
    pub tracking_number: Option<String>,
    pub tracking_carrier: Option<String>,
    pub tracking_url: Option<String>,
    pub line_items: Vec<ShopifyLineItemRecord>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyCustomerRecord {
    pub customer_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub total_spent: ShopifyMoney,
    pub orders_count: i64,
    pub tags: Vec<String>,
    pub tier: Option<String>,
    pub updated_at: Option<String>,
}

pub trait ShopifySalesReadClient: Send + Sync {
    fn fetch_recent_orders_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyOrderRecord>, ShopifySalesReadError>;

    fn fetch_customers_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyCustomerRecord>, ShopifySalesReadError>;

    fn fetch_recent_orders(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyOrderRecord>, ShopifySalesReadError>;

    fn fetch_customers(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyCustomerRecord>, ShopifySalesReadError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifySalesPage<T> {
    pub records: Vec<T>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

pub struct LiveShopifySalesReadClient<H: ShopifyHttp = ReqwestShopifyHttpClient> {
    http: Arc<H>,
    shop_domain: String,
    access_token: String,
    api_version: String,
}

impl<H: ShopifyHttp> LiveShopifySalesReadClient<H> {
    pub fn new(
        http: Arc<H>,
        config: ShopifySalesReadConfig,
    ) -> Result<Self, ShopifySalesReadError> {
        let shop_domain = config
            .shop_domain
            .as_deref()
            .map(normalize_shop_domain)
            .filter(|domain| !domain.is_empty())
            .ok_or_else(|| ShopifySalesReadError::Permanent {
                code: "shopify_shop_domain_missing".to_string(),
                message: "Shopify shop domain is required for reads".to_string(),
            })?;
        let access_token = config
            .access_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| ShopifySalesReadError::Permanent {
                code: "shopify_access_token_missing".to_string(),
                message: "Shopify access token is required for reads".to_string(),
            })?
            .to_string();
        Ok(Self {
            http,
            shop_domain,
            access_token,
            api_version: config.api_version,
        })
    }

    fn graph_url(&self) -> String {
        format!(
            "https://{}/admin/api/{}/graphql.json",
            self.shop_domain, self.api_version
        )
    }

    fn graphql(&self, query: &str, variables: Value) -> Result<Value, ShopifySalesReadError> {
        let response = self.http.post_graphql(
            &self.graph_url(),
            &self.access_token,
            json!({ "query": query, "variables": variables }),
        )?;
        match response.status {
            200..=299 => {
                if response.body.get("errors").is_some() {
                    return Err(ShopifySalesReadError::Retryable {
                        code: "shopify_graphql_errors".to_string(),
                        message: response.body.to_string(),
                    });
                }
                Ok(response.body)
            }
            401 | 403 => Err(ShopifySalesReadError::AuthRejected {
                message: format!("shopify {} auth rejected", response.status),
            }),
            429 => Err(ShopifySalesReadError::RateLimited {
                retry_after_ms: response.retry_after_secs.map(|secs| secs * 1000),
                message: "shopify 429".to_string(),
            }),
            500..=599 => Err(ShopifySalesReadError::Retryable {
                code: "shopify_server_error".to_string(),
                message: format!("shopify {}", response.status),
            }),
            other => Err(ShopifySalesReadError::Permanent {
                code: "shopify_request_rejected".to_string(),
                message: format!("shopify {other}: {}", response.body),
            }),
        }
    }
}

impl<H: ShopifyHttp> ShopifySalesReadClient for LiveShopifySalesReadClient<H> {
    fn fetch_recent_orders_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyOrderRecord>, ShopifySalesReadError> {
        let body = self.graphql(
            ORDERS_QUERY,
            json!({ "first": limit.clamp(1, SHOPIFY_MAX_PAGE_SIZE), "after": after }),
        )?;
        Ok(ShopifySalesPage {
            records: connection_nodes(&body["data"]["orders"])
                .into_iter()
                .filter_map(order_from_node)
                .collect(),
            has_next_page: page_has_next(&body["data"]["orders"]),
            end_cursor: page_end_cursor(&body["data"]["orders"]),
        })
    }

    fn fetch_recent_orders(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyOrderRecord>, ShopifySalesReadError> {
        Ok(self.fetch_recent_orders_page(limit, None)?.records)
    }

    fn fetch_customers_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyCustomerRecord>, ShopifySalesReadError> {
        let body = self.graphql(
            CUSTOMERS_QUERY,
            json!({ "first": limit.clamp(1, SHOPIFY_MAX_PAGE_SIZE), "after": after }),
        )?;
        Ok(ShopifySalesPage {
            records: connection_nodes(&body["data"]["customers"])
                .into_iter()
                .filter_map(customer_from_node)
                .collect(),
            has_next_page: page_has_next(&body["data"]["customers"]),
            end_cursor: page_end_cursor(&body["data"]["customers"]),
        })
    }

    fn fetch_customers(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyCustomerRecord>, ShopifySalesReadError> {
        Ok(self.fetch_customers_page(limit, None)?.records)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FixtureShopifySalesReadClient {
    pub orders: Vec<ShopifyOrderRecord>,
    pub customers: Vec<ShopifyCustomerRecord>,
}

impl ShopifySalesReadClient for FixtureShopifySalesReadClient {
    fn fetch_recent_orders_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyOrderRecord>, ShopifySalesReadError> {
        let start = after
            .and_then(|cursor| cursor.strip_prefix("fixture:"))
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(0);
        let end = (start + limit as usize).min(self.orders.len());
        Ok(ShopifySalesPage {
            records: self.orders[start..end].to_vec(),
            has_next_page: end < self.orders.len(),
            end_cursor: (end < self.orders.len()).then(|| format!("fixture:{end}")),
        })
    }

    fn fetch_recent_orders(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyOrderRecord>, ShopifySalesReadError> {
        Ok(self.orders.iter().take(limit as usize).cloned().collect())
    }

    fn fetch_customers_page(
        &self,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ShopifySalesPage<ShopifyCustomerRecord>, ShopifySalesReadError> {
        let start = after
            .and_then(|cursor| cursor.strip_prefix("fixture:"))
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(0);
        let end = (start + limit as usize).min(self.customers.len());
        Ok(ShopifySalesPage {
            records: self.customers[start..end].to_vec(),
            has_next_page: end < self.customers.len(),
            end_cursor: (end < self.customers.len()).then(|| format!("fixture:{end}")),
        })
    }

    fn fetch_customers(
        &self,
        limit: u32,
    ) -> Result<Vec<ShopifyCustomerRecord>, ShopifySalesReadError> {
        Ok(self
            .customers
            .iter()
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

const ORDERS_QUERY: &str = r#"
query ShopifySalesOrders($first:Int!, $after:String) {
  orders(first:$first, after:$after, sortKey:UPDATED_AT, reverse:true) {
    edges {
      node {
        id
        name
        email
        createdAt
        updatedAt
        displayFinancialStatus
        displayFulfillmentStatus
        totalPriceSet { shopMoney { amount currencyCode } }
        customer { email displayName firstName lastName phone }
        lineItems(first:20) { edges { node { title sku quantity } } }
        fulfillments(first:5) { trackingInfo { number company url } }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

const CUSTOMERS_QUERY: &str = r#"
query ShopifySalesCustomers($first:Int!, $after:String) {
  customers(first:$first, after:$after, sortKey:UPDATED_AT, reverse:true) {
    edges {
      node {
        id
        email
        displayName
        firstName
        lastName
        phone
        amountSpent { amount currencyCode }
        numberOfOrders
        tags
        updatedAt
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

fn normalize_shop_domain(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

pub fn parse_money_cents(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let negative = raw.starts_with('-');
    let raw = raw.trim_start_matches('-');
    let mut parts = raw.split('.');
    let whole = parts.next()?.parse::<i64>().ok()?;
    let frac = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return None;
    }
    let mut cents = 0_i64;
    let mut chars = frac.chars();
    if let Some(ch) = chars.next() {
        if !ch.is_ascii_digit() {
            return None;
        }
        cents += (ch as i64 - '0' as i64) * 10;
    }
    if let Some(ch) = chars.next() {
        if !ch.is_ascii_digit() {
            return None;
        }
        cents += ch as i64 - '0' as i64;
    }
    if let Some(ch) = chars.next() {
        if !ch.is_ascii_digit() {
            return None;
        }
        if ch >= '5' {
            cents += 1;
        }
    }
    let value = whole.checked_mul(100)?.checked_add(cents)?;
    Some(if negative { -value } else { value })
}

fn money_from_value(value: &Value) -> ShopifyMoney {
    ShopifyMoney {
        cents: value
            .get("amount")
            .and_then(Value::as_str)
            .and_then(parse_money_cents)
            .unwrap_or(0),
        currency: string_field(value, "currencyCode"),
    }
}

fn connection_nodes(connection: &Value) -> Vec<&Value> {
    connection
        .get("edges")
        .and_then(Value::as_array)
        .map(|edges| {
            edges
                .iter()
                .filter_map(|edge| edge.get("node"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn page_has_next(connection: &Value) -> bool {
    connection
        .get("pageInfo")
        .and_then(|page| page.get("hasNextPage"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn page_end_cursor(connection: &Value) -> Option<String> {
    connection
        .get("pageInfo")
        .and_then(|page| page.get("endCursor"))
        .and_then(Value::as_str)
        .and_then(clean)
}

fn order_from_node(node: &Value) -> Option<ShopifyOrderRecord> {
    let id = string_field(node, "id")?;
    let customer = node.get("customer").unwrap_or(&Value::Null);
    let name = string_field(node, "name").unwrap_or_else(|| id.clone());
    let tracking = node
        .get("fulfillments")
        .and_then(Value::as_array)
        .and_then(|fulfillments| fulfillments.first())
        .and_then(|fulfillment| fulfillment.get("trackingInfo"))
        .and_then(Value::as_array)
        .and_then(|infos| infos.first())
        .unwrap_or(&Value::Null);
    Some(ShopifyOrderRecord {
        order_id: id,
        order_number: name,
        customer_email: string_field(node, "email").or_else(|| string_field(customer, "email")),
        customer_name: string_field(customer, "displayName").or_else(|| {
            join_name(
                string_field(customer, "firstName").as_deref(),
                string_field(customer, "lastName").as_deref(),
            )
        }),
        total: money_from_value(&node["totalPriceSet"]["shopMoney"]),
        financial_status: string_field(node, "displayFinancialStatus"),
        fulfillment_status: string_field(node, "displayFulfillmentStatus"),
        tracking_number: string_field(tracking, "number"),
        tracking_carrier: string_field(tracking, "company"),
        tracking_url: string_field(tracking, "url"),
        line_items: connection_nodes(&node["lineItems"])
            .into_iter()
            .filter_map(line_item_from_node)
            .collect(),
        created_at: string_field(node, "createdAt"),
        updated_at: string_field(node, "updatedAt"),
    })
}

fn line_item_from_node(node: &Value) -> Option<ShopifyLineItemRecord> {
    Some(ShopifyLineItemRecord {
        title: string_field(node, "title")?,
        sku: string_field(node, "sku"),
        quantity: node.get("quantity").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn customer_from_node(node: &Value) -> Option<ShopifyCustomerRecord> {
    let tags: Vec<String> = node
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .filter_map(clean)
                .collect()
        })
        .unwrap_or_default();
    let tier = tags
        .iter()
        .find_map(|tag: &String| tag.strip_prefix("tier:").and_then(clean))
        .or_else(|| {
            tags.iter()
                .find(|tag| tag.to_ascii_lowercase().contains("tier"))
                .cloned()
        });
    Some(ShopifyCustomerRecord {
        customer_id: string_field(node, "id")?,
        email: string_field(node, "email"),
        name: string_field(node, "displayName").or_else(|| {
            join_name(
                string_field(node, "firstName").as_deref(),
                string_field(node, "lastName").as_deref(),
            )
        }),
        phone: string_field(node, "phone"),
        total_spent: money_from_value(&node["amountSpent"]),
        orders_count: node
            .get("numberOfOrders")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        tags,
        tier,
        updated_at: string_field(node, "updatedAt"),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).and_then(clean)
}

fn clean(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn join_name(first: Option<&str>, last: Option<&str>) -> Option<String> {
    let joined = format!(
        "{} {}",
        first.unwrap_or("").trim(),
        last.unwrap_or("").trim()
    );
    clean(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_parser_rounds_to_cents() {
        assert_eq!(parse_money_cents("123.45"), Some(12345));
        assert_eq!(parse_money_cents("123.456"), Some(12346));
        assert_eq!(parse_money_cents("-1.2"), Some(-120));
        assert_eq!(parse_money_cents("bad"), None);
    }

    #[test]
    fn parses_graphql_order_node() {
        let node = serde_json::json!({
            "id": "gid://shopify/Order/1",
            "name": "#1001",
            "email": "a@example.com",
            "displayFinancialStatus": "PAID",
            "displayFulfillmentStatus": "FULFILLED",
            "createdAt": "2026-06-01T00:00:00Z",
            "updatedAt": "2026-06-02T00:00:00Z",
            "totalPriceSet": { "shopMoney": { "amount": "42.50", "currencyCode": "USD" } },
            "customer": { "displayName": "Ada Buyer" },
            "lineItems": { "edges": [{ "node": { "title": "Widget", "sku": "W1", "quantity": 3 } }] },
            "fulfillments": [{ "trackingInfo": [{ "number": "1Z", "company": "UPS", "url": "https://track" }] }]
        });
        let parsed = order_from_node(&node).expect("order");
        assert_eq!(parsed.total.cents, 4250);
        assert_eq!(parsed.line_items[0].quantity, 3);
        assert_eq!(parsed.tracking_number.as_deref(), Some("1Z"));
    }
}
