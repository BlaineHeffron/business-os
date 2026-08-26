//! Produce + approval domain logic for calendar event drafts.
//!
//! Produce is a bounded typed Extract: the source email goes in, a typed
//! event (every field provenance'd with a literal source quote) comes out,
//! and the result is STAGED — nothing reaches a provider until the operator
//! approves, and approval only enqueues an outbox job that the write-gated
//! calendar client executes (dry-run while the gate is closed).

use bos_contracts::calendar_drafts::{
    CalendarDraftStatus, CalendarEventDraft, DraftFieldProvenance,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::WorkItem;
use bos_integrations::google_calendar::events::GoogleCalendarEventCreateOutboxPayload;
use bos_integrations::google_calendar::GoogleCalendarApprovalMetadata;
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use serde_json::json;

use crate::outbox::NewOutboxJob;

pub const PACKET_KIND: &str = "calendar_event_draft";
pub const EXTRACT_SCHEMA_REF: &str = "bos.calendar_drafts.event_extract.v1";
pub const EXTRACT_PURPOSE: &str = "calendar_event_extract";
pub const EXTRACT_INSTRUCTIONS: &str = "Extract the ONE calendar event this email describes, for a small-business operator's Google Calendar. Respond with a single JSON object with EXACTLY these fields: extractable (boolean — false when the email does not describe a specific dated event), reason (string — only when extractable is false: one sentence why), title (string — short event title), start_at (RFC3339 timestamp WITH a UTC offset, e.g. 2026-06-12T16:00:00-04:00), end_at (RFC3339 timestamp with offset; when the email gives no end time, use start_at plus one hour), timezone (IANA zone like America/New_York when determinable, else null), location (string or null), description (one or two sentences of operator-useful context, or null), attendees (array of {email, quote}; include an address only when quote is a LITERAL span from the email that contains that exact address; otherwise omit it), confidence (\"high\" | \"medium\" | \"low\" — how sure you are the event details are correct), provenance (array of {field, quote} where quote is the LITERAL text span from the email each extracted field came from; use an empty quote for inferred values such as a defaulted end time). Resolve relative dates against the email's Date header. Never invent times or attendee addresses: when the email gives no concrete date/time, set extractable to false.";

pub const PROVIDER_GOOGLE_CALENDAR: &str = "google_calendar";
pub const CAPABILITY_CREATE_EVENT: &str = "create_event";

/// Fields the extractor may attach provenance to.
const PROVENANCE_FIELDS: &[&str] = &[
    "title",
    "start_at",
    "end_at",
    "timezone",
    "location",
    "description",
];

/// The calendar kind's plug into the shared produce flow (crate::produce).
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::calendar_drafts::CalendarDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        EXTRACT_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "calendar_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "calendar_draft_already_active"
    }

    fn proposal_enabled(&self) -> bool {
        true
    }

    fn proposal_contract(&self) -> Option<crate::produce::ProposalContract> {
        Some(crate::produce::ProposalContract {
            packet_kind: PACKET_KIND,
            schema_ref: EXTRACT_SCHEMA_REF,
            response_key: "calendar_event_draft",
            instructions: EXTRACT_INSTRUCTIONS,
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
            super::store::active_draft_for_item(conn, client_id, item_id)?.map(|draft| {
                bos_contracts::calendar_drafts::CalendarDraftProduceResponse { draft }
            }),
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

    /// Ground the event draft with the client's company background (tone only).
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

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        build_event_extract_request(client_id, item, message, context, attempt)
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
        let evidence = event_email_evidence(message);
        let extract = match parse_event_extract_response_with_evidence(
            response,
            Some(&date_context),
            Some(&evidence),
        ) {
            Ok(ExtractOutcome::Event(extract)) => *extract,
            Ok(ExtractOutcome::NoEvent { reason }) => {
                tracing::info!(item_id = %item.item_id, reason, "extract found no event");
                return Err(StoreError::Domain("calendar_extract_no_event".to_string()));
            }
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "extract unparseable");
                return Err(StoreError::Domain(
                    "calendar_extract_invalid_response".to_string(),
                ));
            }
        };
        let draft = draft_from_extract(item, &extract, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    fn stage_failure_message(
        &self,
        response: &serde_json::Value,
        error_code: &str,
    ) -> Option<String> {
        if error_code == "calendar_extract_invalid_response" {
            return parse_event_extract_response(response)
                .err()
                .map(|reason| reason.chars().take(500).collect());
        }
        if error_code == "calendar_extract_no_event" {
            return response
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(|reason| reason.chars().take(500).collect());
        }
        None
    }
}

pub fn build_event_extract_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("cal_extract_{}_{attempt}", item.item_id);
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
            prompt_template_id: "calendar_event_extract".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: EXTRACT_SCHEMA_REF.to_string(),
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
                "instructions": EXTRACT_INSTRUCTIONS,
                "current_category": item.category_id,
                "email_internal_date_ms": message.internal_date_ms,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "email".to_string(),
                text: event_email_evidence(message),
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
    request
}

