//! Stockforge (Inventorium) read-only client: inventory levels, low-stock
//! alerts, reorder suggestions, the live order board, and open purchase
//! orders. GET-only by construction — there is no write (or even POST) path
//! in this module, so the connector can never mutate inventory.
//!
//! Auth: a static org API key (`sfk_live_…`, created by an org ADMIN in
//! Stockforge Settings → API Keys with the VIEWER role) sent as a Bearer
//! token on every request. No login, refresh, or session state. A 401 means
//! the key itself is invalid / revoked / expired (the body's code says
//! which) — retrying cannot help, only an operator swapping the key can.
//!
//! Error taxonomy mirrors qbo_read because the sync pump treats the cases
//! differently: 429 → backoff-with-deadline, 401 → permanent until the key
//! changes, 5xx → retry next cycle, other 4xx/parse → permanent. The caller
//! owns the request budget; this module never loops or retries on its own.

use serde_json::Value;
use std::sync::Arc;

/// Stockforge list page cap (`take` is clamped server-side at 100).
pub const STOCKFORGE_MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum StockforgeError {
    /// 429 — caller must back off (Retry-After honored when present).
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    /// 401 — the API key is invalid, revoked, or expired (message carries
    /// Stockforge's code: INVALID_API_KEY / API_KEY_REVOKED /
    /// API_KEY_EXPIRED). Only an operator swapping the key fixes this.
    AuthRejected { message: String },
    /// 5xx / network / timeout — safe to retry next cycle.
    Retryable { code: String, message: String },
    /// Other 4xx, parse failures — retrying won't help.
    Permanent { code: String, message: String },
}

