//! Produce + approval + delivery logic for CRM sales-intent drafts.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::crm_sales_intent::{
    CrmSalesIntentDraft, CrmSalesIntentDraftStatus, CrmSalesIntentProviderTarget,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::follow_up_tasks::{TaskRecord, TaskStatus};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::espocrm::{
    espocrm_lead_execution_client, EspoCrmApprovalMetadata, EspoCrmLeadCreateOutboxPayload,
    EspoCrmLeadInput, EspoCrmWriteConfig, EspoCrmWriteError,
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

pub const PACKET_KIND: &str = "crm_sales_intent";
pub const FILL_SCHEMA_REF: &str = "bos.crm_sales_intent.fill.v1";
pub const FILL_PURPOSE: &str = "crm_sales_intent_fill";
pub const FILL_INSTRUCTIONS: &str = "Extract CRM pipeline intent from this source. A Lead is sales intent or an unqualified opportunity; it is NOT the same thing as creating a Contact or Company record. Respond with exactly one JSON object: company_name (string|null), contact_name (string|null), contact_email (string|null), lead_title (string), intent_summary (1-3 factual sentences), rationale (why this is sales intent, grounded in the source), qualification_status (\"qualified\"|\"unqualified\"|\"unknown\"), next_step_text (string), follow_up_due_date (YYYY-MM-DD|null, only when explicitly stated), provider_target (\"lead\"|\"deal\"|\"task_only\"; default \"lead\" for explicit sales intent), create_businessos_task (boolean; true only when the source explicitly asks for follow-up), confidence (\"high\"|\"medium\"|\"low\"), provenance (array of {field, quote} with literal source spans). Do not invent names, dates, or intent.";
pub const CAPABILITY_CREATE_LEAD: &str = "create_lead";

const PROVENANCE_FIELDS: &[&str] = &[
    "company_name",
    "contact_name",
    "contact_email",
    "lead_title",
    "intent_summary",
    "rationale",
    "next_step_text",
    "follow_up_due_date",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalesIntentFill {
    pub company_name: Option<String>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub lead_title: String,
    pub intent_summary: String,
    pub rationale: String,
    pub qualification_status: String,
    pub next_step_text: String,
    pub follow_up_due_date: Option<String>,
    pub provider_target: CrmSalesIntentProviderTarget,
    pub create_businessos_task: bool,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

pub fn build_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("crm_sales_intent_{}_{attempt}", item.item_id);
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
            prompt_template_id: "crm_sales_intent_fill".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 6 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
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
                block_id: "source".to_string(),
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
    };
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

pub fn parse_fill_response(response: &serde_json::Value) -> Result<SalesIntentFill, String> {
    parse_fill_response_with_context(response, None)
}

pub fn parse_fill_response_with_context(
    response: &serde_json::Value,
    date_context: Option<&crate::slices::datetime_input::DateInputContext>,
) -> Result<SalesIntentFill, String> {
    let lead_title = string_field(response, "lead_title").ok_or("lead_title missing")?;
    let intent_summary =
        string_field(response, "intent_summary").ok_or("intent_summary missing")?;
    let rationale = string_field(response, "rationale").unwrap_or_else(|| intent_summary.clone());
    let qualification_status = string_field(response, "qualification_status")
        .filter(|raw| matches!(raw.as_str(), "qualified" | "unqualified" | "unknown"))
        .unwrap_or_else(|| "unknown".to_string());
    let next_step_text =
        string_field(response, "next_step_text").ok_or("next_step_text missing")?;
    let provider_target = match string_field(response, "provider_target").as_deref() {
        Some("lead") | None => CrmSalesIntentProviderTarget::Lead,
        Some("deal") => CrmSalesIntentProviderTarget::Deal,
        Some("task_only") => CrmSalesIntentProviderTarget::TaskOnly,
        Some(raw) => return Err(format!("provider_target invalid: {raw}")),
    };
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
    let follow_up_due_date = string_field(response, "follow_up_due_date")
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(&date, date_context)
                .map_err(|_| format!("follow_up_due_date is not a supported date: {date}"))
        })
        .transpose()?;

    Ok(SalesIntentFill {
        company_name: string_field(response, "company_name"),
        contact_name: string_field(response, "contact_name"),
        contact_email: string_field(response, "contact_email")
            .filter(|raw| raw.contains('@') && !raw.contains(char::is_whitespace)),
        lead_title: lead_title.chars().take(200).collect(),
        intent_summary: intent_summary.chars().take(2_000).collect(),
        rationale: rationale.chars().take(1_000).collect(),
        qualification_status,
        next_step_text: next_step_text.chars().take(500).collect(),
        follow_up_due_date,
        provider_target,
        create_businessos_task: response
            .get("create_businessos_task")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        confidence,
        provenance,
    })
}

