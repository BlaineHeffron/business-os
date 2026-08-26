//! Produce + approval + delivery logic for email reply drafts (the
//! `email_draft_reply` packet kind, Demo workflow-map W10 `draft.email`).
//!
//! Produce is a bounded typed fill of the reply BODY only — the recipient is
//! grounded from the source message's From, the subject is computed
//! ("Re: …"), and the thread id rides along so Gmail threads the draft.
//! Approval enqueues a Gmail DRAFT-create outbox job (never send): even with
//! BOS_GMAIL_WRITE_ENABLED open, the human sends from Gmail.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::email_drafts::{
    EmailDraftStatus, EmailOutboundFollowUpStatus, EmailReplyDraft, GmailThreadFollowUpState,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::gmail_draft_write::{
    gmail_draft_execution_client, GmailDraftApprovalMetadata, GmailDraftCreateOutboxPayload,
    GmailDraftCreateRequest, GmailDraftWriteConfig, GmailDraftWriteError,
};
use bos_integrations::gmail_inbox_read::GmailFullMessage;
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use bos_integrations::GoogleOAuthConfig;
use serde_json::json;
use std::collections::HashSet;

use crate::outbox::{
    provider_error_detail, retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob,
};
use crate::slices::email_triage::subjects::normalized_email_addresses;

pub const PACKET_KIND: &str = "email_draft_reply";
pub const FILL_SCHEMA_REF: &str = "bos.email_drafts.reply_fill.v1";
pub const FILL_PURPOSE: &str = "email_reply_fill";
pub const FILL_INSTRUCTIONS: &str = "Draft a reply EMAIL BODY to this message, written as the small-business operator (a local repair company). Respond with a single JSON object with EXACTLY these fields: body_text (string — the complete plain-text reply body: courteous, concise, answers what was asked, asks for the specific missing details when a quote/booking needs them; NO subject line, NO signature block beyond a simple first-name sign-off placeholder like \"— Jordan\"; never invent prices, dates, or commitments not grounded in the message), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span from the source message the reply responds to). The recipient and subject are handled by the system — do not include them.";
pub const REWRITE_INSTRUCTIONS: &str = "Rewrite the CURRENT EMAIL BODY according to the operator instruction. Return one JSON object with EXACTLY: body_text (complete plain-text email body only), confidence (high | medium | low), provenance (array of {field, quote}; quotes must be literal spans from SOURCE CONTEXT when asserting facts). Preserve useful operator-authored facts. Never invent prices, dates, commitments, provider state, or approval. Recipient, Cc, and subject are typed fields owned by the operator and must not appear in the response.";

pub const PROVIDER_GMAIL: &str = "gmail";
pub const CAPABILITY_CREATE_DRAFT: &str = "create_draft";

const PROVENANCE_FIELDS: &[&str] = &["body_text"];

pub fn build_reply_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("email_fill_{}_{attempt}", item.item_id);
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
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "email_reply_fill".to_string(),
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
                "instructions": FILL_INSTRUCTIONS,
                "current_category": item.category_id,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "email".to_string(),
                text: format!(
                    "From: {}\nTo: {}\nSubject: {}\n\n{}",
                    message.from_addr.as_deref().unwrap_or("(unknown)"),
                    message.to_addr.as_deref().unwrap_or("(unknown)"),
                    message.subject.as_deref().unwrap_or("(no subject)"),
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

/// A validated reply fill (body only — everything else is grounded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyFill {
    pub body_text: String,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

pub fn parse_reply_fill_response(response: &serde_json::Value) -> Result<ReplyFill, String> {
    let body_text = response
        .get("body_text")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or("body_text missing or empty")?;
    let confidence = response
        .get("confidence")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| matches!(*raw, "high" | "medium" | "low"))
        .ok_or("confidence missing or invalid")?;
    let provenance = response
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
    Ok(ReplyFill {
        body_text: body_text.chars().take(8_000).collect(),
        confidence: confidence.to_string(),
        provenance,
    })
}

