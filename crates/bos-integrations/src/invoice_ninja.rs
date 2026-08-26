//! Invoice Ninja v5 (self-hosted) implementation of the accounting READ seam
//! — clients/invoices via the REST list endpoints with `?updated_at=`
//! incremental filtering and page-number pagination. Static `X-API-TOKEN`
//! auth: an auth failure is PERMANENT (nothing to refresh), unlike QBO.
//!
//! Invoice Ninja has no profit-and-loss endpoint, so `supports_pnl()` is
//! false and the dashboard's financials fall back to invoice-total sums
//! (basis "invoice_totals").
//!
//! The write half carries two approval-gated capabilities: record_receipt
//! (inbound payments, the ledger_drafts vertical) and create_invoice_draft
//! (outbound invoice DRAFTS, the invoice_drafts vertical — never marked
//! sent; emailing the invoice stays human in Invoice Ninja).

use serde_json::Value;
use std::sync::Arc;

use crate::accounting_read::{
    AccountingError, AccountingReadClient, CustomerRecord, InvoiceRecord, Page, PageRequest,
    PnlReport, PnlReportRequest, TierSource,
};

/// Transport seam (GET-only in the read half).
pub trait InvoiceNinjaHttp: Send + Sync {
    fn get_json(
        &self,
        url: &str,
        api_token: &str,
    ) -> Result<InvoiceNinjaHttpResponse, AccountingError>;
}

pub struct InvoiceNinjaHttpResponse {
    pub status: u16,
    pub body: Value,
    /// Parsed Retry-After seconds on 429 responses.
    pub retry_after_secs: Option<u64>,
}

pub struct ReqwestInvoiceNinjaHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestInvoiceNinjaHttpClient {
    fn default() -> Self {
        // Bound connect + total time so a hung instance cannot pin the
        // calling blocking worker thread indefinitely.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl InvoiceNinjaHttp for ReqwestInvoiceNinjaHttpClient {
    fn get_json(
        &self,
        url: &str,
        api_token: &str,
    ) -> Result<InvoiceNinjaHttpResponse, AccountingError> {
        let response = self
            .client
            .get(url)
            .header("X-API-TOKEN", api_token)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept", "application/json")
            .send()
            .map_err(|err| AccountingError::Retryable {
                code: "invoice_ninja_request_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(InvoiceNinjaHttpResponse {
            status,
            body,
            retry_after_secs,
        })
    }
}

pub struct LiveInvoiceNinjaReadClient<C: InvoiceNinjaHttp = ReqwestInvoiceNinjaHttpClient> {
    http: Arc<C>,
    base_url: String,
    api_token: String,
}

impl<C: InvoiceNinjaHttp> LiveInvoiceNinjaReadClient<C> {
    pub fn new(http: Arc<C>, base_url: impl Into<String>, api_token: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            api_token: api_token.into(),
        }
    }

    fn list(&self, entity: &str, page: &PageRequest) -> Result<Value, AccountingError> {
        let url = list_url(&self.base_url, entity, page);
        let response = self.http.get_json(&url, &self.api_token)?;
        match response.status {
            200..=299 => Ok(response.body),
            429 => Err(AccountingError::RateLimited {
                retry_after_ms: response.retry_after_secs.map(|secs| secs * 1000),
                message: "invoice ninja 429".to_string(),
            }),
            // Static token: an auth failure cannot be refreshed away.
            401 | 403 => Err(AccountingError::Permanent {
                code: "invoice_ninja_auth_failed".to_string(),
                message: "check BOS_INVOICE_NINJA_API_TOKEN".to_string(),
            }),
            500..=599 => Err(AccountingError::Retryable {
                code: "invoice_ninja_server_error".to_string(),
                message: format!("invoice ninja {}", response.status),
            }),
            other => Err(AccountingError::Permanent {
                code: "invoice_ninja_request_rejected".to_string(),
                message: format!("invoice ninja {other}"),
            }),
        }
    }
}

impl<C: InvoiceNinjaHttp> AccountingReadClient for LiveInvoiceNinjaReadClient<C> {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError> {
        let body = self.list("invoices", page)?;
        Ok(Page {
            records: data_array(&body)
                .into_iter()
                .filter_map(invoice_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError> {
        let body = self.list("clients", page)?;
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
            message: "invoice ninja has no profit-and-loss endpoint".to_string(),
        })
    }
}

/// Map the pump's 1-based record offset onto Invoice Ninja's page numbers.
/// The pump only ever requests page-aligned positions (1, 1+size, …).
fn page_number(page: &PageRequest) -> u32 {
    let size = page.effective_page_size();
    let start = page.start_position.max(1);
    debug_assert!(
        (start - 1).is_multiple_of(size),
        "unaligned start_position {start} for page size {size}"
    );
    (start - 1) / size + 1
}

/// `?updated_at=` takes a unix timestamp. Our cursors store whatever the
/// provider's records reported (unix-seconds strings for Invoice Ninja). A
/// non-numeric cursor (e.g. left over from another provider) drops the
/// filter — a full walk is cheap on self-hosted and the content-hash
/// upserts keep it receipt-quiet.
fn since_unix(page: &PageRequest) -> Option<u64> {
    page.since_updated_at
        .as_deref()
        .map(str::trim)
        .filter(|since| !since.is_empty())
        .and_then(|since| since.parse::<u64>().ok())
}

fn list_url(base_url: &str, entity: &str, page: &PageRequest) -> String {
    let mut url = format!(
        "{}/api/v1/{entity}?per_page={}&page={}",
        base_url.trim_end_matches('/'),
        page.effective_page_size(),
        page_number(page),
    );
    if let Some(since) = since_unix(page) {
        url.push_str(&format!("&updated_at={since}"));
    }
    if entity == "invoices" {
        url.push_str("&include=client");
    } else {
        url.push_str("&include=contacts");
    }
    url
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

fn truthy_invoice_ninja_field(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0) != 0,
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            !trimmed.is_empty() && trimmed != "0" && trimmed != "false"
        }
        _ => false,
    }
}

fn invoice_ninja_tombstone_field_is_set(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0) != 0,
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            !trimmed.is_empty() && trimmed != "0"
        }
        Some(Value::Bool(flag)) => *flag,
        _ => false,
    }
}

fn invoice_ninja_record_is_archived(value: &Value) -> bool {
    if truthy_invoice_ninja_field(value, "is_deleted") {
        return true;
    }
    invoice_ninja_tombstone_field_is_set(value, "archived_at")
        || invoice_ninja_tombstone_field_is_set(value, "deleted_at")
}

fn cents_field(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .map(|amount| (amount * 100.0).round() as i64)
        .unwrap_or(0)
}

