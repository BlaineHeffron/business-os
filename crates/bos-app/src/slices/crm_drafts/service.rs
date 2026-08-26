//! Produce + approval + delivery logic for CRM note drafts (the
//! `crm_activity` packet kind.
//!
//! Produce is a bounded typed fill: the source email (typically an
//! answering-service summary) goes in, a CRM-ready note comes out — note_body and contact
//! provenance'd from the source, occurred_at grounded from the email's date
//! (never model-invented). Approval enqueues a CRM note-create outbox job
//! for the configured provider (BOS_CRM_PROVIDER: hubspot | espocrm); each
//! write-gated client dry-runs until its BOS_*_WRITE_ENABLED gate opens.

use bos_contracts::calendar_drafts::DraftFieldProvenance;
use bos_contracts::crm_drafts::{CrmDraftStatus, CrmNoteDraft};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::work_queue::WorkItem;
use bos_integrations::espocrm::{
    espocrm_execution_client, EspoCrmApprovalMetadata, EspoCrmNoteCreateOutboxPayload,
    EspoCrmNoteCreateRequest, EspoCrmWriteConfig, EspoCrmWriteError,
};
use bos_integrations::hubspot::{
    hubspot_execution_client, HubSpotApprovalMetadata, HubSpotNoteCreateOutboxPayload,
    HubSpotNoteCreateRequest, HubSpotWriteConfig, HubSpotWriteError,
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

pub const PACKET_KIND: &str = "crm_activity";
pub const FILL_SCHEMA_REF: &str = "bos.crm_drafts.note_fill.v1";
pub const FILL_PURPOSE: &str = "crm_note_fill";
pub const FILL_INSTRUCTIONS: &str = "Draft the ONE CRM note this email warrants for a small-business operator's CRM (it is usually a phone-call summary from an answering service, or a customer email worth logging). Respond with a single JSON object with EXACTLY these fields: note_body (string — a factual, CRM-ready note: who contacted, what they wanted, any commitments or next steps; 1-4 sentences; never invent facts not in the email), contact_email (the customer's email address when the email states one, else null — NEVER the answering service's own address), confidence (\"high\" | \"medium\" | \"low\"), provenance (array of {field, quote} where quote is the LITERAL text span from the email the field came from; empty quote for inferred values).";

pub const PROVIDER_HUBSPOT: &str = "hubspot";
pub const PROVIDER_ESPOCRM: &str = "espocrm";
pub const CAPABILITY_CREATE_NOTE: &str = "create_note";

/// The CRM provider approved notes deliver to (BOS_CRM_PROVIDER). An unknown
/// value is a configuration error surfaced at approval time — never a silent
/// fallback to the wrong CRM.
pub fn configured_crm_provider() -> Result<&'static str, String> {
    let raw = env_registry::string(&env_registry::BOS_CRM_PROVIDER)
        .unwrap_or_else(|| PROVIDER_HUBSPOT.to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        PROVIDER_HUBSPOT => Ok(PROVIDER_HUBSPOT),
        PROVIDER_ESPOCRM => Ok(PROVIDER_ESPOCRM),
        other => Err(format!("unknown BOS_CRM_PROVIDER: {other}")),
    }
}

const PROVENANCE_FIELDS: &[&str] = &["note_body", "contact_email"];

pub(crate) fn records_autoadd_needed(
    contact_email: Option<&str>,
    matches: &crate::slices::crm_record_drafts::service::RecordMatches,
) -> bool {
    contact_email.is_none() || matches.contact_id.is_none()
}

pub fn build_note_fill_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: &serde_json::Value,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("crm_fill_{}_{attempt}", item.item_id);
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
            prompt_template_id: "crm_note_fill".to_string(),
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
    request
}

/// A validated note fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFill {
    pub note_body: String,
    pub contact_email: Option<String>,
    pub confidence: String,
    pub provenance: Vec<DraftFieldProvenance>,
}

