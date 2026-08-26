//! Produce + approval + delivery logic for ledger entry drafts (the
//! `ledger_entry` packet kind): record a received payment — typically a
//! Stripe receipt email — into the accounting provider.
//!
//! MONEY IS GROUNDED: the fill is rejected unless the amount carries a
//! literal provenance quote from the source that actually contains the
//! amount. paid_date defaults to the source email's date. Approval enqueues
//! the provider write as an outbox job — Invoice Ninja record_receipt or
//! QBO record_payment (the QBO arm, port #3, links the payment to the
//! snapshot invoice whose open balance equals the amount). Each arm
//! dry-runs until its write gate (BOS_INVOICE_NINJA_WRITE_ENABLED /
//! BOS_QBO_WRITE_ENABLED) opens.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::ledger_drafts::{LedgerDraftStatus, LedgerEntryDraft};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::invoice_ninja::{
    invoice_ninja_execution_client, InvoiceNinjaApprovalMetadata, InvoiceNinjaReceiptOutboxPayload,
    InvoiceNinjaWriteConfig, InvoiceNinjaWriteError,
};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use serde_json::json;

use crate::env_registry;
use crate::outbox::{
    provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob,
};

pub const PACKET_KIND: &str = "ledger_entry";
pub const FILL_SCHEMA_REF: &str = "bos.ledger_drafts.receipt_fill.v1";
pub const FILL_PURPOSE: &str = "ledger_receipt_fill";

pub const PROVIDER_INVOICE_NINJA: &str = "invoice_ninja";
pub const CAPABILITY_RECORD_RECEIPT: &str = "record_receipt";

const PROVENANCE_FIELDS: &[&str] = &["payer_name", "payer_email", "amount_cents", "paid_date"];

/// Strict civil YYYY-MM-DD check (shared with the store's edit validation).
pub fn is_civil_date(raw: &str) -> bool {
    crate::slices::datetime_input::is_civil_date(raw)
}

pub fn build_receipt_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("ledger_fill_{}_{attempt}", item.item_id);
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "email_inbound_message".to_string(),
            entity_id: message.message_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Extract,
            prompt_template_id: "ledger_receipt_fill".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 4 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Extract the RECEIVED PAYMENT this email describes (it is usually a payment-processor receipt, e.g. Stripe). Respond with a single JSON object with EXACTLY these fields: payer_name (the paying customer/company name — NEVER the processor's name), payer_email (the payer's email when stated, else null), amount_cents (integer cents of the amount RECEIVED — from the literal amount in the email, never computed or guessed), paid_date (YYYY-MM-DD when the email states the payment date, else null), description (one factual line: what the payment was for), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span from the email the field came from — the amount_cents quote MUST contain the amount text; empty quote for inferred values).",
                "current_category": item.category_id,
                "email_internal_date_ms": message.internal_date_ms,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "email".to_string(),
                text: format!(
                    "From: {}\nTo: {}\nSubject: {}\n{}\n\n{}",
                    message.from_addr.as_deref().unwrap_or("(unknown)"),
                    message.to_addr.as_deref().unwrap_or("(unknown)"),
                    message.subject.as_deref().unwrap_or("(no subject)"),
                    crate::slices::datetime_input::email_prompt_datetime_context(message),
                    crate::slices::email_triage::service::body_for_ai(message)
                ),
            }],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness, // realigned by the router
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 240_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    }
}

/// A validated receipt fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptFill {
    pub payer_name: String,
    pub payer_email: Option<String>,
    pub amount_cents: i64,
    pub paid_date: Option<String>,
    pub description: String,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

/// Does the quote literally contain the amount? Tolerant of currency
/// formatting: commas/$/spaces are stripped from the quote, then we look for
/// "1500.00" (exact cents) or, for whole-dollar amounts, "1500". Shared with
/// the invoice_drafts vertical (the one money-grounding rule, one place).
pub fn quote_contains_amount(quote: &str, amount_cents: i64) -> bool {
    let normalized: String = quote
        .chars()
        .filter(|c| !matches!(c, ',' | '$' | ' ' | '\u{a0}'))
        .collect();
    let dollars = amount_cents / 100;
    let cents = amount_cents % 100;
    let exact = format!("{dollars}.{cents:02}");
    if normalized.contains(&exact) {
        return true;
    }
    cents == 0 && normalized.contains(&dollars.to_string())
}