/// Invoice Ninja sends updated_at as unix seconds (number). Stored as the
/// raw decimal string — lexical order matches numeric order at this
/// magnitude, which is what the cursor walk relies on.
fn updated_at_field(value: &Value) -> Option<String> {
    value
        .get("updated_at")
        .and_then(Value::as_u64)
        .map(|seconds| seconds.to_string())
        .or_else(|| string_field(value, "updated_at"))
}

fn invoice_record_from_value(value: &Value) -> Option<InvoiceRecord> {
    let invoice_id = string_field(value, "id")?;
    // status_id: 1 draft, 2 sent, 3 partial, 4 paid; cancelled invoices are
    // surfaced via is_deleted/archived. Drafts still count as receivables
    // here; the operator sees the status in Invoice Ninja itself.
    Some(InvoiceRecord {
        invoice_id,
        doc_number: string_field(value, "number"),
        customer_id: string_field(value, "client_id"),
        customer_name: value.get("client").and_then(|client| {
            string_field(client, "display_name").or_else(|| string_field(client, "name"))
        }),
        txn_date: string_field(value, "date"),
        due_date: string_field(value, "due_date"),
        total_amt_cents: cents_field(value, "amount"),
        balance_cents: cents_field(value, "balance"),
        voided: invoice_ninja_record_is_archived(value),
        updated_at: updated_at_field(value).unwrap_or_default(),
    })
}

