//! Produce + approval + delivery logic for invoice drafts (the
//! `invoice_draft` packet kind): a note/email describing billable work
//! becomes a provider invoice draft.
//!
//! MONEY IS GROUNDED: every line item's unit amount must carry a literal
//! provenance quote (field "line_{n}_amount") that contains the amount —
//! one ungrounded line refuses the whole fill (the ledger doctrine). Line
//! totals and the subtotal are recomputed deterministically from quantity ×
//! unit amount; the model's arithmetic is never trusted. Approval enqueues
//! the create-invoice-draft write for BOS_ACCOUNTING_PROVIDER's arm —
//! Invoice Ninja (the chosen production path, gated by
//! BOS_INVOICE_NINJA_WRITE_ENABLED) or Stripe (gated by
//! BOS_STRIPE_WRITE_ENABLED). Either way the invoice stays a provider
//! DRAFT — reviewing and sending it is a human action in the provider's UI.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentPlan,
    EnrichmentRunStatus, EnrichmentSeedEvidence, EnrichmentTier, EnrichmentTierEvent,
};
use bos_contracts::invoice_drafts::{InvoiceDraft, InvoiceDraftLineItem, InvoiceDraftStatus};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::invoice_ninja::{
    invoice_ninja_execution_client, InvoiceNinjaApprovalMetadata,
    InvoiceNinjaInvoiceDraftOutboxPayload, InvoiceNinjaInvoiceLineItem, InvoiceNinjaWriteConfig,
    InvoiceNinjaWriteError,
};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use bos_integrations::stripe::{
    stripe_execution_client, StripeApprovalMetadata, StripeInvoiceDraftOutboxPayload,
    StripeInvoiceLineItem, StripeWriteConfig, StripeWriteError,
};
use serde_json::json;

use crate::env_registry;
use crate::outbox::{
    provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob,
};
use crate::slices::async_kickoff::{
    KickoffCapacity, KickoffDecision, KickoffSpec, RecordedKickoff,
};
use crate::slices::enrichment::service as enrichment_engine;
use crate::store_core::{MutationOutcome, StoreError};

pub const PACKET_KIND: &str = "invoice_draft";
pub const FILL_SCHEMA_REF: &str = "bos.invoice_drafts.invoice_fill.v1";
pub const FILL_PURPOSE: &str = "invoice_fill";
pub const CUSTOMER_ENRICH_SCHEMA_REF: &str = "bos.invoice_drafts.customer_enrichment.v1";
pub const CUSTOMER_ENRICH_PURPOSE: &str = "invoice_customer_enrichment";

pub const PROVIDER_STRIPE: &str = "stripe";
pub const CAPABILITY_CREATE_INVOICE_DRAFT: &str = "create_invoice_draft";
/// System actor stamped on the invoice customer-enrichment graft.
pub const CUSTOMER_ENRICHMENT_ACTOR: &str = "invoice_customer_enrichment";
const SEARCH_ENRICH_REASON_WEAK_CUSTOMER_NAME: &str = "weak_domain_customer_name";
const ENRICH_MAX_TEXT_CHARS: usize = 8_000;

/// Cap on line items per draft (a fill beyond this is noise, not an invoice).
const MAX_LINE_ITEMS: usize = 20;

pub fn build_invoice_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("invoice_fill_{}_{attempt}", item.item_id);
    let mut request = TypedLlmTaskRequest {
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
            prompt_template_id: "invoice_fill".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 8 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Extract the BILLABLE WORK this source describes so an invoice can be drafted. Respond with a single JSON object with EXACTLY these fields: customer_name (who gets billed — the client/company name, NEVER the sender's payment processor), customer_email (the billing email when stated, else null), line_items (array of {line_number (1-based), label (short billable line), description (one factual sentence or null), quantity (integer, 1 when not stated), unit_amount_cents (integer cents from the LITERAL amount in the source — never computed, split, or guessed)}), due_date (YYYY-MM-DD ONLY when the source states a due date, else null), memo (one factual line for the invoice memo, else empty string), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span the field came from — EVERY line item needs an entry with field \"line_{line_number}_amount\" whose quote contains the amount text; omit a line entirely rather than inventing an amount).",
                "current_category": item.category_id,
                "source_kind": item.source_kind,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "source".to_string(),
                text: format!(
                    "From: {}\nSubject: {}\n{}\n\n{}",
                    message.from_addr.as_deref().unwrap_or("(unknown)"),
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
    };
    // Optional company-background grounding (tone/context only).
    if let Some(block) = context
        .get("background")
        .and_then(|v| serde_json::from_value::<TypedLlmTextBlock>(v.clone()).ok())
    {
        request.input.text_blocks.push(block);
    }
    if let Some(text) = context
        .get("grounding_text")
        .and_then(serde_json::Value::as_str)
    {
        if !text.trim().is_empty() {
            request.input.text_blocks.push(TypedLlmTextBlock {
                block_id: "grounding".to_string(),
                text: text.to_string(),
            });
        }
    }
    request
}

/// A validated invoice fill (line totals already recomputed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceFill {
    pub customer_name: String,
    pub customer_email: Option<String>,
    pub line_items: Vec<InvoiceDraftLineItem>,
    pub due_date: Option<String>,
    pub memo: String,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

/// Recompute line totals + subtotal from quantity × unit amount. The model's
/// arithmetic is never trusted; this is also the edit-path validation.
pub fn recompute_totals(line_items: &mut [InvoiceDraftLineItem]) -> Result<i64, String> {
    let mut subtotal: i64 = 0;
    for item in line_items.iter_mut() {
        if item.label.trim().is_empty() {
            return Err("line item label is empty".to_string());
        }
        if item.quantity == 0 || item.quantity > 10_000 {
            return Err("line item quantity out of range".to_string());
        }
        if item.unit_amount_cents <= 0 {
            return Err("line item unit amount must be positive".to_string());
        }
        item.line_total_cents = i64::from(item.quantity)
            .checked_mul(item.unit_amount_cents)
            .ok_or("line total overflowed")?;
        subtotal = subtotal
            .checked_add(item.line_total_cents)
            .ok_or("subtotal overflowed")?;
    }
    Ok(subtotal)
}

pub fn parse_invoice_fill_response(response: &serde_json::Value) -> Result<InvoiceFill, String> {
    parse_invoice_fill_response_with_context(response, None)
}

pub fn parse_invoice_fill_response_with_context(
    response: &serde_json::Value,
    date_context: Option<&crate::slices::datetime_input::DateInputContext>,
) -> Result<InvoiceFill, String> {
    let customer_name =
        string_field(response, "customer_name").ok_or("customer_name missing or empty")?;
    let customer_email = string_field(response, "customer_email")
        .filter(|raw| enrichment_engine::valid_email_shape(raw));
    let due_date = string_field(response, "due_date")
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(&date, date_context)
                .map_err(|_| format!("due_date is not a supported date: {date}"))
        })
        .transpose()?;
    let memo = string_field(response, "memo").unwrap_or_default();
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
                    let valid = matches!(
                        field.as_str(),
                        "customer_name" | "customer_email" | "due_date" | "memo"
                    ) || (field.starts_with("line_") && field.ends_with("_amount"));
                    if !valid {
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

    let raw_lines = response
        .get("line_items")
        .and_then(serde_json::Value::as_array)
        .ok_or("line_items missing")?;
    if raw_lines.is_empty() {
        return Err("line_items is empty — nothing billable was found".to_string());
    }
    if raw_lines.len() > MAX_LINE_ITEMS {
        return Err(format!("too many line items ({})", raw_lines.len()));
    }
    let mut line_items = Vec::with_capacity(raw_lines.len());
    for (index, raw) in raw_lines.iter().enumerate() {
        let line_number = raw
            .get("line_number")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32)
            .filter(|n| *n > 0)
            .unwrap_or(index as u32 + 1);
        let label: String = string_field(raw, "label")
            .ok_or(format!("line {line_number}: label missing"))?
            .chars()
            .take(200)
            .collect();
        let description =
            string_field(raw, "description").map(|raw| raw.chars().take(500).collect::<String>());
        let quantity = raw
            .get("quantity")
            .and_then(serde_json::Value::as_u64)
            .map(|q| q as u32)
            .unwrap_or(1);
        let unit_amount_cents = raw
            .get("unit_amount_cents")
            .and_then(serde_json::Value::as_i64)
            .filter(|cents| *cents > 0)
            .ok_or(format!("line {line_number}: unit_amount_cents missing"))?;
        // MONEY IS GROUNDED: one ungrounded line refuses the WHOLE fill —
        // silently dropping a line would under-invoice.
        let grounded = provenance.iter().any(|entry| {
            entry.field == format!("line_{line_number}_amount")
                && !entry.quote.is_empty()
                && crate::slices::ledger_drafts::service::quote_contains_amount(
                    &entry.quote,
                    unit_amount_cents,
                )
        });
        if !grounded {
            return Err(format!(
                "line {line_number}: unit amount has no literal provenance quote containing it"
            ));
        }
        line_items.push(InvoiceDraftLineItem {
            line_number,
            label,
            description,
            quantity,
            unit_amount_cents,
            line_total_cents: 0, // recomputed below
        });
    }
    recompute_totals(&mut line_items)?;
    Ok(InvoiceFill {
        customer_name: customer_name.chars().take(200).collect(),
        customer_email,
        line_items,
        due_date,
        memo: memo.chars().take(500).collect(),
        confidence,
        provenance,
    })
}