pub fn parse_receipt_fill_response(response: &serde_json::Value) -> Result<ReceiptFill, String> {
    parse_receipt_fill_response_with_context(response, None)
}

pub fn parse_receipt_fill_response_with_context(
    response: &serde_json::Value,
    date_context: Option<&crate::slices::datetime_input::DateInputContext>,
) -> Result<ReceiptFill, String> {
    let payer_name = string_field(response, "payer_name").ok_or("payer_name missing or empty")?;
    let payer_email = string_field(response, "payer_email")
        .filter(|raw| raw.contains('@') && !raw.contains(char::is_whitespace));
    let amount_cents = response
        .get("amount_cents")
        .and_then(serde_json::Value::as_i64)
        .filter(|cents| *cents > 0)
        .ok_or("amount_cents missing or non-positive")?;
    let paid_date = string_field(response, "paid_date")
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(&date, date_context)
                .map_err(|_| format!("paid_date is not a supported date: {date}"))
        })
        .transpose()?;
    let description = string_field(response, "description").unwrap_or_default();
    let confidence = string_field(response, "confidence")
        .filter(|raw| matches!(raw.as_str(), "high" | "medium" | "low"))
        .ok_or("confidence missing or invalid")?;
    let provenance: Vec<DraftFieldProvenance> = response
        .get("provenance")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let field = entry.get("field")?.as_str()?.trim().to_string();
                    if !PROVENANCE_FIELDS.contains(&field.as_str()) {
                        return None;
                    }
                    let quote: String = entry
                        .get("quote")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(300)
                        .collect();
                    Some(DraftFieldProvenance { field, quote })
                })
                .collect()
        })
        .unwrap_or_default();
    // MONEY IS GROUNDED: refuse a fill whose amount has no literal quote
    // containing the amount — an invented number must never reach a draft.
    let amount_grounded = provenance.iter().any(|entry| {
        entry.field == "amount_cents"
            && !entry.quote.is_empty()
            && quote_contains_amount(&entry.quote, amount_cents)
    });
    if !amount_grounded {
        return Err("amount_cents has no literal provenance quote containing the amount".into());
    }
    Ok(ReceiptFill {
        payer_name: payer_name.chars().take(200).collect(),
        payer_email,
        amount_cents,
        paid_date,
        description: description.chars().take(500).collect(),
        confidence,
        provenance,
    })
}

/// Assemble the draft. paid_date falls back to the source email's date
/// (grounded), never invented by the model.
pub fn draft_from_fill(
    item: &WorkItem,
    message: &InboundMessageRecord,
    fill: &ReceiptFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> LedgerEntryDraft {
    let grounded_ms = message
        .internal_date_ms
        .unwrap_or(message.ingested_at_ms as i64)
        .max(0) as u64;
    let paid_date = fill
        .paid_date
        .clone()
        .unwrap_or_else(|| crate::slices::accounting::service::today_string(grounded_ms));
    LedgerEntryDraft {
        draft_id: format!("led_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: LedgerDraftStatus::Staged,
        payer_name: fill.payer_name.clone(),
        payer_email: fill.payer_email.clone(),
        amount_cents: fill.amount_cents,
        paid_date,
        description: fill.description.clone(),
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// The ledger kind's plug into the shared produce flow (crate::produce).
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::ledger_drafts::LedgerDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "ledger_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "ledger_draft_already_active"
    }

    fn active_draft(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, crate::store_core::StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|draft| bos_contracts::ledger_drafts::LedgerDraftProduceResponse { draft }),
        )
    }

    fn draft_attempts(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<u64, crate::store_core::StoreError> {
        super::store::count_drafts_for_item(conn, client_id, item_id)
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        _context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        build_receipt_fill_request(client_id, item, message, attempt)
    }

    fn stage(
        &self,
        ctx: crate::produce::StageContext<'_>,
    ) -> Result<(), crate::store_core::StoreError> {
        let crate::produce::StageContext {
            conn,
            client_id,
            actor_id,
            item,
            message,
            response,
            context: _context,
            model,
            attempt,
            idempotency_key,
            now_ms,
        } = ctx;
        use crate::store_core::StoreError;
        let date_context = crate::slices::datetime_input::context_from_email(message);
        let fill = match parse_receipt_fill_response_with_context(response, Some(&date_context)) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "receipt fill unparseable");
                return Err(StoreError::Domain(
                    "ledger_fill_invalid_response".to_string(),
                ));
            }
        };
        let draft = draft_from_fill(item, message, &fill, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }
}

