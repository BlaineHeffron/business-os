//! Produce + approval domain logic for follow-up tasks.
//!
//! Produce is a bounded typed fill: {title, due_date?, context}, each field
//! provenance'd from the source email. Approval writes the LOCAL tasks row in
//! the same receipted transaction — no provider, no outbox, no gate needed.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::follow_up_tasks::{FollowUpDraft, FollowUpDraftStatus, TaskRecord, TaskStatus};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use serde_json::json;

pub const PACKET_KIND: &str = "follow_up_task";
pub const FILL_SCHEMA_REF: &str = "bos.follow_up_tasks.fill.v1";
pub const FILL_PURPOSE: &str = "follow_up_task_fill";
pub const FILL_INSTRUCTIONS: &str = "Draft the ONE follow-up task this email warrants for a small-business operator. Respond with a single JSON object with EXACTLY these fields: title (string — imperative, operator-actionable, e.g. \"Reply to Dana about the quote\"), due_date (ISO date YYYY-MM-DD when the email states or clearly implies a deadline, else null — resolve relative dates against the email's date; never invent a deadline), context (one or two sentences the operator needs when they pick the task up: who is waiting, what they asked for), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span from the email the field came from; empty quote for inferred values).";

/// Fields the filler may attach provenance to.
const PROVENANCE_FIELDS: &[&str] = &["title", "due_date", "context"];

/// The follow-up kind's plug into the shared produce flow (crate::produce).
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::follow_up_tasks::FollowUpDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "follow_up_tasks"
    }

    fn already_active_code(&self) -> &'static str {
        "follow_up_draft_already_active"
    }

    fn proposal_enabled(&self) -> bool {
        true
    }

    fn proposal_contract(&self) -> Option<crate::produce::ProposalContract> {
        Some(crate::produce::ProposalContract {
            packet_kind: PACKET_KIND,
            schema_ref: FILL_SCHEMA_REF,
            response_key: "follow_up_task",
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
            super::store::active_draft_for_item(conn, client_id, item_id)?.map(|draft| {
                bos_contracts::follow_up_tasks::FollowUpDraftProduceResponse { draft }
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

    /// Ground the task draft with the client's company background (tone only).
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
            object.insert(
                "resolve_party".to_string(),
                serde_json::to_value(&resolved).unwrap_or(serde_json::Value::Null),
            );
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
                        "Cached read-only customer order grounding. Use only relevant status, tracking, and order-context facts from this block; do not invent commitments, prices, or dates.\n\n{}",
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
        build_task_fill_request(client_id, item, message, context, attempt)
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
        let fill = match parse_task_fill_response_with_context(response, Some(&date_context)) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "fill unparseable");
                return Err(StoreError::Domain(
                    "follow_up_fill_invalid_response".to_string(),
                ));
            }
        };
        let draft = draft_from_fill(item, &fill, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    fn stage_failure_message(
        &self,
        response: &serde_json::Value,
        error_code: &str,
    ) -> Option<String> {
        if error_code != "follow_up_fill_invalid_response" {
            return None;
        }
        parse_task_fill_response(response)
            .err()
            .map(|reason| reason.chars().take(500).collect())
    }
}

pub fn build_task_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("fut_fill_{}_{attempt}", item.item_id);
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
            prompt_template_id: "follow_up_task_fill".to_string(),
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
                "instructions": FILL_INSTRUCTIONS,
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

/// A validated task fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFill {
    pub title: String,
    pub due_date: Option<String>,
    pub context: String,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

/// Parse + domain-validate the filler's response. A malformed shape or a
/// non-ISO due date is an error — never a half-valid draft.
pub fn parse_task_fill_response(response: &serde_json::Value) -> Result<TaskFill, String> {
    parse_task_fill_response_with_context(response, None)
}

pub fn parse_task_fill_response_with_context(
    response: &serde_json::Value,
    date_context: Option<&crate::slices::datetime_input::DateInputContext>,
) -> Result<TaskFill, String> {
    let title = string_field(response, "title").ok_or("title missing or empty")?;
    let due_date = string_field(response, "due_date")
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(&date, date_context)
                .map_err(|_| format!("due_date is not a supported date: {date}"))
        })
        .transpose()?;
    let context = string_field(response, "context").unwrap_or_default();
    let confidence = string_field(response, "confidence")
        .filter(|raw| matches!(raw.as_str(), "high" | "medium" | "low"))
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
    Ok(TaskFill {
        title: title.chars().take(200).collect(),
        due_date,
        context: context.chars().take(1_000).collect(),
        confidence,
        provenance,
    })
}

