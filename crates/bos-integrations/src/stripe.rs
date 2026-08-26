//! Stripe implementation of the accounting READ seam plus the gated
//! draft-invoice WRITE client (both harvested from agent-monitor-rust's
//! stripe_invoice_live.rs / stripe_invoice_draft.rs and rebuilt on this
//! repo's seams).
//!
//! READ: invoices + customers via the `/v1` list endpoints. Stripe list
//! APIs have NO updated-at filter (change feeds are the Events API, out of
//! scope for v1), so the client IGNORES `since_updated_at` and every walk
//! is a full walk — the caller's content-hash upserts keep re-walks
//! receipt-quiet, and Avery-scale volume fits a few pages. Stripe has no
//! profit-and-loss endpoint: `supports_pnl()` is false and the dashboard
//! falls back to invoice-total sums (basis "invoice_totals"). Stripe DRAFT
//! invoices (incl. the ones this repo's invoice_drafts vertical creates)
//! map to `voided = true` — a draft is not yet a receivable, and dropping
//! the record instead would corrupt the pump's short-page completion check.
//!
//! WRITE: `create_invoice_draft` — find-or-create the customer by email,
//! create the invoice with `auto_advance=false` (stays a DRAFT in Stripe;
//! finalize/send remains a human action in the Stripe dashboard), then one
//! invoice item per line. Every POST carries a step-scoped Idempotency-Key
//! (`{key}:customer`, `{key}:invoice`, `{key}:line:{n}`) so an outbox
//! redelivery replays to the same objects. Dry-run unless the caller's
//! write config enables execution AND a secret key is configured.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::accounting_read::{
    AccountingError, AccountingReadClient, CustomerRecord, InvoiceRecord, Page, PageRequest,
    PnlReport, PnlReportRequest, TierSource,
};

const STRIPE_API_BASE_URL: &str = "https://api.stripe.com";
/// Bail-out for the internal offset-walk fallback (resumed walks only).
const MAX_INTERNAL_CURSOR_PAGES: u32 = 50;

// ---------------------------------------------------------------------------
// Transport seam
// ---------------------------------------------------------------------------

pub struct StripeHttpResponse {
    pub status: u16,
    /// Stripe's Request-Id header (support correlation).
    pub request_id: Option<String>,
    /// Parsed Retry-After seconds on 429 responses.
    pub retry_after_secs: Option<u64>,
    pub body: Value,
}

/// GETs for reads/lookups, form-POSTs (with Idempotency-Key) for creates.
pub trait StripeHttp: Send + Sync {
    fn get_form(
        &self,
        path: &str,
        params: &[(String, String)],
        secret_key: &str,
    ) -> Result<StripeHttpResponse, AccountingError>;

    fn post_form(
        &self,
        path: &str,
        params: &[(String, String)],
        idempotency_key: &str,
        secret_key: &str,
    ) -> Result<StripeHttpResponse, AccountingError>;
}

pub struct ReqwestStripeHttpClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl Default for ReqwestStripeHttpClient {
    fn default() -> Self {
        // Bound connect + total time so a hung request cannot pin the
        // calling blocking worker thread indefinitely.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: STRIPE_API_BASE_URL.to_string(),
            client,
        }
    }
}

impl ReqwestStripeHttpClient {
    fn response_from(
        response: reqwest::blocking::Response,
    ) -> Result<StripeHttpResponse, AccountingError> {
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(StripeHttpResponse {
            status,
            request_id,
            retry_after_secs,
            body,
        })
    }
}

impl StripeHttp for ReqwestStripeHttpClient {
    fn get_form(
        &self,
        path: &str,
        params: &[(String, String)],
        secret_key: &str,
    ) -> Result<StripeHttpResponse, AccountingError> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(secret_key)
            .query(params)
            .send()
            .map_err(|err| AccountingError::Retryable {
                code: "stripe_request_failed".to_string(),
                message: err.to_string(),
            })?;
        Self::response_from(response)
    }

    fn post_form(
        &self,
        path: &str,
        params: &[(String, String)],
        idempotency_key: &str,
        secret_key: &str,
    ) -> Result<StripeHttpResponse, AccountingError> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(secret_key)
            .header("Idempotency-Key", idempotency_key)
            .form(params)
            .send()
            .map_err(|err| AccountingError::Retryable {
                code: "stripe_request_failed".to_string(),
                message: err.to_string(),
            })?;
        Self::response_from(response)
    }
}

// ---------------------------------------------------------------------------
// Read client (AccountingReadClient seam)
// ---------------------------------------------------------------------------