fn customer_record_from_value(value: &Value) -> Option<CustomerRecord> {
    let customer_id = string_field(value, "id")?;
    let display_name = string_field(value, "display_name")
        .or_else(|| string_field(value, "name"))
        .unwrap_or_else(|| customer_id.clone());
    let first_contact = value
        .get("contacts")
        .and_then(Value::as_array)
        .and_then(|contacts| contacts.first());
    Some(CustomerRecord {
        customer_id,
        display_name,
        company_name: string_field(value, "name"),
        email: first_contact.and_then(|contact| string_field(contact, "email")),
        phone: string_field(value, "phone")
            .or_else(|| first_contact.and_then(|contact| string_field(contact, "phone"))),
        active: !invoice_ninja_record_is_archived(value),
        tier_raw: None,
        tier_source: TierSource::NotProvided,
        updated_at: updated_at_field(value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn page(since: Option<&str>, start: u32, size: u32) -> PageRequest {
        PageRequest {
            since_updated_at: since.map(str::to_string),
            start_position: start,
            page_size: size,
        }
    }

    #[test]
    fn list_url_maps_positions_to_pages_and_filters() {
        assert_eq!(
            list_url("https://in.example/", "invoices", &page(None, 1, 100)),
            "https://in.example/api/v1/invoices?per_page=100&page=1&include=client"
        );
        assert_eq!(
            list_url("https://in.example", "invoices", &page(Some("1781000000"), 201, 100)),
            "https://in.example/api/v1/invoices?per_page=100&page=3&updated_at=1781000000&include=client"
        );
        // A cursor left over from another provider (RFC3339) drops the
        // filter instead of sending garbage.
        assert_eq!(
            list_url(
                "https://in.example",
                "clients",
                &page(Some("2026-06-01T00:00:00-07:00"), 1, 100)
            ),
            "https://in.example/api/v1/clients?per_page=100&page=1&include=contacts"
        );
    }

    #[test]
    fn invoice_parsing_reads_amounts_client_and_updated_at() {
        let invoice = serde_json::json!({
            "id": "Opnel5aKBz",
            "number": "INV-0042",
            "client_id": "abc123",
            "client": { "display_name": "Acme LLC" },
            "date": "2026-06-01",
            "due_date": "2026-07-01",
            "amount": 1500.005,
            "balance": 250.0,
            "is_deleted": false,
            "status_id": "2",
            "updated_at": 1781000000u64,
        });
        let record = invoice_record_from_value(&invoice).expect("record");
        assert_eq!(record.invoice_id, "Opnel5aKBz");
        assert_eq!(record.doc_number.as_deref(), Some("INV-0042"));
        assert_eq!(record.customer_name.as_deref(), Some("Acme LLC"));
        assert_eq!(record.total_amt_cents, 150_001); // rounded
        assert_eq!(record.balance_cents, 25_000);
        assert!(!record.voided);
        assert_eq!(record.updated_at, "1781000000");

        let deleted = serde_json::json!({ "id": "x", "is_deleted": true, "amount": 5.0 });
        assert!(invoice_record_from_value(&deleted).expect("record").voided);

        let archived = serde_json::json!({ "id": "y", "archived_at": 1781000100u64 });
        assert!(invoice_record_from_value(&archived).expect("record").voided);
    }

    #[test]
    fn customer_parsing_reads_contacts_and_active() {
        let client = serde_json::json!({
            "id": "abc123",
            "name": "Acme LLC",
            "display_name": "Acme",
            "is_deleted": false,
            "archived_at": 0,
            "deleted_at": 0,
            "contacts": [{ "email": "jane@business-86b318398f.test", "phone": "555-1234" }],
            "updated_at": 1781000001u64,
        });
        let record = customer_record_from_value(&client).expect("record");
        assert_eq!(record.display_name, "Acme");
        assert_eq!(
            record.email.as_deref(),
            Some("jane@business-86b318398f.test")
        );
        assert_eq!(record.phone.as_deref(), Some("555-1234"));
        assert!(record.active);
        assert_eq!(record.tier_source, TierSource::NotProvided);
        assert_eq!(record.updated_at.as_deref(), Some("1781000001"));

        let archived = serde_json::json!({
            "id": "archived",
            "name": "Archived LLC",
            "is_deleted": false,
            "archived_at": 1781000100u64,
        });
        assert!(
            !customer_record_from_value(&archived)
                .expect("record")
                .active
        );
    }

    struct FakeHttp {
        responses: Mutex<VecDeque<InvoiceNinjaHttpResponse>>,
    }

    impl InvoiceNinjaHttp for FakeHttp {
        fn get_json(
            &self,
            _url: &str,
            _token: &str,
        ) -> Result<InvoiceNinjaHttpResponse, AccountingError> {
            Ok(self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted response"))
        }
    }

    fn client(responses: Vec<InvoiceNinjaHttpResponse>) -> LiveInvoiceNinjaReadClient<FakeHttp> {
        LiveInvoiceNinjaReadClient::new(
            Arc::new(FakeHttp {
                responses: Mutex::new(responses.into()),
            }),
            "https://in.example",
            "token-1",
        )
    }

    #[test]
    fn status_codes_map_to_the_error_taxonomy() {
        let client = client(vec![
            InvoiceNinjaHttpResponse {
                status: 429,
                body: Value::Null,
                retry_after_secs: Some(15),
            },
            InvoiceNinjaHttpResponse {
                status: 401,
                body: Value::Null,
                retry_after_secs: None,
            },
            InvoiceNinjaHttpResponse {
                status: 503,
                body: Value::Null,
                retry_after_secs: None,
            },
            InvoiceNinjaHttpResponse {
                status: 200,
                body: serde_json::json!({ "data": [] }),
                retry_after_secs: None,
            },
        ]);
        let page = page(None, 1, 100);
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::RateLimited {
                retry_after_ms: Some(15_000),
                ..
            }
        ));
        // Static token: auth failure is PERMANENT, never AuthExpired.
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::Permanent { code, .. } if code == "invoice_ninja_auth_failed"
        ));
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::Retryable { .. }
        ));
        assert!(client.fetch_invoices(&page).expect("ok").records.is_empty());

        // No P&L capability.
        assert!(!client.supports_pnl());
        assert!(matches!(
            client.fetch_profit_and_loss(&PnlReportRequest::total("2026-06-01", "2026-06-10")),
            Err(AccountingError::Permanent { code, .. }) if code == "pnl_unsupported"
        ));
    }

    // ------------------------- write half -------------------------

    fn receipt() -> InvoiceNinjaReceiptOutboxPayload {
        InvoiceNinjaReceiptOutboxPayload {
            idempotency_key: "ledgerdraft:led_1".to_string(),
            approval: InvoiceNinjaApprovalMetadata {
                approval_id: "appr_led_1".to_string(),
                approved_by: "user_example".to_string(),
                approved_at: "2026-06-10T12:00:00Z".to_string(),
            },
            payer_name: "Acme LLC".to_string(),
            payer_email: Some("jane@business-86b318398f.test".to_string()),
            amount_cents: 150_000,
            paid_date: "2026-06-10".to_string(),
            description: "Stripe receipt".to_string(),
            invoice_number: "BOS-led_1".to_string(),
        }
    }

    #[test]
    fn dry_run_validates_and_never_executes() {
        let client = DryRunInvoiceNinjaClient;
        let response = client.record_receipt(&receipt()).expect("dry-run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);
        assert_eq!(response.client_id, "dry-run");

        let mut bad = receipt();
        bad.amount_cents = 0;
        assert!(matches!(
            client.record_receipt(&bad),
            Err(InvoiceNinjaWriteError::Permanent { code, .. })
                if code == "invoice_ninja_amount_invalid"
        ));
        let mut bad = receipt();
        bad.approval.approved_by = String::new();
        assert!(matches!(
            client.record_receipt(&bad),
            Err(InvoiceNinjaWriteError::Permanent { code, .. })
                if code == "invoice_ninja_approval_missing"
        ));
        let mut bad = receipt();
        bad.paid_date = "06/10/2026".to_string();
        assert!(matches!(
            client.record_receipt(&bad),
            Err(InvoiceNinjaWriteError::Permanent { code, .. })
                if code == "invoice_ninja_date_invalid"
        ));
    }

    #[test]
    fn execution_client_factory_dry_runs_unless_fully_configured_and_gated() {
        let dry = invoice_ninja_execution_client(&InvoiceNinjaWriteConfig {
            base_url: Some("https://in.example".to_string()),
            api_token: Some("t".to_string()),
            write_enabled: false,
        });
        assert!(dry.record_receipt(&receipt()).expect("dry").status.dry_run);
        let unconfigured = invoice_ninja_execution_client(&InvoiceNinjaWriteConfig {
            base_url: None,
            api_token: Some("t".to_string()),
            write_enabled: true,
        });
        assert!(
            unconfigured
                .record_receipt(&receipt())
                .expect("dry")
                .status
                .dry_run
        );
    }

    /// Scripted write transport: each entry is (expected url substring,
    /// response). Panics on a call that doesn't match the script — proving
    /// skipped steps make NO requests. Hit urls are recorded for negative
    /// assertions (e.g. an invoice DRAFT must never carry mark_sent).
    struct ScriptedWriteHttp {
        script: Mutex<VecDeque<(&'static str, InvoiceNinjaHttpResponse)>>,
        seen: Mutex<Vec<String>>,
    }

    impl ScriptedWriteHttp {
        fn new(script: Vec<(&'static str, u16, Value)>) -> Self {
            Self {
                script: Mutex::new(
                    script
                        .into_iter()
                        .map(|(fragment, status, body)| {
                            (
                                fragment,
                                InvoiceNinjaHttpResponse {
                                    status,
                                    body,
                                    retry_after_secs: None,
                                },
                            )
                        })
                        .collect(),
                ),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("lock").clone()
        }

        fn next(&self, url: &str) -> InvoiceNinjaHttpResponse {
            self.seen.lock().expect("lock").push(url.to_string());
            let (fragment, response) = self
                .script
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected request: {url}"));
            assert!(
                url.contains(fragment),
                "expected url containing {fragment}, got {url}"
            );
            response
        }

        fn exhausted(&self) {
            assert!(
                self.script.lock().expect("lock").is_empty(),
                "script not fully consumed"
            );
        }
    }

    impl InvoiceNinjaWriteHttp for ScriptedWriteHttp {
        fn get_json(
            &self,
            url: &str,
            _token: &str,
        ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError> {
            Ok(self.next(url))
        }

        fn post_json(
            &self,
            url: &str,
            _token: &str,
            _body: &Value,
        ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError> {
            Ok(self.next(url))
        }
    }

    fn write_client(
        http: Arc<ScriptedWriteHttp>,
    ) -> LiveInvoiceNinjaWriteClient<ScriptedWriteHttp> {
        LiveInvoiceNinjaWriteClient::new(http, "https://in.example", "token-1")
    }

    fn empty_list() -> Value {
        serde_json::json!({ "data": [] })
    }

    #[test]
    fn record_receipt_runs_the_full_ensure_chain() {
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=jane%40business-86b318398f.test",
                200,
                empty_list(),
            ),
            (
                "clients",
                200,
                serde_json::json!({ "data": { "id": "c1" } }),
            ),
            ("invoices?number=BOS-led_1", 200, empty_list()),
            (
                "invoices?mark_sent=true",
                200,
                serde_json::json!({ "data": { "id": "i1", "balance": 1500.0 } }),
            ),
            ("payments?transaction_reference=", 200, empty_list()),
            (
                "payments?email_receipt=false",
                200,
                serde_json::json!({ "data": { "id": "p1" } }),
            ),
        ]));
        let response = write_client(http.clone())
            .record_receipt(&receipt())
            .expect("chain");
        http.exhausted();
        assert_eq!(
            (
                response.client_id.as_str(),
                response.invoice_id.as_str(),
                response.payment_id.as_deref()
            ),
            ("c1", "i1", Some("p1"))
        );
        assert!(response.status.executed);
    }

    #[test]
    fn redelivery_resumes_at_the_first_unsatisfied_step() {
        // Client + invoice already exist (a prior delivery crashed before
        // the payment); the invoice still carries a balance.
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=",
                200,
                serde_json::json!({ "data": [{ "id": "c1" }] }),
            ),
            (
                "invoices?number=BOS-led_1",
                200,
                serde_json::json!({ "data": [{ "id": "i1", "number": "BOS-led_1", "balance": 1500.0 }] }),
            ),
            ("payments?transaction_reference=", 200, empty_list()),
            (
                "payments?email_receipt=false",
                200,
                serde_json::json!({ "data": { "id": "p1" } }),
            ),
        ]));
        let response = write_client(http.clone())
            .record_receipt(&receipt())
            .expect("resume");
        http.exhausted();
        assert_eq!(response.payment_id.as_deref(), Some("p1"));
    }

    #[test]
    fn record_receipt_ignores_archived_client_lookup_matches() {
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=jane%40business-86b318398f.test",
                200,
                serde_json::json!({
                    "data": [{
                        "id": "archived-client",
                        "is_deleted": false,
                        "archived_at": 1781000100u64,
                    }]
                }),
            ),
            (
                "clients",
                200,
                serde_json::json!({ "data": { "id": "active-client" } }),
            ),
            ("invoices?number=BOS-led_1", 200, empty_list()),
            (
                "invoices?mark_sent=true",
                200,
                serde_json::json!({ "data": { "id": "i1", "balance": 1500.0 } }),
            ),
            ("payments?transaction_reference=", 200, empty_list()),
            (
                "payments?email_receipt=false",
                200,
                serde_json::json!({ "data": { "id": "p1" } }),
            ),
        ]));
        let response = write_client(http.clone())
            .record_receipt(&receipt())
            .expect("chain");
        http.exhausted();
        assert_eq!(response.client_id, "active-client");
    }

    #[test]
    fn fully_delivered_job_is_a_noop_returning_the_same_ids() {
        // Invoice exists with balance 0: the payment step makes ZERO
        // requests (an extra request would panic the script).
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=",
                200,
                serde_json::json!({ "data": [{ "id": "c1" }] }),
            ),
            (
                "invoices?number=BOS-led_1",
                200,
                serde_json::json!({ "data": [{ "id": "i1", "number": "BOS-led_1", "balance": 0.0 }] }),
            ),
        ]));
        let response = write_client(http.clone())
            .record_receipt(&receipt())
            .expect("noop");
        http.exhausted();
        assert_eq!(response.payment_id, None, "nothing left to apply");
    }

    #[test]
    fn duplicate_invoice_number_resumes_via_relookup() {
        // Create returns 422 (number taken by a crashed prior delivery):
        // the chain re-fetches and continues.
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=",
                200,
                serde_json::json!({ "data": [{ "id": "c1" }] }),
            ),
            ("invoices?number=BOS-led_1", 200, empty_list()),
            (
                "invoices?mark_sent=true",
                422,
                serde_json::json!({ "message": "The number has already been taken." }),
            ),
            (
                "invoices?number=BOS-led_1",
                200,
                serde_json::json!({ "data": [{ "id": "i1", "number": "BOS-led_1", "balance": 1500.0 }] }),
            ),
            (
                "payments?transaction_reference=",
                200,
                serde_json::json!({ "data": [{ "id": "p-existing" }] }),
            ),
        ]));
        let response = write_client(http.clone())
            .record_receipt(&receipt())
            .expect("dup resume");
        http.exhausted();
        assert_eq!(response.invoice_id, "i1");
        assert_eq!(
            response.payment_id.as_deref(),
            Some("p-existing"),
            "payment found by transaction_reference, not re-created"
        );
    }

    // -- Outbound invoice drafts (capability create_invoice_draft) ---------

    fn invoice_draft() -> InvoiceNinjaInvoiceDraftOutboxPayload {
        InvoiceNinjaInvoiceDraftOutboxPayload {
            idempotency_key: "invoicedraft:inv_1".to_string(),
            approval: InvoiceNinjaApprovalMetadata {
                approval_id: "appr_inv_1".to_string(),
                approved_by: "user_example".to_string(),
                approved_at: "2026-06-11T12:00:00Z".to_string(),
            },
            draft_ref: "inv_1".to_string(),
            customer_name: "Dana Co".to_string(),
            customer_email: Some("dana@example.com".to_string()),
            due_date: Some("2026-07-01".to_string()),
            memo: Some("June consulting".to_string()),
            line_items: vec![
                InvoiceNinjaInvoiceLineItem {
                    line_number: 1,
                    label: "Consulting".to_string(),
                    description: Some("June engagement".to_string()),
                    quantity: 2,
                    unit_amount_cents: 50_000,
                    line_total_cents: 100_000,
                },
                InvoiceNinjaInvoiceLineItem {
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
    fn invoice_draft_chain_creates_client_and_draft_without_sending() {
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            ("clients?email=dana%40example.com", 200, empty_list()),
            (
                "clients",
                200,
                serde_json::json!({ "data": { "id": "c9" } }),
            ),
            (
                "invoices?client_id=c9&per_page=100&page=1",
                200,
                empty_list(),
            ),
            (
                "invoices",
                200,
                serde_json::json!({ "data": { "id": "i9", "number": "BHC-0042" } }),
            ),
        ]));
        let response = write_client(http.clone())
            .create_invoice_draft(&invoice_draft())
            .expect("chain");
        http.exhausted();
        assert_eq!(response.client_id, "c9");
        assert_eq!(response.invoice_id, "i9");
        // The number is INVOICE NINJA's (Generated Numbers), never ours.
        assert_eq!(response.invoice_number.as_deref(), Some("BHC-0042"));
        assert!(response.status.executed);
        // The invoice stays an IN DRAFT: no mark_sent, no email anywhere.
        for url in http.seen() {
            assert!(
                !url.contains("mark_sent") && !url.contains("send_email=true"),
                "draft create must not send: {url}"
            );
        }
    }

    #[test]
    fn invoice_draft_ignores_archived_client_lookup_matches() {
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=dana%40example.com",
                200,
                serde_json::json!({
                    "data": [{
                        "id": "archived-client",
                        "archived_at": 1781000100u64,
                    }]
                }),
            ),
            (
                "clients",
                200,
                serde_json::json!({ "data": { "id": "active-client" } }),
            ),
            (
                "invoices?client_id=active-client&per_page=100&page=1",
                200,
                empty_list(),
            ),
            (
                "invoices",
                200,
                serde_json::json!({ "data": { "id": "i9", "number": "BHC-0042" } }),
            ),
        ]));
        let response = write_client(http.clone())
            .create_invoice_draft(&invoice_draft())
            .expect("chain");
        http.exhausted();
        assert_eq!(response.client_id, "active-client");
        assert_eq!(response.invoice_id, "i9");
    }

    #[test]
    fn invoice_draft_redelivery_finds_the_existing_invoice_by_marker() {
        // Client + invoice already exist (a prior delivery crashed after
        // create): the chain resolves both by lookup — the invoice via its
        // [bos:draft …] private-notes marker — and creates nothing.
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=",
                200,
                serde_json::json!({ "data": [{ "id": "c9" }] }),
            ),
            (
                "invoices?client_id=c9&per_page=100&page=1",
                200,
                serde_json::json!({ "data": [
                    { "id": "other", "number": "BHC-0041", "private_notes": "" },
                    {
                        "id": "i9",
                        "number": "BHC-0042",
                        "private_notes": format!("{} via BusinessOS (approval appr_inv_1)", bos_draft_marker("inv_1")),
                    },
                ] }),
            ),
        ]));
        let response = write_client(http.clone())
            .create_invoice_draft(&invoice_draft())
            .expect("resume");
        http.exhausted();
        assert_eq!(response.invoice_id, "i9");
        assert_eq!(response.invoice_number.as_deref(), Some("BHC-0042"));
    }

    #[test]
    fn invoice_draft_redelivery_scans_past_the_first_page() {
        // The marker is the idempotency anchor. A retry can be delayed past
        // many later invoices, so dedupe cannot rely on only the first page.
        let first_page: Vec<Value> = (0..100)
            .map(|idx| {
                serde_json::json!({
                    "id": format!("other-{idx}"),
                    "number": format!("BHC-{idx:04}"),
                    "private_notes": "",
                })
            })
            .collect();
        let http = Arc::new(ScriptedWriteHttp::new(vec![
            (
                "clients?email=",
                200,
                serde_json::json!({ "data": [{ "id": "c9" }] }),
            ),
            (
                "invoices?client_id=c9&per_page=100&page=1",
                200,
                serde_json::json!({ "data": first_page }),
            ),
            (
                "invoices?client_id=c9&per_page=100&page=2",
                200,
                serde_json::json!({ "data": [{
                    "id": "i9",
                    "number": "BHC-1042",
                    "private_notes": bos_draft_marker("inv_1"),
                }] }),
            ),
        ]));
        let response = write_client(http.clone())
            .create_invoice_draft(&invoice_draft())
            .expect("resume");
        http.exhausted();
        assert_eq!(response.invoice_id, "i9");
        assert_eq!(response.invoice_number.as_deref(), Some("BHC-1042"));
    }

    #[test]
    fn invoice_draft_validation_rejects_bad_math_and_dry_run_returns_sentinels() {
        let mut bad = invoice_draft();
        bad.line_items[0].line_total_cents = 99_999;
        assert!(matches!(
            DryRunInvoiceNinjaClient.create_invoice_draft(&bad),
            Err(InvoiceNinjaWriteError::Permanent { .. })
        ));
        let response = DryRunInvoiceNinjaClient
            .create_invoice_draft(&invoice_draft())
            .expect("dry run");
        assert!(response.status.dry_run);
        assert_eq!(response.invoice_id, "dry-run");
    }
}