/// Assemble the draft row from a validated fill.
pub fn draft_from_fill(
    item: &WorkItem,
    fill: &TaskFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> FollowUpDraft {
    FollowUpDraft {
        draft_id: format!("fud_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: FollowUpDraftStatus::Staged,
        title: fill.title.clone(),
        due_date: fill.due_date.clone(),
        context: fill.context.clone(),
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        task_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

pub fn manual_draft(
    item: &WorkItem,
    fields: super::store::FollowUpEditableFields,
    attempt: u64,
    now_ms: u64,
) -> FollowUpDraft {
    FollowUpDraft {
        draft_id: format!("fud_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: FollowUpDraftStatus::Staged,
        title: fields.title,
        due_date: fields.due_date,
        context: fields.context,
        provenance: Vec::new(),
        model: "manual".to_string(),
        confidence: "high".to_string(),
        task_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// The local task an approval creates from a draft.
pub fn task_from_draft(draft: &FollowUpDraft, now_ms: u64) -> TaskRecord {
    TaskRecord {
        task_id: format!("task_{}", draft.draft_id),
        title: draft.title.clone(),
        due_date: draft.due_date.clone(),
        context: draft.context.clone(),
        source_kind: draft.source_kind.clone(),
        source_ref: draft.source_ref.clone(),
        source_user_id: draft.source_user_id.clone(),
        source_item_id: Some(draft.item_id.clone()),
        status: TaskStatus::Open,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
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

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}

/// Strict civil YYYY-MM-DD check.
pub fn is_iso_date(raw: &str) -> bool {
    crate::slices::datetime_input::is_civil_date(raw)
}

// ---------------------------------------------------------------------------
// Watchdog escalation. Classification + policy ported from agent-monitor-
// rust's customer_follow_up watchdog (port #2): a stored task is classified
// against the operator's local date into overdue / due-today / upcoming
// lanes, and overdue tasks escalate by age — missed below the escalation
// threshold, escalated at/after it, critical at/after the critical
// threshold. Pure planning logic: nothing here mutates or notifies; the
// Tasks view renders the lanes and the Queue surfaces the overdue count.
// ---------------------------------------------------------------------------

use bos_contracts::follow_up_tasks::{
    TaskDueLane, TaskEscalation, TaskEscalationLevel, TaskWithRevision,
};

/// Escalation thresholds (days overdue). AgentMonitor's client defaults: escalate
/// after 1 day, critical after 7.
pub struct WatchdogPolicy {
    pub escalation_after_days: u32,
    pub critical_after_days: u32,
}

impl Default for WatchdogPolicy {
    fn default() -> Self {
        Self {
            escalation_after_days: 1,
            critical_after_days: 7,
        }
    }
}

/// Classify one due date against `today` (both YYYY-MM-DD). An unparseable
/// or absent due date is the no-due-date lane — it never escalates.
pub fn classify_task_due(
    due_date: Option<&str>,
    today: &str,
    policy: &WatchdogPolicy,
) -> TaskEscalation {
    let (Some(due_day), Some(today_day)) = (due_date.and_then(day_number), day_number(today))
    else {
        return TaskEscalation {
            lane: TaskDueLane::NoDueDate,
            level: TaskEscalationLevel::None,
            days_overdue: 0,
            days_until_due: 0,
            reason: None,
        };
    };
    match due_day - today_day {
        days if days > 0 => TaskEscalation {
            lane: TaskDueLane::Upcoming,
            level: TaskEscalationLevel::None,
            days_overdue: 0,
            days_until_due: days,
            reason: None,
        },
        0 => TaskEscalation {
            lane: TaskDueLane::DueToday,
            level: TaskEscalationLevel::None,
            days_overdue: 0,
            days_until_due: 0,
            reason: None,
        },
        days => {
            let days_overdue = days.abs();
            let level = if days_overdue >= i64::from(policy.critical_after_days) {
                TaskEscalationLevel::Critical
            } else if days_overdue >= i64::from(policy.escalation_after_days) {
                TaskEscalationLevel::Escalated
            } else {
                TaskEscalationLevel::Missed
            };
            let reason = match level {
                TaskEscalationLevel::Missed => {
                    Some(format!("missed follow-up by {days_overdue} day(s)"))
                }
                TaskEscalationLevel::Escalated => Some(format!(
                    "missed follow-up by {days_overdue} day(s); escalation threshold reached"
                )),
                TaskEscalationLevel::Critical => Some(format!(
                    "missed follow-up by {days_overdue} day(s); critical escalation threshold reached"
                )),
                TaskEscalationLevel::None => None,
            };
            TaskEscalation {
                lane: TaskDueLane::Overdue,
                level,
                days_overdue,
                days_until_due: 0,
                reason,
            }
        }
    }
}

/// Decorate open tasks with their escalation; done tasks stay None.
pub fn decorate_task_escalations(entries: &mut [TaskWithRevision], today: &str) {
    let policy = WatchdogPolicy::default();
    for entry in entries {
        if entry.task.status == TaskStatus::Open {
            entry.escalation = Some(classify_task_due(
                entry.task.due_date.as_deref(),
                today,
                &policy,
            ));
        }
    }
}

/// Day number (days since 1970-01-01) for a YYYY-MM-DD date, via the
/// days-from-civil algorithm. None when not structurally a date.
fn day_number(date: &str) -> Option<i64> {
    crate::slices::datetime_input::civil_day_number(date)
}