/// Deterministically derive a due date from a "Net N" payment term in the
/// source when the fill produced no explicit date. The anchor is the draft date
/// (now): BOS only DRAFTS — the actual send happens later in the provider UI, so
/// for send-relative precision set the client's payment terms in Invoice Ninja
/// (it computes the due date at send). This is the sensible default that stops
/// approval from showing a blank due date. Returns (YYYY-MM-DD, matched term).
pub fn due_date_from_net_terms(source: &str, now_ms: u64) -> Option<(String, String)> {
    let bytes = source.as_bytes();
    let mut from = 0usize;
    while from + 3 <= bytes.len() {
        let rel = bytes[from..]
            .windows(3)
            .position(|window| window.eq_ignore_ascii_case(b"net"))?;
        let idx = from + rel;
        from = idx + 3;
        // Word boundary before "net".
        if idx > 0 && bytes[idx - 1].is_ascii_alphanumeric() {
            continue;
        }
        // Skip a space/hyphen, then read the day count.
        let mut j = idx + 3;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'-') {
            j += 1;
        }
        let digits_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == digits_start {
            continue;
        }
        if j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
            continue;
        }
        let Ok(days) = source[digits_start..j].parse::<u32>() else {
            continue;
        };
        if !(1..=365).contains(&days) {
            continue;
        }
        let due_ms = now_ms.saturating_add(u64::from(days) * 86_400_000);
        let stamp = crate::produce::epoch_ms_to_rfc3339_utc(due_ms);
        let date = stamp.get(..10).unwrap_or(&stamp).to_string();
        let term = source.get(idx..j).unwrap_or("Net").to_string();
        return Some((date, term));
    }
    None
}

pub fn draft_from_fill(
    item: &WorkItem,
    fill: &InvoiceFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> InvoiceDraft {
    let subtotal: i64 = fill
        .line_items
        .iter()
        .map(|line| line.line_total_cents)
        .sum();
    InvoiceDraft {
        draft_id: format!("inv_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: InvoiceDraftStatus::Staged,
        customer_name: fill.customer_name.clone(),
        customer_email: fill.customer_email.clone(),
        currency: "usd".to_string(),
        line_items: fill.line_items.clone(),
        subtotal_cents: subtotal,
        total_cents: subtotal,
        due_date: fill.due_date.clone(),
        memo: fill.memo.clone(),
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

// ---------------------------------------------------------------------------
// Invoice customer enrichment: deterministic-first, typed gap-filler last.
// ---------------------------------------------------------------------------

use super::store::{CustomerEnrichedValue, CustomerEnrichmentApply};

fn enriched(field: &bos_integrations::web_page_read::EnrichmentField) -> CustomerEnrichedValue {
    CustomerEnrichedValue {
        value: field.value.clone(),
        provenance_quote: field.provenance.clone(),
    }
}

pub fn deterministic_customer_apply(
    enrich: &bos_integrations::web_page_read::WebEnrichment,
    draft: &InvoiceDraft,
) -> CustomerEnrichmentApply {
    let mut apply = CustomerEnrichmentApply::default();
    if let Some(name) = enrich.company_name.as_ref().filter(|name| {
        customer_name_enrichment_reason(draft, &CustomerEnrichmentApply::default()).is_some()
            && !crate::produce::draft_field_policy::is_domain_like_display_name(&name.value)
    }) {
        apply.customer_name = Some(enriched(name));
    }
    if draft.customer_email.is_none() {
        apply.customer_email = enrich.company_email.as_ref().map(enriched);
    }
    apply
}

pub fn missing_customer_enrich_fields(
    draft: &InvoiceDraft,
    apply: &CustomerEnrichmentApply,
) -> Vec<String> {
    let mut missing = Vec::new();
    if customer_name_enrichment_reason(draft, apply).is_some() {
        missing.push("customer_name".to_string());
    }
    if draft.customer_email.is_none() && apply.customer_email.is_none() {
        missing.push("customer_email".to_string());
    }
    missing
}

pub fn build_customer_enrichment_request(
    client_id: &str,
    item: &WorkItem,
    draft: &InvoiceDraft,
    missing_fields: &[String],
    page_texts: &[bos_integrations::web_page_read::EnrichedPageText],
) -> TypedLlmTaskRequest {
    let task_id = format!("invoice_customer_enrich_{}", draft.draft_id);
    let text_blocks = page_texts
        .iter()
        .enumerate()
        .map(|(idx, page)| TypedLlmTextBlock {
            block_id: format!("page_{idx}"),
            text: format!("URL: {}\n{}", page.url, page.text),
        })
        .collect();
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "invoice_draft".to_string(),
            entity_id: draft.draft_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Extract,
            prompt_template_id: "invoice_customer_enrichment".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: CUSTOMER_ENRICH_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 48 * 1024,
            max_output_bytes: 2 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "You are reading curated public web evidence to fill MISSING invoice customer fields. Return a single JSON object with OPTIONAL customer_name, OPTIONAL customer_email, required confidence (\"high\"|\"medium\"|\"low\"), and provenance (array of {field, quote}). FILL ONLY these fields: ".to_string() + &missing_fields.join(", ") + ". GROUNDING: include a value ONLY if you can quote the LITERAL text from the evidence in a provenance entry for that field; omit any field you cannot ground. customer_email must be a billing/contact email for this customer domain, not a third-party/payment processor address. Do NOT invent or guess.",
                "current_customer_name": draft.customer_name,
                "fields_to_fill": missing_fields,
            }),
            text_blocks,
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness,
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 180_000,
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

pub fn parse_customer_enrichment_response(
    response: &serde_json::Value,
    page_text: &str,
    missing_fields: &[String],
) -> CustomerEnrichmentApply {
    let quotes: std::collections::HashMap<String, String> = response
        .get("provenance")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let field = entry.get("field")?.as_str()?.trim().to_string();
                    let quote = entry.get("quote")?.as_str()?.trim().to_string();
                    (!quote.is_empty()).then_some((field, quote))
                })
                .collect()
        })
        .unwrap_or_default();
    let grounded = |field: &str| -> Option<CustomerEnrichedValue> {
        if !missing_fields.iter().any(|f| f == field) {
            return None;
        }
        let value = string_field(response, field)?;
        let quote = quotes.get(field)?;
        if enrichment_engine::literal_span_in_text(page_text, quote)
            && enrichment_engine::quote_contains_value(quote, &value)
        {
            Some(CustomerEnrichedValue {
                value: value.chars().take(300).collect(),
                provenance_quote: quote.chars().take(300).collect(),
            })
        } else {
            None
        }
    };
    let email_ok = |v: CustomerEnrichedValue| -> Option<CustomerEnrichedValue> {
        enrichment_engine::valid_email_shape(&v.value).then_some(v)
    };
    CustomerEnrichmentApply {
        customer_name: grounded("customer_name"),
        customer_email: grounded("customer_email").and_then(email_ok),
    }
}

