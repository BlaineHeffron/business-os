//! Claim packet assembly (the `claim_draft` packet kind): a shipping damage
//! event becomes a provider-neutral damage packet. Everything checkable is
//! DETERMINISTIC — shipment/order/evidence fields come from local caches
//! (prepare_context), the completeness gate applies required evidence roles,
//! and the claim amount is grounded (damage report amount, else order total
//! — never model-invented). The ONE LLM call writes the damage narrative +
//! item description, grounded on the damage report with a literal provenance
//! quote.
//!
//! Approval is HUMAN-CLAIM: the deliverable is a Gmail draft through the
//! existing gated create-draft path. The draft includes the packet and
//! evidence links, and approval also creates a follow-up task to track the
//! claim. Carrier and platform portal work stays human.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::claim_drafts::{
    ClaimDraft, ClaimDraftStatus, ClaimEvidence, ClaimPacketGate, ClaimShipmentRefs,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::follow_up_tasks::{TaskRecord, TaskStatus};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::gmail_draft_write::{
    GmailDraftApprovalMetadata, GmailDraftCreateOutboxPayload,
};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{self, DamageSnapshot};
use crate::outbox::NewOutboxJob;
use crate::store_core::StoreError;

pub const PACKET_KIND: &str = "claim_draft";
pub const FILL_SCHEMA_REF: &str = "bos.claim_drafts.narrative_fill.v1";
pub const FILL_PURPOSE: &str = "claim_narrative_fill";

/// Follow-up task due this many days after approval (claim tracking).
const CLAIM_FOLLOW_UP_DAYS: u64 = 7;

/// Provider-neutral required evidence roles for a shipping damage packet.
pub const ROLE_ORDER_REFERENCE: &str = "order_reference";
pub const ROLE_PACKING_PROOF: &str = "packing_proof";
pub const ROLE_TRACKING_REFERENCE: &str = "tracking_reference";
pub const ROLE_DAMAGE_PHOTO: &str = "damage_photo";

/// The deterministic context a claim draft grounds on — assembled in
/// prepare_context from the LOCAL caches (damage snapshot + order card),
/// persisted through the fill so the model and the gate see the same data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimContext {
    pub damage_event_id: String,
    pub shipment_id: String,
    pub reported_at: Option<String>,
    pub reported_by: String,
    pub severity: String,
    pub damage_type: String,
    pub damage_photo_urls: Vec<String>,
    pub description: Option<String>,
    pub damage_claim_amount_cents: Option<i64>,
    pub shipment_number: Option<String>,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub shipment_refs: Option<ClaimShipmentRefs>,
    pub shipment_context_source: Option<String>,
    pub order_number: Option<String>,
    pub order_platform: Option<String>,
    pub external_order_id: Option<String>,
    pub customer_name: Option<String>,
    pub order_total_cents: Option<i64>,
    pub order_date: Option<String>,
    pub ship_date: Option<String>,
    pub item_count: Option<i64>,
    pub pack_photo_urls: Vec<String>,
    pub pack_photo_count: i64,
}

/// Build the deterministic claim context for a work item's damage event:
/// the cached damage snapshot joined with the cached order card (by
/// shipment id). Local reads only — never a provider call.
pub fn build_claim_context(
    conn: &Connection,
    client_id: &str,
    damage_event_id: &str,
) -> Result<ClaimContext, StoreError> {
    let Some(snapshot) = store::get_damage_snapshot(conn, client_id, damage_event_id)? else {
        return Err(StoreError::Domain(
            "claim_damage_snapshot_missing".to_string(),
        ));
    };
    let order = crate::slices::inventory::store::get_order_by_shipment(
        conn,
        client_id,
        &snapshot.shipment_id,
    )?;
    let pack_photo_count = order
        .as_ref()
        .map(|order| order.photo_count)
        .unwrap_or(0)
        .max(snapshot.pack_photo_urls.len() as i64);
    let shipment_refs = merge_shipment_refs(
        snapshot.shipment_refs.clone(),
        order.as_ref().and_then(|order| order.shipment_refs.clone()),
    );
    Ok(ClaimContext {
        damage_event_id: snapshot.damage_event_id.clone(),
        shipment_id: snapshot.shipment_id.clone(),
        reported_at: snapshot.reported_at.clone(),
        reported_by: snapshot.reported_by.clone(),
        severity: snapshot.severity.clone(),
        damage_type: snapshot.damage_type.clone(),
        damage_photo_urls: snapshot.photos.clone(),
        description: snapshot.description.clone(),
        damage_claim_amount_cents: snapshot.claim_amount_cents,
        shipment_number: snapshot.shipment_number.clone(),
        carrier: snapshot.carrier.clone(),
        tracking_number: snapshot.tracking_number.clone(),
        shipment_refs,
        shipment_context_source: Some("stockforge".to_string()),
        order_number: order.as_ref().map(|order| order.order_number.clone()),
        order_platform: order.as_ref().and_then(|order| order.platform.clone()),
        external_order_id: order
            .as_ref()
            .and_then(|order| order.external_order_id.clone()),
        customer_name: order.as_ref().and_then(|order| order.customer_name.clone()),
        order_total_cents: order.as_ref().map(|order| order.total_amount_cents),
        order_date: order.as_ref().and_then(|order| order.order_date.clone()),
        ship_date: order.as_ref().and_then(|order| order.ship_date.clone()),
        item_count: order.as_ref().map(|order| order.item_count),
        pack_photo_urls: snapshot.pack_photo_urls.clone(),
        pack_photo_count,
    })
}