/// Keep only provenance quotes that are literal spans of the exact evidence
/// blocks supplied to the rewrite model. Empty quotes carry no grounding.
pub fn grounded_rewrite_provenance(
    provenance: &[DraftFieldProvenance],
    evidence_blocks: &[TypedLlmTextBlock],
) -> Vec<DraftFieldProvenance> {
    provenance
        .iter()
        .filter(|entry| {
            !entry.quote.is_empty()
                && evidence_blocks
                    .iter()
                    .any(|block| block.text.contains(&entry.quote))
        })
        .cloned()
        .collect()
}

/// "Re: <subject>" without stacking Re: prefixes.
pub fn reply_subject(original: Option<&str>) -> String {
    let original = original.unwrap_or("").trim();
    if original.is_empty() {
        return "Re: (no subject)".to_string();
    }
    if original.to_ascii_lowercase().starts_with("re:") {
        return original.to_string();
    }
    format!("Re: {original}")
}

/// Assemble the draft. Recipient, subject, and thread are GROUNDED from the
/// source message; the model only supplied the body.
pub fn draft_from_fill(
    item: &WorkItem,
    message: &InboundMessageRecord,
    fill: &ReplyFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> Result<EmailReplyDraft, String> {
    let to_addr = message
        .from_addr
        .as_deref()
        .map(str::trim)
        .filter(|addr| addr.contains('@'))
        .ok_or("email_reply_no_recipient")?;
    Ok(EmailReplyDraft {
        draft_id: format!("erd_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: EmailDraftStatus::Staged,
        to_addr: to_addr.to_string(),
        cc_addrs: reply_all_cc_addrs(message, to_addr),
        subject: reply_subject(message.subject.as_deref()),
        body_text: fill.body_text.clone(),
        thread_id: message.thread_id.clone(),
        reply_message_id: source_message_id(message),
        reference_message_ids: source_reference_message_ids(message),
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

pub fn manual_draft(
    item: &WorkItem,
    fields: super::store::EmailEditableFields,
    attempt: u64,
    now_ms: u64,
) -> EmailReplyDraft {
    EmailReplyDraft {
        draft_id: format!("erd_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: EmailDraftStatus::Staged,
        to_addr: fields.to_addr,
        cc_addrs: fields.cc_addrs,
        subject: fields.subject,
        body_text: fields.body_text,
        thread_id: None,
        reply_message_id: None,
        reference_message_ids: Vec::new(),
        provenance: Vec::new(),
        model: "manual".to_string(),
        confidence: "high".to_string(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

pub fn build_rewrite_request(
    client_id: &str,
    draft: &EmailReplyDraft,
    source: &InboundMessageRecord,
    operator_instructions: &str,
    background: Option<TypedLlmTextBlock>,
    revision: u64,
) -> Result<TypedLlmTaskRequest, &'static str> {
    let operator_instructions: String = operator_instructions.trim().chars().take(4_000).collect();
    if operator_instructions.is_empty() {
        return Err("email_rewrite_instructions_required");
    }
    let task_id = format!("email_rewrite_{}_{}", draft.draft_id, revision);
    let mut text_blocks = vec![
        TypedLlmTextBlock {
            block_id: "current_draft".to_string(),
            text: format!(
                "To: {}\nCc: {}\nSubject: {}\n\n{}",
                draft.to_addr,
                draft.cc_addrs.join(", "),
                draft.subject,
                draft.body_text,
            ),
        },
        TypedLlmTextBlock {
            block_id: "source_context".to_string(),
            text: format!(
                "Source kind: {}\nSource subject: {}\n\n{}",
                draft.source_kind,
                source.subject.as_deref().unwrap_or("(no subject)"),
                crate::slices::email_triage::service::body_for_ai(source),
            ),
        },
    ];
    if let Some(background) = background {
        text_blocks.push(background);
    }
    Ok(TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: draft.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
            entity_id: draft.draft_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Rewrite,
            prompt_template_id: "email_body_rewrite".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 8 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": REWRITE_INSTRUCTIONS,
                "operator_instructions": operator_instructions,
            }),
            text_blocks,
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness,
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
    })
}

pub fn reply_all_cc_addrs(message: &InboundMessageRecord, to_addr: &str) -> Vec<String> {
    let mut excluded = HashSet::new();
    excluded.extend(email_addrs(to_addr));
    excluded.extend(email_addrs(
        message.source_user_id.as_deref().unwrap_or_default(),
    ));
    for header_name in ["Delivered-To", "X-Original-To"] {
        for value in header_values(&message.headers, header_name) {
            for addr in email_addrs(value) {
                excluded.insert(addr);
            }
        }
    }
    let to_addrs = message
        .to_addr
        .as_deref()
        .map(email_addrs)
        .unwrap_or_default();
    if to_addrs.len() == 1 {
        excluded.insert(to_addrs[0].clone());
    }

    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    for addr in to_addrs.into_iter().chain(
        header_values(&message.headers, "Cc")
            .into_iter()
            .flat_map(email_addrs),
    ) {
        if excluded.contains(&addr) || !seen.insert(addr.clone()) {
            continue;
        }
        recipients.push(addr);
    }
    recipients
}

fn source_message_id(message: &InboundMessageRecord) -> Option<String> {
    header_values(&message.headers, "Message-ID")
        .into_iter()
        .flat_map(message_id_tokens)
        .next()
}

fn source_reference_message_ids(message: &InboundMessageRecord) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for header_name in ["References", "In-Reply-To", "Message-ID"] {
        for value in header_values(&message.headers, header_name) {
            for id in message_id_tokens(value) {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

fn message_id_tokens(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find('<') {
        rest = &rest[start..];
        let Some(end) = rest.find('>') else {
            break;
        };
        let token = &rest[..=end];
        if valid_message_id_token(token) {
            tokens.push(token.to_string());
        }
        rest = &rest[end + 1..];
    }
    tokens
}

fn valid_message_id_token(token: &str) -> bool {
    let inner = token
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or_default();
    token.len() >= 3
        && token.len() <= 255
        && inner.contains('@')
        && !inner
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '<' || ch == '>')
}

fn header_values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

fn email_addrs(raw: &str) -> Vec<String> {
    normalized_email_addresses(Some(raw))
}

/// Deterministic Gmail-thread classification for outbound follow-up tracking.
/// DRAFT-only messages are ignored; sent mail is anchored by Gmail's SENT
/// label, not from-address, because aliases are common.
pub fn classify_thread_follow_up(
    messages: &[GmailFullMessage],
    approved_at_ms: u64,
) -> super::store::ThreadReconciliation {
    let mut messages = messages.to_vec();
    messages.sort_by(|a, b| {
        let a_time = a.internal_date_epoch_ms.unwrap_or(0);
        let b_time = b.internal_date_epoch_ms.unwrap_or(0);
        a_time
            .cmp(&b_time)
            .then_with(|| a.message_id.cmp(&b.message_id))
    });

    let mut sent_anchor: Option<&GmailFullMessage> = None;
    for message in &messages {
        let labels = normalized_labels(message);
        if labels.contains("DRAFT") && !labels.contains("SENT") {
            continue;
        }
        let sent_at = message.internal_date_epoch_ms.unwrap_or(0);
        if labels.contains("SENT") && sent_at >= approved_at_ms as i64 {
            sent_anchor = Some(message);
            break;
        }
    }

    let Some(sent) = sent_anchor else {
        return super::store::ThreadReconciliation {
            thread_state: GmailThreadFollowUpState::DraftCreated,
            status: EmailOutboundFollowUpStatus::Active,
            sent_message_id: None,
            sent_at_ms: None,
            reply_message_id: None,
            reply_at_ms: None,
            resolution_reason: None,
            last_check_error: None,
        };
    };
    let sent_at = sent.internal_date_epoch_ms.unwrap_or(0).max(0) as u64;

    let reply = messages.iter().find(|message| {
        let labels = normalized_labels(message);
        if labels.contains("DRAFT") || labels.contains("SENT") {
            return false;
        }
        let at = message.internal_date_epoch_ms.unwrap_or(0);
        at > sent_at as i64 && (labels.contains("INBOX") || message.from.is_some())
    });

    if let Some(reply) = reply {
        return super::store::ThreadReconciliation {
            thread_state: GmailThreadFollowUpState::RepliedAfterSend,
            status: EmailOutboundFollowUpStatus::Resolved,
            sent_message_id: Some(sent.message_id.clone()),
            sent_at_ms: Some(sent_at),
            reply_message_id: Some(reply.message_id.clone()),
            reply_at_ms: reply.internal_date_epoch_ms.map(|v| v.max(0) as u64),
            resolution_reason: Some(super::store::RESOLUTION_THEY_REPLIED.to_string()),
            last_check_error: None,
        };
    }

    super::store::ThreadReconciliation {
        thread_state: GmailThreadFollowUpState::SentWaitingReply,
        status: EmailOutboundFollowUpStatus::Active,
        sent_message_id: Some(sent.message_id.clone()),
        sent_at_ms: Some(sent_at),
        reply_message_id: None,
        reply_at_ms: None,
        resolution_reason: None,
        last_check_error: None,
    }
}

pub fn stale_reconciliation(error: impl Into<String>) -> super::store::ThreadReconciliation {
    super::store::ThreadReconciliation {
        thread_state: GmailThreadFollowUpState::StaleUnknown,
        status: EmailOutboundFollowUpStatus::Stale,
        sent_message_id: None,
        sent_at_ms: None,
        reply_message_id: None,
        reply_at_ms: None,
        resolution_reason: None,
        last_check_error: Some(error.into().chars().take(500).collect()),
    }
}

pub fn not_applicable_reconciliation() -> super::store::ThreadReconciliation {
    super::store::ThreadReconciliation {
        thread_state: GmailThreadFollowUpState::NotApplicable,
        status: EmailOutboundFollowUpStatus::Active,
        sent_message_id: None,
        sent_at_ms: None,
        reply_message_id: None,
        reply_at_ms: None,
        resolution_reason: None,
        last_check_error: None,
    }
}

fn normalized_labels(message: &GmailFullMessage) -> std::collections::BTreeSet<String> {
    message
        .label_ids
        .iter()
        .map(|label| label.to_ascii_uppercase())
        .collect()
}

pub fn follow_up_draft_work_item(
    follow_up: &bos_contracts::email_drafts::EmailOutboundFollowUpSummary,
    source_user_id: Option<String>,
    now_ms: u64,
) -> WorkItem {
    WorkItem {
        item_id: format!(
            "wi_{}_{}",
            super::store::SOURCE_KIND_EMAIL_FOLLOW_UP,
            follow_up.follow_up_id
        ),
        source_kind: super::store::SOURCE_KIND_EMAIL_FOLLOW_UP.to_string(),
        source_ref: follow_up.follow_up_id.clone(),
        category_id: "follow_up".to_string(),
        title: follow_up.follow_up_title.clone(),
        summary: "Draft a follow-up reply for an outbound email with no observed reply."
            .to_string(),
        packet_kinds: vec![PACKET_KIND.to_string()],
        status: WorkItemStatus::Accepted,
        accept_actor: Some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

pub fn epoch_ms_to_iso_date(epoch_ms: u64) -> String {
    crate::produce::epoch_ms_to_rfc3339_utc(epoch_ms)
        .chars()
        .take(10)
        .collect()
}

/// The email-reply kind's plug into the shared produce flow (crate::produce).
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::email_drafts::EmailDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "email_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "email_draft_already_active"
    }

    fn proposal_enabled(&self) -> bool {
        true
    }

    fn proposal_contract(&self) -> Option<crate::produce::ProposalContract> {
        Some(crate::produce::ProposalContract {
            packet_kind: PACKET_KIND,
            schema_ref: FILL_SCHEMA_REF,
            response_key: "email_draft_reply",
            instructions: FILL_INSTRUCTIONS,
        })
    }

    fn evidence_requirements(&self) -> &'static [&'static str] {
        &["source_text", "company_background"]
    }

    fn active_draft(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, crate::store_core::StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|draft| bos_contracts::email_drafts::EmailDraftProduceResponse { draft }),
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

    /// Ground the reply draft with the client's company background (tone only).
    fn prepare_context(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        _item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, crate::store_core::StoreError> {
        Ok(
            serde_json::json!({ "background": crate::produce::background_text_block(conn, client_id)? }),
        )
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
        let mut blocks = Vec::new();
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        if let Some(sender) = message.from_addr.as_deref() {
            if let Ok(prior) = crate::slices::grounding::prior_conversation_lookup(
                conn,
                &state.client_id,
                scope,
                sender,
                Some(&message.source_key),
            ) {
                if let Some(text) = crate::slices::grounding::render_prior_conversation(&prior) {
                    append_grounding_evidence(
                        conn,
                        &state.client_id,
                        item,
                        attempt,
                        message,
                        scope,
                        actor_id,
                        crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP,
                        &json!({ "sender_email": sender }).to_string(),
                        &format!("email_sender:{sender}"),
                        &text,
                        actor_kind,
                        now_ms,
                    );
                    blocks.push(text);
                }
            }
            if let Ok(crm) = crate::slices::grounding::crm_contact_lookup(
                conn,
                &state.client_id,
                scope,
                Some(sender),
                None,
            ) {
                if let Some(text) = crate::slices::grounding::render_crm_contact(&crm) {
                    append_grounding_evidence(
                        conn,
                        &state.client_id,
                        item,
                        attempt,
                        message,
                        scope,
                        actor_id,
                        crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP,
                        &json!({ "email": sender }).to_string(),
                        &format!("crm_contact:{sender}"),
                        &text,
                        actor_kind,
                        now_ms,
                    );
                    blocks.push(text);
                }
            }
        }
        for token in cheap_order_tokens(message).into_iter().take(2) {
            if let Ok(orders) =
                crate::slices::grounding::order_status_lookup(conn, &state.client_id, scope, &token)
            {
                if let Some(text) = crate::slices::grounding::render_orders(&orders) {
                    append_grounding_evidence(
                        conn,
                        &state.client_id,
                        item,
                        attempt,
                        message,
                        scope,
                        actor_id,
                        crate::slices::grounding::TOOL_ORDER_STATUS_LOOKUP,
                        &json!({ "query": token }).to_string(),
                        &format!("order_lookup:{token}"),
                        &text,
                        actor_kind,
                        now_ms,
                    );
                    blocks.push(text);
                }
            }
        }
        for token in cheap_product_tokens(message).into_iter().take(2) {
            if let Ok(products) =
                crate::slices::grounding::product_lookup(conn, &state.client_id, scope, &token)
            {
                if let Some(text) = crate::slices::grounding::render_products(&products) {
                    append_grounding_evidence(
                        conn,
                        &state.client_id,
                        item,
                        attempt,
                        message,
                        scope,
                        actor_id,
                        crate::slices::grounding::TOOL_PRODUCT_LOOKUP,
                        &json!({ "query": token }).to_string(),
                        &format!("product_lookup:{token}"),
                        &text,
                        actor_kind,
                        now_ms,
                    );
                    blocks.push(text);
                }
            }
        }
        if !blocks.is_empty() {
            let text = format!(
                "Cached read-only grounding. Use these facts only when relevant; do not invent commitments, prices, or dates beyond this evidence.\n\n{}",
                blocks.join("\n\n")
            );
            if let Some(object) = context.as_object_mut() {
                object.insert(
                    "grounding_text".to_string(),
                    serde_json::Value::String(text),
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
        build_reply_fill_request(client_id, item, message, context, attempt)
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
        let fill = match parse_reply_fill_response(response) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "reply fill unparseable");
                return Err(StoreError::Domain(
                    "email_fill_invalid_response".to_string(),
                ));
            }
        };
        let draft = draft_from_fill(item, message, &fill, attempt, model, now_ms)
            .map_err(|code| StoreError::Domain(code.to_string()))?;
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    fn stage_failure_message(
        &self,
        response: &serde_json::Value,
        error_code: &str,
    ) -> Option<String> {
        if error_code != "email_fill_invalid_response" {
            return None;
        }
        parse_reply_fill_response(response)
            .err()
            .map(|reason| reason.chars().take(500).collect())
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

fn cheap_order_tokens(message: &InboundMessageRecord) -> Vec<String> {
    let text = source_search_text(message);
    text.split_whitespace()
        .filter_map(|raw| {
            let token = raw
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '#')
                .to_string();
            let normalized: String = token
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect();
            let looks_like_hash_order = token.starts_with('#') && normalized.len() >= 3;
            let looks_like_tracking_or_order = normalized.len() >= 10
                && normalized.chars().any(|ch| ch.is_ascii_alphabetic())
                && normalized.chars().any(|ch| ch.is_ascii_digit());
            if looks_like_hash_order || looks_like_tracking_or_order {
                Some(token)
            } else {
                None
            }
        })
        .collect()
}

fn cheap_product_tokens(message: &InboundMessageRecord) -> Vec<String> {
    let text = source_search_text(message);
    text.split_whitespace()
        .filter_map(|raw| {
            let token = raw
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
                .to_string();
            let normalized: String = token
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect();
            if normalized.len() >= 4
                && normalized.len() <= 24
                && normalized.chars().any(|ch| ch.is_ascii_alphabetic())
                && normalized.chars().any(|ch| ch.is_ascii_digit())
            {
                Some(token)
            } else {
                None
            }
        })
        .collect()
}

fn source_search_text(message: &InboundMessageRecord) -> String {
    format!(
        "{} {}",
        message.subject.as_deref().unwrap_or(""),
        crate::slices::email_triage::service::body_for_ai(message)
    )
}

/// Build the provider-write outbox job for an approved draft. The reply
/// draft must be created in the mailbox that RECEIVED the message, so the
/// job binds the source account's credential (the item's source user);
/// items with no source binding fall back to the approver.
pub fn build_approval_job(
    draft: &EmailReplyDraft,
    actor_id: &str,
    credential_user_id: &str,
    now_ms: u64,
) -> Result<NewOutboxJob, String> {
    let idempotency_key = format!("emaildraft:{}", draft.draft_id);
    let payload = GmailDraftCreateOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        credential_user_id: Some(credential_user_id.to_string()),
        approval: GmailDraftApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        to: draft.to_addr.clone(),
        cc: draft.cc_addrs.clone(),
        subject: draft.subject.clone(),
        body_text: draft.body_text.clone(),
        thread_id: draft.thread_id.clone(),
        reply_message_id: draft.reply_message_id.clone(),
        reference_message_ids: draft.reference_message_ids.clone(),
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: PROVIDER_GMAIL.to_string(),
        capability: CAPABILITY_CREATE_DRAFT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// Gmail delivery executor for the spine outbox pump. Resolves credentials
/// (brief persistence lock) + the gate, then executes off-lock. The
/// credential is the payload's bound user's; legacy jobs without a binding
/// resolve through the fallback chain (env, then the only stored credential).
pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let credential_user = serde_json::from_str::<serde_json::Value>(&job.payload_json)
        .ok()
        .and_then(|payload| {
            payload
                .get("credential_user_id")?
                .as_str()
                .map(str::to_string)
        });
    let (oauth, write_enabled) = {
        let persistence = state.persistence.lock();
        let oauth = crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            credential_user.as_deref(),
        )
        .unwrap_or_default();
        let write_enabled = crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &crate::env_registry::BOS_GMAIL_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "gmail draft write gate read failed");
            false
        });
        (oauth, write_enabled)
    };
    execute_job(job, oauth.as_ref(), write_enabled, now_ms)
}

pub fn execute_job(
    job: &ClaimedJob,
    oauth: Option<&GoogleOAuthConfig>,
    write_enabled: bool,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != PROVIDER_GMAIL || job.capability != CAPABILITY_CREATE_DRAFT {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<GmailDraftCreateOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("gmail_draft_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let Some(oauth) = oauth else {
        return AttemptOutcome::Retry {
            error: "google_credential_unavailable".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        };
    };
    let config = GmailDraftWriteConfig {
        oauth: oauth.clone(),
        write_enabled,
    };
    let client = gmail_draft_execution_client(&config);
    let request = GmailDraftCreateRequest {
        idempotency_key: payload.idempotency_key,
        approval: payload.approval,
        to: payload.to,
        cc: payload.cc,
        subject: payload.subject,
        body_text: payload.body_text,
        thread_id: payload.thread_id,
        reply_message_id: payload.reply_message_id,
        reference_message_ids: payload.reference_message_ids,
    };
    match client.create_draft(&request) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.draft_id,
                "provider_status": response.status.reason,
            })
            .to_string(),
        },
        Err(GmailDraftWriteError::Retryable {
            code,
            retry_after_ms,
            ..
        }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_after_ms.unwrap_or_else(|| retry_backoff_ms(job.attempts)),
        },
        Err(GmailDraftWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}