pub fn merge_customer_apply(apply: &mut CustomerEnrichmentApply, patch: CustomerEnrichmentApply) {
    if apply.customer_name.is_none() {
        apply.customer_name = patch.customer_name;
    }
    if apply.customer_email.is_none() {
        apply.customer_email = patch.customer_email;
    }
}

fn customer_name_enrichment_reason(
    draft: &InvoiceDraft,
    apply: &CustomerEnrichmentApply,
) -> Option<&'static str> {
    let current = draft.customer_name.trim();
    (apply.customer_name.is_none()
        && crate::produce::draft_field_policy::is_domain_like_display_name(current)
        && crate::produce::draft_field_policy::still_ai_prefill(
            &draft.provenance,
            "customer_name",
            current,
        ))
    .then_some(SEARCH_ENRICH_REASON_WEAK_CUSTOMER_NAME)
}

fn invoice_search_enrichment_queries(domain: &str, draft: &InvoiceDraft) -> Vec<String> {
    vec![format!(
        "{} official company name {domain}",
        draft.customer_name
    )]
}

fn customer_apply_diagnostics(
    apply: &CustomerEnrichmentApply,
    tier: EnrichmentTier,
    reason: &str,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
    let values = [
        ("customer_name", &apply.customer_name),
        ("customer_email", &apply.customer_email),
    ]
    .into_iter()
    .filter_map(|(field_id, value)| {
        value
            .as_ref()
            .map(|value| crate::slices::enrichment::web_tier::AcceptedValue {
                field_id,
                value: &value.value,
                quote: &value.provenance_quote,
                provenance_refs: vec![value.provenance_quote.clone()],
            })
    });
    crate::slices::enrichment::web_tier::accepted_value_diagnostics(values, tier, reason)
}

fn customer_enrichment_plan(
    draft: &InvoiceDraft,
    note_text: &str,
    domain_override: Option<&str>,
) -> EnrichmentPlan {
    let mut seed_evidence = vec![EnrichmentSeedEvidence {
        source_id: format!("{}:{}", draft.source_kind, draft.source_ref),
        label: "Source".to_string(),
        quote: Some(note_text.chars().take(500).collect()),
    }];
    if let Some(domain) = domain_override {
        seed_evidence.push(EnrichmentSeedEvidence {
            source_id: "operator_domain_seed".to_string(),
            label: "Operator domain seed".to_string(),
            quote: Some(domain.to_string()),
        });
    }
    EnrichmentPlan {
        subject: "invoice_customer".to_string(),
        fields: vec![
            enrichment_engine::field_spec(
                "customer_name",
                "name",
                EnrichmentEligibility::WeakPrefill,
                EnrichmentConfidence::Medium,
            ),
            enrichment_engine::field_spec(
                "customer_email",
                "email",
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::Medium,
            ),
        ],
        seed_evidence,
        enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
        stop_policy: vec![
            "all_fields_accepted".to_string(),
            "no_literal_domain_for_tier3".to_string(),
            "tier_budget_exhausted".to_string(),
            "draft_left_staged_state".to_string(),
        ],
    }
}

fn customer_tier1_events(
    draft: &InvoiceDraft,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
    let source_id = format!("{}:{}", draft.source_kind, draft.source_ref);
    let mut events = vec![enrichment_engine::source_evidence_event(
        &source_id,
        "operator_source_loaded",
    )];
    let (prefill_events, proposals) = enrichment_engine::existing_prefill_events(
        &source_id,
        [
            ("customer_name", Some(draft.customer_name.as_str())),
            ("customer_email", draft.customer_email.as_deref()),
        ],
    );
    events.extend(prefill_events);
    (events, proposals)
}

struct InvoiceCustomerEnrichmentSubject {
    draft: InvoiceDraft,
    note_text: String,
    domain_override: Option<String>,
}

impl InvoiceCustomerEnrichmentSubject {
    fn new(draft: InvoiceDraft, note_text: String, domain_override: Option<String>) -> Self {
        Self {
            draft,
            note_text,
            domain_override,
        }
    }
}

impl enrichment_engine::EnrichableDraft for InvoiceCustomerEnrichmentSubject {
    type Apply = CustomerEnrichmentApply;

    fn deterministic_apply(
        &self,
        enrich: &bos_integrations::web_page_read::WebEnrichment,
    ) -> Self::Apply {
        deterministic_customer_apply(enrich, &self.draft)
    }

    fn apply_is_empty(&self, apply: &Self::Apply) -> bool {
        apply.is_empty()
    }

    fn missing_fields(&self, apply: &Self::Apply) -> Vec<String> {
        missing_customer_enrich_fields(&self.draft, apply)
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        missing_fields: &[String],
        page_texts: &[bos_integrations::web_page_read::EnrichedPageText],
    ) -> TypedLlmTaskRequest {
        build_customer_enrichment_request(client_id, item, &self.draft, missing_fields, page_texts)
    }