// ---------------------------------------------------------------------------
// Write half: record_receipt — the approval-gated path that records a
// received payment (e.g. a Stripe receipt) as client + invoice + payment.
//
// At-least-once outbox delivery makes idempotency the design center. There
// is no provider transaction, so the executor is a deterministic ENSURE
// chain — every step looks for its outcome before creating it:
//   1. ensure_client   (lookup by email, then name; create if absent)
//   2. ensure_invoice  (number = the payload's invoice_number, globally
//                       unique; duplicate-number 422 = already created)
//   3. ensure_payment  (skip when the invoice balance is 0; search by
//                       transaction_reference = idempotency key first)
// A redelivered job resumes at the first unsatisfied step; a completed job
// is a no-op returning the same ids.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct InvoiceNinjaWriteConfig {
    /// None = unconfigured (dry-run regardless of the gate).
    pub base_url: Option<String>,
    pub api_token: Option<String>,
    /// Execution gate. `false` => the dry-run client.
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceNinjaWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceNinjaApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl InvoiceNinjaApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "invoice_ninja", capability =
/// "record_receipt"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceNinjaReceiptOutboxPayload {
    pub idempotency_key: String,
    pub approval: InvoiceNinjaApprovalMetadata,
    pub payer_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_email: Option<String>,
    pub amount_cents: i64,
    /// YYYY-MM-DD.
    pub paid_date: String,
    pub description: String,
    /// The invoice's unique number — the cross-delivery idempotency anchor.
    pub invoice_number: String,
}