pub struct LiveStripeReadClient<C: StripeHttp = ReqwestStripeHttpClient> {
    http: Arc<C>,
    secret_key: String,
    /// Maps the pump's NEXT page-aligned start_position → the last object id
    /// of the page that preceded it (Stripe paginates by `starting_after`
    /// object ids, not offsets). Populated as pages are served within one
    /// cycle; a walk resumed in a LATER cycle misses the cache and re-walks
    /// from the start internally (bounded, content-hash keeps it quiet).
    page_cursors: Mutex<HashMap<(String, u32), String>>,
}

impl<C: StripeHttp> LiveStripeReadClient<C> {
    pub fn new(http: Arc<C>, secret_key: impl Into<String>) -> Self {
        Self {
            http,
            secret_key: secret_key.into(),
            page_cursors: Mutex::new(HashMap::new()),
        }
    }

    fn get_checked(
        &self,
        path: &str,
        params: &[(String, String)],
    ) -> Result<Value, AccountingError> {
        let response = self.http.get_form(path, params, &self.secret_key)?;
        match response.status {
            200..=299 => Ok(response.body),
            429 => Err(AccountingError::RateLimited {
                retry_after_ms: response.retry_after_secs.map(|secs| secs * 1000),
                message: "stripe 429".to_string(),
            }),
            // Static secret key: an auth failure cannot be refreshed away.
            401 | 403 => Err(AccountingError::Permanent {
                code: "stripe_auth_failed".to_string(),
                message: "check BOS_STRIPE_SECRET_KEY".to_string(),
            }),
            500..=599 => Err(AccountingError::Retryable {
                code: "stripe_server_error".to_string(),
                message: format!("stripe {}", response.status),
            }),
            other => Err(AccountingError::Permanent {
                code: "stripe_request_rejected".to_string(),
                message: format!("stripe {other}"),
            }),
        }
    }

    fn list_params(size: u32, starting_after: Option<&str>) -> Vec<(String, String)> {
        let mut params = vec![("limit".to_string(), size.to_string())];
        if let Some(cursor) = starting_after {
            params.push(("starting_after".to_string(), cursor.to_string()));
        }
        params
    }

    /// One page of a list endpoint at the pump's record offset. Page-aligned
    /// offsets map onto `starting_after` cursors cached from the previous
    /// page; a cache miss (resumed walk) re-walks from the start internally.
    fn list_page(&self, path: &'static str, page: &PageRequest) -> Result<Value, AccountingError> {
        let size = page.effective_page_size();
        let start = page.start_position.max(1);
        let cursor = if start == 1 {
            None
        } else {
            let cached = self
                .page_cursors
                .lock()
                .expect("stripe page cursor lock")
                .get(&(path.to_string(), start))
                .cloned();
            match cached {
                Some(cursor) => Some(cursor),
                None => Some(self.walk_cursor_to(path, size, start)?),
            }
        };
        let body = self.get_checked(path, &Self::list_params(size, cursor.as_deref()))?;
        if let Some(last_id) = data_array(&body).last().and_then(|record| id_field(record)) {
            self.page_cursors
                .lock()
                .expect("stripe page cursor lock")
                .insert((path.to_string(), start + size), last_id);
        }
        Ok(body)
    }

    /// Re-derive the `starting_after` cursor for a record offset by walking
    /// pages from the beginning. Only runs when a walk resumes in a new
    /// cycle (cold cursor cache); each hop is a real Stripe request.
    fn walk_cursor_to(
        &self,
        path: &'static str,
        size: u32,
        start: u32,
    ) -> Result<String, AccountingError> {
        let mut cursor: Option<String> = None;
        let mut reached: u32 = 1;
        let mut hops = 0;
        while reached < start {
            hops += 1;
            if hops > MAX_INTERNAL_CURSOR_PAGES {
                return Err(AccountingError::Permanent {
                    code: "stripe_cursor_walk_overrun".to_string(),
                    message: format!("could not reach offset {start} within bounds"),
                });
            }
            let body = self.get_checked(path, &Self::list_params(size, cursor.as_deref()))?;
            let last_id = data_array(&body).last().and_then(|record| id_field(record));
            match last_id {
                Some(last_id) => cursor = Some(last_id),
                // Fewer records than the offset implies: the walk restarts
                // cleanly on the next short page the pump sees.
                None => break,
            }
            reached += size;
        }
        Ok(cursor.unwrap_or_default())
    }
}