impl std::fmt::Display for StockforgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthRejected { message } => write!(formatter, "auth_rejected: {message}"),
            Self::Retryable { code, message } => write!(formatter, "{code}: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

/// Transport seam: GET-only on purpose — the bearer credential is the static
/// API key.
pub trait StockforgeHttp: Send + Sync {
    fn get_json(&self, url: &str, api_key: &str)
        -> Result<StockforgeHttpResponse, StockforgeError>;
}

pub struct StockforgeHttpResponse {
    pub status: u16,
    pub body: Value,
    /// Parsed Retry-After seconds on 429 responses.
    pub retry_after_secs: Option<u64>,
}

pub struct ReqwestStockforgeHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestStockforgeHttpClient {
    fn default() -> Self {
        // Bound connect + total time so a hung Stockforge instance cannot pin
        // the calling blocking worker thread indefinitely.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl ReqwestStockforgeHttpClient {
    fn finish(
        response: reqwest::blocking::Response,
    ) -> Result<StockforgeHttpResponse, StockforgeError> {
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(StockforgeHttpResponse {
            status,
            body,
            retry_after_secs,
        })
    }
}

impl StockforgeHttp for ReqwestStockforgeHttpClient {
    fn get_json(
        &self,
        url: &str,
        api_key: &str,
    ) -> Result<StockforgeHttpResponse, StockforgeError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(api_key)
            .header("Accept", "application/json")
            .send()
            .map_err(|err| StockforgeError::Retryable {
                code: if err.is_timeout() {
                    "timeout".to_string()
                } else {
                    "stockforge_request_failed".to_string()
                },
                message: err.to_string(),
            })?;
        Self::finish(response)
    }
}

fn error_for_status(
    status: u16,
    retry_after_secs: Option<u64>,
    body: &Value,
    path: &str,
) -> StockforgeError {
    // Stockforge error envelopes carry a machine code (e.g. INVALID_API_KEY,
    // API_KEY_REVOKED, API_KEY_EXPIRED) — surface it so the connector status
    // tells the operator WHICH problem the key has.
    let body_code = body
        .get("code")
        .and_then(Value::as_str)
        .map(|code| format!(" {code}"))
        .unwrap_or_default();
    match status {
        429 => StockforgeError::RateLimited {
            retry_after_ms: retry_after_secs.map(|secs| secs * 1000),
            message: format!("stockforge 429 on {path}"),
        },
        401 => StockforgeError::AuthRejected {
            message: format!("stockforge 401{body_code} on {path}"),
        },
        500..=599 => StockforgeError::Retryable {
            code: "stockforge_server_error".to_string(),
            message: format!("stockforge {status} on {path}"),
        },
        other => StockforgeError::Permanent {
            code: "stockforge_request_rejected".to_string(),
            message: format!("stockforge {other}{body_code} on {path}"),
        },
    }
}

/// One page of a paginated list walk (`skip`/`take`).
#[derive(Debug)]
pub struct SfPage<T> {
    pub records: Vec<T>,
    /// The `take` actually requested; `records.len() < requested_take` means
    /// this was the last page of the walk.
    pub requested_take: u32,
}

/// Cached material (stock-on-hand) row. Quantities stay f64 — materials can use fractional quantities /// and consumed in fractional gallons/liters; money converts to integer
/// cents at the parse boundary like every other connector.
#[derive(Debug, Clone, PartialEq)]
pub struct SfMaterialRecord {
    pub material_id: String,
    pub name: String,
    pub sku: Option<String>,
    /// LIQUID | FABRIC | DISCRETE.
    pub category: Option<String>,
    pub current_quantity: f64,
    /// Stockforge reservedQty from ACTIVE MaterialReservation residuals.
    /// None when the payload omits the field — never invented as zero.
    pub reserved_qty: Option<f64>,
    /// Stockforge onOrderQty (SENT|CONFIRMED PO lines, already stock-unit).
    pub incoming_qty: Option<f64>,
    pub unit: Option<String>,
    pub warning_threshold: Option<f64>,
    pub critical_threshold: Option<f64>,
    /// PERCENTAGE | ABSOLUTE.
    pub threshold_type: Option<String>,
    pub unit_cost_cents: i64,
    pub lead_time_days: Option<i64>,
    pub vendor_name: Option<String>,
    pub is_active: bool,
    /// Stockforge's explicit stock-behavior fields. `None` means the server
    /// predates that schema; callers must not infer alert eligibility.
    pub is_purchasable: Option<bool>,
    /// AUTO | PURCHASE | PRODUCTION | NONE.
    pub replenishment_policy: Option<String>,
    /// STOCK | COMPONENTS | NONE.
    pub sale_depletion_policy: Option<String>,
    /// Raw updatedAt (RFC3339) — freshness display only, not a cursor
    /// (the materials list has no updated-since filter).
    pub updated_at: Option<String>,
}

/// One ACTIVE low-stock alert (Stockforge owns dedup/ack lifecycle).
#[derive(Debug, Clone, PartialEq)]
pub struct SfAlertRecord {
    pub alert_id: String,
    pub material_id: Option<String>,
    pub material_name: Option<String>,
    pub material_sku: Option<String>,
    /// WARNING | CRITICAL.
    pub severity: String,
    /// ACTIVE | ACKNOWLEDGED | DISMISSED | RESOLVED.
    pub status: String,
    pub current_quantity: Option<f64>,
    pub threshold_value: Option<f64>,
    pub percentage_remaining: Option<f64>,
    pub message: Option<String>,
    pub created_at: Option<String>,
}

/// One reorder/PO suggestion from the prediction service.
#[derive(Debug, Clone, PartialEq)]
pub struct SfReorderSuggestionRecord {
    pub suggestion_id: String,
    pub material_id: Option<String>,
    pub material_name: Option<String>,
    pub material_sku: Option<String>,
    pub vendor_name: Option<String>,
    /// LOW | MEDIUM | HIGH | CRITICAL.
    pub urgency: String,
    /// PENDING | ACCEPTED | REJECTED | EXPIRED.
    pub status: String,
    pub current_quantity: Option<f64>,
    pub suggested_quantity: Option<f64>,
    pub unit: Option<String>,
    pub estimated_cost_cents: i64,
    pub days_until_stockout: Option<f64>,
    pub lead_time_days: Option<i64>,
    pub reasoning: Option<String>,
    pub created_at: Option<String>,
}

/// One card from the live order board, flattened to what the dashboard
/// renders: pipeline status plus the action/blocked flags that drive the
/// order-controls metrics (missed-order prevention, SKU mapping backlog,
/// deduction reconciliation).
#[derive(Debug, Clone, PartialEq)]
pub struct SfOrderCardRecord {
    pub order_id: String,
    pub order_number: String,
    pub external_order_id: Option<String>,
    /// shopify | woocommerce | amazon | ...
    pub platform: Option<String>,
    /// NEW | PICKING | PACKED | SHIPPED | DELIVERED | EXCEPTION.
    pub board_status: String,
    pub raw_status: Option<String>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub total_amount_cents: i64,
    pub currency: Option<String>,
    pub order_date: Option<String>,
    pub processed_at: Option<String>,
    pub item_count: i64,
    pub unit_count: i64,
    pub mapped_line_count: i64,
    /// Distinct Stockforge material ids found on the embedded order lines.
    /// This comes from the existing 30-day board response; no per-order read
    /// is needed.
    pub line_material_ids: Vec<String>,
    /// True only when every reported order line carried a recognized material
    /// identity. False means per-SKU demand is unknown and callers must fail
    /// closed rather than infer no demand.
    pub line_identity_complete: bool,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub shipment_refs: Option<SfShipmentRefs>,
    /// Shipment id linking this card to damage events (claim packets).
    pub shipment_id: Option<String>,
    pub ship_date: Option<String>,
    /// Pack-time photos captured at the pack station (claim packing proof).
    pub photo_count: i64,
    /// Container the pack station works in — its photos are fetchable.
    pub pack_station_container_id: Option<String>,
    pub needs_mapping: bool,
    pub blocked: bool,
    pub deducted: bool,
    pub deduction_failed: bool,
    pub label_needed: bool,
    pub packed_missing_photo: bool,
    pub exception: bool,
    pub depletion_total: i64,
    pub depletion_applied: i64,
    pub depletion_failed: i64,
    pub depletion_reversed: i64,
    /// Human-readable blocked reasons, JSON array string (display-only).
    pub blocked_reasons_json: String,
}

/// One damage event from `GET /api/v1/damage` (shipping_claims:read — a
/// VIEWER key passes). The list embeds the shipment reference, so a single
/// request carries damage details AND the tracking/carrier refs a claim
/// packet needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SfDamageEventRecord {
    pub damage_event_id: String,
    pub shipment_id: String,
    pub reported_at: Option<String>,
    /// CUSTOMER | INTERNAL.
    pub reported_by: String,
    /// LOW | MEDIUM | HIGH | CRITICAL.
    pub severity: String,
    pub damage_type: String,
    /// Damage photo URLs (stored as external URLs in Stockforge).
    pub photos: Vec<String>,
    pub description: Option<String>,
    /// OPEN | FILED | APPROVED | DENIED | RESOLVED | CLOSED.
    pub claim_status: String,
    pub claim_amount_cents: Option<i64>,
    pub resolution: Option<String>,
    pub shipment_number: Option<String>,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub shipment_refs: Option<SfShipmentRefs>,
    pub shipment_status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SfShipmentRefs {
    pub shipping_platform: Option<String>,
    pub platform_shipment_id: Option<String>,
    pub carrier: Option<String>,
    pub carrier_service: Option<String>,
    pub mode: Option<String>,
    pub tracking_number: Option<String>,
    pub pro_number: Option<String>,
    pub bol_number: Option<String>,
    pub tracking_url: Option<String>,
    pub document_refs: Vec<SfShipmentDocumentRef>,
    pub claim_platform: Option<String>,
    pub claim_api_supported: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfShipmentDocumentRef {
    pub kind: String,
    pub url: String,
}

/// One pack-time photo from `GET /api/v1/packing/{container_id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfPackPhotoRecord {
    pub photo_id: String,
    pub url: String,
    pub captured_at: Option<String>,
}

/// One purchase order (inbound stock pipeline).
#[derive(Debug, Clone, PartialEq)]
pub struct SfPurchaseOrderRecord {
    pub po_id: String,
    pub vendor_name: Option<String>,
    /// DRAFT | PENDING_APPROVAL | SENT | CONFIRMED | RECEIVED | CANCELLED.
    pub status: String,
    pub total_estimated_cost_cents: i64,
    pub freight_mode: Option<String>,
    pub line_count: i64,
    /// Distinct Stockforge material ids found on the embedded PO line items.
    pub line_material_ids: Vec<String>,
    /// True only when every reported PO line carried a recognized material
    /// identity. False means per-SKU inbound state is unknown.
    pub line_identity_complete: bool,
    pub created_at: Option<String>,
    pub sent_at: Option<String>,
    pub received_at: Option<String>,
}

pub trait StockforgeReadClient: Send + Sync {
    fn fetch_materials(
        &self,
        api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfMaterialRecord>, StockforgeError>;
    /// ACTIVE alerts only — acknowledged/dismissed/resolved stay in Stockforge.
    fn fetch_active_alerts(&self, api_key: &str) -> Result<Vec<SfAlertRecord>, StockforgeError>;
    fn fetch_reorder_suggestions(
        &self,
        api_key: &str,
    ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError>;
    /// The full board for [start_date, end_date] (YYYY-MM-DD, inclusive),
    /// flattened across columns. One request returns the whole window.
    fn fetch_order_board(
        &self,
        api_key: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<SfOrderCardRecord>, StockforgeError>;
    fn fetch_purchase_orders(
        &self,
        api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError>;
    /// Damage events filtered by claim status (e.g. "OPEN"), newest first.
    fn fetch_damage_events(
        &self,
        api_key: &str,
        claim_status: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfDamageEventRecord>, StockforgeError>;
    /// Pack-time photos of one packing container. `None` = container gone.
    fn fetch_container_photos(
        &self,
        api_key: &str,
        container_id: &str,
    ) -> Result<Option<Vec<SfPackPhotoRecord>>, StockforgeError>;
}

pub struct LiveStockforgeReadClient<C: StockforgeHttp = ReqwestStockforgeHttpClient> {
    http: Arc<C>,
    base_url: String,
}

impl<C: StockforgeHttp> LiveStockforgeReadClient<C> {
    pub fn new(http: Arc<C>, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    fn run_get(&self, api_key: &str, path_and_query: &str) -> Result<Value, StockforgeError> {
        let url = format!("{}{path_and_query}", self.base_url.trim_end_matches('/'));
        let response = self.http.get_json(&url, api_key)?;
        match response.status {
            200..=299 => Ok(response.body),
            other => Err(error_for_status(
                other,
                response.retry_after_secs,
                &response.body,
                path_and_query,
            )),
        }
    }
}

impl<C: StockforgeHttp> StockforgeReadClient for LiveStockforgeReadClient<C> {
    fn fetch_materials(
        &self,
        api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfMaterialRecord>, StockforgeError> {
        let take = take.clamp(1, STOCKFORGE_MAX_PAGE_SIZE);
        let body = self.run_get(
            api_key,
            &format!("/api/v1/materials?skip={skip}&take={take}"),
        )?;
        Ok(SfPage {
            records: data_array(&body)
                .into_iter()
                .filter_map(material_record_from_value)
                .collect(),
            requested_take: take,
        })
    }

    fn fetch_active_alerts(&self, api_key: &str) -> Result<Vec<SfAlertRecord>, StockforgeError> {
        let body = self.run_get(api_key, "/api/v1/alerts?status=ACTIVE")?;
        Ok(data_array(&body)
            .into_iter()
            .filter_map(alert_record_from_value)
            .collect())
    }

    fn fetch_reorder_suggestions(
        &self,
        api_key: &str,
    ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError> {
        let body = self.run_get(api_key, "/api/v1/predictions/suggestions")?;
        Ok(data_array(&body)
            .into_iter()
            .filter_map(suggestion_record_from_value)
            .collect())
    }

    fn fetch_order_board(
        &self,
        api_key: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
        let body = self.run_get(
            api_key,
            &format!(
                "/api/v1/order-board?range=custom&startDate={}&endDate={}",
                encode_query_component(start_date),
                encode_query_component(end_date),
            ),
        )?;
        let columns = body
            .get("data")
            .and_then(|data| data.get("columns"))
            .and_then(Value::as_array)
            .map(|columns| columns.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut cards = Vec::new();
        for column in columns {
            let column_status = string_field(column, "status").unwrap_or_default();
            for order in column
                .get("orders")
                .and_then(Value::as_array)
                .map(|orders| orders.iter().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if let Some(card) = order_card_from_value(order, &column_status) {
                    cards.push(card);
                }
            }
        }
        Ok(cards)
    }

    fn fetch_purchase_orders(
        &self,
        api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError> {
        let take = take.clamp(1, STOCKFORGE_MAX_PAGE_SIZE);
        let body = self.run_get(
            api_key,
            &format!("/api/v1/purchase-orders?skip={skip}&take={take}"),
        )?;
        Ok(SfPage {
            records: data_array(&body)
                .into_iter()
                .filter_map(po_record_from_value)
                .collect(),
            requested_take: take,
        })
    }

    fn fetch_damage_events(
        &self,
        api_key: &str,
        claim_status: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfDamageEventRecord>, StockforgeError> {
        let take = take.clamp(1, STOCKFORGE_MAX_PAGE_SIZE);
        let body = self.run_get(
            api_key,
            &format!(
                "/api/v1/damage?claimStatus={}&skip={skip}&take={take}",
                encode_query_component(claim_status),
            ),
        )?;
        Ok(SfPage {
            records: data_array(&body)
                .into_iter()
                .filter_map(damage_record_from_value)
                .collect(),
            requested_take: take,
        })
    }

    fn fetch_container_photos(
        &self,
        api_key: &str,
        container_id: &str,
    ) -> Result<Option<Vec<SfPackPhotoRecord>>, StockforgeError> {
        let path = format!("/api/v1/packing/{}", encode_query_component(container_id));
        let url = format!("{}{path}", self.base_url.trim_end_matches('/'));
        let response = self.http.get_json(&url, api_key)?;
        match response.status {
            200..=299 => {
                let photos = response
                    .body
                    .get("data")
                    .and_then(|data| data.get("photos"))
                    .and_then(Value::as_array)
                    .map(|photos| {
                        photos
                            .iter()
                            .filter_map(pack_photo_from_value)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(Some(photos))
            }
            404 => Ok(None),
            other => Err(error_for_status(
                other,
                response.retry_after_secs,
                &response.body,
                &path,
            )),
        }
    }
}

/// Minimal query-component encoder (dates and enum-ish values only).
fn encode_query_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn data_array(body: &Value) -> Vec<&Value> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|records| records.iter().collect())
        .unwrap_or_default()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

fn f64_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|raw| {
        raw.as_f64()
            .or_else(|| raw.as_i64().map(|n| n as f64))
            .or_else(|| raw.as_u64().map(|n| n as f64))
    })
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn opt_bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

/// Stockforge sends money as JSON float dollars; round-half-away to integer
/// cents at the parse boundary so floats never reach money arithmetic.
fn cents_field(value: &Value, key: &str) -> i64 {
    f64_field(value, key)
        .map(|amount| (amount * 100.0).round() as i64)
        .unwrap_or(0)
}

fn nested_material_name(value: &Value) -> (Option<String>, Option<String>) {
    match value.get("material") {
        Some(material) => (
            string_field(material, "name"),
            string_field(material, "sku"),
        ),
        None => (None, None),
    }
}

/// Material identity deliberately recognizes only Stockforge's documented
/// line shapes. Unknown shapes stay incomplete so reporting cannot turn a
/// missing mapping into a false "no demand/inbound" conclusion.
fn line_material_id(value: &Value) -> Option<String> {
    string_field(value, "materialId").or_else(|| {
        value
            .get("material")
            .and_then(|material| string_field(material, "id"))
    })
}

fn line_material_identity(value: &Value, key: &str, reported_count: i64) -> (Vec<String>, bool) {
    let Some(lines) = value.get(key).and_then(Value::as_array) else {
        return (Vec::new(), false);
    };
    let mut ids = Vec::new();
    let mut complete = lines.len() as i64 == reported_count;
    for line in lines {
        if let Some(id) = line_material_id(line) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            complete = false;
        }
    }
    ids.sort();
    (ids, complete)
}

fn material_record_from_value(value: &Value) -> Option<SfMaterialRecord> {
    Some(SfMaterialRecord {
        material_id: string_field(value, "id")?,
        name: string_field(value, "name").unwrap_or_default(),
        sku: string_field(value, "sku"),
        category: string_field(value, "category"),
        current_quantity: f64_field(value, "currentQuantity").unwrap_or(0.0),
        reserved_qty: f64_field(value, "reservedQty"),
        incoming_qty: f64_field(value, "onOrderQty"),
        unit: string_field(value, "unit"),
        warning_threshold: f64_field(value, "warningThreshold"),
        critical_threshold: f64_field(value, "criticalThreshold"),
        threshold_type: string_field(value, "thresholdType"),
        unit_cost_cents: cents_field(value, "unitCost"),
        lead_time_days: i64_field(value, "leadTimeDays"),
        vendor_name: value
            .get("vendor")
            .and_then(|vendor| string_field(vendor, "name")),
        is_active: value
            .get("isActive")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        is_purchasable: opt_bool_field(value, "isPurchasable"),
        replenishment_policy: string_field(value, "replenishmentPolicy"),
        sale_depletion_policy: string_field(value, "saleDepletionPolicy"),
        updated_at: string_field(value, "updatedAt"),
    })
}

fn alert_record_from_value(value: &Value) -> Option<SfAlertRecord> {
    let (material_name, material_sku) = nested_material_name(value);
    Some(SfAlertRecord {
        alert_id: string_field(value, "id")?,
        material_id: string_field(value, "materialId"),
        // Current Stockforge denormalizes these at the top level. Retain the
        // nested fallback for older deployments.
        material_name: string_field(value, "materialName").or(material_name),
        material_sku: string_field(value, "materialSku").or(material_sku),
        severity: string_field(value, "severity").unwrap_or_else(|| "WARNING".to_string()),
        status: string_field(value, "status").unwrap_or_else(|| "ACTIVE".to_string()),
        current_quantity: f64_field(value, "currentQuantity"),
        threshold_value: f64_field(value, "thresholdValue"),
        percentage_remaining: f64_field(value, "percentageRemaining"),
        message: string_field(value, "message"),
        created_at: string_field(value, "createdAt"),
    })
}

fn suggestion_record_from_value(value: &Value) -> Option<SfReorderSuggestionRecord> {
    Some(SfReorderSuggestionRecord {
        suggestion_id: string_field(value, "id")?,
        material_id: string_field(value, "materialId"),
        // Suggestions denormalize the names; fall back to the nested object.
        material_name: string_field(value, "materialName")
            .or_else(|| nested_material_name(value).0),
        material_sku: string_field(value, "materialSku").or_else(|| nested_material_name(value).1),
        vendor_name: string_field(value, "vendorName").or_else(|| {
            value
                .get("vendor")
                .and_then(|vendor| string_field(vendor, "name"))
        }),
        urgency: string_field(value, "urgency").unwrap_or_else(|| "LOW".to_string()),
        status: string_field(value, "status").unwrap_or_else(|| "PENDING".to_string()),
        current_quantity: f64_field(value, "currentQuantity"),
        suggested_quantity: f64_field(value, "suggestedQuantity"),
        unit: string_field(value, "unit"),
        estimated_cost_cents: cents_field(value, "estimatedCost"),
        days_until_stockout: f64_field(value, "daysUntilStockout"),
        lead_time_days: i64_field(value, "leadTimeDays"),
        reasoning: string_field(value, "reasoning"),
        created_at: string_field(value, "createdAt"),
    })
}

fn order_card_from_value(value: &Value, column_status: &str) -> Option<SfOrderCardRecord> {
    let flags = value.get("actionFlags").cloned().unwrap_or(Value::Null);
    let shipment = value.get("shipment");
    let depletion = value
        .get("depletionSummary")
        .cloned()
        .unwrap_or(Value::Null);
    let item_count = i64_field(value, "itemCount").unwrap_or(0);
    let (line_material_ids, line_identity_complete) =
        line_material_identity(value, "lines", item_count);
    let needs_mapping = bool_field(&flags, "needsMapping");
    let mapped_line_count = value
        .get("lines")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter(|line| bool_field(line, "mapped"))
                .count() as i64
        })
        .unwrap_or_else(|| if !needs_mapping { item_count } else { 0 });
    let blocked_reasons = value
        .get("blockedReasons")
        .and_then(Value::as_array)
        .map(|reasons| {
            reasons
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let shipment_refs = shipment.and_then(shipment_refs_from_value);
    let carrier = shipment_refs
        .as_ref()
        .and_then(|refs| refs.carrier.clone())
        .or_else(|| shipment.and_then(|ship| string_field(ship, "carrier")));
    let tracking_number = shipment_refs
        .as_ref()
        .and_then(|refs| refs.tracking_number.clone())
        .or_else(|| shipment.and_then(|ship| string_field(ship, "trackingNumber")));
    Some(SfOrderCardRecord {
        order_id: string_field(value, "id")?,
        order_number: string_field(value, "orderNumber").unwrap_or_default(),
        external_order_id: string_field(value, "externalOrderId"),
        platform: string_field(value, "platform"),
        board_status: string_field(value, "status").unwrap_or_else(|| column_status.to_string()),
        raw_status: string_field(value, "rawStatus"),
        customer_name: string_field(value, "customerName"),
        customer_email: string_field(value, "customerEmail"),
        total_amount_cents: cents_field(value, "totalAmount"),
        currency: string_field(value, "currency"),
        order_date: string_field(value, "orderDate"),
        processed_at: string_field(value, "processedAt"),
        item_count,
        unit_count: i64_field(value, "unitCount").unwrap_or(0),
        mapped_line_count,
        line_material_ids,
        line_identity_complete,
        carrier,
        tracking_number,
        shipment_refs,
        shipment_id: shipment.and_then(|ship| string_field(ship, "id")),
        ship_date: shipment.and_then(|ship| string_field(ship, "shipDate")),
        photo_count: value
            .get("packingSummary")
            .and_then(|summary| i64_field(summary, "photoCount"))
            .unwrap_or(0),
        pack_station_container_id: value
            .get("packingSummary")
            .and_then(|summary| string_field(summary, "packStationContainerId")),
        needs_mapping,
        blocked: bool_field(&flags, "blocked"),
        deducted: bool_field(&flags, "deducted"),
        deduction_failed: bool_field(&flags, "deductionFailed"),
        label_needed: bool_field(&flags, "labelNeeded"),
        packed_missing_photo: bool_field(&flags, "packedMissingPhoto"),
        exception: bool_field(&flags, "exception"),
        depletion_total: i64_field(&depletion, "total").unwrap_or(0),
        depletion_applied: i64_field(&depletion, "applied").unwrap_or(0),
        depletion_failed: i64_field(&depletion, "failed").unwrap_or(0),
        depletion_reversed: i64_field(&depletion, "reversed").unwrap_or(0),
        blocked_reasons_json: serde_json::to_string(&blocked_reasons)
            .unwrap_or_else(|_| "[]".to_string()),
    })
}

fn damage_record_from_value(value: &Value) -> Option<SfDamageEventRecord> {
    let shipment = value.get("shipment");
    let shipment_refs = shipment.and_then(shipment_refs_from_value);
    let carrier = shipment_refs
        .as_ref()
        .and_then(|refs| refs.carrier.clone())
        .or_else(|| shipment.and_then(|ship| string_field(ship, "carrier")));
    let tracking_number = shipment_refs
        .as_ref()
        .and_then(|refs| refs.tracking_number.clone())
        .or_else(|| shipment.and_then(|ship| string_field(ship, "trackingNumber")));
    Some(SfDamageEventRecord {
        damage_event_id: string_field(value, "id")?,
        shipment_id: string_field(value, "shipmentId")?,
        reported_at: string_field(value, "reportedAt"),
        reported_by: string_field(value, "reportedBy").unwrap_or_else(|| "INTERNAL".to_string()),
        severity: string_field(value, "severity").unwrap_or_else(|| "MEDIUM".to_string()),
        damage_type: string_field(value, "damageType").unwrap_or_default(),
        photos: value
            .get("photos")
            .and_then(Value::as_array)
            .map(|photos| {
                photos
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        description: string_field(value, "description"),
        claim_status: string_field(value, "claimStatus").unwrap_or_else(|| "OPEN".to_string()),
        claim_amount_cents: value
            .get("claimAmount")
            .and_then(Value::as_f64)
            .map(|dollars| (dollars * 100.0).round() as i64),
        resolution: string_field(value, "resolution"),
        shipment_number: shipment.and_then(|ship| string_field(ship, "shipmentNumber")),
        carrier,
        tracking_number,
        shipment_refs,
        shipment_status: shipment.and_then(|ship| string_field(ship, "status")),
    })
}

fn shipment_refs_from_value(shipment: &Value) -> Option<SfShipmentRefs> {
    let refs_value = shipment
        .get("shipmentRefs")
        .or_else(|| shipment.get("shipment_refs"));
    let source = refs_value.unwrap_or(shipment);
    let document_refs = source
        .get("documentRefs")
        .or_else(|| source.get("document_refs"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(SfShipmentDocumentRef {
                        kind: string_field(entry, "kind")?,
                        url: string_field(entry, "url")?,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let refs = SfShipmentRefs {
        shipping_platform: string_field(source, "shippingPlatform")
            .or_else(|| string_field(source, "shipping_platform")),
        platform_shipment_id: string_field(source, "platformShipmentId")
            .or_else(|| string_field(source, "platform_shipment_id"))
            .or_else(|| string_field(shipment, "id")),
        carrier: string_field(source, "carrier"),
        carrier_service: string_field(source, "carrierService")
            .or_else(|| string_field(source, "carrier_service")),
        mode: string_field(source, "mode"),
        tracking_number: string_field(source, "trackingNumber")
            .or_else(|| string_field(source, "tracking_number")),
        pro_number: string_field(source, "proNumber")
            .or_else(|| string_field(source, "pro_number")),
        bol_number: string_field(source, "bolNumber")
            .or_else(|| string_field(source, "bol_number")),
        tracking_url: string_field(source, "trackingUrl")
            .or_else(|| string_field(source, "tracking_url")),
        document_refs,
        claim_platform: string_field(source, "claimPlatform")
            .or_else(|| string_field(source, "claim_platform")),
        claim_api_supported: opt_bool_field(source, "claimApiSupported")
            .or_else(|| opt_bool_field(source, "claim_api_supported")),
    };
    let has_any = refs.shipping_platform.is_some()
        || refs.platform_shipment_id.is_some()
        || refs.carrier.is_some()
        || refs.carrier_service.is_some()
        || refs.mode.is_some()
        || refs.tracking_number.is_some()
        || refs.pro_number.is_some()
        || refs.bol_number.is_some()
        || refs.tracking_url.is_some()
        || !refs.document_refs.is_empty()
        || refs.claim_platform.is_some()
        || refs.claim_api_supported.is_some();
    has_any.then_some(refs)
}

fn pack_photo_from_value(value: &Value) -> Option<SfPackPhotoRecord> {
    Some(SfPackPhotoRecord {
        photo_id: string_field(value, "id")?,
        url: string_field(value, "url")?,
        captured_at: string_field(value, "capturedAt"),
    })
}

fn po_record_from_value(value: &Value) -> Option<SfPurchaseOrderRecord> {
    let line_items = value.get("lineItems").and_then(Value::as_array);
    let line_count = line_items.map(|lines| lines.len() as i64).unwrap_or(0);
    // Header-only payloads (`lineItems` missing or `[]`) are incomplete.
    // `line_count` is derived from the array, so an empty list would otherwise
    // look complete and mark every SKU as having no inbound stock.
    let (line_material_ids, line_identity_complete) = match line_items {
        Some(lines) if !lines.is_empty() => line_material_identity(value, "lineItems", line_count),
        _ => (Vec::new(), false),
    };
    Some(SfPurchaseOrderRecord {
        po_id: string_field(value, "id")?,
        vendor_name: value
            .get("vendor")
            .and_then(|vendor| string_field(vendor, "name")),
        status: string_field(value, "status").unwrap_or_else(|| "DRAFT".to_string()),
        total_estimated_cost_cents: cents_field(value, "totalEstimatedCost"),
        freight_mode: string_field(value, "freightMode"),
        line_count,
        line_material_ids,
        line_identity_complete,
        created_at: string_field(value, "createdAt"),
        sent_at: string_field(value, "sentAt"),
        received_at: string_field(value, "receivedAt"),
    })
}

/// Deterministic in-memory client with the same paging semantics the live
/// client relies on (skip/take, short page ends the walk) — the sync pump's
/// cursor math is tested against this.
#[derive(Default, Clone)]
pub struct FixtureStockforgeReadClient {
    pub materials: Vec<SfMaterialRecord>,
    pub alerts: Vec<SfAlertRecord>,
    pub suggestions: Vec<SfReorderSuggestionRecord>,
    pub order_cards: Vec<SfOrderCardRecord>,
    pub purchase_orders: Vec<SfPurchaseOrderRecord>,
    pub damage_events: Vec<SfDamageEventRecord>,
    /// container_id → photos.
    pub container_photos: std::collections::HashMap<String, Vec<SfPackPhotoRecord>>,
}

impl FixtureStockforgeReadClient {
    fn page<T: Clone>(records: &[T], skip: u32, take: u32) -> SfPage<T> {
        let take = take.clamp(1, STOCKFORGE_MAX_PAGE_SIZE);
        SfPage {
            records: records
                .iter()
                .skip(skip as usize)
                .take(take as usize)
                .cloned()
                .collect(),
            requested_take: take,
        }
    }
}

impl StockforgeReadClient for FixtureStockforgeReadClient {
    fn fetch_materials(
        &self,
        _api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfMaterialRecord>, StockforgeError> {
        Ok(Self::page(&self.materials, skip, take))
    }

    fn fetch_active_alerts(&self, _api_key: &str) -> Result<Vec<SfAlertRecord>, StockforgeError> {
        Ok(self
            .alerts
            .iter()
            .filter(|alert| alert.status == "ACTIVE")
            .cloned()
            .collect())
    }

    fn fetch_reorder_suggestions(
        &self,
        _api_key: &str,
    ) -> Result<Vec<SfReorderSuggestionRecord>, StockforgeError> {
        Ok(self.suggestions.clone())
    }

    fn fetch_order_board(
        &self,
        _api_key: &str,
        _start_date: &str,
        _end_date: &str,
    ) -> Result<Vec<SfOrderCardRecord>, StockforgeError> {
        Ok(self.order_cards.clone())
    }

    fn fetch_purchase_orders(
        &self,
        _api_key: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfPurchaseOrderRecord>, StockforgeError> {
        Ok(Self::page(&self.purchase_orders, skip, take))
    }

    fn fetch_damage_events(
        &self,
        _api_key: &str,
        claim_status: &str,
        skip: u32,
        take: u32,
    ) -> Result<SfPage<SfDamageEventRecord>, StockforgeError> {
        let filtered: Vec<SfDamageEventRecord> = self
            .damage_events
            .iter()
            .filter(|event| event.claim_status == claim_status)
            .cloned()
            .collect();
        Ok(Self::page(&filtered, skip, take))
    }

    fn fetch_container_photos(
        &self,
        _api_key: &str,
        container_id: &str,
    ) -> Result<Option<Vec<SfPackPhotoRecord>>, StockforgeError> {
        Ok(self.container_photos.get(container_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeHttp {
        responses: Mutex<VecDeque<StockforgeHttpResponse>>,
        last_url: Mutex<Option<String>>,
    }

    impl FakeHttp {
        fn new(responses: Vec<StockforgeHttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                last_url: Mutex::new(None),
            }
        }

        fn pop(&self, url: &str) -> Result<StockforgeHttpResponse, StockforgeError> {
            *self.last_url.lock().expect("lock") = Some(url.to_string());
            Ok(self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted response"))
        }
    }

    impl StockforgeHttp for FakeHttp {
        fn get_json(
            &self,
            url: &str,
            _api_key: &str,
        ) -> Result<StockforgeHttpResponse, StockforgeError> {
            self.pop(url)
        }
    }

    fn response(status: u16, body: Value) -> StockforgeHttpResponse {
        StockforgeHttpResponse {
            status,
            body,
            retry_after_secs: None,
        }
    }

    #[test]
    fn material_parsing_converts_money_and_keeps_quantities() {
        let body = serde_json::json!({
            "success": true,
            "data": [{
                "id": "m1",
                "name": "example Blue Base",
                "sku": "QB-001",
                "category": "LIQUID",
                "currentQuantity": 12.5,
                "reservedQty": 3,
                "onOrderQty": 5,
                "unit": "gal",
                "warningThreshold": 20,
                "criticalThreshold": 5,
                "thresholdType": "ABSOLUTE",
                "unitCost": 84.995,
                "leadTimeDays": 14,
                "vendor": { "id": "v1", "name": "Champion" },
                "isActive": true,
                "isPurchasable": true,
                "replenishmentPolicy": "PURCHASE",
                "saleDepletionPolicy": "STOCK",
                "updatedAt": "2026-06-09T12:00:00Z"
            }],
            "pagination": { "total": 1, "skip": 0, "take": 100 }
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http.clone(), "https://sf.example.test");
        let page = client.fetch_materials("at", 0, 100).expect("page");
        assert_eq!(page.records.len(), 1);
        let record = &page.records[0];
        assert_eq!(record.unit_cost_cents, 8500, "round half away from zero");
        assert_eq!(record.current_quantity, 12.5);
        assert_eq!(record.reserved_qty, Some(3.0));
        assert_eq!(record.incoming_qty, Some(5.0));
        assert_eq!(record.vendor_name.as_deref(), Some("Champion"));
        assert_eq!(record.is_purchasable, Some(true));
        assert_eq!(record.replenishment_policy.as_deref(), Some("PURCHASE"));
        assert_eq!(record.sale_depletion_policy.as_deref(), Some("STOCK"));
        assert_eq!(
            http.last_url.lock().expect("lock").as_deref(),
            Some("https://sf.example.test/api/v1/materials?skip=0&take=100")
        );
    }

    #[test]
    fn material_parsing_omits_reserved_and_incoming_when_absent() {
        let body = serde_json::json!({
            "success": true,
            "data": [{
                "id": "m1",
                "name": "Legacy",
                "currentQuantity": 4
            }]
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http, "https://sf.example.test");
        let page = client.fetch_materials("at", 0, 100).expect("page");
        assert_eq!(page.records[0].reserved_qty, None);
        assert_eq!(page.records[0].incoming_qty, None);
    }

    #[test]
    fn material_parsing_keeps_explicit_zero_reserved_and_incoming() {
        let body = serde_json::json!({
            "success": true,
            "data": [{
                "id": "m1",
                "name": "Unallocated",
                "currentQuantity": 4,
                "reservedQty": 0,
                "onOrderQty": 0
            }]
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http, "https://sf.example.test");
        let page = client.fetch_materials("at", 0, 100).expect("page");
        assert_eq!(page.records[0].reserved_qty, Some(0.0));
        assert_eq!(page.records[0].incoming_qty, Some(0.0));
    }

    #[test]
    fn alerts_parse_flat_material_identity_from_current_stockforge_shape() {
        let body = serde_json::json!({
            "success": true,
            "data": [{
                "id": "a1",
                "materialId": "m1",
                "materialName": "example Blue Base",
                "materialSku": "QB-001",
                "severity": "WARNING",
                "status": "ACTIVE",
                "currentQuantity": 3
            }]
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http, "https://sf.example.test");

        let alerts = client.fetch_active_alerts("at").expect("alerts");

        assert_eq!(
            alerts[0].material_name.as_deref(),
            Some("example Blue Base")
        );
        assert_eq!(alerts[0].material_sku.as_deref(), Some("QB-001"));
    }

    #[test]
    fn order_board_flattens_columns_and_action_flags() {
        let body = serde_json::json!({
            "success": true,
            "data": {
                "columns": [
                    { "status": "NEW", "label": "New", "count": 1, "orders": [{
                        "id": "o1", "orderNumber": "#1001", "platform": "shopify",
                        "externalOrderId": "9001",
                        "status": "NEW", "rawStatus": "pending",
                        "customerName": "Dana", "customerEmail": "dana@example.test",
                        "totalAmount": 219.99, "currency": "USD",
                        "orderDate": "2026-06-08T10:00:00Z",
                        "processedAt": "2026-06-08T10:05:00Z",
                        "itemCount": 2, "unitCount": 3,
                        "lines": [
                            { "id": "l1", "mapped": true, "materialId": "m1" },
                            { "id": "l2", "mapped": false }
                        ],
                        "depletionSummary": { "total": 2, "applied": 1, "failed": 1, "reversed": 0 },
                        "shipment": null,
                        "actionFlags": {
                            "readyToPickPack": false, "blocked": true, "needsMapping": true,
                            "readyToDeduct": false, "deducted": false, "deductionFailed": false,
                            "labelNeeded": false, "packedMissingPhoto": false, "exception": false
                        },
                        "blockedReasons": ["2 lines unmapped"]
                    }] },
                    { "status": "SHIPPED", "label": "Shipped", "count": 1, "orders": [{
                        "id": "o2", "orderNumber": "#1000",
                        "status": "SHIPPED", "totalAmount": 50,
                        "itemCount": 1, "unitCount": 1,
                        "shipment": { "carrier": "UPS", "trackingNumber": "1Z999" },
                        "actionFlags": { "deducted": true },
                        "blockedReasons": []
                    }] }
                ],
                "counts": { "NEW": 1, "SHIPPED": 1 },
                "total": 2
            }
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http.clone(), "https://sf.example.test");
        let cards = client
            .fetch_order_board("at", "2026-05-11", "2026-06-10")
            .expect("cards");
        assert_eq!(cards.len(), 2);
        assert!(cards[0].needs_mapping && cards[0].blocked);
        assert_eq!(cards[0].external_order_id.as_deref(), Some("9001"));
        assert_eq!(
            cards[0].customer_email.as_deref(),
            Some("dana@example.test")
        );
        assert_eq!(
            cards[0].processed_at.as_deref(),
            Some("2026-06-08T10:05:00Z")
        );
        assert_eq!(cards[0].total_amount_cents, 21_999);
        assert_eq!(cards[0].mapped_line_count, 1);
        assert_eq!(cards[0].line_material_ids, vec!["m1"]);
        assert!(!cards[0].line_identity_complete);
        assert_eq!(cards[0].depletion_total, 2);
        assert_eq!(cards[0].depletion_applied, 1);
        assert_eq!(cards[0].depletion_failed, 1);
        assert_eq!(cards[0].blocked_reasons_json, "[\"2 lines unmapped\"]");
        assert_eq!(cards[1].board_status, "SHIPPED");
        assert_eq!(cards[1].carrier.as_deref(), Some("UPS"));
        assert_eq!(
            cards[1]
                .shipment_refs
                .as_ref()
                .and_then(|refs| refs.tracking_number.as_deref()),
            Some("1Z999")
        );
        assert!(cards[1].deducted);
        assert!(!cards[1].line_identity_complete);
        assert_eq!(
            http.last_url.lock().expect("lock").as_deref(),
            Some(
                "https://sf.example.test/api/v1/order-board?range=custom&\
                 startDate=2026-05-11&endDate=2026-06-10"
            )
        );
    }

    #[test]
    fn purchase_order_lines_parse_flat_and_nested_material_identity() {
        let value = serde_json::json!({
            "id": "po-1",
            "status": "SENT",
            "lineItems": [
                { "id": "pol-1", "materialId": "m2" },
                { "id": "pol-2", "material": { "id": "m1" } }
            ]
        });

        let po = po_record_from_value(&value).expect("purchase order");

        assert_eq!(po.line_material_ids, vec!["m1", "m2"]);
        assert!(po.line_identity_complete);
    }

    #[test]
    fn unknown_line_identity_is_incomplete() {
        let value = serde_json::json!({
            "id": "po-1",
            "status": "SENT",
            "lineItems": [{ "id": "pol-1", "sku": "QB-001" }]
        });

        let po = po_record_from_value(&value).expect("purchase order");

        assert!(po.line_material_ids.is_empty());
        assert!(!po.line_identity_complete);
    }

    #[test]
    fn empty_or_missing_po_line_items_are_incomplete() {
        let empty = serde_json::json!({
            "id": "po-1",
            "status": "SENT",
            "lineItems": []
        });
        let missing = serde_json::json!({
            "id": "po-2",
            "status": "SENT"
        });

        let empty_po = po_record_from_value(&empty).expect("empty po");
        let missing_po = po_record_from_value(&missing).expect("missing po");

        assert!(empty_po.line_material_ids.is_empty());
        assert!(!empty_po.line_identity_complete);
        assert!(missing_po.line_material_ids.is_empty());
        assert!(!missing_po.line_identity_complete);
    }

    #[test]
    fn damage_events_parse_normalized_shipment_refs() {
        let body = serde_json::json!({
            "success": true,
            "data": [{
                "id": "dmg-1",
                "shipmentId": "shp-1",
                "reportedBy": "CUSTOMER",
                "severity": "HIGH",
                "damageType": "Crushed carton",
                "claimStatus": "OPEN",
                "shipment": {
                    "id": "sf-shp-1",
                    "shipmentRefs": {
                        "shippingPlatform": "speedship",
                        "platformShipmentId": "ss-123",
                        "carrier": "LTL Carrier",
                        "carrierService": "standard",
                        "mode": "ltl",
                        "proNumber": "PRO-456",
                        "bolNumber": "BOL-789",
                        "trackingUrl": "https://speedship.example/track/ss-123",
                        "documentRefs": [
                            { "kind": "pod", "url": "https://files.example/pod.pdf" }
                        ],
                        "claimPlatform": "speedship",
                        "claimApiSupported": false
                    }
                }
            }]
        });
        let http = Arc::new(FakeHttp::new(vec![response(200, body)]));
        let client = LiveStockforgeReadClient::new(http, "https://sf.example.test");
        let page = client
            .fetch_damage_events("at", "OPEN", 0, 100)
            .expect("damage page");
        let refs = page.records[0].shipment_refs.as_ref().expect("refs");

        assert_eq!(page.records[0].carrier.as_deref(), Some("LTL Carrier"));
        assert_eq!(refs.shipping_platform.as_deref(), Some("speedship"));
        assert_eq!(refs.pro_number.as_deref(), Some("PRO-456"));
        assert_eq!(refs.bol_number.as_deref(), Some("BOL-789"));
        assert_eq!(refs.document_refs[0].kind, "pod");
        assert_eq!(refs.claim_api_supported, Some(false));
    }

    #[test]
    fn status_codes_map_to_the_error_taxonomy() {
        let http = Arc::new(FakeHttp::new(vec![
            StockforgeHttpResponse {
                status: 429,
                body: Value::Null,
                retry_after_secs: Some(30),
            },
            response(
                401,
                serde_json::json!({ "error": "Unauthorized", "code": "API_KEY_REVOKED" }),
            ),
            response(502, Value::Null),
            response(400, Value::Null),
        ]));
        let client = LiveStockforgeReadClient::new(http, "https://sf.example.test");
        assert_eq!(
            client.fetch_active_alerts("at").unwrap_err(),
            StockforgeError::RateLimited {
                retry_after_ms: Some(30_000),
                message: "stockforge 429 on /api/v1/alerts?status=ACTIVE".to_string(),
            }
        );
        // The body's machine code rides along so the operator learns WHICH
        // key problem they have.
        assert_eq!(
            client.fetch_active_alerts("at").unwrap_err(),
            StockforgeError::AuthRejected {
                message: "stockforge 401 API_KEY_REVOKED on /api/v1/alerts?status=ACTIVE"
                    .to_string(),
            }
        );
        assert!(matches!(
            client.fetch_active_alerts("at").unwrap_err(),
            StockforgeError::Retryable { .. }
        ));
        assert!(matches!(
            client.fetch_active_alerts("at").unwrap_err(),
            StockforgeError::Permanent { .. }
        ));
    }

    #[test]
    fn fixture_client_pages_deterministically() {
        let material = |id: &str| SfMaterialRecord {
            material_id: id.to_string(),
            name: id.to_string(),
            sku: None,
            category: None,
            current_quantity: 1.0,
            reserved_qty: None,
            incoming_qty: None,
            unit: None,
            warning_threshold: None,
            critical_threshold: None,
            threshold_type: None,
            unit_cost_cents: 0,
            lead_time_days: None,
            vendor_name: None,
            is_active: true,
            is_purchasable: Some(true),
            replenishment_policy: Some("PURCHASE".to_string()),
            sale_depletion_policy: Some("STOCK".to_string()),
            updated_at: None,
        };
        let fixture = FixtureStockforgeReadClient {
            materials: vec![material("a"), material("b"), material("c")],
            ..Default::default()
        };
        let first = fixture.fetch_materials("at", 0, 2).expect("page");
        assert_eq!(first.records.len(), 2);
        let second = fixture.fetch_materials("at", 2, 2).expect("page");
        assert_eq!(second.records.len(), 1, "short page ends the walk");
    }
}