    fn parse_response(
        &self,
        response: &serde_json::Value,
        page_text: &str,
        missing_fields: &[String],
    ) -> Self::Apply {
        parse_customer_enrichment_response(response, page_text, missing_fields)
    }

    fn merge_apply(&self, apply: &mut Self::Apply, patch: Self::Apply) {
        merge_customer_apply(apply, patch);
    }

    fn apply_diagnostics(
        &self,
        apply: &Self::Apply,
        tier: EnrichmentTier,
        reason: &str,
    ) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        customer_apply_diagnostics(apply, tier, reason)
    }

    fn search_trigger_reason(&self, apply: &Self::Apply) -> Option<&'static str> {
        customer_name_enrichment_reason(&self.draft, apply)
    }

    fn search_queries(&self, domain: &str) -> Vec<String> {
        invoice_search_enrichment_queries(domain, &self.draft)
    }

    fn search_fields(&self, _apply: &Self::Apply) -> Vec<String> {
        vec!["customer_name".to_string()]
    }

    fn purpose(&self) -> &'static str {
        CUSTOMER_ENRICH_PURPOSE
    }

    fn slice_id(&self) -> &'static str {
        "invoice_drafts"
    }

    fn max_text_chars(&self) -> usize {
        ENRICH_MAX_TEXT_CHARS
    }

    fn gap_fill_log_message(&self) -> &'static str {
        "invoice customer web enrichment gap-fill failed"
    }

    fn search_gap_fill_log_message(&self) -> &'static str {
        "invoice customer web-search enrichment gap-fill failed"
    }

    fn finalize_web_enrichment(
        &self,
        state: &crate::http::AppState,
        ctx: enrichment_engine::EnrichmentRunContext<'_>,
        run: enrichment_engine::EnrichmentRunHandle<'_>,
        inputs: enrichment_engine::WebEnrichmentFinalizeInputs<Self::Apply>,
    ) -> enrichment_engine::EnrichmentOutcome {
        let enrichment_engine::WebEnrichmentFinalizeInputs {
            apply,
            llm_apply: _,
            deterministic: _,
            pages: _,
            page_texts: _,
            search_evidence: _,
            llm_ran: _,
            domain: _,
        } = inputs;
        let mut applied = false;
        if !self.apply_is_empty(&apply) {
            let mut persistence = state.persistence.lock();
            let idempotency_key = format!("invoiceenrich:{}:{}", self.draft.draft_id, run.run_id());
            let apply_ctx = super::store::DraftActionContext {
                client_id: &state.client_id,
                actor_id: ctx.actor_id,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                now_ms: crate::http::now_ms(),
            };
            match super::store::apply_customer_enrichment(
                persistence.connection(),
                apply_ctx,
                &self.draft.draft_id,
                &apply,
            ) {
                Ok(Some(_)) => {
                    applied = true;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(item_id = %ctx.item.item_id, error = %err, "invoice customer enrichment apply failed");
                    drop(persistence);
                    let events = vec![enrichment_engine::skip_event(
                        EnrichmentTier::WebSearch,
                        "failure",
                        &format!("apply_failed:{err}"),
                    )];
                    run.append(state, "tier3-apply-failed", &events, &[], 0);
                    return run.transition(state, EnrichmentRunStatus::Failed, "apply_failed");
                }
            }
        }
        run.transition(
            state,
            if applied {
                EnrichmentRunStatus::Completed
            } else {
                EnrichmentRunStatus::Partial
            },
            if applied {
                "accepted_fields_applied"
            } else {
                "no_fields_applied"
            },
        )
    }
}

impl enrichment_engine::EnrichmentSubject for InvoiceCustomerEnrichmentSubject {
    fn draft_id(&self) -> &str {
        &self.draft.draft_id
    }

    fn item_id(&self) -> &str {
        &self.draft.item_id
    }

    fn plan(&self) -> EnrichmentPlan {
        customer_enrichment_plan(
            &self.draft,
            &self.note_text,
            self.domain_override.as_deref(),
        )
    }

    fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        customer_tier1_events(&self.draft)
    }

    fn literal_domain(&self) -> Option<String> {
        self.domain_override
            .clone()
            .or_else(|| bos_integrations::web_page_read::find_domain(&self.note_text))
    }

    fn run_web_search_tier(
        &self,
        state: &crate::http::AppState,
        ctx: enrichment_engine::EnrichmentRunContext<'_>,
        run: enrichment_engine::EnrichmentRunHandle<'_>,
        domain: &str,
    ) -> enrichment_engine::EnrichmentOutcome {
        enrichment_engine::run_web_search_tier(self, state, ctx, run, domain)
    }
}

#[derive(Debug)]
pub(crate) enum OnDemandEnrichmentError {
    DraftNotFound,
    DraftNotStaged,
    SourceMissing,
    DomainSeedInvalid,
    Store(StoreError),
}

impl From<StoreError> for OnDemandEnrichmentError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

pub(crate) struct OnDemandEnrichmentKickoff {
    pub run_id: String,
    pub already_running: bool,
}

pub(crate) fn normalize_enrichment_domain_seed(
    domain_seed: Option<&str>,
) -> Result<Option<String>, OnDemandEnrichmentError> {
    crate::slices::enrichment::web_tier::normalize_domain_seed(domain_seed)
        .map_err(|_| OnDemandEnrichmentError::DomainSeedInvalid)
}

pub(crate) fn kick_on_demand_enrichment(
    state: crate::http::AppState,
    draft_id: String,
    actor_id: String,
    idempotency_key: String,
    domain_override: Option<String>,
) -> Result<OnDemandEnrichmentKickoff, OnDemandEnrichmentError> {
    let (draft, item, note_text, planned_run_id) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = load_staged_enrichment_draft(conn, &state.client_id, &draft_id)?;
        let item = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )?
        .ok_or(OnDemandEnrichmentError::SourceMissing)?
        .item;
        let note_text = enrichment_note_text(conn, &state.client_id, &item)?
            .ok_or(OnDemandEnrichmentError::SourceMissing)?;
        let subject = InvoiceCustomerEnrichmentSubject::new(
            draft.clone(),
            note_text.clone(),
            domain_override.clone(),
        );
        let ctx = enrichment_engine::EnrichmentRunContext {
            slice_id: "invoice_drafts",
            actor_id: &actor_id,
            item: &item,
        };
        let planned_run_id = enrichment_engine::planned_run_id(ctx, &subject);
        (draft, item, note_text, planned_run_id)
    };

    match crate::slices::async_kickoff::begin(
        KickoffSpec {
            slice_id: "invoice_drafts",
            draft_id: &draft_id,
            planned_run_id: &planned_run_id,
            capacity: KickoffCapacity::Unbounded,
        },
        || {
            record_enrichment_kickoff(
                &state,
                &actor_id,
                &idempotency_key,
                &planned_run_id,
                &draft_id,
                &item.item_id,
            )
        },
    )? {
        KickoffDecision::AlreadyRunning { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: true,
        }),
        KickoffDecision::CapacityExceeded => {
            unreachable!("standard enrichment does not request capacity")
        }
        KickoffDecision::Replayed { run_id } => Ok(OnDemandEnrichmentKickoff {
            run_id,
            already_running: false,
        }),
        KickoffDecision::Spawn { run_id, guard } => {
            std::thread::Builder::new()
                .name(format!("enrich-invoice-{draft_id}"))
                .spawn(move || {
                    let _guard = guard;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_customer_enrichment(
                            &state,
                            &item,
                            draft,
                            note_text,
                            &actor_id,
                            domain_override,
                        );
                    }));
                    if result.is_err() {
                        tracing::error!(draft_id = %draft_id, "invoice on-demand enrichment panicked");
                    }
                })
                .expect("spawn invoice enrichment thread");
            Ok(OnDemandEnrichmentKickoff {
                run_id,
                already_running: false,
            })
        }
    }
}