/// A validated extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventExtract {
    pub title: String,
    pub start_at: String,
    pub end_at: String,
    pub timezone: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub attendees: Vec<String>,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractOutcome {
    Event(Box<EventExtract>),
    /// The model judged the email does not describe a concrete dated event.
    NoEvent {
        reason: String,
    },
}

/// Parse + domain-validate the extractor's response. Malformed shapes and
/// invalid timestamps are errors (the operator sees a produce failure, never
/// a half-valid draft).
pub fn parse_event_extract_response(
    response: &serde_json::Value,
) -> Result<ExtractOutcome, String> {
    parse_event_extract_response_with_context(response, None)
}

pub fn parse_event_extract_response_with_context(
    response: &serde_json::Value,
    _date_context: Option<&crate::slices::datetime_input::DateInputContext>,
) -> Result<ExtractOutcome, String> {
    parse_event_extract_response_with_evidence(response, _date_context, None)
}

pub(crate) fn parse_event_extract_response_with_evidence(
    response: &serde_json::Value,
    _date_context: Option<&crate::slices::datetime_input::DateInputContext>,
    evidence: Option<&str>,
) -> Result<ExtractOutcome, String> {
    let extractable = response
        .get("extractable")
        .and_then(serde_json::Value::as_bool)
        .ok_or("extractable missing or not a boolean")?;
    if !extractable {
        let reason = string_field(response, "reason")
            .unwrap_or_else(|| "no concrete dated event found".to_string());
        return Ok(ExtractOutcome::NoEvent { reason });
    }
    let title = string_field(response, "title").ok_or("title missing or empty")?;
    let raw_start_at = string_field(response, "start_at").ok_or("start_at missing or empty")?;
    let raw_end_at = string_field(response, "end_at").ok_or("end_at missing or empty")?;
    let start_at = crate::slices::datetime_input::normalize_rfc3339_datetime(&raw_start_at)
        .map_err(|_| format!("start_at is not RFC3339 with offset: {raw_start_at}"))?;
    let end_at = crate::slices::datetime_input::normalize_rfc3339_datetime(&raw_end_at)
        .map_err(|_| format!("end_at is not RFC3339 with offset: {raw_end_at}"))?;
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
    let mut attendee_provenance = Vec::new();
    let proposed_attendees = response
        .get("attendees")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let email = entry.get("email")?.as_str()?.trim();
            let quote = entry.get("quote")?.as_str()?.trim();
            let source = evidence?;
            if quote.is_empty()
                || !source.contains(quote)
                || !quote_contains_exact_email(quote, email)
            {
                return None;
            }
            Some((email.to_string(), quote.to_string()))
        })
        .collect::<Vec<_>>();
    let mut raw_attendees = Vec::new();
    for (email, _) in &proposed_attendees {
        if bos_integrations::google_calendar::normalize_calendar_attendees(std::slice::from_ref(
            email,
        ))
        .is_err()
            || raw_attendees
                .iter()
                .any(|kept: &String| kept.eq_ignore_ascii_case(email))
        {
            continue;
        }
        if raw_attendees.len() == bos_integrations::google_calendar::MAX_CALENDAR_ATTENDEES {
            tracing::info!(
                proposed_count = proposed_attendees.len(),
                kept_count = raw_attendees.len(),
                "calendar attendee extraction capped"
            );
            break;
        }
        raw_attendees.push(email.clone());
    }
    let attendees = raw_attendees;
    for attendee in &attendees {
        if let Some((_, quote)) = proposed_attendees
            .iter()
            .find(|(email, _)| email.eq_ignore_ascii_case(attendee))
        {
            attendee_provenance.push(DraftFieldProvenance {
                field: format!("attendee:{attendee}"),
                quote: quote.chars().take(300).collect(),
            });
        }
    }
    let mut provenance = provenance;
    provenance.extend(attendee_provenance);
    Ok(ExtractOutcome::Event(Box::new(EventExtract {
        title: title.chars().take(200).collect(),
        start_at,
        end_at,
        timezone: string_field(response, "timezone"),
        location: string_field(response, "location").map(|s| s.chars().take(300).collect()),
        description: string_field(response, "description").map(|s| s.chars().take(1_000).collect()),
        attendees,
        confidence,
        provenance,
    })))
}