/// One billable line on an outbound invoice draft. Totals arrive
/// pre-validated (the caller recomputes them); the live client re-checks the
/// math anyway — money requests are validated at every boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceNinjaInvoiceLineItem {
    pub line_number: u32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub quantity: u32,
    pub unit_amount_cents: i64,
    pub line_total_cents: i64,
}

/// Outbox payload for `provider = "invoice_ninja", capability =
/// "create_invoice_draft"` — an OUTBOUND invoice staged as an Invoice Ninja
/// DRAFT (never marked sent; emailing it to the client stays a human action
/// in Invoice Ninja).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceNinjaInvoiceDraftOutboxPayload {
    pub idempotency_key: String,
    pub approval: InvoiceNinjaApprovalMetadata,
    /// The BOS draft id (traceability in private notes).
    pub draft_ref: String,
    pub customer_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_email: Option<String>,
    /// YYYY-MM-DD, only when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    /// Lands on the invoice's public notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    pub line_items: Vec<InvoiceNinjaInvoiceLineItem>,
    pub subtotal_cents: i64,
    pub total_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceNinjaExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceNinjaReceiptResponse {
    pub status: InvoiceNinjaExecutionStatus,
    /// Provider ids ("dry-run" sentinels when not executed). `payment_id`
    /// is None when the invoice was already fully paid (nothing to apply).
    pub client_id: String,
    pub invoice_id: String,
    pub payment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceNinjaInvoiceDraftResponse {
    pub status: InvoiceNinjaExecutionStatus,
    /// Provider ids ("dry-run" sentinels when not executed).
    pub client_id: String,
    pub invoice_id: String,
    /// The number INVOICE NINJA assigned from its Generated Numbers pattern
    /// (None for dry-run, or when the instance numbers on send, not save).
    pub invoice_number: Option<String>,
}

pub trait InvoiceNinjaExecutionClient: Send + Sync {
    fn record_receipt(
        &self,
        request: &InvoiceNinjaReceiptOutboxPayload,
    ) -> Result<InvoiceNinjaReceiptResponse, InvoiceNinjaWriteError>;

    /// Stage an OUTBOUND invoice as an Invoice Ninja DRAFT (find-or-create
    /// client → create the invoice with NO number, so Invoice Ninja assigns
    /// the next one from its Generated Numbers pattern; no mark_sent —
    /// review and send stay human in Invoice Ninja). Redelivery dedupe rides
    /// the [bos:draft …] marker in private notes, scanned over the client's
    /// recent invoices.
    fn create_invoice_draft(
        &self,
        request: &InvoiceNinjaInvoiceDraftOutboxPayload,
    ) -> Result<InvoiceNinjaInvoiceDraftResponse, InvoiceNinjaWriteError>;
}

fn validate_receipt(
    request: &InvoiceNinjaReceiptOutboxPayload,
) -> Result<(), InvoiceNinjaWriteError> {
    let permanent = |code: &str, message: &str| InvoiceNinjaWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if !request.approval.is_complete() {
        return Err(permanent(
            "invoice_ninja_approval_missing",
            "approval metadata is incomplete",
        ));
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "invoice_ninja_idempotency_key_missing",
            "idempotency key is required",
        ));
    }
    if request.payer_name.trim().is_empty() {
        return Err(permanent(
            "invoice_ninja_payer_missing",
            "payer name is empty",
        ));
    }
    if request.amount_cents <= 0 {
        return Err(permanent(
            "invoice_ninja_amount_invalid",
            "amount must be positive",
        ));
    }
    let date = request.paid_date.as_bytes();
    if date.len() != 10 || date[4] != b'-' || date[7] != b'-' {
        return Err(permanent(
            "invoice_ninja_date_invalid",
            "paid_date must be YYYY-MM-DD",
        ));
    }
    if request.invoice_number.trim().is_empty() {
        return Err(permanent(
            "invoice_ninja_invoice_number_missing",
            "invoice number is required",
        ));
    }
    Ok(())
}