pub fn parse_note_fill_response(response: &serde_json::Value) -> Result<NoteFill, String> {
    let note_body = string_field(response, "note_body").ok_or("note_body missing or empty")?;
    let contact_email = string_field(response, "contact_email")
        .filter(|raw| raw.contains('@') && !raw.contains(char::is_whitespace));
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
    Ok(NoteFill {
        note_body: note_body.chars().take(4_000).collect(),
        contact_email,
        confidence,
        provenance,
    })
}

/// Assemble the draft. `occurred_at` is grounded from the source email's
/// date (fallback: ingestion time) — the model never supplies timestamps.
pub fn draft_from_fill(
    item: &WorkItem,
    message: &InboundMessageRecord,
    fill: &NoteFill,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> CrmNoteDraft {
    let occurred_ms = message
        .internal_date_ms
        .unwrap_or(message.ingested_at_ms as i64);
    CrmNoteDraft {
        draft_id: format!("cnd_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        source_user_id: item.source_user_id.clone(),
        status: CrmDraftStatus::Staged,
        note_body: fill.note_body.clone(),
        contact_email: fill.contact_email.clone(),
        occurred_at: crate::slices::datetime_input::epoch_ms_to_rfc3339_utc(
            occurred_ms.max(0) as u64
        ),
        provenance: fill.provenance.clone(),
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        outbox_job_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// The crm kind's plug into the shared produce flow (crate::produce).
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::crm_drafts::CrmDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "crm_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "crm_draft_already_active"
    }

    fn proposal_enabled(&self) -> bool {
        true
    }

    fn proposal_contract(&self) -> Option<crate::produce::ProposalContract> {
        Some(crate::produce::ProposalContract {
            packet_kind: PACKET_KIND,
            schema_ref: FILL_SCHEMA_REF,
            response_key: "crm_activity",
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
                .map(|draft| bos_contracts::crm_drafts::CrmDraftProduceResponse { draft }),
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

    /// Ground the note draft with the client's company background (tone only).
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

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest {
        build_note_fill_request(client_id, item, message, context, attempt)
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
        let fill = match parse_note_fill_response(response) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "note fill unparseable");
                return Err(StoreError::Domain("crm_fill_invalid_response".to_string()));
            }
        };
        let draft = draft_from_fill(item, message, &fill, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }

    fn stage_failure_message(
        &self,
        response: &serde_json::Value,
        error_code: &str,
    ) -> Option<String> {
        if error_code != "crm_fill_invalid_response" {
            return None;
        }
        parse_note_fill_response(response)
            .err()
            .map(|reason| reason.chars().take(500).collect())
    }

    /// CRM-aware coordination (EspoCRM only): when the just-logged note names a
    /// contact who is NOT yet in the CRM, add the `crm_record_create` kind to
    /// the item and kick its produce — the operator sees the records draft
    /// appear without having had to know the contact was new. A contact that
    /// already exists adds nothing; HubSpot (no record-create vertical) is a
    /// no-op. Best-effort: failures only log, never block the note.
    fn after_stage(
        &self,
        state: &crate::http::AppState,
        item: &bos_contracts::work_queue::WorkItem,
        _actor_id: &str,
    ) {
        let records_kind = crate::slices::crm_record_drafts::service::PACKET_KIND;
        // EspoCRM-only, and skip if the records kind is already on the item.
        if configured_crm_provider().ok() != Some(PROVIDER_ESPOCRM)
            || item.packet_kinds.iter().any(|kind| kind == records_kind)
        {
            return;
        }
        // The note's contact email, if the fill captured one.
        let contact_email = {
            let persistence = state.persistence.lock();
            match super::store::active_draft_for_item(
                persistence.connection_ref(),
                &state.client_id,
                &item.item_id,
            ) {
                Ok(Some(entry)) => entry.draft.contact_email,
                _ => None,
            }
        };
        // Fast skip: if the note named an email that ALREADY resolves to a CRM
        // contact, the note attaches at delivery and no records are needed.
        // Otherwise — no email, or an email that doesn't match — we can't be
        // sure the person exists, so we add the records kind and let
        // crm_record_create's own GROUNDED fill + name-based Espo search decide
        // what's actually missing (it stages nothing when both records already
        // exist). This is what makes a "Casey Sullivan is the contact" note with
        // no email create the contact + company, and triggers website
        // enrichment — matching the design's driving example.
        let should_add_records = if let Some(email) = contact_email.as_deref() {
            let matches = crate::slices::crm_record_drafts::service::search_existing_records(
                None,
                Some(email),
                None,
            );
            records_autoadd_needed(Some(email), &matches)
        } else {
            records_autoadd_needed(
                None,
                &crate::slices::crm_record_drafts::service::RecordMatches::default(),
            )
        };
        if !should_add_records {
            return;
        }
        // Add the records kind and kick its produce.
        let mut kinds = item.packet_kinds.clone();
        kinds.push(records_kind.to_string());
        let autoadd_key = records_autoadd_kinds_key(&item.item_id);
        let produce_key = records_autoadd_produce_key(&item.item_id);
        {
            let mut persistence = state.persistence.lock();
            let ctx = crate::slices::work_queue::store::ItemActionContext {
                client_id: &state.client_id,
                actor_id: RECORDS_AUTOADD_ACTOR,
                scope: &crate::http::OperatorScope::All,
                expected_revision: None,
                idempotency_key: &autoadd_key,
                now_ms: crate::http::now_ms(),
            };
            if let Err(err) = crate::slices::work_queue::store::update_packet_kinds(
                persistence.connection(),
                ctx,
                &item.item_id,
                &kinds,
            ) {
                tracing::warn!(item_id = %item.item_id, error = %err, "crm records auto-add failed");
                return;
            }
        }
        crate::produce::kick_produce_for_kind(
            state.clone(),
            item.item_id.clone(),
            records_kind.to_string(),
            produce_key,
            RECORDS_AUTOADD_ACTOR.to_string(),
            bos_contracts::receipt::ActorKindDto::System,
        );
    }
}

/// System actor stamped on the auto-added records kind + its produce.
const RECORDS_AUTOADD_ACTOR: &str = "crm_records_autoadd";

pub(crate) fn records_autoadd_kinds_key(item_id: &str) -> String {
    format!("crm_records_autoadd_kinds:{item_id}")
}

pub(crate) fn records_autoadd_produce_key(item_id: &str) -> String {
    format!("crm_records_autoadd_produce:{item_id}")
}

/// Build the provider-write outbox job for an approved draft, for the
/// selected CRM provider. Contact + source references fold into the note
/// text (no associations/parent-record API on either provider — the
/// agent_monitor-proven free-tier posture).
pub fn build_approval_job(
    draft: &CrmNoteDraft,
    actor_id: &str,
    now_ms: u64,
    provider: &str,
) -> Result<NewOutboxJob, String> {
    let mut note_body = draft.note_body.clone();
    if let Some(email) = draft.contact_email.as_deref() {
        note_body.push_str(&format!("\n\nContact: {email}"));
    }
    note_body.push_str(&format!(
        "\nSource: {} {} (via BusinessOS, draft {})",
        draft.source_kind, draft.source_ref, draft.draft_id
    ));
    let idempotency_key = format!("crmdraft:{}", draft.draft_id);
    let approved_at = crate::produce::epoch_ms_to_rfc3339_utc(now_ms);
    let payload_json = match provider {
        PROVIDER_HUBSPOT => serde_json::to_string(&HubSpotNoteCreateOutboxPayload {
            idempotency_key: idempotency_key.clone(),
            approval: HubSpotApprovalMetadata {
                approval_id: format!("appr_{}", draft.draft_id),
                approved_by: actor_id.to_string(),
                approved_at,
            },
            note_body,
            occurred_at: draft.occurred_at.clone(),
        }),
        PROVIDER_ESPOCRM => serde_json::to_string(&EspoCrmNoteCreateOutboxPayload {
            idempotency_key: idempotency_key.clone(),
            approval: EspoCrmApprovalMetadata {
                approval_id: format!("appr_{}", draft.draft_id),
                approved_by: actor_id.to_string(),
                approved_at,
            },
            note_body,
            occurred_at: draft.occurred_at.clone(),
            // Resolved to a Contact at delivery so the note attaches to the
            // record (D3); folded into the text too, harmlessly.
            contact_email: draft.contact_email.clone(),
        }),
        other => return Err(format!("unknown crm provider: {other}")),
    }
    .map_err(|err| format!("serialize outbox payload: {err}"))?;
    Ok(NewOutboxJob {
        job_id: format!("obj_{}", draft.draft_id),
        provider: provider.to_string(),
        capability: CAPABILITY_CREATE_NOTE.to_string(),
        payload_json,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key,
    })
}

/// HubSpot delivery executor for the spine outbox pump. `execute_job` is the
/// testable core; the env read happens here (config only — the client itself
/// is env-free).
pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    let write_enabled = {
        let persistence = state.persistence.lock();
        crate::slices::admin_settings::service::flag(
            persistence.connection_ref(),
            &state.client_id,
            &env_registry::BOS_HUBSPOT_WRITE_ENABLED,
        )
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "hubspot write gate read failed");
            false
        })
    };
    let config = HubSpotWriteConfig {
        access_token: env_registry::string(&env_registry::BOS_HUBSPOT_ACCESS_TOKEN),
        write_enabled,
    };
    execute_job(job, &config, now_ms)
}