fn quote_contains_exact_email(quote: &str, email: &str) -> bool {
    let quote = quote.to_ascii_lowercase();
    let email = email.to_ascii_lowercase();
    quote.match_indices(&email).any(|(start, _)| {
        let end = start + email.len();
        let before = quote[..start].chars().next_back();
        let after = quote[end..].chars().next();
        !before.is_some_and(is_email_address_character)
            && !after.is_some_and(is_email_address_character)
    })
}

fn is_email_address_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '%' | '+' | '-' | '@')
}

/// Assemble the draft row from a validated extraction.
pub fn draft_from_extract(
    item: &WorkItem,
    extract: &EventExtract,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> CalendarEventDraft {
    CalendarEventDraft {
        draft_id: format!("ced_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: CalendarDraftStatus::Staged,
        title: extract.title.clone(),
        start_at: extract.start_at.clone(),
        end_at: extract.end_at.clone(),
        timezone: extract.timezone.clone(),
        location: extract.location.clone(),
        description: extract.description.clone(),
        // The operator picks a calendar while staged; None = server default.
        calendar_id: None,
        attendees: extract.attendees.clone(),
        send_invitations: false,
        provenance: extract.provenance.clone(),
        model: model.to_string(),
        confidence: extract.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// Build the provider-write outbox job for an approved draft. The calendar
/// client has no location field, so location folds into the description.
/// The write target is the draft's picked calendar, else `default_calendar_id`
/// (BOS_GOOGLE_CALENDAR_ID at the route).
pub fn build_approval_job(
    draft: &CalendarEventDraft,
    credential_user_id: &str,
    approved_by: &str,
    now_ms: u64,
    default_calendar_id: &str,
) -> Result<NewOutboxJob, String> {
    let description = match (draft.location.as_deref(), draft.description.as_deref()) {
        (Some(location), Some(description)) => {
            Some(format!("Location: {location}\n\n{description}"))
        }
        (Some(location), None) => Some(format!("Location: {location}")),
        (None, Some(description)) => Some(description.to_string()),
        (None, None) => None,
    };
    let idempotency_key = format!("caldraft:{}", draft.draft_id);
    let calendar_id = draft
        .calendar_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(default_calendar_id)
        .to_string();
    let payload = GoogleCalendarEventCreateOutboxPayload {
        calendar_id,
        idempotency_key: idempotency_key.clone(),
        credential_user_id: Some(credential_user_id.to_string()),
        approval: GoogleCalendarApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: approved_by.to_string(),
            approved_at: crate::produce::epoch_ms_to_rfc3339_utc(now_ms),
        },
        summary: draft.title.clone(),
        description,
        start_at: draft.start_at.clone(),
        end_at: draft.end_at.clone(),
        timezone: draft.timezone.clone(),
        attendees: draft.attendees.clone(),
        send_invitations: draft.send_invitations,
        expected_revision: None,
    };
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: PROVIDER_GOOGLE_CALENDAR.to_string(),
        capability: CAPABILITY_CREATE_EVENT.to_string(),
        payload_json: serde_json::to_string(&payload)
            .map_err(|err| format!("serialize outbox payload: {err}"))?,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

fn event_email_evidence(message: &InboundMessageRecord) -> String {
    let cc = message
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("cc"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "From: {}\nTo: {}\nCc: {}\nSubject: {}\n{}\n\n{}",
        message.from_addr.as_deref().unwrap_or("(unknown)"),
        message.to_addr.as_deref().unwrap_or("(unknown)"),
        if cc.is_empty() { "(none)" } else { &cc },
        message.subject.as_deref().unwrap_or("(no subject)"),
        crate::slices::datetime_input::email_prompt_datetime_context(message),
        crate::slices::email_triage::service::body_for_ai(message)
    )
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}

/// Structural RFC3339 check: `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)`. An
/// explicit offset is REQUIRED — Google interprets offset-less times in the
/// calendar's zone, which silently shifts events.
pub fn is_rfc3339_with_offset(raw: &str) -> bool {
    crate::slices::datetime_input::is_rfc3339_with_offset(raw)
}