fn record_enrichment_kickoff(
    state: &crate::http::AppState,
    actor_id: &str,
    idempotency_key: &str,
    planned_run_id: &str,
    draft_id: &str,
    item_id: &str,
) -> Result<RecordedKickoff, OnDemandEnrichmentError> {
    let mut persistence = state.persistence.lock();
    let kickoff = crate::slices::enrichment::store::record_on_demand_kickoff(
        persistence.connection(),
        &state.client_id,
        actor_id,
        crate::slices::enrichment::store::OnDemandKickoff {
            run_id: planned_run_id,
            slice_id: "invoice_drafts",
            draft_id,
            item_id,
            idempotency_key,
            now_ms: crate::http::now_ms(),
        },
    )?;
    Ok(RecordedKickoff {
        run_id: kickoff.run_id,
        replayed: matches!(kickoff.mutation, MutationOutcome::ReplayedIdempotent { .. }),
    })
}

fn load_staged_enrichment_draft(
    conn: &rusqlite::Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<InvoiceDraft, OnDemandEnrichmentError> {
    let draft = super::store::get_draft(conn, client_id, draft_id)?
        .ok_or(OnDemandEnrichmentError::DraftNotFound)?
        .draft;
    if draft.status != InvoiceDraftStatus::Staged {
        return Err(OnDemandEnrichmentError::DraftNotStaged);
    }
    Ok(draft)
}

fn enrichment_note_text(
    conn: &rusqlite::Connection,
    client_id: &str,
    item: &WorkItem,
) -> Result<Option<String>, OnDemandEnrichmentError> {
    match crate::produce::resolve_source(conn, client_id, item) {
        Ok(message) => Ok(message.map(|message| {
            format!(
                "{} {}",
                message.subject.as_deref().unwrap_or(""),
                crate::slices::email_triage::service::body_for_ai(&message)
            )
        })),
        Err(crate::produce::SourceError::Store(err)) => Err(OnDemandEnrichmentError::Store(err)),
        Err(crate::produce::SourceError::Unsupported) => Ok(None),
    }
}

fn run_customer_enrichment(
    state: &crate::http::AppState,
    item: &WorkItem,
    draft: InvoiceDraft,
    note_text: String,
    actor_id: &str,
    domain_override: Option<String>,
) -> enrichment_engine::EnrichmentOutcome {
    let subject = InvoiceCustomerEnrichmentSubject::new(draft, note_text, domain_override);
    enrichment_engine::run(
        state,
        enrichment_engine::EnrichmentRunContext {
            slice_id: "invoice_drafts",
            actor_id,
            item,
        },
        &subject,
    )
}

pub(crate) fn freshness_candidates(
    state: &crate::http::AppState,
    adapter: &enrichment_engine::FreshnessAdapterRegistration,
    stale_after_ms: u64,
    now_ms: u64,
    limit: usize,
) -> Result<Vec<enrichment_engine::FreshnessCandidate>, String> {
    let mut out = Vec::new();
    let epoch = enrichment_engine::freshness_epoch(stale_after_ms, now_ms);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    for entry in super::store::list_drafts(conn, &state.client_id, None, limit.max(1) * 4)
        .map_err(|err| err.to_string())?
    {
        if out.len() >= limit {
            break;
        }
        let draft = entry.draft;
        if draft.status != InvoiceDraftStatus::Staged {
            continue;
        }
        let actionable_fields =
            missing_customer_enrich_fields(&draft, &CustomerEnrichmentApply::default())
                .into_iter()
                .filter(|field| adapter.critical_fields.contains(&field.as_str()))
                .collect::<Vec<_>>();
        if actionable_fields.is_empty() {
            continue;
        }
        let Some(item) = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )
        .map_err(|err| err.to_string())?
        .map(|entry| entry.item) else {
            continue;
        };
        let Some(note_text) = enrichment_note_text(conn, &state.client_id, &item)
            .map_err(|err| format!("{err:?}"))?
        else {
            continue;
        };
        let subject = InvoiceCustomerEnrichmentSubject::new(draft.clone(), note_text, None);
        if enrichment_engine::EnrichmentSubject::plan(&subject).subject != adapter.subject_id {
            continue;
        }
        let actionable_field_refs = actionable_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let last_accepted = crate::slices::enrichment::store::last_accepted_proposal_at_ms(
            conn,
            &state.client_id,
            adapter.slice_id,
            &draft.draft_id,
            adapter.subject_id,
            &actionable_field_refs,
        )
        .map_err(|err| err.to_string())?;
        if last_accepted.is_some_and(|at| at > now_ms.saturating_sub(stale_after_ms)) {
            continue;
        }
        let ctx = enrichment_engine::EnrichmentRunContext {
            slice_id: adapter.slice_id,
            actor_id: enrichment_engine::FRESHNESS_ACTOR,
            item: &item,
        };
        let run_id = enrichment_engine::planned_run_id_with_epoch(ctx, &subject, &epoch);
        if crate::slices::enrichment::store::run_exists(conn, &state.client_id, &run_id)
            .map_err(|err| err.to_string())?
        {
            continue;
        }
        out.push(enrichment_engine::FreshnessCandidate {
            slice_id: adapter.slice_id,
            subject_id: adapter.subject_id,
            draft_id: draft.draft_id.clone(),
            item_id: draft.item_id.clone(),
            run_id,
        });
    }
    Ok(out)
}

pub(crate) fn run_freshness_enrichment(
    state: &crate::http::AppState,
    candidate: &enrichment_engine::FreshnessCandidate,
    trigger_epoch: &str,
) -> enrichment_engine::EnrichmentOutcome {
    let loaded = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let draft = match load_staged_enrichment_draft(conn, &state.client_id, &candidate.draft_id)
        {
            Ok(draft) => draft,
            Err(err) => {
                tracing::info!(draft_id = %candidate.draft_id, error = ?err, "invoice freshness candidate skipped");
                return enrichment_engine::EnrichmentOutcome {
                    run_id: candidate.run_id.clone(),
                    status: EnrichmentRunStatus::Skipped,
                    reason: "candidate_no_longer_staged".to_string(),
                };
            }
        };
        let Some(item) = crate::slices::work_queue::store::get_item_unscoped(
            conn,
            &state.client_id,
            &draft.item_id,
        )
        .ok()
        .flatten()
        .map(|entry| entry.item) else {
            return enrichment_engine::EnrichmentOutcome {
                run_id: candidate.run_id.clone(),
                status: EnrichmentRunStatus::Skipped,
                reason: "source_missing".to_string(),
            };
        };
        let note_text = match enrichment_note_text(conn, &state.client_id, &item) {
            Ok(Some(note_text)) => note_text,
            _ => {
                return enrichment_engine::EnrichmentOutcome {
                    run_id: candidate.run_id.clone(),
                    status: EnrichmentRunStatus::Skipped,
                    reason: "source_missing".to_string(),
                };
            }
        };
        (draft, item, note_text)
    };
    let (draft, item, note_text) = loaded;
    let subject = InvoiceCustomerEnrichmentSubject::new(draft, note_text, None);
    enrichment_engine::run_with_trigger_epoch(
        state,
        enrichment_engine::EnrichmentRunContext {
            slice_id: candidate.slice_id,
            actor_id: enrichment_engine::FRESHNESS_ACTOR,
            item: &item,
        },
        &subject,
        trigger_epoch,
    )
}