pub fn task_from_draft(draft: &CrmSalesIntentDraft, now_ms: u64) -> TaskRecord {
    TaskRecord {
        task_id: format!("task_{}", draft.draft_id),
        title: draft.next_step_text.clone(),
        due_date: draft.follow_up_due_date.clone(),
        context: format!(
            "{}\n\nLead: {}\nIntent: {}",
            draft.rationale, draft.lead_title, draft.intent_summary
        ),
        source_kind: draft.source_kind.clone(),
        source_ref: draft.source_ref.clone(),
        source_user_id: draft.source_user_id.clone(),
        source_item_id: None,
        status: TaskStatus::Open,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

pub fn draft_from_fill(
    item: &WorkItem,
    fill: &SalesIntentFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> CrmSalesIntentDraft {
    CrmSalesIntentDraft {
        draft_id: format!("csi_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: CrmSalesIntentDraftStatus::Staged,
        company_name: fill.company_name.clone(),
        contact_name: fill.contact_name.clone(),
        contact_email: fill.contact_email.clone(),
        lead_title: fill.lead_title.clone(),
        intent_summary: fill.intent_summary.clone(),
        rationale: fill.rationale.clone(),
        qualification_status: fill.qualification_status.clone(),
        next_step_text: fill.next_step_text.clone(),
        follow_up_due_date: fill.follow_up_due_date.clone(),
        provider_target: fill.provider_target,
        create_businessos_task: fill.create_businessos_task,
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::crm_sales_intent::CrmSalesIntentProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "crm_sales_intent"
    }

    fn already_active_code(&self) -> &'static str {
        "crm_sales_intent_already_active"
    }

    fn proposal_enabled(&self) -> bool {
        true
    }

    fn proposal_contract(&self) -> Option<crate::produce::ProposalContract> {
        Some(crate::produce::ProposalContract {
            packet_kind: PACKET_KIND,
            schema_ref: FILL_SCHEMA_REF,
            response_key: "crm_sales_intent",
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
                bos_contracts::crm_sales_intent::CrmSalesIntentProduceResponse { draft }
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

    fn prepare_context(
        &self,
        conn: &rusqlite::Connection,
        client_id: &str,
        _item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, crate::store_core::StoreError> {
        Ok(json!({ "background": crate::produce::background_text_block(conn, client_id)? }))
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
        let Some(sender) = message.from_addr.as_deref() else {
            return context;
        };
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let Ok(crm) = crate::slices::grounding::crm_contact_lookup(
            conn,
            &state.client_id,
            scope,
            Some(sender),
            None,
        ) else {
            return context;
        };
        let Some(text) = crate::slices::grounding::render_crm_contact(&crm) else {
            return context;
        };
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
        if let Some(object) = context.as_object_mut() {
            object.insert(
                "crm_contact_lookup".to_string(),
                serde_json::to_value(crm).unwrap_or(serde_json::Value::Null),
            );
            object.insert(
                "grounding_text".to_string(),
                serde_json::Value::String(format!(
                    "Cached read-only CRM grounding. Use only contact and deal facts from this block; do not invent customer identity, pipeline stage, amounts, or dates.\n\n{}",
                    text
                )),
            );
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
        build_fill_request(client_id, item, message, context, attempt)
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
        let date_context = crate::slices::datetime_input::context_from_email(message);
        let mut fill =
            parse_fill_response_with_context(response, Some(&date_context)).map_err(|err| {
                tracing::warn!(item_id = %item.item_id, error = %err, "sales-intent fill invalid");
                crate::store_core::StoreError::Domain(
                    "crm_sales_intent_invalid_response".to_string(),
                )
            })?;
        if let Some(contact) = exact_cached_crm_contact(context) {
            if fill.contact_email.is_none() {
                if let Some(email) = contact.email.clone() {
                    fill.contact_email = Some(email);
                    fill.provenance.push(DraftFieldProvenance {
                        field: "contact_email".to_string(),
                        quote: "crm_cache:exact_email".to_string(),
                    });
                }
            }
            if fill.contact_name.is_none() {
                if let Some(name) = contact.name.clone() {
                    fill.contact_name = Some(name);
                    fill.provenance.push(DraftFieldProvenance {
                        field: "contact_name".to_string(),
                        quote: "crm_cache:exact_email".to_string(),
                    });
                }
            }
            if fill.company_name.is_none() {
                if let Some(company) = contact.company.clone() {
                    fill.company_name = Some(company);
                    fill.provenance.push(DraftFieldProvenance {
                        field: "company_name".to_string(),
                        quote: "crm_cache:exact_email".to_string(),
                    });
                }
            }
        }
        let draft = draft_from_fill(item, &fill, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    fn stage_failure_message(
        &self,
        response: &serde_json::Value,
        error_code: &str,
    ) -> Option<String> {
        if error_code != "crm_sales_intent_invalid_response" {
            return None;
        }
        parse_fill_response(response)
            .err()
            .map(|reason| reason.chars().take(500).collect())
    }
}

fn exact_cached_crm_contact(
    context: &serde_json::Value,
) -> Option<bos_contracts::crm_cache::CrmContactSnapshot> {
    let lookup: crate::slices::grounding::CrmContactLookup =
        serde_json::from_value(context.get("crm_contact_lookup")?.clone()).ok()?;
    if lookup.contacts.len() == 1 {
        lookup.contacts.into_iter().next()
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

pub fn build_approval_job(
    draft: &CrmSalesIntentDraft,
    actor_id: &str,
    now_ms: u64,
    provider: &str,
) -> Result<NewOutboxJob, String> {
    if provider != crate::slices::crm_drafts::service::PROVIDER_ESPOCRM {
        return Err("crm_sales_intent_provider_unsupported".to_string());
    }
    if draft.provider_target != CrmSalesIntentProviderTarget::Lead {
        return Err("crm_sales_intent_target_unsupported".to_string());
    }
    let idempotency_key = format!("crmlead:{}", draft.draft_id);
    let approved_at = crate::produce::epoch_ms_to_rfc3339_utc(now_ms);
    let payload_json = serde_json::to_string(&EspoCrmLeadCreateOutboxPayload {
        idempotency_key: idempotency_key.clone(),
        approval: EspoCrmApprovalMetadata {
            approval_id: format!("appr_{}", draft.draft_id),
            approved_by: actor_id.to_string(),
            approved_at,
        },
        lead: EspoCrmLeadInput {
            title: draft.lead_title.clone(),
            intent_summary: format!(
                "{}\n\nRationale: {}\nQualification: {}",
                draft.intent_summary, draft.rationale, draft.qualification_status
            ),
            company_name: draft.company_name.clone(),
            contact_name: draft.contact_name.clone(),
            contact_email: draft.contact_email.clone(),
            next_step_text: Some(draft.next_step_text.clone()),
            source_ref: Some(format!(
                "{} {} (via BusinessOS, draft {})",
                draft.source_kind, draft.source_ref, draft.draft_id
            )),
        },
    })
    .map_err(|err| format!("serialize outbox payload: {err}"))?;
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: provider.to_string(),
        capability: CAPABILITY_CREATE_LEAD.to_string(),
        payload_json,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

pub fn deliver_espocrm(
    state: &crate::http::AppState,
    job: &ClaimedJob,
    now_ms: u64,
) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_ESPOCRM_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "espocrm lead write gate read failed");
            false
        })
    };
    let config = EspoCrmWriteConfig {
        base_url: env_registry::string(&env_registry::BOS_ESPOCRM_BASE_URL),
        api_key: env_registry::string(&env_registry::BOS_ESPOCRM_API_KEY),
        write_enabled,
    };
    execute_espocrm_job(job, &config, now_ms)
}

pub fn execute_espocrm_job(
    job: &ClaimedJob,
    config: &EspoCrmWriteConfig,
    now_ms: u64,
) -> AttemptOutcome {
    if job.provider != crate::slices::crm_drafts::service::PROVIDER_ESPOCRM
        || job.capability != CAPABILITY_CREATE_LEAD
    {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<EspoCrmLeadCreateOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("espocrm_lead_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = espocrm_lead_execution_client(config);
    match client.create_lead(&payload) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.lead_id,
                "provider_status": response.status.reason,
            })
            .to_string(),
        },
        Err(EspoCrmWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(EspoCrmWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
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

/// Strict civil YYYY-MM-DD check.
pub fn is_iso_date(raw: &str) -> bool {
    crate::slices::datetime_input::is_civil_date(raw)
}