/// Build the provider-write outbox job for an approved draft. The invoice
/// number "BOS-{draft_id}" is the provider-side idempotency anchor.
pub fn build_approval_job(
    draft: &LedgerEntryDraft,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let idempotency_key = format!("ledgerdraft:{}", draft.draft_id);
    let payload = InvoiceNinjaReceiptOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        approval: InvoiceNinjaApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        payer_name: draft.payer_name.clone(),
        payer_email: draft.payer_email.clone(),
        amount_cents: draft.amount_cents,
        paid_date: draft.paid_date.clone(),
        description: if draft.description.is_empty() {
            format!(
                "Received payment (via BusinessOS, draft {})",
                draft.draft_id
            )
        } else {
            draft.description.clone()
        },
        invoice_number: format!("BOS-{}", draft.draft_id),
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: PROVIDER_INVOICE_NINJA.to_string(),
        capability: CAPABILITY_RECORD_RECEIPT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// Invoice Ninja delivery executor for the spine outbox pump. Env read here;
/// the client itself is env-free.
pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_INVOICE_NINJA_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "ledger invoice ninja write gate read failed");
            false
        })
    };
    let config = InvoiceNinjaWriteConfig {
        base_url: env_registry::string(&env_registry::BOS_INVOICE_NINJA_BASE_URL),
        api_token: env_registry::string(&env_registry::BOS_INVOICE_NINJA_API_TOKEN),
        write_enabled,
    };
    execute_job(job, &config, now_ms)
}

