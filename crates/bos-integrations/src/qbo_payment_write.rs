//! QBO record-payment write client (the ledger vertical's QBO arm).
//! Payload shape, the QBO Payment body (TotalAmt + TxnDate + Line linked to
//! the invoice), and the grounded-payload validation are ported from
//! agent-monitor-rust's qbo_invoice_draft.rs (QboRecordPaymentWriteClient).
//! Same write discipline as the Invoice Ninja arm: the dry-run client
//! validates and plans the exact provider body without touching the network;
//! the live client only exists behind the BOS_QBO_WRITE_ENABLED gate.
//!
//! Only the record-payment half is ported — invoice creation stays un-ported
//! until quoting (OQ-8) unblocks.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QboWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

fn permanent(code: &str, message: impl Into<String>) -> QboWriteError {
    QboWriteError::Permanent {
        code: code.to_string(),
        message: message.into(),
    }
}

fn retryable(code: &str, message: impl Into<String>) -> QboWriteError {
    QboWriteError::Retryable {
        code: code.to_string(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QboApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl QboApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "qbo", capability = "record_payment"`.
/// The invoice link is resolved at approval time against the local QBO
/// invoice snapshot (amount must match the invoice's open balance), so a
/// payload always names the exact invoice the payment applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QboPaymentOutboxPayload {
    pub idempotency_key: String,
    pub approval: QboApprovalMetadata,
    /// QBO Invoice id the payment applies to (LinkedTxn).
    pub provider_invoice_id: String,
    /// QBO Customer id (CustomerRef) — required by the Payment entity.
    pub provider_customer_id: String,
    pub amount_cents: i64,
    /// YYYY-MM-DD → TxnDate.
    pub paid_date: String,
    /// Free-text method ("stripe", "check", …) — folded into PrivateNote;
    /// QBO's PaymentMethodRef needs an entity id we don't sync.
    pub payment_method: String,
    /// Audit memo (PrivateNote). Carries the draft id — QBO has no
    /// idempotency keys, so the memo is the manual-dedupe anchor.
    pub memo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QboWriteStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QboPaymentResponse {
    pub status: QboWriteStatus,
    /// Provider Payment id ("dry-run" sentinel when not executed).
    pub payment_id: String,
}

pub trait QboPaymentExecutionClient: Send + Sync {
    fn record_payment(
        &self,
        payload: &QboPaymentOutboxPayload,
    ) -> Result<QboPaymentResponse, QboWriteError>;
}

/// Grounded-payload validation ported from agent_monitor's
/// validate_qbo_record_payment_payload: every reference the provider write
/// needs must be present and the amount positive. The body builder below
/// additionally keeps the agent_monitor invariant that the payment's line amounts
/// equal TotalAmt (single line linked to the invoice, so it holds by
/// construction — asserted here anyway so a future multi-line change cannot
/// silently break it).
pub fn validate_payment_payload(payload: &QboPaymentOutboxPayload) -> Result<(), QboWriteError> {
    if !payload.approval.is_complete() {
        return Err(permanent(
            "qbo_payment_approval_missing",
            "approval metadata is incomplete",
        ));
    }
    if payload.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "qbo_payment_idempotency_key_missing",
            "idempotency key is required",
        ));
    }
    if payload.provider_invoice_id.trim().is_empty()
        || payload.provider_customer_id.trim().is_empty()
        || payload.paid_at_invalid()
        || payload.payment_method.trim().is_empty()
    {
        return Err(permanent(
            "qbo_payment_payload_not_grounded",
            "QBO record payment payload requires invoice id, customer id, paid date, and payment method",
        ));
    }
    if payload.amount_cents <= 0 {
        return Err(permanent(
            "qbo_payment_amount_invalid",
            "amount must be positive",
        ));
    }
    let body = qbo_payment_body(payload);
    let total = body["TotalAmt"].as_str().unwrap_or_default();
    let line_sum = body["Line"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .map(|line| line["Amount"].as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("+")
        })
        .unwrap_or_default();
    if total != line_sum {
        return Err(permanent(
            "qbo_payment_amount_mismatch",
            "payment line amounts do not match the payment total",
        ));
    }
    Ok(())
}

impl QboPaymentOutboxPayload {
    fn paid_at_invalid(&self) -> bool {
        let date = self.paid_date.as_bytes();
        date.len() != 10 || date[4] != b'-' || date[7] != b'-'
    }
}

/// The QBO Payment entity body, ported from agent_monitor's dry-run plan: total +
/// date + a single line applying the amount to the linked invoice. Method
/// and draft provenance go into PrivateNote (max 4000 chars in QBO).
pub fn qbo_payment_body(payload: &QboPaymentOutboxPayload) -> Value {
    let note: String = format!(
        "{} [method: {}; {}]",
        payload.memo, payload.payment_method, payload.idempotency_key
    )
    .chars()
    .take(4_000)
    .collect();
    serde_json::json!({
        "TotalAmt": cents_to_decimal_string(payload.amount_cents),
        "TxnDate": payload.paid_date,
        "CustomerRef": { "value": payload.provider_customer_id },
        "PrivateNote": note,
        "Line": [{
            "Amount": cents_to_decimal_string(payload.amount_cents),
            "LinkedTxn": [{
                "TxnId": payload.provider_invoice_id,
                "TxnType": "Invoice"
            }]
        }]
    })
}

fn cents_to_decimal_string(cents: i64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// Validates and plans the exact provider body; never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunQboPaymentClient;

impl QboPaymentExecutionClient for DryRunQboPaymentClient {
    fn record_payment(
        &self,
        payload: &QboPaymentOutboxPayload,
    ) -> Result<QboPaymentResponse, QboWriteError> {
        validate_payment_payload(payload)?;
        Ok(QboPaymentResponse {
            status: QboWriteStatus {
                executed: false,
                dry_run: true,
                reason: Some("qbo_write_disabled_dry_run".to_string()),
            },
            payment_id: "dry-run".to_string(),
        })
    }
}

/// Write transport seam: GET for write-time reference preflight, POST for
/// the approved Payment create.
pub trait QboWriteHttp: Send + Sync {
    fn get_json(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<QboWriteHttpResponse, QboWriteError>;

    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<QboWriteHttpResponse, QboWriteError>;
}

#[derive(Debug, Clone)]
pub struct QboWriteHttpResponse {
    pub status: u16,
    pub body: Value,
}

pub struct ReqwestQboWriteHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestQboWriteHttpClient {
    fn default() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl QboWriteHttp for ReqwestQboWriteHttpClient {
    fn get_json(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<QboWriteHttpResponse, QboWriteError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .send()
            .map_err(|err| retryable("qbo_request_failed", err.to_string()))?;
        decode_qbo_http_response(response)
    }

    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<QboWriteHttpResponse, QboWriteError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .map_err(|err| retryable("qbo_request_failed", err.to_string()))?;
        decode_qbo_http_response(response)
    }
}

fn decode_qbo_http_response(
    response: reqwest::blocking::Response,
) -> Result<QboWriteHttpResponse, QboWriteError> {
    let status = response.status().as_u16();
    let text = response
        .text()
        .map_err(|err| retryable("qbo_request_failed", err.to_string()))?;
    let body = serde_json::from_str::<Value>(&text).map_err(|err| {
        retryable(
            "qbo_response_decode_failed",
            format!("failed to decode QBO response JSON for status {status}: {err}"),
        )
    })?;
    Ok(QboWriteHttpResponse { status, body })
}

/// Live client: POST /v3/company/{realm}/payment with the approved body.
/// QBO has no idempotency keys — a replayed timed-out create could
/// double-post (PrivateNote carries the draft id for manual dedupe; same
/// documented caveat as HubSpot note-create).
pub struct LiveQboPaymentWriteClient<C: QboWriteHttp = ReqwestQboWriteHttpClient> {
    http: Arc<C>,
    api_base_url: String,
    realm_id: String,
    access_token: String,
}

impl<C: QboWriteHttp> LiveQboPaymentWriteClient<C> {
    pub fn new(
        http: Arc<C>,
        api_base_url: impl Into<String>,
        realm_id: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            http,
            api_base_url: api_base_url.into(),
            realm_id: realm_id.into(),
            access_token: access_token.into(),
        }
    }

    fn query_one(&self, entity: &str, id: &str) -> Result<Option<Value>, QboWriteError> {
        let safe_id = qbo_id_literal(id)?;
        let query = if entity == "Customer" {
            format!("SELECT * FROM Customer WHERE Id = '{safe_id}' AND Active IN (true,false)")
        } else {
            format!("SELECT * FROM {entity} WHERE Id = '{safe_id}'")
        };
        let url = crate::qbo_read::query_url(&self.api_base_url, &self.realm_id, &query);
        let response = self.http.get_json(&url, &self.access_token)?;
        match response.status {
            200..=299 => Ok(crate::qbo_read::entity_array(&response.body, entity)
                .into_iter()
                .next()
                .cloned()),
            401 | 403 => Err(QboWriteError::Retryable {
                code: "qbo_payment_unauthorized".to_string(),
                message: "access token rejected during QBO payment preflight".to_string(),
            }),
            429 => Err(QboWriteError::Retryable {
                code: "qbo_payment_throttled".to_string(),
                message: "provider throttled the QBO payment preflight".to_string(),
            }),
            status if status >= 500 => Err(QboWriteError::Retryable {
                code: "qbo_payment_server_error".to_string(),
                message: format!("provider returned {status} during QBO payment preflight"),
            }),
            status => Err(permanent(
                "qbo_payment_preflight_rejected",
                format!(
                    "provider returned {status} during QBO payment preflight: {}",
                    response.body
                ),
            )),
        }
    }

    fn preflight_payment_refs(
        &self,
        payload: &QboPaymentOutboxPayload,
    ) -> Result<(), QboWriteError> {
        let customer = self
            .query_one("Customer", &payload.provider_customer_id)?
            .and_then(|value| crate::qbo_read::customer_record_from_value(&value))
            .ok_or_else(|| {
                permanent(
                    "qbo_payment_customer_missing",
                    format!(
                        "QBO customer {} was not found at write time; sync accounting and re-approve the payment",
                        payload.provider_customer_id
                    ),
                )
            })?;
        if !customer.active {
            return Err(permanent(
                "qbo_payment_customer_inactive",
                format!(
                    "QBO customer {} is inactive; reactivate it in QBO or choose another invoice before approving this payment",
                    payload.provider_customer_id
                ),
            ));
        }

        let invoice = self
            .query_one("Invoice", &payload.provider_invoice_id)?
            .and_then(|value| crate::qbo_read::invoice_record_from_value(&value))
            .ok_or_else(|| {
                permanent(
                    "qbo_payment_invoice_missing",
                    format!(
                        "QBO invoice {} was not found at write time; it may have been voided or deleted",
                        payload.provider_invoice_id
                    ),
                )
            })?;
        if invoice.voided {
            return Err(permanent(
                "qbo_payment_invoice_voided",
                format!(
                    "QBO invoice {} is voided and cannot receive a payment",
                    payload.provider_invoice_id
                ),
            ));
        }
        if invoice.balance_cents <= 0 {
            return Err(permanent(
                "qbo_payment_invoice_not_payable",
                format!(
                    "QBO invoice {} has no open balance at write time",
                    payload.provider_invoice_id
                ),
            ));
        }
        if invoice.balance_cents != payload.amount_cents {
            return Err(permanent(
                "qbo_payment_invoice_balance_changed",
                format!(
                    "QBO invoice {} open balance changed before write; sync accounting and re-approve the payment",
                    payload.provider_invoice_id
                ),
            ));
        }
        if invoice.customer_id.as_deref() != Some(payload.provider_customer_id.as_str()) {
            return Err(permanent(
                "qbo_payment_invoice_customer_changed",
                format!(
                    "QBO invoice {} no longer belongs to customer {}",
                    payload.provider_invoice_id, payload.provider_customer_id
                ),
            ));
        }
        Ok(())
    }
}

fn qbo_id_literal(id: &str) -> Result<String, QboWriteError> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.contains('\'') || trimmed.contains('\\') {
        return Err(permanent(
            "qbo_payment_payload_not_grounded",
            "QBO provider ids must be non-empty simple literals",
        ));
    }
    Ok(trimmed.chars().take(100).collect())
}

impl<C: QboWriteHttp> QboPaymentExecutionClient for LiveQboPaymentWriteClient<C> {
    fn record_payment(
        &self,
        payload: &QboPaymentOutboxPayload,
    ) -> Result<QboPaymentResponse, QboWriteError> {
        validate_payment_payload(payload)?;
        self.preflight_payment_refs(payload)?;
        let url = format!(
            "{}/v3/company/{}/payment?minorversion={}",
            self.api_base_url.trim_end_matches('/'),
            self.realm_id,
            crate::qbo_common::QBO_MINOR_VERSION
        );
        let body = qbo_payment_body(payload);
        let response = self.http.post_json(&url, &self.access_token, &body)?;
        match response.status {
            200 | 201 => {
                let payment_id = response.body["Payment"]["Id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if payment_id.is_empty() {
                    return Err(permanent(
                        "qbo_payment_response_missing_id",
                        "provider response carried no Payment.Id",
                    ));
                }
                Ok(QboPaymentResponse {
                    status: QboWriteStatus {
                        executed: true,
                        dry_run: false,
                        reason: None,
                    },
                    payment_id,
                })
            }
            401 | 403 => Err(QboWriteError::Retryable {
                code: "qbo_payment_unauthorized".to_string(),
                message: "access token rejected; will re-resolve credentials".to_string(),
            }),
            429 => Err(QboWriteError::Retryable {
                code: "qbo_payment_throttled".to_string(),
                message: "provider throttled the request".to_string(),
            }),
            status if status >= 500 => Err(QboWriteError::Retryable {
                code: "qbo_payment_server_error".to_string(),
                message: format!("provider returned {status}"),
            }),
            status => Err(permanent(
                "qbo_payment_rejected",
                format!("provider returned {status}: {}", response.body),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn payload() -> QboPaymentOutboxPayload {
        QboPaymentOutboxPayload {
            idempotency_key: "ledgerdraft:led_1".to_string(),
            approval: QboApprovalMetadata {
                approval_id: "appr_led_1".to_string(),
                approved_by: "op_test".to_string(),
                approved_at: "2026-06-10T12:00:00Z".to_string(),
            },
            provider_invoice_id: "146".to_string(),
            provider_customer_id: "58".to_string(),
            amount_cents: 150_000,
            paid_date: "2026-06-09".to_string(),
            payment_method: "stripe".to_string(),
            memo: "Received payment (BusinessOS draft led_1)".to_string(),
        }
    }

    #[test]
    fn dry_run_validates_and_never_executes() {
        let response = DryRunQboPaymentClient
            .record_payment(&payload())
            .expect("dry run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);
        assert_eq!(response.payment_id, "dry-run");
    }

    #[test]
    fn payment_body_links_invoice_and_matches_amounts() {
        let body = qbo_payment_body(&payload());
        assert_eq!(body["TotalAmt"], "1500.00");
        assert_eq!(body["TxnDate"], "2026-06-09");
        assert_eq!(body["CustomerRef"]["value"], "58");
        assert_eq!(body["Line"][0]["Amount"], "1500.00");
        assert_eq!(body["Line"][0]["LinkedTxn"][0]["TxnId"], "146");
        assert_eq!(body["Line"][0]["LinkedTxn"][0]["TxnType"], "Invoice");
        assert!(body["PrivateNote"]
            .as_str()
            .unwrap()
            .contains("ledgerdraft:led_1"));
    }

    #[test]
    fn ungrounded_payloads_are_permanent_errors() {
        let mut missing_invoice = payload();
        missing_invoice.provider_invoice_id = " ".to_string();
        let err = DryRunQboPaymentClient
            .record_payment(&missing_invoice)
            .expect_err("missing invoice");
        assert!(
            matches!(err, QboWriteError::Permanent { code, .. } if code == "qbo_payment_payload_not_grounded")
        );

        let mut zero_amount = payload();
        zero_amount.amount_cents = 0;
        let err = DryRunQboPaymentClient
            .record_payment(&zero_amount)
            .expect_err("zero amount");
        assert!(
            matches!(err, QboWriteError::Permanent { code, .. } if code == "qbo_payment_amount_invalid")
        );

        let mut no_approval = payload();
        no_approval.approval.approved_by = String::new();
        let err = DryRunQboPaymentClient
            .record_payment(&no_approval)
            .expect_err("approval missing");
        assert!(
            matches!(err, QboWriteError::Permanent { code, .. } if code == "qbo_payment_approval_missing")
        );
    }

    struct ScriptedHttp {
        get_responses: Mutex<VecDeque<Result<QboWriteHttpResponse, QboWriteError>>>,
        post_status: u16,
        post_body: Value,
        post_result: Mutex<Option<Result<QboWriteHttpResponse, QboWriteError>>>,
        post_calls: Mutex<usize>,
    }

    impl ScriptedHttp {
        fn new(
            get_responses: Vec<QboWriteHttpResponse>,
            post_status: u16,
            post_body: Value,
        ) -> Self {
            Self {
                get_responses: Mutex::new(get_responses.into_iter().map(Ok).collect()),
                post_status,
                post_body,
                post_result: Mutex::new(None),
                post_calls: Mutex::new(0),
            }
        }
    }

    impl QboWriteHttp for ScriptedHttp {
        fn get_json(
            &self,
            url: &str,
            _access_token: &str,
        ) -> Result<QboWriteHttpResponse, QboWriteError> {
            assert!(url.starts_with("https://quickbooks.api.intuit.com/v3/company/realm-9/query?"));
            assert!(url.contains("minorversion=75"));
            self.get_responses
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted get response")
        }

        fn post_json(
            &self,
            url: &str,
            _access_token: &str,
            body: &Value,
        ) -> Result<QboWriteHttpResponse, QboWriteError> {
            assert!(url.ends_with("/v3/company/realm-9/payment?minorversion=75"));
            assert_eq!(body["Line"][0]["LinkedTxn"][0]["TxnId"], "146");
            *self.post_calls.lock().expect("lock") += 1;
            if let Some(result) = self.post_result.lock().expect("lock").take() {
                return result;
            }
            Ok(QboWriteHttpResponse {
                status: self.post_status,
                body: self.post_body.clone(),
            })
        }
    }

    fn active_customer_response() -> QboWriteHttpResponse {
        QboWriteHttpResponse {
            status: 200,
            body: serde_json::json!({"QueryResponse": {"Customer": [{
                "Id": "58",
                "DisplayName": "Acme LLC",
                "Active": true
            }]}}),
        }
    }

    fn payable_invoice_response() -> QboWriteHttpResponse {
        QboWriteHttpResponse {
            status: 200,
            body: serde_json::json!({"QueryResponse": {"Invoice": [{
                "Id": "146",
                "CustomerRef": { "value": "58", "name": "Acme LLC" },
                "TotalAmt": 1500.00,
                "Balance": 1500.00
            }]}}),
        }
    }

    fn decode_error() -> QboWriteError {
        QboWriteError::Retryable {
            code: "qbo_response_decode_failed".to_string(),
            message: "failed to decode QBO response JSON for status 200".to_string(),
        }
    }

    #[test]
    fn live_client_parses_payment_id_and_classifies_failures() {
        let ok = LiveQboPaymentWriteClient::new(
            Arc::new(ScriptedHttp::new(
                vec![active_customer_response(), payable_invoice_response()],
                200,
                serde_json::json!({"Payment": {"Id": "987"}}),
            )),
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let response = ok.record_payment(&payload()).expect("live ok");
        assert!(response.status.executed);
        assert_eq!(response.payment_id, "987");

        let throttled = LiveQboPaymentWriteClient::new(
            Arc::new(ScriptedHttp::new(
                vec![active_customer_response(), payable_invoice_response()],
                429,
                Value::Null,
            )),
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let err = throttled.record_payment(&payload()).expect_err("throttle");
        assert!(
            matches!(err, QboWriteError::Retryable { code, .. } if code == "qbo_payment_throttled")
        );

        let rejected = LiveQboPaymentWriteClient::new(
            Arc::new(ScriptedHttp::new(
                vec![active_customer_response(), payable_invoice_response()],
                400,
                serde_json::json!({"Fault": {"type": "ValidationFault"}}),
            )),
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let err = rejected.record_payment(&payload()).expect_err("reject");
        assert!(
            matches!(err, QboWriteError::Permanent { code, .. } if code == "qbo_payment_rejected")
        );
    }

    #[test]
    fn live_client_treats_malformed_preflight_json_as_retryable() {
        let http = Arc::new(ScriptedHttp::new(
            vec![],
            200,
            serde_json::json!({
                "Payment": {"Id": "987"}
            }),
        ));
        *http.get_responses.lock().expect("lock") = VecDeque::from([Err(decode_error())]);
        let client = LiveQboPaymentWriteClient::new(
            http.clone(),
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let err = client
            .record_payment(&payload())
            .expect_err("malformed preflight response");
        assert!(
            matches!(&err, QboWriteError::Retryable { code, .. } if code == "qbo_response_decode_failed"),
            "got {err:?}"
        );
        assert_eq!(*http.post_calls.lock().expect("lock"), 0);
    }

    #[test]
    fn live_client_treats_malformed_payment_post_json_as_retryable() {
        let http = Arc::new(ScriptedHttp::new(
            vec![active_customer_response(), payable_invoice_response()],
            200,
            Value::Null,
        ));
        *http.post_result.lock().expect("lock") = Some(Err(decode_error()));
        let client = LiveQboPaymentWriteClient::new(
            http,
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let err = client
            .record_payment(&payload())
            .expect_err("malformed payment response");
        assert!(
            matches!(&err, QboWriteError::Retryable { code, .. } if code == "qbo_response_decode_failed"),
            "got {err:?}"
        );
    }

    #[test]
    fn live_client_refuses_inactive_customer_before_posting() {
        let http = Arc::new(ScriptedHttp::new(
            vec![QboWriteHttpResponse {
                status: 200,
                body: serde_json::json!({"QueryResponse": {"Customer": [{
                    "Id": "58",
                    "DisplayName": "Acme LLC",
                    "Active": false
                }]}}),
            }],
            200,
            serde_json::json!({"Payment": {"Id": "987"}}),
        ));
        let client = LiveQboPaymentWriteClient::new(
            http.clone(),
            "https://quickbooks.api.intuit.com",
            "realm-9",
            "token",
        );
        let err = client
            .record_payment(&payload())
            .expect_err("inactive customer");
        assert!(
            matches!(err, QboWriteError::Permanent { code, .. } if code == "qbo_payment_customer_inactive")
        );
        assert_eq!(*http.post_calls.lock().expect("lock"), 0);
    }

    #[test]
    fn live_client_refuses_missing_voided_or_changed_invoice_before_posting() {
        for (invoice_response, expected_code) in [
            (
                QboWriteHttpResponse {
                    status: 200,
                    body: serde_json::json!({"QueryResponse": {}}),
                },
                "qbo_payment_invoice_missing",
            ),
            (
                QboWriteHttpResponse {
                    status: 200,
                    body: serde_json::json!({"QueryResponse": {"Invoice": [{
                        "Id": "146",
                        "CustomerRef": { "value": "58" },
                        "TotalAmt": 0,
                        "Balance": 0,
                        "PrivateNote": "Voided"
                    }]}}),
                },
                "qbo_payment_invoice_voided",
            ),
            (
                QboWriteHttpResponse {
                    status: 200,
                    body: serde_json::json!({"QueryResponse": {"Invoice": [{
                        "Id": "146",
                        "CustomerRef": { "value": "58" },
                        "TotalAmt": 1500.00,
                        "Balance": 1200.00
                    }]}}),
                },
                "qbo_payment_invoice_balance_changed",
            ),
        ] {
            let http = Arc::new(ScriptedHttp::new(
                vec![active_customer_response(), invoice_response],
                200,
                serde_json::json!({"Payment": {"Id": "987"}}),
            ));
            let client = LiveQboPaymentWriteClient::new(
                http.clone(),
                "https://quickbooks.api.intuit.com",
                "realm-9",
                "token",
            );
            let err = client
                .record_payment(&payload())
                .expect_err("stale invoice");
            assert!(
                matches!(&err, QboWriteError::Permanent { code, .. } if code == expected_code),
                "expected {expected_code}, got {err:?}"
            );
            assert_eq!(*http.post_calls.lock().expect("lock"), 0);
        }
    }
}