impl<C: StripeHttp> AccountingReadClient for LiveStripeReadClient<C> {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError> {
        let body = self.list_page("/v1/invoices", page)?;
        Ok(Page {
            records: data_array(&body)
                .into_iter()
                .filter_map(invoice_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError> {
        let body = self.list_page("/v1/customers", page)?;
        Ok(Page {
            records: data_array(&body)
                .into_iter()
                .filter_map(customer_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn supports_pnl(&self) -> bool {
        false
    }

    fn fetch_profit_and_loss(
        &self,
        _request: &PnlReportRequest<'_>,
    ) -> Result<PnlReport, AccountingError> {
        Err(AccountingError::Permanent {
            code: "pnl_unsupported".to_string(),
            message: "stripe has no profit-and-loss endpoint".to_string(),
        })
    }
}

fn data_array(body: &Value) -> Vec<&Value> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|records| records.iter().collect())
        .unwrap_or_default()
}

fn id_field(value: &Value) -> Option<String> {
    string_field(value, "id")
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

fn cents_field(value: &Value, key: &str) -> i64 {
    // Stripe amounts are already integer minor units.
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Epoch-seconds → YYYY-MM-DD (UTC), via Howard Hinnant's civil algorithm.
fn epoch_date(value: &Value, key: &str) -> Option<String> {
    let seconds = value.get(key).and_then(Value::as_i64)?;
    let days = seconds.div_euclid(86_400);
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shift + 2) / 5 + 1;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Stripe invoices have no updated-at on the list API; `created` (stable)
/// stands in for the cursor field. Walks are full anyway (see module doc).
fn created_field(value: &Value) -> Option<String> {
    value
        .get("created")
        .and_then(Value::as_u64)
        .map(|seconds| seconds.to_string())
}

fn invoice_record_from_value(value: &Value) -> Option<InvoiceRecord> {
    let invoice_id = id_field(value)?;
    let status = string_field(value, "status").unwrap_or_default();
    // draft: not yet a receivable (includes drafts this repo staged);
    // void/uncollectible/deleted: never collectible. All map to voided so
    // the views' sums and aging exclude them.
    let voided = matches!(status.as_str(), "draft" | "void" | "uncollectible")
        || value
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    Some(InvoiceRecord {
        invoice_id,
        doc_number: string_field(value, "number"),
        customer_id: string_field(value, "customer"),
        customer_name: string_field(value, "customer_name"),
        txn_date: epoch_date(value, "created"),
        due_date: epoch_date(value, "due_date"),
        total_amt_cents: cents_field(value, "total"),
        balance_cents: cents_field(value, "amount_remaining"),
        voided,
        updated_at: created_field(value).unwrap_or_default(),
    })
}

fn customer_record_from_value(value: &Value) -> Option<CustomerRecord> {
    let customer_id = id_field(value)?;
    let email = string_field(value, "email");
    let display_name = string_field(value, "name")
        .or_else(|| email.clone())
        .unwrap_or_else(|| customer_id.clone());
    Some(CustomerRecord {
        customer_id,
        display_name,
        company_name: None,
        email,
        phone: string_field(value, "phone"),
        active: !value
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tier_raw: None,
        tier_source: TierSource::NotProvided,
        updated_at: created_field(value),
    })
}

// ---------------------------------------------------------------------------
// Draft-invoice write client (outbox capability "create_invoice_draft")
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StripeWriteConfig {
    /// None = unconfigured (dry-run regardless of the gate).
    pub secret_key: Option<String>,
    /// Execution gate. `false` => the dry-run client.
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StripeWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StripeApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl StripeApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StripeInvoiceLineItem {
    /// 1-based; feeds the per-line Idempotency-Key.
    pub line_number: u32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub quantity: u32,
    pub unit_amount_cents: u64,
    pub line_total_cents: u64,
}

/// Outbox payload for `provider = "stripe", capability =
/// "create_invoice_draft"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StripeInvoiceDraftOutboxPayload {
    pub idempotency_key: String,
    pub approval: StripeApprovalMetadata,
    /// The BOS draft id, stamped into Stripe metadata for traceability.
    pub draft_ref: String,
    pub customer_name: String,
    pub customer_email: String,
    /// ISO 4217, lowercased on send.
    pub currency: String,
    /// Optional memo, lands on the invoice's description field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    /// Pre-resolved by the caller (this crate does no date math).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date_epoch_seconds: Option<u64>,
    pub line_items: Vec<StripeInvoiceLineItem>,
    pub subtotal_cents: u64,
    pub total_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeInvoiceDraftResponse {
    pub status: StripeExecutionStatus,
    /// Provider ids ("dry-run" sentinels when not executed).
    pub customer_id: String,
    pub invoice_id: String,
    /// Stripe's own status for the created invoice ("draft").
    pub provider_status: Option<String>,
    pub hosted_invoice_url: Option<String>,
}

pub trait StripeExecutionClient: Send + Sync {
    fn create_invoice_draft(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
    ) -> Result<StripeInvoiceDraftResponse, StripeWriteError>;
}

/// Deterministic payload validation (harvested from agent_monitor's
/// validate_stripe_invoice_draft_payload): completeness + line math. Runs in
/// both the dry-run and live clients — a payload that fails here is
/// terminally rejected, never retried.
fn validate_invoice_draft(
    request: &StripeInvoiceDraftOutboxPayload,
) -> Result<(), StripeWriteError> {
    let permanent = |code: &str, message: &str| StripeWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if !request.approval.is_complete() {
        return Err(permanent(
            "stripe_approval_missing",
            "approval metadata is incomplete",
        ));
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "stripe_idempotency_key_missing",
            "idempotency key is required",
        ));
    }
    if request.customer_name.trim().is_empty()
        || request.customer_email.trim().is_empty()
        || !request.customer_email.contains('@')
    {
        return Err(permanent(
            "stripe_customer_not_grounded",
            "customer name and a valid email are required",
        ));
    }
    if request.currency.trim().is_empty() {
        return Err(permanent("stripe_currency_missing", "currency is required"));
    }
    if request.line_items.is_empty() || request.total_cents == 0 {
        return Err(permanent(
            "stripe_invoice_draft_empty",
            "at least one line item and a non-zero total are required",
        ));
    }
    let mut subtotal: u64 = 0;
    for item in &request.line_items {
        if item.line_number == 0
            || item.label.trim().is_empty()
            || item.quantity == 0
            || item.unit_amount_cents == 0
            || item.line_total_cents == 0
        {
            return Err(permanent(
                "stripe_invoice_line_not_grounded",
                "line items require label, quantity, unit amount, and line total",
            ));
        }
        let expected = u64::from(item.quantity)
            .checked_mul(item.unit_amount_cents)
            .ok_or_else(|| permanent("stripe_invoice_line_overflow", "line total overflowed"))?;
        if expected != item.line_total_cents {
            return Err(permanent(
                "stripe_invoice_line_total_mismatch",
                "line total does not match quantity times unit amount",
            ));
        }
        subtotal = subtotal
            .checked_add(item.line_total_cents)
            .ok_or_else(|| permanent("stripe_invoice_total_overflow", "total overflowed"))?;
    }
    if subtotal != request.subtotal_cents || request.subtotal_cents != request.total_cents {
        return Err(permanent(
            "stripe_invoice_total_mismatch",
            "totals do not match line items",
        ));
    }
    Ok(())
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunStripeClient;

impl StripeExecutionClient for DryRunStripeClient {
    fn create_invoice_draft(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
    ) -> Result<StripeInvoiceDraftResponse, StripeWriteError> {
        validate_invoice_draft(request)?;
        Ok(StripeInvoiceDraftResponse {
            status: StripeExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("stripe_write_disabled_dry_run".to_string()),
            },
            customer_id: "dry-run".to_string(),
            invoice_id: "dry-run".to_string(),
            provider_status: None,
            hosted_invoice_url: None,
        })
    }
}

/// Live or dry-run by config: execution requires BOTH the gate and a key.
pub fn stripe_execution_client(config: &StripeWriteConfig) -> Box<dyn StripeExecutionClient> {
    match (&config.secret_key, config.write_enabled) {
        (Some(secret_key), true) => Box::new(LiveStripeWriteClient::new(
            Arc::new(ReqwestStripeHttpClient::default()),
            secret_key.clone(),
        )),
        _ => Box::new(DryRunStripeClient),
    }
}

pub struct LiveStripeWriteClient<C: StripeHttp = ReqwestStripeHttpClient> {
    http: Arc<C>,
    secret_key: String,
}

impl<C: StripeHttp> LiveStripeWriteClient<C> {
    pub fn new(http: Arc<C>, secret_key: impl Into<String>) -> Self {
        Self {
            http,
            secret_key: secret_key.into(),
        }
    }

    fn get(
        &self,
        path: &str,
        params: &[(String, String)],
        step: &'static str,
    ) -> Result<Value, StripeWriteError> {
        let response = self
            .http
            .get_form(path, params, &self.secret_key)
            .map_err(accounting_to_write_error)?;
        check_write_status(step, response)
    }

    fn post(
        &self,
        path: &str,
        params: &[(String, String)],
        idempotency_key: &str,
        step: &'static str,
    ) -> Result<Value, StripeWriteError> {
        let response = self
            .http
            .post_form(path, params, idempotency_key, &self.secret_key)
            .map_err(accounting_to_write_error)?;
        check_write_status(step, response)
    }

    /// Lookup by email first so redeliveries (and repeat customers) reuse
    /// the existing Stripe customer instead of minting duplicates.
    fn find_or_create_customer(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
    ) -> Result<String, StripeWriteError> {
        let found = self.get(
            "/v1/customers",
            &[
                ("email".to_string(), request.customer_email.clone()),
                ("limit".to_string(), "1".to_string()),
            ],
            "stripe_customer_lookup",
        )?;
        if let Some(customer_id) = data_array(&found)
            .first()
            .and_then(|record| id_field(record))
        {
            return Ok(customer_id);
        }
        let created = self.post(
            "/v1/customers",
            &[
                ("email".to_string(), request.customer_email.clone()),
                ("name".to_string(), request.customer_name.clone()),
                (
                    "metadata[bos_draft_id]".to_string(),
                    request.draft_ref.clone(),
                ),
            ],
            &format!("{}:customer", request.idempotency_key),
            "stripe_customer_create",
        )?;
        id_field(&created).ok_or_else(|| StripeWriteError::Permanent {
            code: "stripe_customer_create_invalid_response".to_string(),
            message: "customer create response had no id".to_string(),
        })
    }

    fn create_invoice(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
        customer_id: &str,
    ) -> Result<Value, StripeWriteError> {
        let mut params = vec![
            ("customer".to_string(), customer_id.to_string()),
            ("collection_method".to_string(), "send_invoice".to_string()),
            // The invoice stays a DRAFT — finalize/send is a human action
            // in the Stripe dashboard.
            ("auto_advance".to_string(), "false".to_string()),
            ("currency".to_string(), request.currency.to_lowercase()),
            (
                "metadata[bos_draft_id]".to_string(),
                request.draft_ref.clone(),
            ),
            (
                "metadata[bos_approval_id]".to_string(),
                request.approval.approval_id.clone(),
            ),
        ];
        if let Some(due) = request.due_date_epoch_seconds {
            params.push(("due_date".to_string(), due.to_string()));
        }
        if let Some(memo) = request
            .memo
            .as_deref()
            .map(str::trim)
            .filter(|memo| !memo.is_empty())
        {
            params.push(("description".to_string(), memo.to_string()));
        }
        self.post(
            "/v1/invoices",
            &params,
            &format!("{}:invoice", request.idempotency_key),
            "stripe_invoice_create",
        )
    }

    fn create_invoice_items(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
        customer_id: &str,
        invoice_id: &str,
    ) -> Result<(), StripeWriteError> {
        for line in &request.line_items {
            let description = match line.description.as_deref().map(str::trim) {
                Some(description) if !description.is_empty() => {
                    format!("{} — {}", line.label.trim(), description)
                }
                _ => line.label.trim().to_string(),
            };
            self.post(
                "/v1/invoiceitems",
                &[
                    ("customer".to_string(), customer_id.to_string()),
                    ("invoice".to_string(), invoice_id.to_string()),
                    ("amount".to_string(), line.line_total_cents.to_string()),
                    ("currency".to_string(), request.currency.to_lowercase()),
                    ("description".to_string(), description),
                    (
                        "metadata[bos_draft_id]".to_string(),
                        request.draft_ref.clone(),
                    ),
                    (
                        "metadata[bos_line_number]".to_string(),
                        line.line_number.to_string(),
                    ),
                    (
                        "metadata[bos_quantity]".to_string(),
                        line.quantity.to_string(),
                    ),
                    (
                        "metadata[bos_unit_amount_cents]".to_string(),
                        line.unit_amount_cents.to_string(),
                    ),
                ],
                &format!("{}:line:{}", request.idempotency_key, line.line_number),
                "stripe_invoice_item_create",
            )?;
        }
        Ok(())
    }
}

impl<C: StripeHttp> StripeExecutionClient for LiveStripeWriteClient<C> {
    fn create_invoice_draft(
        &self,
        request: &StripeInvoiceDraftOutboxPayload,
    ) -> Result<StripeInvoiceDraftResponse, StripeWriteError> {
        validate_invoice_draft(request)?;
        let customer_id = self.find_or_create_customer(request)?;
        let invoice = self.create_invoice(request, &customer_id)?;
        let invoice_id = id_field(&invoice).ok_or_else(|| StripeWriteError::Permanent {
            code: "stripe_invoice_create_invalid_response".to_string(),
            message: "invoice create response had no id".to_string(),
        })?;
        self.create_invoice_items(request, &customer_id, &invoice_id)?;
        Ok(StripeInvoiceDraftResponse {
            status: StripeExecutionStatus {
                executed: true,
                dry_run: false,
                reason: None,
            },
            customer_id,
            invoice_id,
            provider_status: string_field(&invoice, "status"),
            hosted_invoice_url: string_field(&invoice, "hosted_invoice_url"),
        })
    }
}

fn accounting_to_write_error(err: AccountingError) -> StripeWriteError {
    match err {
        AccountingError::RateLimited { message, .. } => StripeWriteError::Retryable {
            code: "stripe_rate_limited".to_string(),
            message,
        },
        AccountingError::Retryable { code, message } => {
            StripeWriteError::Retryable { code, message }
        }
        AccountingError::AuthExpired { message } | AccountingError::Permanent { message, .. } => {
            StripeWriteError::Permanent {
                code: "stripe_request_failed_permanently".to_string(),
                message,
            }
        }
    }
}

fn check_write_status(
    step: &'static str,
    response: StripeHttpResponse,
) -> Result<Value, StripeWriteError> {
    match response.status {
        200..=299 => Ok(response.body),
        401 | 403 => Err(StripeWriteError::Permanent {
            code: "stripe_write_unauthorized".to_string(),
            message: "check BOS_STRIPE_SECRET_KEY scopes".to_string(),
        }),
        429 | 500..=599 => Err(StripeWriteError::Retryable {
            code: format!("{step}_retryable_http_error"),
            message: format!("stripe {}", response.status),
        }),
        other => Err(StripeWriteError::Permanent {
            code: format!("{step}_failed"),
            message: format!(
                "stripe {other}: {}",
                response
                    .body
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("(no error message)")
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// (method, path, params, idempotency_key).
    type RecordedCall = (String, String, Vec<(String, String)>, Option<String>);

    #[derive(Default)]
    struct FakeHttp {
        responses: Mutex<VecDeque<(u16, Value)>>,
        calls: Mutex<Vec<RecordedCall>>,
    }

    impl FakeHttp {
        fn push(&self, status: u16, body: Value) {
            self.responses
                .lock()
                .expect("lock")
                .push_back((status, body));
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().expect("lock").clone()
        }

        fn next(&self) -> (u16, Value) {
            self.responses
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or((200, serde_json::json!({ "data": [] })))
        }
    }

    impl StripeHttp for FakeHttp {
        fn get_form(
            &self,
            path: &str,
            params: &[(String, String)],
            _secret_key: &str,
        ) -> Result<StripeHttpResponse, AccountingError> {
            self.calls.lock().expect("lock").push((
                "GET".to_string(),
                path.to_string(),
                params.to_vec(),
                None,
            ));
            let (status, body) = self.next();
            Ok(StripeHttpResponse {
                status,
                request_id: None,
                retry_after_secs: (status == 429).then_some(7),
                body,
            })
        }

        fn post_form(
            &self,
            path: &str,
            params: &[(String, String)],
            idempotency_key: &str,
            _secret_key: &str,
        ) -> Result<StripeHttpResponse, AccountingError> {
            self.calls.lock().expect("lock").push((
                "POST".to_string(),
                path.to_string(),
                params.to_vec(),
                Some(idempotency_key.to_string()),
            ));
            let (status, body) = self.next();
            Ok(StripeHttpResponse {
                status,
                request_id: None,
                retry_after_secs: None,
                body,
            })
        }
    }

    fn stripe_invoice(id: &str, status: &str, total: i64, remaining: i64) -> Value {
        serde_json::json!({
            "id": id,
            "number": format!("INV-{id}"),
            "status": status,
            "customer": "cus_1",
            "customer_name": "Dana Co",
            "created": 1_780_000_000u64,
            "due_date": 1_781_000_000u64,
            "total": total,
            "amount_remaining": remaining,
        })
    }

    fn payload() -> StripeInvoiceDraftOutboxPayload {
        StripeInvoiceDraftOutboxPayload {
            idempotency_key: "invoicedraft:inv_1".to_string(),
            approval: StripeApprovalMetadata {
                approval_id: "appr_1".to_string(),
                approved_by: "avery".to_string(),
                approved_at: "2026-06-11T00:00:00Z".to_string(),
            },
            draft_ref: "inv_1".to_string(),
            customer_name: "Dana Co".to_string(),
            customer_email: "dana@example.com".to_string(),
            currency: "USD".to_string(),
            memo: Some("June consulting".to_string()),
            due_date_epoch_seconds: Some(1_781_000_000),
            line_items: vec![
                StripeInvoiceLineItem {
                    line_number: 1,
                    label: "Consulting".to_string(),
                    description: Some("June engagement".to_string()),
                    quantity: 2,
                    unit_amount_cents: 50_000,
                    line_total_cents: 100_000,
                },
                StripeInvoiceLineItem {
                    line_number: 2,
                    label: "Materials".to_string(),
                    description: None,
                    quantity: 1,
                    unit_amount_cents: 25_000,
                    line_total_cents: 25_000,
                },
            ],
            subtotal_cents: 125_000,
            total_cents: 125_000,
        }
    }

    #[test]
    fn invoice_parsing_maps_statuses_and_amounts() {
        let open = invoice_record_from_value(&stripe_invoice("in_1", "open", 12_500, 12_500))
            .expect("record");
        assert_eq!(open.total_amt_cents, 12_500);
        assert_eq!(open.balance_cents, 12_500);
        assert!(!open.voided);
        assert_eq!(open.txn_date.as_deref(), Some("2026-05-28"));
        assert_eq!(open.doc_number.as_deref(), Some("INV-in_1"));

        // Drafts/void/uncollectible are not receivables — voided, not dropped
        // (dropping would corrupt the pump's short-page completion check).
        for status in ["draft", "void", "uncollectible"] {
            let record = invoice_record_from_value(&stripe_invoice("in_2", status, 100, 100))
                .expect("record");
            assert!(record.voided, "{status} should map to voided");
        }
    }

    #[test]
    fn customer_parsing_falls_back_to_email_then_id() {
        let named = customer_record_from_value(&serde_json::json!({
            "id": "cus_1", "name": "Dana Co", "email": "dana@example.com",
        }))
        .expect("record");
        assert_eq!(named.display_name, "Dana Co");
        let email_only =
            customer_record_from_value(&serde_json::json!({ "id": "cus_2", "email": "x@y.z" }))
                .expect("record");
        assert_eq!(email_only.display_name, "x@y.z");
        let bare = customer_record_from_value(&serde_json::json!({ "id": "cus_3" })).expect("rec");
        assert_eq!(bare.display_name, "cus_3");
    }

    #[test]
    fn read_pagination_caches_starting_after_cursors() {
        let http = Arc::new(FakeHttp::default());
        // Page 1: two records (page size 2), so page 2 exists.
        http.push(
            200,
            serde_json::json!({ "data": [
                stripe_invoice("in_1", "open", 100, 100),
                stripe_invoice("in_2", "open", 200, 200),
            ]}),
        );
        http.push(
            200,
            serde_json::json!({ "data": [ stripe_invoice("in_3", "paid", 300, 0) ]}),
        );
        let client = LiveStripeReadClient::new(http.clone(), "sk_test");
        let first = client
            .fetch_invoices(&PageRequest {
                since_updated_at: None,
                start_position: 1,
                page_size: 2,
            })
            .expect("page 1");
        assert_eq!(first.records.len(), 2);
        let second = client
            .fetch_invoices(&PageRequest {
                since_updated_at: None,
                start_position: 3,
                page_size: 2,
            })
            .expect("page 2");
        assert_eq!(second.records.len(), 1); // short page → walk complete
        let calls = http.calls();
        assert_eq!(calls.len(), 2);
        // Page 1: no cursor. Page 2: starting_after = last id of page 1.
        assert!(!calls[0].2.iter().any(|(key, _)| key == "starting_after"));
        assert!(calls[1]
            .2
            .iter()
            .any(|(key, value)| key == "starting_after" && value == "in_2"));
    }

    #[test]
    fn read_resumed_walk_rederives_the_cursor() {
        let http = Arc::new(FakeHttp::default());
        // Cold cache, start_position 3: one internal hop, then the real page.
        http.push(
            200,
            serde_json::json!({ "data": [
                stripe_invoice("in_1", "open", 100, 100),
                stripe_invoice("in_2", "open", 200, 200),
            ]}),
        );
        http.push(
            200,
            serde_json::json!({ "data": [ stripe_invoice("in_3", "paid", 300, 0) ]}),
        );
        let client = LiveStripeReadClient::new(http.clone(), "sk_test");
        let page = client
            .fetch_invoices(&PageRequest {
                since_updated_at: None,
                start_position: 3,
                page_size: 2,
            })
            .expect("resumed page");
        assert_eq!(page.records.len(), 1);
        let calls = http.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1]
            .2
            .iter()
            .any(|(key, value)| key == "starting_after" && value == "in_2"));
    }

    #[test]
    fn read_429_maps_to_rate_limited_with_retry_after() {
        let http = Arc::new(FakeHttp::default());
        http.push(429, Value::Null);
        let client = LiveStripeReadClient::new(http, "sk_test");
        let err = client
            .fetch_invoices(&PageRequest {
                since_updated_at: None,
                start_position: 1,
                page_size: 100,
            })
            .unwrap_err();
        match err {
            AccountingError::RateLimited { retry_after_ms, .. } => {
                assert_eq!(retry_after_ms, Some(7_000));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn validation_rejects_bad_line_math_and_missing_customer() {
        let mut bad_math = payload();
        bad_math.line_items[0].line_total_cents = 99_999;
        match DryRunStripeClient
            .create_invoice_draft(&bad_math)
            .unwrap_err()
        {
            StripeWriteError::Permanent { code, .. } => {
                assert_eq!(code, "stripe_invoice_line_total_mismatch");
            }
            other => panic!("expected permanent, got {other:?}"),
        }
        let mut no_email = payload();
        no_email.customer_email = "not-an-email".to_string();
        assert!(DryRunStripeClient.create_invoice_draft(&no_email).is_err());
        let mut total_mismatch = payload();
        total_mismatch.total_cents = 1;
        total_mismatch.subtotal_cents = 1;
        assert!(DryRunStripeClient
            .create_invoice_draft(&total_mismatch)
            .is_err());
    }

    #[test]
    fn dry_run_validates_and_returns_sentinels() {
        let response = DryRunStripeClient
            .create_invoice_draft(&payload())
            .expect("dry run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);
        assert_eq!(response.invoice_id, "dry-run");
    }

    #[test]
    fn live_write_runs_the_chain_with_step_scoped_idempotency_keys() {
        let http = Arc::new(FakeHttp::default());
        // Customer lookup misses, create succeeds, invoice + 2 lines.
        http.push(200, serde_json::json!({ "data": [] }));
        http.push(200, serde_json::json!({ "id": "cus_9" }));
        http.push(
            200,
            serde_json::json!({
                "id": "in_9", "status": "draft",
                "hosted_invoice_url": "https://invoice.stripe.com/i/in_9",
            }),
        );
        http.push(200, serde_json::json!({ "id": "ii_1" }));
        http.push(200, serde_json::json!({ "id": "ii_2" }));
        let client = LiveStripeWriteClient::new(http.clone(), "sk_test");
        let response = client.create_invoice_draft(&payload()).expect("created");
        assert!(response.status.executed);
        assert_eq!(response.customer_id, "cus_9");
        assert_eq!(response.invoice_id, "in_9");
        assert_eq!(response.provider_status.as_deref(), Some("draft"));

        let calls = http.calls();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[0].1, "/v1/customers"); // lookup
        assert_eq!(calls[1].3.as_deref(), Some("invoicedraft:inv_1:customer"));
        assert_eq!(calls[2].1, "/v1/invoices");
        assert_eq!(calls[2].3.as_deref(), Some("invoicedraft:inv_1:invoice"));
        // Draft semantics ride on every invoice create.
        assert!(calls[2]
            .2
            .iter()
            .any(|(key, value)| key == "auto_advance" && value == "false"));
        assert!(calls[2]
            .2
            .iter()
            .any(|(key, value)| key == "collection_method" && value == "send_invoice"));
        assert_eq!(calls[3].3.as_deref(), Some("invoicedraft:inv_1:line:1"));
        assert_eq!(calls[4].3.as_deref(), Some("invoicedraft:inv_1:line:2"));
    }

    #[test]
    fn live_write_reuses_an_existing_customer() {
        let http = Arc::new(FakeHttp::default());
        http.push(
            200,
            serde_json::json!({ "data": [{ "id": "cus_existing" }] }),
        );
        http.push(200, serde_json::json!({ "id": "in_9", "status": "draft" }));
        http.push(200, serde_json::json!({ "id": "ii_1" }));
        http.push(200, serde_json::json!({ "id": "ii_2" }));
        let client = LiveStripeWriteClient::new(http.clone(), "sk_test");
        let response = client.create_invoice_draft(&payload()).expect("created");
        assert_eq!(response.customer_id, "cus_existing");
        assert_eq!(http.calls().len(), 4); // no customer create POST
    }

    #[test]
    fn write_status_mapping_is_retryable_only_for_429_and_5xx() {
        let http = Arc::new(FakeHttp::default());
        http.push(500, Value::Null);
        let client = LiveStripeWriteClient::new(http, "sk_test");
        assert!(matches!(
            client.create_invoice_draft(&payload()).unwrap_err(),
            StripeWriteError::Retryable { .. }
        ));
        let http = Arc::new(FakeHttp::default());
        http.push(
            402,
            serde_json::json!({ "error": { "message": "card declined" } }),
        );
        let client = LiveStripeWriteClient::new(http, "sk_test");
        assert!(matches!(
            client.create_invoice_draft(&payload()).unwrap_err(),
            StripeWriteError::Permanent { .. }
        ));
    }

    #[test]
    fn execution_client_selection_requires_gate_and_key() {
        let gated_off = StripeWriteConfig {
            secret_key: Some("sk_test".to_string()),
            write_enabled: false,
        };
        assert!(
            stripe_execution_client(&gated_off)
                .create_invoice_draft(&payload())
                .expect("dry run")
                .status
                .dry_run
        );
        let keyless = StripeWriteConfig {
            secret_key: None,
            write_enabled: true,
        };
        assert!(
            stripe_execution_client(&keyless)
                .create_invoice_draft(&payload())
                .expect("dry run")
                .status
                .dry_run
        );
    }
}