/// The invoice kind's plug into the shared produce flow.
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::invoice_drafts::InvoiceDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "invoice_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "invoice_draft_already_active"
    }

    fn active_draft(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, crate::store_core::StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|draft| bos_contracts::invoice_drafts::InvoiceDraftProduceResponse { draft }),
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

    /// Deterministic prefill: when the same work item already carries a CRM
    /// contact email (from the note's crm_activity draft or its
    /// crm_record_create draft), ride it into the context so the stage can fill
    /// `customer_email` and approval stops asking for it. The fill prompt is
    /// unchanged — this is a deterministic graft, not a model output.
    fn prepare_context(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, crate::store_core::StoreError> {
        // CRM billing-email prefill + optional company-background grounding.
        let background = crate::produce::background_text_block(conn, client_id)?;
        Ok(serde_json::json!({
            "crm_billing": crm_billing_context_for_item(conn, client_id, &item.item_id),
            "background": background,
        }))
    }

    fn enrich_context_unlocked(&self, ctx: crate::produce::EnrichContext<'_>) -> serde_json::Value {
        let crate::produce::EnrichContext {
            state,
            item,
            message,
            scope,
            actor_id,
            actor_kind,
            mut context,
            attempt,
            now_ms,
        } = ctx;
        let sender_email = message.from_addr.as_deref();
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let Ok(resolved) = crate::slices::grounding::resolve_party(
            conn,
            &state.client_id,
            scope,
            sender_email,
            None,
        ) else {
            return context;
        };
        let resolved_json = serde_json::to_value(&resolved).unwrap_or(serde_json::Value::Null);
        let resolved_excerpt = if let Some(selected) = &resolved.selected {
            format!(
                "Resolved party by exact email: {} <{}>",
                selected
                    .company_name
                    .as_deref()
                    .or(selected.display_name.as_deref())
                    .unwrap_or("(unknown)"),
                selected.email.as_deref().unwrap_or("unknown")
            )
        } else {
            format!(
                "No high-confidence party resolution. reason={}, candidates={}",
                resolved.reason,
                resolved.candidates.len()
            )
        };
        append_grounding_evidence(
            conn,
            &state.client_id,
            item,
            attempt,
            message,
            scope,
            actor_id,
            crate::slices::grounding::TOOL_RESOLVE_PARTY,
            &json!({ "email": sender_email }).to_string(),
            &format!("resolve_party:{}", resolved.reason),
            &resolved_excerpt,
            actor_kind,
            now_ms,
        );
        let mut blocks = Vec::new();
        let history = crate::slices::grounding::customer_invoice_history(
            conn,
            &state.client_id,
            scope,
            state.accounting_visibility_policy,
            resolved.selected.as_ref(),
            now_ms,
        )
        .ok();
        if let Some(history) = &history {
            if let Some(text) = crate::slices::grounding::render_invoice_history(history) {
                append_grounding_evidence(
                    conn,
                    &state.client_id,
                    item,
                    attempt,
                    message,
                    scope,
                    actor_id,
                    crate::slices::grounding::TOOL_CUSTOMER_INVOICE_HISTORY,
                    &json!({
                        "party_email": resolved.selected.as_ref().and_then(|p| p.email.as_deref()),
                        "party_source": resolved.selected.as_ref().map(|p| p.source.as_str()),
                    })
                    .to_string(),
                    "customer_invoice_history",
                    &text,
                    actor_kind,
                    now_ms,
                );
                blocks.push(text);
            }
        }
        let shopify = resolved
            .selected
            .as_ref()
            .and_then(|party| party.email.as_deref())
            .and_then(|email| {
                crate::slices::grounding::shopify_order_grounding(
                    conn,
                    &state.client_id,
                    scope,
                    None,
                    Some(email),
                )
                .ok()
            });
        if let Some(shopify) = &shopify {
            if let Some(text) = crate::slices::grounding::render_shopify_order_grounding(shopify) {
                append_grounding_evidence(
                    conn,
                    &state.client_id,
                    item,
                    attempt,
                    message,
                    scope,
                    actor_id,
                    crate::slices::grounding::TOOL_ORDER_STATUS_LOOKUP,
                    &json!({
                        "email": resolved.selected.as_ref().and_then(|p| p.email.as_deref()),
                        "source": "shopify",
                    })
                    .to_string(),
                    "shopify_orders_by_resolved_customer",
                    &text,
                    actor_kind,
                    now_ms,
                );
                blocks.push(text);
            }
        }
        if let Some(object) = context.as_object_mut() {
            object.insert("resolve_party".to_string(), resolved_json);
            if let Some(history) = history {
                object.insert(
                    "customer_invoice_history".to_string(),
                    serde_json::to_value(history).unwrap_or(serde_json::Value::Null),
                );
            }
            if let Some(shopify) = shopify {
                object.insert(
                    "shopify_order_grounding".to_string(),
                    serde_json::to_value(shopify).unwrap_or(serde_json::Value::Null),
                );
            }
            if !blocks.is_empty() {
                object.insert(
                    "grounding_text".to_string(),
                    serde_json::Value::String(format!(
                        "Cached read-only accounting and customer order grounding. Use only high-confidence identity, invoice history, and order-context facts from this block; do not invent invoice numbers, balances, terms, customer identity, prices, or dates.\n\n{}",
                        blocks.join("\n\n")
                    )),
                );
            }
        }
        context
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        build_invoice_fill_request(client_id, item, message, context, attempt)
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
            context,
            model,
            attempt,
            idempotency_key,
            now_ms,
        } = ctx;
        use crate::store_core::StoreError;
        let date_context = crate::slices::datetime_input::context_from_email(message);
        let mut fill = match parse_invoice_fill_response_with_context(response, Some(&date_context))
        {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "invoice fill unparseable");
                return Err(StoreError::Domain(
                    "invoice_fill_invalid_response".to_string(),
                ));
            }
        };
        // No explicit due date, but the source states a "Net N" term → derive
        // the due date deterministically (draft date + N). Provenance records
        // the matched term so the operator sees where it came from.
        if fill.due_date.is_none() {
            let source = format!(
                "{} {}",
                message.subject.as_deref().unwrap_or(""),
                crate::slices::email_triage::service::body_for_ai(message)
            );
            if let Some((date, term)) = due_date_from_net_terms(&source, now_ms) {
                fill.due_date = Some(date);
                fill.provenance.push(DraftFieldProvenance {
                    field: "due_date".to_string(),
                    quote: term,
                });
            }
        }
        // Still no due date → apply the configured default term (Settings →
        // Invoicing, Net N) when set. The operator can still edit it before
        // approval; the source-stated date / Net-N term above always win.
        if fill.due_date.is_none() {
            if let Some(days) = super::store::get_invoice_settings(conn, client_id)?
                .and_then(|settings| settings.default_due_days)
            {
                let due_ms = now_ms.saturating_add(u64::from(days) * 86_400_000);
                let stamp = crate::produce::epoch_ms_to_rfc3339_utc(due_ms);
                fill.due_date = Some(stamp.get(..10).unwrap_or(&stamp).to_string());
                fill.provenance.push(DraftFieldProvenance {
                    field: "due_date".to_string(),
                    quote: format!("default Net {days}"),
                });
            }
        }
        // Graft the CRM contact email when the source itself didn't state one,
        // so approval no longer blocks on a missing customer email. Provenance
        // records "crm_match" so the operator sees where it came from.
        if fill.customer_email.is_none() {
            if let Some(email) = context
                .get("crm_billing")
                .and_then(|v| v.get("email"))
                .and_then(|v| v.as_str())
            {
                fill.customer_email = Some(email.to_string());
                fill.provenance.push(DraftFieldProvenance {
                    field: "customer_email".to_string(),
                    quote: "crm_match".to_string(),
                });
            }
        }
        if fill.customer_email.is_none() {
            if let Some(email) = high_confidence_grounded_party(context)
                .and_then(|party| party.email.clone())
                .filter(|email| enrichment_engine::valid_email_shape(email))
            {
                fill.customer_email = Some(email);
                fill.provenance.push(DraftFieldProvenance {
                    field: "customer_email".to_string(),
                    quote: "grounding:exact_email".to_string(),
                });
            }
        }
        if let Some(name) = context
            .get("crm_billing")
            .and_then(|v| v.get("customer_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            fill.customer_name = name.chars().take(200).collect();
            fill.provenance.push(DraftFieldProvenance {
                field: "customer_name".to_string(),
                quote: "crm_match".to_string(),
            });
        }
        let has_crm_name = context
            .get("crm_billing")
            .and_then(|v| v.get("customer_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .is_some_and(|name| !name.is_empty());
        if !has_crm_name {
            if let Some(name) = high_confidence_grounded_party(context).and_then(|party| {
                party
                    .company_name
                    .clone()
                    .or(party.display_name.clone())
                    .map(|name| name.trim().chars().take(200).collect::<String>())
                    .filter(|name| !name.is_empty())
            }) {
                fill.customer_name = name;
                fill.provenance.push(DraftFieldProvenance {
                    field: "customer_name".to_string(),
                    quote: "grounding:exact_email".to_string(),
                });
            }
        }
        let draft = draft_from_fill(item, &fill, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    /// Customer enrichment runs after the initial invoice draft is staged. It
    /// only fills weak/missing customer identity fields and leaves CRM context,
    /// operator edits, and invoice money/line fields untouched.
    fn after_stage(&self, state: &crate::http::AppState, item: &WorkItem, _actor_id: &str) {
        let (draft, note_text) = {
            let persistence = state.persistence.lock();
            let conn = persistence.connection_ref();
            let draft =
                match super::store::active_draft_for_item(conn, &state.client_id, &item.item_id) {
                    Ok(Some(entry)) => entry.draft,
                    _ => return,
                };
            if missing_customer_enrich_fields(&draft, &CustomerEnrichmentApply::default())
                .is_empty()
            {
                return;
            }
            let note_text = match enrichment_note_text(conn, &state.client_id, item) {
                Ok(Some(note_text)) => note_text,
                _ => return,
            };
            (draft, note_text)
        };

        run_customer_enrichment(
            state,
            item,
            draft,
            note_text,
            CUSTOMER_ENRICHMENT_ACTOR,
            None,
        );
    }
}

/// CRM billing identity associated with a work item. Prefer the
/// crm_record_create draft because it is the operator-reviewed record that will
/// become the provider customer/contact; fall back to crm_activity for email.
fn crm_billing_context_for_item(
    conn: &rusqlite::Connection,
    client_id: &str,
    item_id: &str,
) -> serde_json::Value {
    let mut email = None;
    let mut customer_name = None;
    if let Ok(Some(entry)) =
        crate::slices::crm_record_drafts::store::active_draft_for_item(conn, client_id, item_id)
    {
        if let Some(value) = entry.draft.contact_email.filter(|e| e.contains('@')) {
            email = Some(value);
        }
        if let Some(value) = entry
            .draft
            .company_name
            .map(|name| name.trim().chars().take(200).collect::<String>())
            .filter(|name| !name.is_empty())
        {
            customer_name = Some(value);
        }
    }
    if let Ok(Some(entry)) =
        crate::slices::crm_drafts::store::active_draft_for_item(conn, client_id, item_id)
    {
        if email.is_none() {
            if let Some(value) = entry.draft.contact_email.filter(|e| e.contains('@')) {
                email = Some(value);
            }
        }
    }
    serde_json::json!({
        "email": email,
        "customer_name": customer_name,
    })
}

fn high_confidence_grounded_party(
    context: &serde_json::Value,
) -> Option<crate::slices::grounding::PartyCandidate> {
    let resolved: crate::slices::grounding::ResolvedParty =
        serde_json::from_value(context.get("resolve_party")?.clone()).ok()?;
    if resolved.confidence == "high" && resolved.reason == "exact_email" {
        resolved.selected
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn append_grounding_evidence(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    item: &WorkItem,
    attempt: u64,
    _message: &InboundMessageRecord,
    scope: &crate::http::OperatorScope,
    actor_id: &str,
    tool_name: &str,
    tool_args_json: &str,
    result_ref: &str,
    result_excerpt: &str,
    actor_kind: bos_contracts::receipt::ActorKindDto,
    now_ms: u64,
) {
    let _ = crate::slices::grounding::append_grounding_evidence(
        conn,
        client_id,
        crate::slices::grounding::NewGroundingEvidence {
            work_item_id: &item.item_id,
            draft_id: None,
            packet_kind: PACKET_KIND,
            attempt,
            source_kind: &item.source_kind,
            source_ref: &item.source_ref,
            tool_name,
            tool_args_json,
            result_ref,
            result_excerpt,
            scope,
            actor_id,
            actor_kind,
            now_ms,
        },
    );
}

/// Build the Stripe create-invoice-draft outbox job for an approved draft.
/// The store's approve gate has already required an email + non-zero total.
pub fn build_approval_job(
    draft: &InvoiceDraft,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let Some(customer_email) = draft
        .customer_email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
    else {
        return Err("invoice_draft_email_required".to_string());
    };
    let idempotency_key = format!("invoicedraft:{}", draft.draft_id);
    let payload = StripeInvoiceDraftOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        approval: StripeApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        draft_ref: draft.draft_id.clone(),
        customer_name: draft.customer_name.clone(),
        customer_email: customer_email.to_string(),
        currency: draft.currency.clone(),
        memo: (!draft.memo.trim().is_empty()).then(|| draft.memo.clone()),
        due_date_epoch_seconds: draft.due_date.as_deref().and_then(|date| {
            crate::slices::accounting::service::date_to_epoch_ms(date).map(|ms| ms / 1000)
        }),
        line_items: draft
            .line_items
            .iter()
            .map(|line| StripeInvoiceLineItem {
                line_number: line.line_number,
                label: line.label.clone(),
                description: line.description.clone(),
                quantity: line.quantity,
                unit_amount_cents: line.unit_amount_cents.max(0) as u64,
                line_total_cents: line.line_total_cents.max(0) as u64,
            })
            .collect(),
        subtotal_cents: draft.subtotal_cents.max(0) as u64,
        total_cents: draft.total_cents.max(0) as u64,
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: PROVIDER_STRIPE.to_string(),
        capability: CAPABILITY_CREATE_INVOICE_DRAFT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// Build the Invoice Ninja create-invoice-draft outbox job for an approved
/// draft (BOS_ACCOUNTING_PROVIDER=invoice_ninja). Same gates as the Stripe
/// arm. The invoice NUMBER is Invoice Ninja's to assign (its Generated
/// Numbers pattern — BOS invoices match the books' existing format);
/// redelivery dedupe rides the [bos:draft …] private-notes marker instead.
/// The invoice lands as an IN DRAFT — emailing it stays human.
pub fn build_invoice_ninja_approval_job(
    draft: &InvoiceDraft,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let Some(customer_email) = draft
        .customer_email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
    else {
        return Err("invoice_draft_email_required".to_string());
    };
    let idempotency_key = format!("invoicedraft:{}", draft.draft_id);
    let payload = InvoiceNinjaInvoiceDraftOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        approval: InvoiceNinjaApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        draft_ref: draft.draft_id.clone(),
        customer_name: draft.customer_name.clone(),
        customer_email: Some(customer_email.to_string()),
        due_date: draft.due_date.clone(),
        memo: (!draft.memo.trim().is_empty()).then(|| draft.memo.clone()),
        line_items: draft
            .line_items
            .iter()
            .map(|line| InvoiceNinjaInvoiceLineItem {
                line_number: line.line_number,
                label: line.label.clone(),
                description: line.description.clone(),
                quantity: line.quantity,
                unit_amount_cents: line.unit_amount_cents,
                line_total_cents: line.line_total_cents,
            })
            .collect(),
        subtotal_cents: draft.subtotal_cents,
        total_cents: draft.total_cents,
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: crate::slices::ledger_drafts::service::PROVIDER_INVOICE_NINJA.to_string(),
        capability: CAPABILITY_CREATE_INVOICE_DRAFT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// Invoice Ninja delivery executor for the spine outbox pump (capability
/// create_invoice_draft; record_receipt stays with ledger_drafts). Rides the
/// same BOS_INVOICE_NINJA_WRITE_ENABLED gate as every IN write.
pub fn deliver_invoice_ninja(
    state: &crate::http::AppState,
    job: &ClaimedJob,
    now_ms: u64,
) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_INVOICE_NINJA_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "invoice ninja write gate read failed");
            false
        })
    };
    let config = InvoiceNinjaWriteConfig {
        base_url: env_registry::string(&env_registry::BOS_INVOICE_NINJA_BASE_URL),
        api_token: env_registry::string(&env_registry::BOS_INVOICE_NINJA_API_TOKEN),
        write_enabled,
    };
    execute_invoice_ninja_job(job, &config, now_ms)
}

pub fn execute_invoice_ninja_job(
    job: &ClaimedJob,
    config: &InvoiceNinjaWriteConfig,
    now_ms: u64,
) -> AttemptOutcome {
    if job.capability != CAPABILITY_CREATE_INVOICE_DRAFT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload =
        match serde_json::from_str::<InvoiceNinjaInvoiceDraftOutboxPayload>(&job.payload_json) {
            Ok(payload) => payload,
            Err(err) => {
                return AttemptOutcome::Terminal {
                    error: format!("invoice_ninja_payload_invalid:{err}"),
                    result_json: None,
                }
            }
        };
    let client = invoice_ninja_execution_client(config);
    match client.create_invoice_draft(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            // provider_object_id carries the IN-assigned number when one
            // exists — it is the identifier a human recognizes; the raw id
            // rides alongside.
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response
                    .invoice_number
                    .clone()
                    .unwrap_or_else(|| response.invoice_id.clone()),
                "provider_status": response.status.reason,
                "client_id": response.client_id,
                "invoice_id": response.invoice_id,
                "invoice_number": response.invoice_number,
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

/// Stripe delivery executor for the spine outbox pump. Env read here; the
/// client itself is env-free. Gate closed (or key unset) => dry-run.
pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_STRIPE_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "stripe write gate read failed");
            false
        })
    };
    let config = StripeWriteConfig {
        secret_key: env_registry::string(&env_registry::BOS_STRIPE_SECRET_KEY),
        write_enabled,
    };
    execute_job(job, &config, now_ms)
}