pub fn execute_job(job: &ClaimedJob, config: &HubSpotWriteConfig, now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_HUBSPOT || job.capability != CAPABILITY_CREATE_NOTE {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<HubSpotNoteCreateOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("hubspot_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let request = HubSpotNoteCreateRequest {
        idempotency_key: payload.idempotency_key,
        approval: payload.approval,
        note_body: payload.note_body,
        occurred_at: payload.occurred_at,
    };
    let client = hubspot_execution_client(config);
    match client.create_note(&request) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.note_id,
                "provider_status": response.status.reason,
            })
            .to_string(),
        },
        Err(HubSpotWriteError::Retryable { code, .. }) => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        },
        Err(HubSpotWriteError::Permanent { code, message }) => AttemptOutcome::Terminal {
            error: provider_error_detail(&code, &message),
            result_json: Some(serde_json::json!({ "message": message }).to_string()),
        },
    }
}

/// EspoCRM delivery executor for the spine outbox pump (the espocrm arm of
/// the provider dispatch). Env read here; the client itself is env-free.
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
            tracing::warn!(error = %err, "espocrm write gate read failed");
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
    if job.provider != PROVIDER_ESPOCRM || job.capability != CAPABILITY_CREATE_NOTE {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let payload = match serde_json::from_str::<EspoCrmNoteCreateOutboxPayload>(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("espocrm_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    // Resolve the note's contact to a Contact id so it attaches to that record
    // (D3). Best-effort: a search miss or unconfigured instance just leaves the
    // note unattached. The approve gate already required the contact to exist
    // when the note named one and the records path was a miss.
    let parent_contact_id = payload.contact_email.as_deref().and_then(|email| {
        bos_integrations::espocrm::espocrm_records_search_client(config)
            .and_then(|client| client.find_contact(Some(email), None).ok().flatten())
    });
    let request = EspoCrmNoteCreateRequest {
        idempotency_key: payload.idempotency_key,
        approval: payload.approval,
        note_body: payload.note_body,
        occurred_at: payload.occurred_at,
        parent_contact_id,
    };
    let client = espocrm_execution_client(config);
    match client.create_note(&request) {
        Ok(response) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": response.status.dry_run,
                "provider_object_id": response.note_id,
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