pub fn execute_job(
    job: &ClaimedJob,
    config: &InvoiceNinjaWriteConfig,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != PROVIDER_INVOICE_NINJA || job.capability != CAPABILITY_RECORD_RECEIPT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<InvoiceNinjaReceiptOutboxPayload>(&job.payload_json)
    {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("invoice_ninja_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = invoice_ninja_execution_client(config);
    match client.record_receipt(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.invoice_id,
                "provider_status": response.status.reason,
                "client_id": response.client_id,
                "payment_id": response.payment_id,
            })
            .to_string(),
        },
        Err(InvoiceNinjaWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(InvoiceNinjaWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// QBO arm (port #3). Payload + payment-body shapes harvested from agent_monitor's
// QboRecordPaymentWriteClient; the amount-must-match validation lands as a
// deterministic invoice link at approval: the payment amount must equal the
// open balance of exactly one non-voided invoice in the local QBO snapshot
// (narrowed by payer↔customer name when several match). Delivery goes
// through the same outbox; the client dry-runs until BOS_QBO_WRITE_ENABLED.
// ---------------------------------------------------------------------------

use bos_integrations::qbo_payment_write::{
    DryRunQboPaymentClient, LiveQboPaymentWriteClient, QboApprovalMetadata,
    QboPaymentExecutionClient, QboPaymentOutboxPayload, QboWriteError, ReqwestQboWriteHttpClient,
};

pub const PROVIDER_QBO: &str = "qbo";
pub const CAPABILITY_RECORD_PAYMENT: &str = "record_payment";

/// Method tag carried into the provider memo. Drafts come from receipt
/// emails; the actual instrument is not extracted today.
const QBO_PAYMENT_METHOD: &str = "email_receipt";

/// Build the QBO record-payment outbox job for an approved draft, resolving
/// the invoice link by the amount-must-match rule. Errors are domain codes
/// the approval route surfaces verbatim (the operator's fix is to adjust the
/// draft amount or wait for the snapshot sync).
pub fn build_qbo_approval_job(
    conn: &rusqlite::Connection,
    client_id: &str,
    draft: &bos_contracts::ledger_drafts::LedgerEntryDraft,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let candidates = crate::slices::accounting::store::open_invoices_by_balance(
        conn,
        client_id,
        draft.amount_cents,
    )
    .map_err(|err| format!("qbo_payment_snapshot_read_failed:{err}"))?;
    if candidates.is_empty() {
        return Err("qbo_payment_no_invoice_with_matching_balance".to_string());
    }
    let matched = if candidates.len() == 1 {
        &candidates[0]
    } else {
        let payer = draft.payer_name.to_ascii_lowercase();
        let narrowed: Vec<_> = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .customer_name
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .is_some_and(|name| name.contains(&payer) || payer.contains(&name))
            })
            .collect();
        match narrowed.as_slice() {
            [single] => *single,
            _ => return Err("qbo_payment_invoice_match_ambiguous".to_string()),
        }
    };
    let Some(customer_id) = matched
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Err("qbo_payment_invoice_missing_customer".to_string());
    };
    if matched.customer_active == Some(false) {
        return Err("qbo_payment_customer_inactive".to_string());
    }
    let idempotency_key = format!("ledgerdraft:{}", draft.draft_id);
    let payload = QboPaymentOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        approval: QboApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        provider_invoice_id: matched.invoice_id.clone(),
        provider_customer_id: customer_id.to_string(),
        amount_cents: draft.amount_cents,
        paid_date: draft.paid_date.clone(),
        payment_method: QBO_PAYMENT_METHOD.to_string(),
        memo: if draft.description.is_empty() {
            format!(
                "Received payment from {} (via BusinessOS, draft {})",
                draft.payer_name, draft.draft_id
            )
        } else {
            draft.description.clone()
        },
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: PROVIDER_QBO.to_string(),
        capability: CAPABILITY_RECORD_PAYMENT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// QBO delivery executor for the spine outbox pump. Gate + credential
/// resolution happen here; the clients themselves are env-free. The live
/// path reuses the accounting slice's stored OAuth credential (refreshing —
/// and persisting — the rotated grant exactly like the sync pump does).
pub fn deliver_qbo(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    if job.capability != CAPABILITY_RECORD_PAYMENT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_QBO_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "qbo write gate read failed");
            false
        })
    };
    if !write_enabled {
        return execute_qbo_job(job, &DryRunQboPaymentClient, now_ms);
    }
    let Some(app) = crate::slices::accounting::service::oauth_app_from_env() else {
        return AttemptOutcome::Retry {
            error: "qbo_oauth_app_unconfigured".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        };
    };
    let api_base_url = app.environment.api_base_url().to_string();
    let refresher = bos_integrations::qbo_oauth::LiveQboTokenRefresher { app };
    let mut budget = 2u32;
    match crate::slices::accounting::worker::prepare_qbo_credentials(
        state,
        &refresher,
        &mut budget,
        now_ms,
    ) {
        Ok(Some((credential, access_token, _requests))) => {
            let client = LiveQboPaymentWriteClient::new(
                std::sync::Arc::new(ReqwestQboWriteHttpClient::default()),
                api_base_url,
                credential.realm_id,
                access_token,
            );
            execute_qbo_job(job, &client, now_ms)
        }
        Ok(None) => AttemptOutcome::Retry {
            error: "qbo_not_connected".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(err) => AttemptOutcome::Retry {
            error: format!("qbo_credential_resolution_failed:{err}"),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
    }
}

pub fn execute_qbo_job(
    job: &ClaimedJob,
    client: &dyn QboPaymentExecutionClient,
    now_ms: u64,
) -> AttemptOutcome {
    let payload = match serde_json::from_str::<QboPaymentOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("qbo_payment_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    match client.record_payment(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.payment_id,
                "provider_status": response.status.reason,
                "linked_invoice_id": payload.provider_invoice_id,
            })
            .to_string(),
        },
        Err(QboWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(QboWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}