pub fn execute_job(job: &ClaimedJob, config: &StripeWriteConfig, now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_STRIPE || job.capability != CAPABILITY_CREATE_INVOICE_DRAFT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<StripeInvoiceDraftOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("stripe_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = stripe_execution_client(config);
    match client.create_invoice_draft(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.invoice_id,
                "provider_status": response
                    .provider_status
                    .or(response.status.reason),
                "customer_id": response.customer_id,
                "hosted_invoice_url": response.hosted_invoice_url,
            })
            .to_string(),
        },
        Err(StripeWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(StripeWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
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

/// Settings → Invoicing read model: the configured invoicing defaults.
pub fn settings_response(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<bos_contracts::invoice_drafts::InvoiceSettingsResponse, crate::store_core::StoreError> {
    let stored = super::store::get_invoice_settings(conn, client_id)?;
    Ok(bos_contracts::invoice_drafts::InvoiceSettingsResponse {
        revision: stored.as_ref().and_then(|settings| settings.revision),
        default_due_days: stored.and_then(|settings| settings.default_due_days),
    })
}

/// Replace the invoicing defaults. default_due_days, when set, must be a sane
/// payment term (1..=365 days) — mirrors the "Net N" derivation bound.
pub fn replace_invoice_settings(
    conn: &mut rusqlite::Connection,
    client_id: &str,
    actor_id: &str,
    request: &bos_contracts::invoice_drafts::InvoiceSettingsUpdateRequest,
    now_ms: u64,
) -> Result<crate::store_core::MutationOutcome, crate::store_core::StoreError> {
    if let Some(days) = request.default_due_days {
        if !(1..=365).contains(&days) {
            return Err(crate::store_core::StoreError::Domain(
                "invoice_default_due_days_out_of_range".to_string(),
            ));
        }
    }
    super::store::replace_invoice_settings(conn, client_id, actor_id, request, now_ms)
}