/// Deterministic outbound-invoice validation: completeness + line math.
/// Runs in both the dry-run and live clients — a payload that fails here is
/// terminally rejected, never retried.
fn validate_invoice_draft(
    request: &InvoiceNinjaInvoiceDraftOutboxPayload,
) -> Result<(), InvoiceNinjaWriteError> {
    let permanent = |code: &str, message: &str| InvoiceNinjaWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if !request.approval.is_complete() {
        return Err(permanent(
            "invoice_ninja_approval_missing",
            "approval metadata is incomplete",
        ));
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "invoice_ninja_idempotency_key_missing",
            "idempotency key is required",
        ));
    }
    if request.customer_name.trim().is_empty() {
        return Err(permanent(
            "invoice_ninja_customer_missing",
            "customer name is empty",
        ));
    }
    if let Some(email) = request.customer_email.as_deref() {
        if !email.contains('@') || email.contains(char::is_whitespace) {
            return Err(permanent(
                "invoice_ninja_email_invalid",
                "customer email is malformed",
            ));
        }
    }
    if let Some(date) = request.due_date.as_deref() {
        let bytes = date.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(permanent(
                "invoice_ninja_date_invalid",
                "due_date must be YYYY-MM-DD",
            ));
        }
    }
    if request.line_items.is_empty() || request.total_cents <= 0 {
        return Err(permanent(
            "invoice_ninja_invoice_empty",
            "at least one line item and a positive total are required",
        ));
    }
    let mut subtotal: i64 = 0;
    for item in &request.line_items {
        if item.label.trim().is_empty()
            || item.quantity == 0
            || item.unit_amount_cents <= 0
            || item.line_total_cents != i64::from(item.quantity) * item.unit_amount_cents
        {
            return Err(permanent(
                "invoice_ninja_line_not_grounded",
                "line items require label, quantity, unit amount, and matching line total",
            ));
        }
        subtotal += item.line_total_cents;
    }
    if subtotal != request.subtotal_cents || request.subtotal_cents != request.total_cents {
        return Err(permanent(
            "invoice_ninja_total_mismatch",
            "totals do not match line items",
        ));
    }
    Ok(())
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunInvoiceNinjaClient;

impl InvoiceNinjaExecutionClient for DryRunInvoiceNinjaClient {
    fn record_receipt(
        &self,
        request: &InvoiceNinjaReceiptOutboxPayload,
    ) -> Result<InvoiceNinjaReceiptResponse, InvoiceNinjaWriteError> {
        validate_receipt(request)?;
        Ok(InvoiceNinjaReceiptResponse {
            status: InvoiceNinjaExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("invoice_ninja_write_disabled_dry_run".to_string()),
            },
            client_id: "dry-run".to_string(),
            invoice_id: "dry-run".to_string(),
            payment_id: Some("dry-run".to_string()),
        })
    }

    fn create_invoice_draft(
        &self,
        request: &InvoiceNinjaInvoiceDraftOutboxPayload,
    ) -> Result<InvoiceNinjaInvoiceDraftResponse, InvoiceNinjaWriteError> {
        validate_invoice_draft(request)?;
        Ok(InvoiceNinjaInvoiceDraftResponse {
            status: InvoiceNinjaExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("invoice_ninja_write_disabled_dry_run".to_string()),
            },
            client_id: "dry-run".to_string(),
            invoice_id: "dry-run".to_string(),
            invoice_number: None,
        })
    }
}