/// Completeness rule over provider-neutral evidence: order reference,
/// packing proof, tracking reference, damage photo. Missing roles block
/// approval-readiness, never staging — the operator sees exactly what
/// evidence to chase in the shipment/order source.
pub fn evaluate_packet_gate(context: &ClaimContext) -> ClaimPacketGate {
    let mut missing_roles = Vec::new();
    if context.order_number.is_none() {
        missing_roles.push(ROLE_ORDER_REFERENCE.to_string());
    }
    if context.pack_photo_count == 0 && context.pack_photo_urls.is_empty() {
        missing_roles.push(ROLE_PACKING_PROOF.to_string());
    }
    if !has_shipment_reference(context) {
        missing_roles.push(ROLE_TRACKING_REFERENCE.to_string());
    }
    if context.damage_photo_urls.is_empty() {
        missing_roles.push(ROLE_DAMAGE_PHOTO.to_string());
    }
    ClaimPacketGate {
        ready: missing_roles.is_empty(),
        missing_roles,
    }
}

/// The produce-source view: a damage snapshot rendered as the message record
/// the produce spine consumes (and the operator reads via source-peek).
pub fn produce_source_view(snapshot: &DamageSnapshot) -> InboundMessageRecord {
    let title = damage_item_title(snapshot);
    let ref_label = shipment_reference_label(
        snapshot.tracking_number.as_deref(),
        snapshot.shipment_number.as_deref(),
        snapshot.shipment_refs.as_ref(),
    );
    let mut body = format!(
        "Shipping damage reported via Stockforge ({}, severity {}).\nType: {}\nShipment: {}{}{}\n",
        snapshot.reported_by,
        snapshot.severity,
        snapshot.damage_type,
        ref_label,
        snapshot
            .tracking_number
            .as_deref()
            .map(|tracking| format!("\nTracking: {tracking}"))
            .unwrap_or_default(),
        snapshot
            .carrier
            .as_deref()
            .map(|carrier| format!(" ({carrier})"))
            .unwrap_or_default(),
    );
    if let Some(description) = snapshot.description.as_deref().filter(|d| !d.is_empty()) {
        body.push('\n');
        body.push_str(description);
        body.push('\n');
    }
    if !snapshot.photos.is_empty() {
        body.push_str(&format!(
            "\n{} damage photo(s) attached.\n",
            snapshot.photos.len()
        ));
    }
    InboundMessageRecord {
        source_key: snapshot.damage_event_id.clone(),
        message_id: snapshot.damage_event_id.clone(),
        thread_id: None,
        internal_date_ms: None,
        from_addr: None,
        to_addr: None,
        subject: Some(title),
        body_excerpt: body.clone(),
        body_full: body,
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: super::DAMAGE_CATEGORY.to_string(),
        matched_rule_id: None,
        ingested_at_ms: snapshot.first_seen_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

/// Work-item title for a damage event.
pub fn damage_item_title(snapshot: &DamageSnapshot) -> String {
    let shipment = shipment_reference_label(
        snapshot.tracking_number.as_deref(),
        snapshot.shipment_number.as_deref(),
        snapshot.shipment_refs.as_ref(),
    );
    format!(
        "Shipping damage — {} ({} {})",
        shipment, snapshot.severity, snapshot.damage_type
    )
}

pub fn build_narrative_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &ClaimContext,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("claim_fill_{}_{attempt}", item.item_id);
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "shipping_damage_event".to_string(),
            entity_id: context.damage_event_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "claim_narrative_fill".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 32 * 1024,
            max_output_bytes: 8 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Write the damage narrative for a carrier or shipping-platform damage claim from the DAMAGE REPORT. Respond with a single JSON object with EXACTLY these fields: damage_narrative (2-5 factual sentences for a claim form: what was shipped, what damage was found, when/how it was reported — ONLY facts from the report, never invented details, costs, or causes), item_description (one line describing the damaged item(s)/order contents from the report, else empty string), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span from the damage report the field came from — the damage_narrative quote MUST be a verbatim span of the report's description when one exists; empty quote when the report has no description).",
                "shipment": {
                    "tracking_number": context.tracking_number,
                    "carrier": context.carrier,
                    "references": context.shipment_refs,
                    "source": context.shipment_context_source,
                    "order_number": context.order_number,
                    "order_platform": context.order_platform,
                    "external_order_id": context.external_order_id,
                    "severity": context.severity,
                    "damage_type": context.damage_type,
                },
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "damage_report".to_string(),
                text: crate::slices::email_triage::service::body_for_ai_with_byte_limit(
                    message,
                    crate::slices::email_triage::service::MODEL_BODY_SMALL_MAX_BYTES,
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

/// A validated narrative fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrativeFill {
    pub damage_narrative: String,
    pub item_description: String,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

/// Parse + ground the fill. When the damage report carries a description,
/// the narrative must cite a literal span of it (≥12 chars) — a narrative
/// with no anchor in the report is refused, like an ungrounded amount.
pub fn parse_narrative_fill_response(
    response: &serde_json::Value,
    source_description: Option<&str>,
) -> Result<NarrativeFill, String> {
    let damage_narrative =
        string_field(response, "damage_narrative").ok_or("damage_narrative missing or empty")?;
    let item_description = string_field(response, "item_description").unwrap_or_default();
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
                    if !matches!(field.as_str(), "damage_narrative" | "item_description") {
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
    if let Some(description) = source_description.map(str::trim).filter(|d| !d.is_empty()) {
        let lowered = description.to_ascii_lowercase();
        let grounded = provenance.iter().any(|entry| {
            entry.field == "damage_narrative"
                && entry.quote.len() >= 12
                && lowered.contains(&entry.quote.to_ascii_lowercase())
        });
        if !grounded {
            return Err(
                "damage_narrative has no literal provenance quote from the damage report".into(),
            );
        }
    }
    Ok(NarrativeFill {
        damage_narrative: damage_narrative.chars().take(2_000).collect(),
        item_description: item_description.chars().take(500).collect(),
        confidence,
        provenance,
    })
}

/// Assemble the draft: deterministic packet fields from the context, the
/// model's narrative, and the grounded claim amount (damage report amount,
/// else order total, else 0 — the operator must set one before approval).
pub fn draft_from_fill(
    item: &WorkItem,
    context: &ClaimContext,
    fill: &NarrativeFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> ClaimDraft {
    let gate = evaluate_packet_gate(context);
    ClaimDraft {
        draft_id: format!("clm_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: ClaimDraftStatus::Staged,
        tracking_number: context.tracking_number.clone(),
        carrier: context.carrier.clone(),
        shipment_number: context.shipment_number.clone(),
        shipment_context_source: context.shipment_context_source.clone(),
        shipment_refs: context.shipment_refs.clone(),
        order_number: context.order_number.clone(),
        order_platform: context.order_platform.clone(),
        external_order_id: context.external_order_id.clone(),
        customer_name: context.customer_name.clone(),
        order_total_cents: context.order_total_cents,
        ship_date: context.ship_date.clone(),
        damage_type: context.damage_type.clone(),
        damage_severity: context.severity.clone(),
        damage_reported_at: context.reported_at.clone(),
        claim_amount_cents: context
            .damage_claim_amount_cents
            .or(context.order_total_cents)
            .unwrap_or(0),
        damage_narrative: fill.damage_narrative.clone(),
        item_description: fill.item_description.clone(),
        evidence: ClaimEvidence {
            damage_photo_urls: context.damage_photo_urls.clone(),
            pack_photo_urls: context.pack_photo_urls.clone(),
            pack_photo_count: context.pack_photo_count,
        },
        packet: gate,
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        follow_up_task_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// The claim kind's plug into the shared produce flow.
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::claim_drafts::ClaimDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "claim_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "claim_draft_already_active"
    }

    fn active_draft(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|draft| bos_contracts::claim_drafts::ClaimDraftProduceResponse { draft }),
        )
    }

    fn draft_attempts(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<u64, StoreError> {
        super::store::count_drafts_for_item(conn, client_id, item_id)
    }

    fn prepare_context(
        &self,
        conn: &Connection,
        client_id: &str,
        item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let context = build_claim_context(conn, client_id, &item.source_ref)?;
        serde_json::to_value(context)
            .map_err(|err| StoreError::Domain(format!("serialize claim context: {err}")))
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        let context: ClaimContext = serde_json::from_value(context.clone()).unwrap_or_else(|_| {
            // Unreachable in practice (prepare_context produced it); a
            // degenerate context still yields a valid request shape.
            ClaimContext {
                damage_event_id: item.source_ref.clone(),
                shipment_id: String::new(),
                reported_at: None,
                reported_by: String::new(),
                severity: String::new(),
                damage_type: String::new(),
                damage_photo_urls: Vec::new(),
                description: None,
                damage_claim_amount_cents: None,
                shipment_number: None,
                carrier: None,
                tracking_number: None,
                shipment_refs: None,
                shipment_context_source: None,
                order_number: None,
                order_platform: None,
                external_order_id: None,
                customer_name: None,
                order_total_cents: None,
                order_date: None,
                ship_date: None,
                item_count: None,
                pack_photo_urls: Vec::new(),
                pack_photo_count: 0,
            }
        });
        build_narrative_fill_request(client_id, item, message, &context, attempt)
    }

    fn stage(&self, ctx: crate::produce::StageContext<'_>) -> Result<(), StoreError> {
        let crate::produce::StageContext {
            conn,
            client_id,
            actor_id,
            item,
            message: _message,
            response,
            context,
            model,
            attempt,
            idempotency_key,
            now_ms,
        } = ctx;
        let context: ClaimContext = serde_json::from_value(context.clone())
            .map_err(|err| StoreError::Domain(format!("deserialize claim context: {err}")))?;
        let fill = match parse_narrative_fill_response(response, context.description.as_deref()) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "claim fill unparseable");
                return Err(StoreError::Domain(
                    "claim_fill_invalid_response".to_string(),
                ));
            }
        };
        let draft = draft_from_fill(item, &context, &fill, attempt, model, now_ms);
        if !draft.packet.ready {
            tracing::info!(
                item_id = %item.item_id,
                missing = ?draft.packet.missing_roles,
                "claim draft staged with an INCOMPLETE packet (approval blocked)"
            );
        }
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }
}

/// Render the claim packet as the Gmail draft the approver files from —
/// deterministic; evidence rides as LINKS (source systems store the bytes;
/// gmail_mime is text-only today, a documented seam).
pub fn render_packet_email(draft: &ClaimDraft) -> (String, String) {
    let shipment_label = shipment_reference_label(
        draft.tracking_number.as_deref(),
        draft.shipment_number.as_deref(),
        draft.shipment_refs.as_ref(),
    );
    let tracking = draft.tracking_number.as_deref().unwrap_or("(none)");
    let carrier = draft.carrier.as_deref().unwrap_or("(unknown carrier)");
    let source = draft
        .shipment_context_source
        .as_deref()
        .unwrap_or("local cache");
    let order_platform = draft.order_platform.as_deref().unwrap_or("unknown source");
    let external_order = draft.external_order_id.as_deref().unwrap_or("(unknown)");
    let subject = format!(
        "Shipping damage packet — {} / order {}",
        shipment_label,
        draft.order_number.as_deref().unwrap_or("(unmatched)"),
    );
    let mut body = String::new();
    body.push_str(&format!(
        "SHIPPING DAMAGE PACKET (file in the appropriate carrier or shipping-platform workflow)\n\
         \n\
         Shipment reference: {shipment_label}\n\
         Tracking number: {tracking}\n\
         Carrier: {carrier}\n\
         Shipment: {}\n\
         Shipment context source: {source}\n\
         Order: {} — {}\n\
         Order source: {order_platform} / external id {external_order}\n\
         Ship date: {}\n\
         Order value: {}\n\
         Claim amount: {}\n\
         Damage: {} (severity {}, reported {})\n\
         \n\
         NARRATIVE\n{}\n\
         \n\
         ITEM(S)\n{}\n",
        draft.shipment_number.as_deref().unwrap_or("(unknown)"),
        draft.order_number.as_deref().unwrap_or("(unmatched)"),
        draft
            .customer_name
            .as_deref()
            .unwrap_or("(unknown customer)"),
        draft.ship_date.as_deref().unwrap_or("(unknown)"),
        draft
            .order_total_cents
            .map(format_dollars)
            .unwrap_or_else(|| "(unknown)".to_string()),
        format_dollars(draft.claim_amount_cents),
        draft.damage_type,
        draft.damage_severity,
        draft.damage_reported_at.as_deref().unwrap_or("(unknown)"),
        draft.damage_narrative,
        if draft.item_description.is_empty() {
            "(see order)"
        } else {
            &draft.item_description
        },
    ));
    append_shipment_refs(&mut body, draft.shipment_refs.as_ref());
    body.push_str("\nEVIDENCE (download and attach in the portal)\n");
    if draft.evidence.damage_photo_urls.is_empty() {
        body.push_str("- Damage photos: NONE ON FILE\n");
    } else {
        for url in &draft.evidence.damage_photo_urls {
            body.push_str(&format!("- Damage photo: {url}\n"));
        }
    }
    if draft.evidence.pack_photo_urls.is_empty() {
        body.push_str(&format!(
            "- Pack-time photos: {} in the shipment/order source (open the pack record when available)\n",
            draft.evidence.pack_photo_count
        ));
    } else {
        for url in &draft.evidence.pack_photo_urls {
            body.push_str(&format!("- Pack-time photo: {url}\n"));
        }
    }
    body.push_str(
        "\nCHECKLIST\n\
         1. Open the appropriate carrier, Shopify, or shipping-platform workflow for this shipment.\n\
         2. Paste the narrative; enter the claim amount.\n\
         3. Download the evidence links above and upload them as attachments.\n\
         4. Submit when the provider workflow is ready, then record the claim/reference number on the follow-up task.\n",
    );
    (subject, body)
}

fn format_dollars(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, (cents % 100).abs())
}

/// Build the gated Gmail-draft outbox job for an approved claim. `to_addr`
/// comes from BOS_CLAIM_DRAFT_TO_ADDR (the filing mailbox).
pub fn build_approval_job(
    draft: &ClaimDraft,
    to_addr: &str,
    credential_user_id: Option<&str>,
    actor_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let idempotency_key = format!("claimdraft:{}", draft.draft_id);
    let (subject, body_text) = render_packet_email(draft);
    let payload = GmailDraftCreateOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        credential_user_id: credential_user_id.map(str::to_string),
        approval: GmailDraftApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        to: to_addr.to_string(),
        cc: Vec::new(),
        subject,
        body_text,
        thread_id: None,
        reply_message_id: None,
        reference_message_ids: Vec::new(),
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: crate::slices::email_drafts::service::PROVIDER_GMAIL.to_string(),
        capability: crate::slices::email_drafts::service::CAPABILITY_CREATE_DRAFT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// The claim-tracking follow-up task created at approval (same transaction).
pub fn tracking_task(draft: &ClaimDraft, now_ms: u64) -> TaskRecord {
    let due_ms = now_ms + CLAIM_FOLLOW_UP_DAYS * 24 * 60 * 60 * 1000;
    TaskRecord {
        task_id: format!("task_{}", draft.draft_id),
        title: format!(
            "Track shipping damage claim — {} (order {})",
            shipment_reference_label(
                draft.tracking_number.as_deref(),
                draft.shipment_number.as_deref(),
                draft.shipment_refs.as_ref(),
            ),
            draft.order_number.as_deref().unwrap_or("unmatched"),
        ),
        due_date: Some(crate::slices::accounting::service::today_string(due_ms)),
        context: format!(
            "Filed from claim draft {}. Record the provider claim/reference number and outcome; claim amount {}.",
            draft.draft_id,
            format_dollars(draft.claim_amount_cents),
        ),
        source_kind: draft.source_kind.clone(),
        source_ref: draft.source_ref.clone(),
        source_user_id: None,
        source_item_id: None,
        status: TaskStatus::Open,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
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

fn merge_shipment_refs(
    primary: Option<ClaimShipmentRefs>,
    fallback: Option<ClaimShipmentRefs>,
) -> Option<ClaimShipmentRefs> {
    match (primary, fallback) {
        (Some(mut primary), Some(fallback)) => {
            fill_missing_refs(&mut primary, fallback);
            Some(primary)
        }
        (Some(primary), None) => Some(primary),
        (None, Some(fallback)) => Some(fallback),
        (None, None) => None,
    }
}

fn fill_missing_refs(target: &mut ClaimShipmentRefs, fallback: ClaimShipmentRefs) {
    target.shipping_platform = target
        .shipping_platform
        .take()
        .or(fallback.shipping_platform);
    target.platform_shipment_id = target
        .platform_shipment_id
        .take()
        .or(fallback.platform_shipment_id);
    target.carrier = target.carrier.take().or(fallback.carrier);
    target.carrier_service = target.carrier_service.take().or(fallback.carrier_service);
    target.mode = target.mode.take().or(fallback.mode);
    target.tracking_number = target.tracking_number.take().or(fallback.tracking_number);
    target.pro_number = target.pro_number.take().or(fallback.pro_number);
    target.bol_number = target.bol_number.take().or(fallback.bol_number);
    target.tracking_url = target.tracking_url.take().or(fallback.tracking_url);
    if target.document_refs.is_empty() {
        target.document_refs = fallback.document_refs;
    }
    target.claim_platform = target.claim_platform.take().or(fallback.claim_platform);
    target.claim_api_supported = target.claim_api_supported.or(fallback.claim_api_supported);
}

fn has_shipment_reference(context: &ClaimContext) -> bool {
    context.tracking_number.as_deref().is_some_and(non_empty)
        || context.shipment_number.as_deref().is_some_and(non_empty)
        || context.shipment_refs.as_ref().is_some_and(|refs| {
            refs.tracking_number.as_deref().is_some_and(non_empty)
                || refs.pro_number.as_deref().is_some_and(non_empty)
                || refs.bol_number.as_deref().is_some_and(non_empty)
                || refs.platform_shipment_id.as_deref().is_some_and(non_empty)
                || refs.tracking_url.as_deref().is_some_and(non_empty)
        })
}

fn shipment_reference_label(
    tracking_number: Option<&str>,
    shipment_number: Option<&str>,
    refs: Option<&ClaimShipmentRefs>,
) -> String {
    if let Some(refs) = refs {
        if let Some(pro) = refs.pro_number.as_deref().filter(|raw| non_empty(raw)) {
            return format!("PRO {pro}");
        }
        if let Some(bol) = refs.bol_number.as_deref().filter(|raw| non_empty(raw)) {
            return format!("BOL {bol}");
        }
        if let Some(tracking) = refs.tracking_number.as_deref().filter(|raw| non_empty(raw)) {
            return tracking.to_string();
        }
        if let Some(platform_id) = refs
            .platform_shipment_id
            .as_deref()
            .filter(|raw| non_empty(raw))
        {
            let platform = refs.shipping_platform.as_deref().unwrap_or("platform");
            return format!("{platform} shipment {platform_id}");
        }
    }
    tracking_number
        .filter(|raw| non_empty(raw))
        .or_else(|| shipment_number.filter(|raw| non_empty(raw)))
        .unwrap_or("(unknown shipment)")
        .to_string()
}

fn append_shipment_refs(body: &mut String, refs: Option<&ClaimShipmentRefs>) {
    let Some(refs) = refs else {
        return;
    };
    body.push_str("\nSHIPMENT REFERENCES\n");
    append_ref_line(body, "Shipping platform", refs.shipping_platform.as_deref());
    append_ref_line(
        body,
        "Platform shipment id",
        refs.platform_shipment_id.as_deref(),
    );
    append_ref_line(body, "Carrier", refs.carrier.as_deref());
    append_ref_line(body, "Carrier service", refs.carrier_service.as_deref());
    append_ref_line(body, "Mode", refs.mode.as_deref());
    append_ref_line(body, "Tracking number", refs.tracking_number.as_deref());
    append_ref_line(body, "PRO number", refs.pro_number.as_deref());
    append_ref_line(body, "BOL number", refs.bol_number.as_deref());
    append_ref_line(body, "Tracking URL", refs.tracking_url.as_deref());
    append_ref_line(body, "Claim platform", refs.claim_platform.as_deref());
    if let Some(supported) = refs.claim_api_supported {
        body.push_str(&format!(
            "- Claim API supported: {}\n",
            if supported { "yes" } else { "no" }
        ));
    }
    for doc in &refs.document_refs {
        body.push_str(&format!("- Document {}: {}\n", doc.kind, doc.url));
    }
}

fn append_ref_line(body: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|raw| non_empty(raw)) {
        body.push_str(&format!("- {label}: {value}\n"));
    }
}

fn non_empty(raw: &str) -> bool {
    !raw.trim().is_empty()
}