/// Write transport seam: the ensure-chain needs lookups (GET) and creates
/// (POST). The read half stays GET-only by construction.
pub trait InvoiceNinjaWriteHttp: Send + Sync {
    fn get_json(
        &self,
        url: &str,
        api_token: &str,
    ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError>;
    fn post_json(
        &self,
        url: &str,
        api_token: &str,
        body: &Value,
    ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError>;
}

impl InvoiceNinjaWriteHttp for ReqwestInvoiceNinjaHttpClient {
    fn get_json(
        &self,
        url: &str,
        api_token: &str,
    ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError> {
        self.send(self.client.get(url), api_token)
    }

    fn post_json(
        &self,
        url: &str,
        api_token: &str,
        body: &Value,
    ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError> {
        self.send(self.client.post(url).json(body), api_token)
    }
}

impl ReqwestInvoiceNinjaHttpClient {
    fn send(
        &self,
        request: reqwest::blocking::RequestBuilder,
        api_token: &str,
    ) -> Result<InvoiceNinjaHttpResponse, InvoiceNinjaWriteError> {
        let response = request
            .header("X-API-TOKEN", api_token)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept", "application/json")
            .send()
            .map_err(|err| InvoiceNinjaWriteError::Retryable {
                code: "invoice_ninja_request_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(InvoiceNinjaHttpResponse {
            status,
            body,
            retry_after_secs,
        })
    }
}

pub struct LiveInvoiceNinjaWriteClient<C: InvoiceNinjaWriteHttp = ReqwestInvoiceNinjaHttpClient> {
    http: Arc<C>,
    base_url: String,
    api_token: String,
}

impl<C: InvoiceNinjaWriteHttp> LiveInvoiceNinjaWriteClient<C> {
    pub fn new(http: Arc<C>, base_url: impl Into<String>, api_token: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            api_token: api_token.into(),
        }
    }

    fn url(&self, path_and_query: &str) -> String {
        format!(
            "{}/api/v1/{path_and_query}",
            self.base_url.trim_end_matches('/')
        )
    }

    fn check(
        &self,
        response: InvoiceNinjaHttpResponse,
        context: &str,
    ) -> Result<Value, InvoiceNinjaWriteError> {
        match response.status {
            200..=299 => Ok(response.body),
            401 | 403 => Err(InvoiceNinjaWriteError::Permanent {
                code: "invoice_ninja_auth_failed".to_string(),
                message: format!("{context}: check BOS_INVOICE_NINJA_API_TOKEN"),
            }),
            429 => Err(InvoiceNinjaWriteError::Retryable {
                code: "invoice_ninja_rate_limited".to_string(),
                message: format!("{context}: 429"),
            }),
            500..=599 => Err(InvoiceNinjaWriteError::Retryable {
                code: "invoice_ninja_server_error".to_string(),
                message: format!("{context}: {}", response.status),
            }),
            other => Err(InvoiceNinjaWriteError::Permanent {
                code: "invoice_ninja_request_rejected".to_string(),
                message: format!(
                    "{context}: {other} {}",
                    response
                        .body
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                ),
            }),
        }
    }

    fn first_id(body: &Value) -> Option<String> {
        body.get("data")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn first_active_client_id(body: &Value) -> Option<String> {
        body.get("data")
            .and_then(Value::as_array)
            .and_then(|records| {
                records
                    .iter()
                    .find(|record| !invoice_ninja_record_is_archived(record))
            })
            .and_then(|record| record.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn created_id(body: &Value, context: &str) -> Result<String, InvoiceNinjaWriteError> {
        body.get("data")
            .and_then(|data| data.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| InvoiceNinjaWriteError::Permanent {
                code: "invoice_ninja_response_invalid".to_string(),
                message: format!("{context}: created entity has no id"),
            })
    }

    /// Find-or-create a client by email (preferred) or name. Shared by the
    /// receipt and outbound-invoice chains so redeliveries — and repeat
    /// customers — reuse the existing client instead of minting duplicates.
    fn ensure_client(
        &self,
        name: &str,
        email: Option<&str>,
    ) -> Result<String, InvoiceNinjaWriteError> {
        let encode = crate::qbo_oauth::encode_query_component;
        let lookup = if let Some(email) = email {
            self.url(&format!("clients?email={}&per_page=5", encode(email)))
        } else {
            self.url(&format!("clients?name={}&per_page=5", encode(name)))
        };
        let body = self.check(
            self.http.get_json(&lookup, &self.api_token)?,
            "client lookup",
        )?;
        if let Some(id) = Self::first_active_client_id(&body) {
            return Ok(id);
        }
        let mut contact = serde_json::Map::new();
        if let Some(email) = email {
            contact.insert("email".to_string(), Value::String(email.to_string()));
        }
        contact.insert("send_email".to_string(), Value::Bool(false));
        let created = self.http.post_json(
            &self.url("clients"),
            &self.api_token,
            &serde_json::json!({
                "name": name,
                "contacts": [Value::Object(contact)],
            }),
        )?;
        let body = self.check(created, "client create")?;
        Self::created_id(&body, "client create")
    }

    fn ensure_invoice(
        &self,
        request: &InvoiceNinjaReceiptOutboxPayload,
        client_id: &str,
    ) -> Result<(String, i64), InvoiceNinjaWriteError> {
        let encode = crate::qbo_oauth::encode_query_component;
        let lookup = self.url(&format!(
            "invoices?number={}&per_page=5",
            encode(&request.invoice_number)
        ));
        let find = |body: &Value| -> Option<(String, i64)> {
            body.get("data")
                .and_then(Value::as_array)
                .and_then(|records| {
                    records.iter().find(|record| {
                        record.get("number").and_then(Value::as_str)
                            == Some(request.invoice_number.as_str())
                    })
                })
                .and_then(|record| {
                    Some((
                        record.get("id")?.as_str()?.to_string(),
                        record
                            .get("balance")
                            .and_then(Value::as_f64)
                            .map(|amount| (amount * 100.0).round() as i64)
                            .unwrap_or(0),
                    ))
                })
        };
        let body = self.check(
            self.http.get_json(&lookup, &self.api_token)?,
            "invoice lookup",
        )?;
        if let Some(found) = find(&body) {
            return Ok(found);
        }
        let amount_dollars = request.amount_cents as f64 / 100.0;
        let created = self.http.post_json(
            &self.url("invoices?mark_sent=true"),
            &self.api_token,
            &serde_json::json!({
                "client_id": client_id,
                "date": request.paid_date,
                "number": request.invoice_number,
                "idempotency_key": request.idempotency_key,
                "line_items": [{
                    "product_key": "Receipt",
                    "notes": request.description,
                    "cost": amount_dollars,
                    "quantity": 1,
                    "type_id": "1",
                }],
            }),
        )?;
        // Duplicate invoice number = a previous delivery already created it
        // (we crashed before seeing the response). Re-fetch instead of
        // failing — this is the resume path.
        if created.status == 422 {
            let body = self.check(
                self.http.get_json(&lookup, &self.api_token)?,
                "invoice re-lookup",
            )?;
            return find(&body).ok_or(InvoiceNinjaWriteError::Permanent {
                code: "invoice_ninja_invoice_create_rejected".to_string(),
                message: "422 on create but the invoice number is not taken".to_string(),
            });
        }
        let body = self.check(created, "invoice create")?;
        let id = Self::created_id(&body, "invoice create")?;
        let balance = body
            .get("data")
            .and_then(|data| data.get("balance"))
            .and_then(Value::as_f64)
            .map(|amount| (amount * 100.0).round() as i64)
            .unwrap_or(request.amount_cents);
        Ok((id, balance))
    }

    fn ensure_payment(
        &self,
        request: &InvoiceNinjaReceiptOutboxPayload,
        client_id: &str,
        invoice_id: &str,
        invoice_balance_cents: i64,
    ) -> Result<Option<String>, InvoiceNinjaWriteError> {
        if invoice_balance_cents <= 0 {
            return Ok(None); // already fully applied by a prior delivery
        }
        let encode = crate::qbo_oauth::encode_query_component;
        let lookup = self.url(&format!(
            "payments?transaction_reference={}&per_page=5",
            encode(&request.idempotency_key)
        ));
        let body = self.check(
            self.http.get_json(&lookup, &self.api_token)?,
            "payment lookup",
        )?;
        if let Some(id) = Self::first_id(&body) {
            return Ok(Some(id));
        }
        let amount_dollars = request.amount_cents as f64 / 100.0;
        let created = self.http.post_json(
            &self.url("payments?email_receipt=false"),
            &self.api_token,
            &serde_json::json!({
                "client_id": client_id,
                "amount": amount_dollars,
                "date": request.paid_date,
                "transaction_reference": request.idempotency_key,
                "idempotency_key": request.idempotency_key,
                "private_notes": format!("via BusinessOS ({})", request.approval.approval_id),
                "invoices": [{ "invoice_id": invoice_id, "amount": amount_dollars }],
            }),
        )?;
        let body = self.check(created, "payment create")?;
        Self::created_id(&body, "payment create").map(Some)
    }
}

/// The redelivery-dedupe marker an outbound BOS invoice carries in its
/// private notes. Deterministic per draft so a crashed delivery's retry
/// finds the invoice a prior attempt already created — the invoice NUMBER
/// is Invoice Ninja's to assign (Generated Numbers pattern), not ours.
pub fn bos_draft_marker(draft_ref: &str) -> String {
    format!("[bos:draft {draft_ref}]")
}

/// A non-empty `number` field ("numbered on send" instances return "").
fn invoice_number_field(record: &Value) -> Option<String> {
    record
        .get("number")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|number| !number.is_empty())
        .map(str::to_string)
}

impl<C: InvoiceNinjaWriteHttp> LiveInvoiceNinjaWriteClient<C> {
    /// Lookup-by-marker-then-create for an OUTBOUND invoice DRAFT. The
    /// create sends NO number — Invoice Ninja assigns the next one from its
    /// Generated Numbers pattern, so BOS invoices match the books' existing
    /// format. Dedupe scans the client's invoices for the [bos:draft …]
    /// private-notes marker instead. No `mark_sent`, no email — the invoice
    /// lands in Invoice Ninja's Drafts for human review and sending.
    fn ensure_invoice_draft(
        &self,
        request: &InvoiceNinjaInvoiceDraftOutboxPayload,
        client_id: &str,
    ) -> Result<(String, Option<String>), InvoiceNinjaWriteError> {
        let encode = crate::qbo_oauth::encode_query_component;
        let marker = bos_draft_marker(&request.draft_ref);
        for page in 1.. {
            let lookup = self.url(&format!(
                "invoices?client_id={}&per_page=100&page={page}",
                encode(client_id)
            ));
            let body = self.check(
                self.http.get_json(&lookup, &self.api_token)?,
                "invoice lookup",
            )?;
            let records = body
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let found = records
                .iter()
                .find(|record| {
                    record
                        .get("private_notes")
                        .and_then(Value::as_str)
                        .is_some_and(|notes| notes.contains(&marker))
                })
                .and_then(|record| {
                    Some((
                        record.get("id")?.as_str()?.to_string(),
                        invoice_number_field(record),
                    ))
                });
            if let Some(found) = found {
                return Ok(found);
            }
            if records.len() < 100 {
                break;
            }
        }
        let line_items: Vec<Value> = request
            .line_items
            .iter()
            .map(|line| {
                serde_json::json!({
                    "product_key": line.label,
                    "notes": line.description.as_deref().unwrap_or(""),
                    "cost": line.unit_amount_cents as f64 / 100.0,
                    "quantity": line.quantity,
                    "type_id": "1",
                })
            })
            .collect();
        let mut invoice = serde_json::Map::new();
        invoice.insert(
            "client_id".to_string(),
            Value::String(client_id.to_string()),
        );
        invoice.insert(
            "idempotency_key".to_string(),
            Value::String(request.idempotency_key.clone()),
        );
        invoice.insert("line_items".to_string(), Value::Array(line_items));
        if let Some(due_date) = request.due_date.as_deref() {
            invoice.insert("due_date".to_string(), Value::String(due_date.to_string()));
        }
        if let Some(memo) = request.memo.as_deref() {
            invoice.insert("public_notes".to_string(), Value::String(memo.to_string()));
        }
        invoice.insert(
            "private_notes".to_string(),
            Value::String(format!(
                "{marker} via BusinessOS (approval {})",
                request.approval.approval_id
            )),
        );
        let created = self.http.post_json(
            &self.url("invoices"),
            &self.api_token,
            &Value::Object(invoice),
        )?;
        let body = self.check(created, "invoice create")?;
        let id = Self::created_id(&body, "invoice create")?;
        let number = body.get("data").and_then(invoice_number_field);
        Ok((id, number))
    }
}

impl<C: InvoiceNinjaWriteHttp> InvoiceNinjaExecutionClient for LiveInvoiceNinjaWriteClient<C> {
    fn record_receipt(
        &self,
        request: &InvoiceNinjaReceiptOutboxPayload,
    ) -> Result<InvoiceNinjaReceiptResponse, InvoiceNinjaWriteError> {
        validate_receipt(request)?;
        let client_id = self.ensure_client(&request.payer_name, request.payer_email.as_deref())?;
        let (invoice_id, balance_cents) = self.ensure_invoice(request, &client_id)?;
        let payment_id = self.ensure_payment(request, &client_id, &invoice_id, balance_cents)?;
        Ok(InvoiceNinjaReceiptResponse {
            status: InvoiceNinjaExecutionStatus {
                executed: true,
                dry_run: false,
                reason: None,
            },
            client_id,
            invoice_id,
            payment_id,
        })
    }

    fn create_invoice_draft(
        &self,
        request: &InvoiceNinjaInvoiceDraftOutboxPayload,
    ) -> Result<InvoiceNinjaInvoiceDraftResponse, InvoiceNinjaWriteError> {
        validate_invoice_draft(request)?;
        let client_id =
            self.ensure_client(&request.customer_name, request.customer_email.as_deref())?;
        let (invoice_id, invoice_number) = self.ensure_invoice_draft(request, &client_id)?;
        Ok(InvoiceNinjaInvoiceDraftResponse {
            status: InvoiceNinjaExecutionStatus {
                executed: true,
                dry_run: false,
                reason: None,
            },
            client_id,
            invoice_id,
            invoice_number,
        })
    }
}

/// The gate: write_enabled with full config => live; anything else dry-run.
pub fn invoice_ninja_execution_client(
    config: &InvoiceNinjaWriteConfig,
) -> Box<dyn InvoiceNinjaExecutionClient> {
    match (
        config.write_enabled,
        config.base_url.as_deref(),
        config.api_token.as_deref(),
    ) {
        (true, Some(base_url), Some(api_token)) => Box::new(LiveInvoiceNinjaWriteClient::new(
            Arc::new(ReqwestInvoiceNinjaHttpClient::default()),
            base_url,
            api_token,
        )),
        _ => Box::new(DryRunInvoiceNinjaClient),
    }
}
