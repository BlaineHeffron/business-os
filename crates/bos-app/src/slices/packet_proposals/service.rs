use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

use bos_contracts::email_triage::{CategoryRecord, InboundMessageRecord, FALLBACK_CATEGORY_ID};
use bos_contracts::packet_proposals::{
    PacketProposalDecisionMode, PacketProposalExecutionMode, PacketProposalKindOutcome,
    PacketProposalKindOutcomeStatus, PacketProposalReasonCode, PacketProposalRun,
    PacketProposalRunStatus, SmartDraftResponse, SmartDraftSourceStateResponse,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::{
    WorkItem, WorkItemAcceptActor, WorkItemStatus, WorkItemWithRevision, WorkQueuePolicy,
    AI_SUGGEST_ALL_SENTINEL,
};
use bos_integrations::llm_api::{DirectLlmToolCall, DirectLlmToolDefinition, DirectLlmToolResult};
#[cfg(test)]
use bos_integrations::llm_api::{DirectLlmToolTurnResponse, MockScriptedDirectLlmClient};
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskOutputEnvelope,
    TypedLlmTaskRequest, TypedLlmTaskSpec, TypedLlmTextBlock,
};
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::store::{self, NewEvidence, NewRun, RunUpdate};
use crate::env_registry;
use crate::http::{now_ms, AppState, OperatorScope};
use crate::produce::{self, PreparedProposalKind};
use crate::slices::async_kickoff::{
    KickoffCapacity, KickoffDecision, KickoffSpec, RecordedKickoff,
};
use crate::slices::email_triage::service::AiConfidence;
use crate::store_core::{MutationOutcome, StoreError};

pub const EXECUTION_MODE_BOUNDED_TYPED: PacketProposalExecutionMode =
    PacketProposalExecutionMode::BoundedTyped;
pub const PROPOSAL_SCHEMA_REF: &str = "bos.packet_proposals.bounded_typed.v1";
pub const PROPOSAL_PURPOSE: &str = "packet_proposals";
pub const STALE_RUNNING_ERROR_CODE: &str = "packet_proposal_run_stale";
pub const TOOL_LOOP_UNAVAILABLE_ERROR_CODE: &str = "packet_proposal_tool_loop_unavailable";
pub const TOOL_LOOP_EXHAUSTED_ERROR_CODE: &str = "packet_proposal_tool_loop_exhausted";

const TOOL_LOOP_MAX_TURNS: u32 = 4;
const TOOL_LOOP_MAX_TOOL_CALLS: u32 = 8;
const TOOL_LOOP_MAX_EVIDENCE_BYTES: usize = 24 * 1024;
const TOOL_LOOP_WALL_CLOCK_MS: u64 = 180_000;

const PROPOSAL_ENABLED_KINDS: &[&str] = &[
    "follow_up_task",
    "crm_activity",
    "email_draft_reply",
    "calendar_event_draft",
    "crm_sales_intent",
];

#[derive(Debug)]
pub enum SmartDraftError {
    BadRequest(&'static str),
    SourceNotFound,
    SourceUnsupported,
    NoProposalCandidates,
    Llm(String),
    RevisionConflict { current_revision: Option<u64> },
    Store(StoreError),
}

impl From<StoreError> for SmartDraftError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

#[derive(Debug, Clone)]
pub struct SmartDraftInput {
    pub source_kind: String,
    pub source_ref: String,
    pub idempotency_key: String,
    pub expected_revision: Option<u64>,
    pub min_confidence: Option<AiConfidence>,
    pub candidate_mode: SmartDraftCandidateMode,
    pub actor_id: String,
    pub scope: OperatorScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartDraftCandidateMode {
    Policy,
    AllEnabled,
}

#[derive(Debug, Clone)]
pub struct SmartDraftSourceStateInput {
    pub source_kind: String,
    pub source_ref: String,
    pub run_id: Option<String>,
    pub scope: OperatorScope,
}

#[derive(Debug, Clone)]
struct PreparedRun {
    run_id: String,
    source_kind: String,
    source_ref: String,
    message: InboundMessageRecord,
    item: WorkItem,
    existing_item: Option<WorkItemWithRevision>,
    candidate_packet_kinds: Vec<String>,
    prepared: Vec<PreparedProposalKind>,
    decision_mode: PacketProposalDecisionMode,
    execution_mode: PacketProposalExecutionMode,
    category_catalog: Vec<CategoryRecord>,
    scope: OperatorScope,
}

#[derive(Debug, Clone)]
struct ParsedProposal {
    suggested_category: Option<String>,
    confidence: AiConfidence,
    outcomes: BTreeMap<String, ParsedOutcome>,
}

#[derive(Debug, Clone)]
struct ParsedOutcome {
    status: PacketProposalKindOutcomeStatus,
    reason_code: Option<PacketProposalReasonCode>,
    draft: Option<serde_json::Value>,
    raw: serde_json::Value,
}

enum PreparedSmartDraft {
    Ready {
        prepared: Box<PreparedRun>,
        input: SmartDraftInput,
    },
    Existing(Box<SmartDraftResponse>),
}

pub fn run_smart_draft(
    state: AppState,
    input: SmartDraftInput,
) -> Result<SmartDraftResponse, SmartDraftError> {
    match prepare_smart_draft_run(&state, input)? {
        PreparedSmartDraft::Ready { prepared, input } => {
            execute_prepared_smart_draft(state, *prepared, input)
        }
        PreparedSmartDraft::Existing(response) => Ok(*response),
    }
}

pub fn kickoff_smart_draft(
    state: AppState,
    input: SmartDraftInput,
) -> Result<SmartDraftResponse, SmartDraftError> {
    let (prepared, input) = match prepare_smart_draft_run(&state, input)? {
        PreparedSmartDraft::Ready { prepared, input } => (*prepared, input),
        PreparedSmartDraft::Existing(response) => return Ok(*response),
    };

    match crate::slices::async_kickoff::begin(
        KickoffSpec {
            slice_id: "packet_proposals",
            draft_id: &format!(
                "{}:{}:{}",
                prepared.source_kind,
                prepared.source_ref,
                prepared.candidate_packet_kinds.join(",")
            ),
            planned_run_id: &prepared.run_id,
            capacity: KickoffCapacity::Unbounded,
        },
        || record_smart_draft_kickoff(&state, &prepared, &input),
    )? {
        KickoffDecision::AlreadyRunning { run_id } | KickoffDecision::Replayed { run_id } => {
            response_for_kickoff_run(&state, &prepared, &run_id)
        }
        KickoffDecision::CapacityExceeded => {
            unreachable!("smart draft kickoff does not request capacity")
        }
        KickoffDecision::Spawn { run_id, guard } => {
            let response = response_for_kickoff_run(&state, &prepared, &run_id)?;
            let worker_state = state.clone();
            std::thread::Builder::new()
                .name(format!("smart-draft-{}", prepared.source_ref))
                .spawn(move || {
                    let _guard = guard;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let run_id = prepared.run_id.clone();
                        if let Err(err) = execute_claimed_smart_draft(worker_state, prepared, input)
                        {
                            tracing::warn!(run_id = %run_id, error = ?err, "smart draft worker failed");
                        }
                    }));
                    if result.is_err() {
                        tracing::error!("smart draft worker panicked");
                    }
                })
                .expect("spawn smart draft worker");
            Ok(response)
        }
    }
}

fn response_for_kickoff_run(
    state: &AppState,
    prepared: &PreparedRun,
    run_id: &str,
) -> Result<SmartDraftResponse, SmartDraftError> {
    let persistence = state.persistence.lock();
    if let Some(run) = store::get_run(persistence.connection_ref(), &state.client_id, run_id)? {
        drop(persistence);
        return response_for_existing_run(state, run);
    }
    let item = prepared.existing_item.clone();
    Ok(SmartDraftResponse {
        run: PacketProposalRun {
            run_id: run_id.to_string(),
            source_kind: prepared.source_kind.clone(),
            source_ref: prepared.source_ref.clone(),
            item_id: item.as_ref().map(|entry| entry.item.item_id.clone()),
            resolved_decision_mode: prepared.decision_mode,
            execution_mode: prepared.execution_mode,
            status: PacketProposalRunStatus::Running,
            candidate_packet_kinds: prepared.candidate_packet_kinds.clone(),
            outcomes: Vec::new(),
            model: None,
            confidence: None,
            error_code: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        },
        item,
    })
}

fn prepare_smart_draft_run(
    state: &AppState,
    input: SmartDraftInput,
) -> Result<PreparedSmartDraft, SmartDraftError> {
    if input.source_kind.trim().is_empty() || input.source_ref.trim().is_empty() {
        return Err(SmartDraftError::BadRequest(
            "packet_proposal_source_required",
        ));
    }
    if input.idempotency_key.trim().is_empty() {
        return Err(SmartDraftError::BadRequest("idempotency_key_required"));
    }

    let prepared = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        prepare_run(conn, state, &input)?
    };

    if let Some(response) = pre_llm_short_circuit(state, &prepared)? {
        return Ok(PreparedSmartDraft::Existing(Box::new(response)));
    }

    Ok(PreparedSmartDraft::Ready {
        prepared: Box::new(prepared),
        input,
    })
}

fn execute_prepared_smart_draft(
    state: AppState,
    prepared: PreparedRun,
    input: SmartDraftInput,
) -> Result<SmartDraftResponse, SmartDraftError> {
    if let Some(response) = pre_llm_short_circuit(&state, &prepared)? {
        return Ok(response);
    }
    if let Some(response) = claim_run_or_replay(&state, &prepared, &input)? {
        return Ok(response);
    }
    execute_claimed_smart_draft(state, prepared, input)
}

fn execute_claimed_smart_draft(
    state: AppState,
    prepared: PreparedRun,
    input: SmartDraftInput,
) -> Result<SmartDraftResponse, SmartDraftError> {
    let request = build_proposal_request(&state.client_id, &prepared);
    let envelope = if prepared.execution_mode == PacketProposalExecutionMode::ToolLoopAgentic {
        execute_packet_proposal_tool_loop(&state, &prepared, &request).map_err(|code| {
            let _ = finish_failed_run(&state, &prepared, &input, &code);
            SmartDraftError::Llm(code)
        })?
    } else {
        execute_packet_proposal_llm(&state, &request).map_err(|code| {
            let _ = finish_failed_run(&state, &prepared, &input, &code);
            SmartDraftError::Llm(code)
        })?
    };

    let parsed = parse_proposal_response(
        &envelope.response_json,
        prepared.decision_mode,
        &prepared.candidate_packet_kinds,
        &prepared.category_catalog,
    )
    .map_err(|code| {
        let _ = finish_failed_run(&state, &prepared, &input, code);
        SmartDraftError::Llm(code.to_string())
    })?;

    if input
        .min_confidence
        .is_some_and(|minimum| parsed.confidence < minimum)
    {
        let run = finish_low_confidence_run(&state, &prepared, &input, &envelope, &parsed)?;
        let item = run.item_id.as_deref().and_then(|item_id| {
            let persistence = state.persistence.lock();
            crate::slices::work_queue::store::get_item_unscoped(
                persistence.connection_ref(),
                &state.client_id,
                item_id,
            )
            .ok()
            .flatten()
        });
        return Ok(SmartDraftResponse { run, item });
    }

    let (run, item) = finish_successful_run(&state, &prepared, &input, &envelope, parsed)?;
    Ok(SmartDraftResponse {
        run,
        item: Some(item),
    })
}

fn record_smart_draft_kickoff(
    state: &AppState,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
) -> Result<RecordedKickoff, SmartDraftError> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    for run in store::running_runs_for_source(
        conn,
        &state.client_id,
        &prepared.source_kind,
        &prepared.source_ref,
    )? {
        if !same_kinds(
            &run.candidate_packet_kinds,
            &prepared.candidate_packet_kinds,
        ) {
            continue;
        }
        let run = maybe_fail_stale_running_run_conn(conn, &state.client_id, &run)?;
        if run.status == PacketProposalRunStatus::Running {
            return Ok(RecordedKickoff {
                run_id: run.run_id,
                replayed: true,
            });
        }
    }
    let outcome = store::insert_run(
        conn,
        &state.client_id,
        NewRun {
            run_id: &prepared.run_id,
            source_kind: &prepared.source_kind,
            source_ref: &prepared.source_ref,
            item_id: prepared
                .existing_item
                .as_ref()
                .map(|entry| entry.item.item_id.as_str()),
            resolved_decision_mode: prepared.decision_mode,
            execution_mode: prepared.execution_mode,
            candidate_packet_kinds: &prepared.candidate_packet_kinds,
            idempotency_key: &format!("{}:run", input.idempotency_key),
            actor_id: &input.actor_id,
            actor_kind: ActorKindDto::Operator,
            now_ms: now_ms(),
        },
    )?;
    Ok(match outcome {
        MutationOutcome::Applied { .. } => RecordedKickoff {
            run_id: prepared.run_id.clone(),
            replayed: false,
        },
        MutationOutcome::ReplayedIdempotent { .. } | MutationOutcome::RevisionConflict { .. } => {
            RecordedKickoff {
                run_id: prepared.run_id.clone(),
                replayed: true,
            }
        }
    })
}

pub fn smart_draft_source_state(
    state: AppState,
    input: SmartDraftSourceStateInput,
) -> Result<SmartDraftSourceStateResponse, SmartDraftError> {
    if input.source_kind.trim().is_empty() || input.source_ref.trim().is_empty() {
        return Err(SmartDraftError::BadRequest(
            "packet_proposal_source_required",
        ));
    }
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    resolve_source_for_smart_draft(
        conn,
        &state.client_id,
        &input.source_kind,
        &input.source_ref,
        &input.scope,
    )?
    .ok_or(SmartDraftError::SourceNotFound)?;
    let item = crate::slices::work_queue::store::get_item_for_source(
        conn,
        &state.client_id,
        &input.source_kind,
        &input.source_ref,
    )?;
    if let Some(existing) = &item {
        if !item_visible_to_scope(conn, &state, &existing.item, &input.scope)? {
            return Err(SmartDraftError::SourceNotFound);
        }
    }
    let expected_revision = item
        .as_ref()
        .filter(|entry| entry.item.status != WorkItemStatus::Accepted)
        .map(|entry| entry.revision);
    let run = if let Some(run_id) = input
        .run_id
        .as_deref()
        .filter(|run_id| !run_id.trim().is_empty())
    {
        let Some(run) = store::get_run(conn, &state.client_id, run_id)? else {
            return Err(SmartDraftError::SourceNotFound);
        };
        if run.source_kind != input.source_kind || run.source_ref != input.source_ref {
            return Err(SmartDraftError::SourceNotFound);
        }
        Some(maybe_fail_stale_running_run_conn(
            conn,
            &state.client_id,
            &run,
        )?)
    } else {
        store::latest_run_for_source(
            conn,
            &state.client_id,
            &input.source_kind,
            &input.source_ref,
        )?
        .map(|run| maybe_fail_stale_running_run_conn(conn, &state.client_id, &run))
        .transpose()?
    };
    Ok(SmartDraftSourceStateResponse {
        item,
        expected_revision,
        run,
    })
}

fn claim_run_or_replay(
    state: &AppState,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
) -> Result<Option<SmartDraftResponse>, SmartDraftError> {
    let existing = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let mut active_running = None;
        for run in store::running_runs_for_source(
            conn,
            &state.client_id,
            &prepared.source_kind,
            &prepared.source_ref,
        )? {
            if !same_kinds(
                &run.candidate_packet_kinds,
                &prepared.candidate_packet_kinds,
            ) {
                continue;
            }
            let run = maybe_fail_stale_running_run_conn(conn, &state.client_id, &run)?;
            if run.status == PacketProposalRunStatus::Running {
                active_running = Some(run);
                break;
            }
        }
        if active_running.is_some() {
            active_running
        } else {
            let outcome = store::insert_run(
                conn,
                &state.client_id,
                NewRun {
                    run_id: &prepared.run_id,
                    source_kind: &prepared.source_kind,
                    source_ref: &prepared.source_ref,
                    item_id: prepared
                        .existing_item
                        .as_ref()
                        .map(|entry| entry.item.item_id.as_str()),
                    resolved_decision_mode: prepared.decision_mode,
                    execution_mode: prepared.execution_mode,
                    candidate_packet_kinds: &prepared.candidate_packet_kinds,
                    idempotency_key: &format!("{}:run", input.idempotency_key),
                    actor_id: &input.actor_id,
                    actor_kind: ActorKindDto::Operator,
                    now_ms: now_ms(),
                },
            )?;
            match outcome {
                MutationOutcome::Applied { .. } => None,
                MutationOutcome::ReplayedIdempotent { .. }
                | MutationOutcome::RevisionConflict { .. } => Some(
                    store::get_run(conn, &state.client_id, &prepared.run_id)?.ok_or_else(|| {
                        StoreError::Domain("packet_proposal_run_not_found".to_string())
                    })?,
                ),
            }
        }
    };
    existing
        .map(|run| response_for_existing_run(state, run))
        .transpose()
}

fn prepare_run(
    conn: &mut Connection,
    state: &AppState,
    input: &SmartDraftInput,
) -> Result<PreparedRun, SmartDraftError> {
    let message = resolve_source_for_smart_draft(
        conn,
        &state.client_id,
        &input.source_kind,
        &input.source_ref,
        &input.scope,
    )?
    .ok_or(SmartDraftError::SourceNotFound)?;
    let category_catalog =
        crate::slices::email_triage::store::list_categories(conn, &state.client_id, now_ms())?;
    if !category_catalog
        .iter()
        .any(|category| category.category_id == message.resolved_category)
    {
        return Err(SmartDraftError::BadRequest(
            "packet_proposal_category_invalid",
        ));
    }

    let policy = crate::slices::work_queue::store::policy_for_category(
        conn,
        &state.client_id,
        &message.resolved_category,
    )?;
    let existing_item = crate::slices::work_queue::store::get_item_for_source(
        conn,
        &state.client_id,
        &input.source_kind,
        &input.source_ref,
    )?;
    if let Some(existing) = &existing_item {
        if !item_visible_to_scope(conn, state, &existing.item, &input.scope)? {
            return Err(SmartDraftError::SourceNotFound);
        }
        if existing.item.status != WorkItemStatus::Accepted {
            match input.expected_revision {
                Some(expected_revision) if expected_revision == existing.revision => {}
                Some(_) => {
                    return Err(SmartDraftError::RevisionConflict {
                        current_revision: Some(existing.revision),
                    });
                }
                None => {
                    return Err(SmartDraftError::Store(StoreError::Domain(
                        "expected_revision_required".to_string(),
                    )));
                }
            }
        }
    }

    let decision_mode = resolve_decision_mode(&message, policy.as_ref(), input.candidate_mode);
    let candidate_packet_kinds =
        candidate_packet_kinds(state, policy.as_ref(), decision_mode, input.candidate_mode);
    if candidate_packet_kinds.is_empty() {
        return Err(SmartDraftError::NoProposalCandidates);
    }

    let item = existing_item
        .as_ref()
        .map(|entry| {
            let mut item = entry.item.clone();
            item.status = WorkItemStatus::Accepted;
            item.accept_actor = Some(WorkItemAcceptActor::System);
            item
        })
        .unwrap_or_else(|| {
            build_system_accepted_item(
                &input.source_kind,
                &input.source_ref,
                &message,
                policy.as_ref(),
            )
        });

    let mut prepared = Vec::new();
    for kind in &candidate_packet_kinds {
        if let Some(prepared_kind) = produce::prepare_proposal_kind(
            conn,
            &state.client_id,
            &item,
            &message,
            &input.scope,
            &input.actor_id,
            kind,
        )? {
            prepared.push(prepared_kind);
        }
    }
    if prepared.is_empty() {
        return Err(SmartDraftError::NoProposalCandidates);
    }
    let candidate_packet_kinds = prepared
        .iter()
        .map(|kind| kind.packet_kind.clone())
        .collect::<Vec<_>>();
    let run_id = smart_draft_run_id(
        &input.source_kind,
        &input.source_ref,
        &input.idempotency_key,
    );

    Ok(PreparedRun {
        run_id,
        source_kind: input.source_kind.clone(),
        source_ref: input.source_ref.clone(),
        message,
        item,
        existing_item,
        candidate_packet_kinds,
        prepared,
        decision_mode,
        execution_mode: packet_proposal_execution_mode(),
        category_catalog,
        scope: input.scope.clone(),
    })
}

fn pre_llm_short_circuit(
    state: &AppState,
    prepared: &PreparedRun,
) -> Result<Option<SmartDraftResponse>, SmartDraftError> {
    if let Some(run) = {
        let persistence = state.persistence.lock();
        store::get_run(
            persistence.connection_ref(),
            &state.client_id,
            &prepared.run_id,
        )?
    } {
        return Ok(Some(response_for_existing_run(state, run)?));
    }
    if let Some(run) = {
        let persistence = state.persistence.lock();
        store::latest_run_for_source(
            persistence.connection_ref(),
            &state.client_id,
            &prepared.source_kind,
            &prepared.source_ref,
        )?
    } {
        if run.status == PacketProposalRunStatus::Completed
            && same_kinds(
                &run.candidate_packet_kinds,
                &prepared.candidate_packet_kinds,
            )
        {
            return Ok(Some(response_for_existing_run(state, run)?));
        }
        if run.status == PacketProposalRunStatus::Running
            && same_kinds(
                &run.candidate_packet_kinds,
                &prepared.candidate_packet_kinds,
            )
        {
            let _ = maybe_fail_stale_running_run(state, &run)?;
        }
    }
    if let Some(existing) = &prepared.existing_item {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let mut outcomes = Vec::new();
        for kind in &prepared.candidate_packet_kinds {
            let Some(draft_id) = produce::active_draft_id_for_kind(
                conn,
                &state.client_id,
                &existing.item.item_id,
                kind,
            )?
            else {
                return Ok(None);
            };
            outcomes.push(PacketProposalKindOutcome {
                packet_kind: kind.clone(),
                status: PacketProposalKindOutcomeStatus::Drafted,
                reason_code: Some(PacketProposalReasonCode::ActiveDraftExists),
                message: None,
                draft_id: Some(draft_id),
            });
        }
        drop(persistence);
        let run = finish_no_spend_active_drafts(state, prepared, &outcomes)?;
        let persistence = state.persistence.lock();
        let item = run.item_id.as_deref().and_then(|item_id| {
            crate::slices::work_queue::store::get_item_unscoped(
                persistence.connection_ref(),
                &state.client_id,
                item_id,
            )
            .ok()
            .flatten()
        });
        return Ok(Some(SmartDraftResponse { run, item }));
    }
    Ok(None)
}

fn finish_no_spend_active_drafts(
    state: &AppState,
    prepared: &PreparedRun,
    outcomes: &[PacketProposalKindOutcome],
) -> Result<PacketProposalRun, SmartDraftError> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::insert_run(
        conn,
        &state.client_id,
        NewRun {
            run_id: &prepared.run_id,
            source_kind: &prepared.source_kind,
            source_ref: &prepared.source_ref,
            item_id: prepared
                .existing_item
                .as_ref()
                .map(|entry| entry.item.item_id.as_str()),
            resolved_decision_mode: prepared.decision_mode,
            execution_mode: prepared.execution_mode,
            candidate_packet_kinds: &prepared.candidate_packet_kinds,
            idempotency_key: &format!("smart_draft:{}:active_drafts:start", prepared.run_id),
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )?;
    store::update_run(
        conn,
        &state.client_id,
        RunUpdate {
            run_id: &prepared.run_id,
            item_id: prepared
                .existing_item
                .as_ref()
                .map(|entry| entry.item.item_id.as_str()),
            status: PacketProposalRunStatus::Completed,
            outcomes,
            model: None,
            confidence: None,
            error_code: None,
            idempotency_key: &format!("smart_draft:{}:active_drafts:finish", prepared.run_id),
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )?;
    store::get_run(conn, &state.client_id, &prepared.run_id)?
        .ok_or_else(|| StoreError::Domain("packet_proposal_run_not_found".to_string()).into())
}

fn finish_failed_run(
    state: &AppState,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
    code: &str,
) -> Result<(), StoreError> {
    let mut persistence = state.persistence.lock();
    finish_failed_run_conn(
        persistence.connection(),
        &state.client_id,
        prepared,
        input,
        code,
    )
}

fn finish_failed_run_conn(
    conn: &mut Connection,
    client_id: &str,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
    code: &str,
) -> Result<(), StoreError> {
    store::update_run(
        conn,
        client_id,
        RunUpdate {
            run_id: &prepared.run_id,
            item_id: prepared
                .existing_item
                .as_ref()
                .map(|entry| entry.item.item_id.as_str()),
            status: PacketProposalRunStatus::Failed,
            outcomes: &[],
            model: None,
            confidence: None,
            error_code: Some(code),
            idempotency_key: &format!("{}:finish_failed", input.idempotency_key),
            actor_id: &input.actor_id,
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )?;
    Ok(())
}

fn finish_low_confidence_run(
    state: &AppState,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
    envelope: &TypedLlmTaskOutputEnvelope,
    parsed: &ParsedProposal,
) -> Result<PacketProposalRun, SmartDraftError> {
    let outcomes = prepared
        .prepared
        .iter()
        .map(|kind| PacketProposalKindOutcome {
            packet_kind: kind.packet_kind.clone(),
            status: PacketProposalKindOutcomeStatus::Unavailable,
            reason_code: Some(PacketProposalReasonCode::LowConfidence),
            message: None,
            draft_id: None,
        })
        .collect::<Vec<_>>();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    store::update_run(
        conn,
        &state.client_id,
        RunUpdate {
            run_id: &prepared.run_id,
            item_id: prepared
                .existing_item
                .as_ref()
                .map(|entry| entry.item.item_id.as_str()),
            status: PacketProposalRunStatus::Completed,
            outcomes: &outcomes,
            model: Some(&envelope.model),
            confidence: Some(confidence_str(parsed.confidence)),
            error_code: None,
            idempotency_key: &format!("{}:finish_low_confidence", input.idempotency_key),
            actor_id: &input.actor_id,
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )?;
    store::get_run(conn, &state.client_id, &prepared.run_id)?
        .ok_or_else(|| StoreError::Domain("packet_proposal_run_not_found".to_string()).into())
}

fn response_for_existing_run(
    state: &AppState,
    run: PacketProposalRun,
) -> Result<SmartDraftResponse, SmartDraftError> {
    let run = maybe_fail_stale_running_run(state, &run)?;
    response_for_run_without_stale(state, run)
}

fn response_for_run_without_stale(
    state: &AppState,
    run: PacketProposalRun,
) -> Result<SmartDraftResponse, SmartDraftError> {
    let persistence = state.persistence.lock();
    let item = run.item_id.as_deref().and_then(|item_id| {
        crate::slices::work_queue::store::get_item_unscoped(
            persistence.connection_ref(),
            &state.client_id,
            item_id,
        )
        .ok()
        .flatten()
    });
    Ok(SmartDraftResponse { run, item })
}

fn maybe_fail_stale_running_run(
    state: &AppState,
    run: &PacketProposalRun,
) -> Result<PacketProposalRun, SmartDraftError> {
    if run.status != PacketProposalRunStatus::Running {
        return Ok(run.clone());
    }
    let threshold_ms = packet_proposal_running_stale_after_ms();
    if now_ms().saturating_sub(run.updated_at_ms) < threshold_ms {
        return Ok(run.clone());
    }
    let mut persistence = state.persistence.lock();
    maybe_fail_stale_running_run_conn(persistence.connection(), &state.client_id, run)
}

fn maybe_fail_stale_running_run_conn(
    conn: &mut Connection,
    client_id: &str,
    run: &PacketProposalRun,
) -> Result<PacketProposalRun, SmartDraftError> {
    if run.status != PacketProposalRunStatus::Running {
        return Ok(run.clone());
    }
    let threshold_ms = packet_proposal_running_stale_after_ms();
    if now_ms().saturating_sub(run.updated_at_ms) < threshold_ms {
        return Ok(run.clone());
    }
    let Some(current) = store::get_run(conn, client_id, &run.run_id)? else {
        return Err(StoreError::Domain("packet_proposal_run_not_found".to_string()).into());
    };
    if current.status == PacketProposalRunStatus::Running
        && now_ms().saturating_sub(current.updated_at_ms) >= threshold_ms
    {
        store::update_run(
            conn,
            client_id,
            RunUpdate {
                run_id: &current.run_id,
                item_id: current.item_id.as_deref(),
                status: PacketProposalRunStatus::Failed,
                outcomes: &current.outcomes,
                model: current.model.as_deref(),
                confidence: current.confidence.as_deref(),
                error_code: Some(STALE_RUNNING_ERROR_CODE),
                idempotency_key: &format!("smart_draft:{}:stale", current.run_id),
                actor_id: "smart_draft",
                actor_kind: ActorKindDto::System,
                now_ms: now_ms(),
            },
        )?;
    }
    store::get_run(conn, client_id, &run.run_id)?
        .ok_or_else(|| StoreError::Domain("packet_proposal_run_not_found".to_string()).into())
}

fn finish_successful_run(
    state: &AppState,
    prepared: &PreparedRun,
    input: &SmartDraftInput,
    envelope: &TypedLlmTaskOutputEnvelope,
    parsed: ParsedProposal,
) -> Result<(PacketProposalRun, WorkItemWithRevision), SmartDraftError> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let mut item = prepared.item.clone();
    if prepared.message.resolved_category == FALLBACK_CATEGORY_ID {
        if let Some(category) = parsed.suggested_category {
            item.category_id = category;
        }
    }
    item.updated_at_ms = now_ms();

    let item_id = item.item_id.clone();
    let item = if prepared.existing_item.is_none() {
        crate::slices::work_queue::store::insert_item_with_actor(
            conn,
            &state.client_id,
            &item,
            "smart_draft",
            ActorKindDto::System,
            &format!("{}:emit_accept", input.idempotency_key),
        )?;
        crate::slices::work_queue::store::get_item_unscoped(conn, &state.client_id, &item_id)?
            .ok_or_else(|| StoreError::Domain("work_item_not_found".to_string()))?
            .item
    } else {
        if prepared
            .existing_item
            .as_ref()
            .is_some_and(|existing| existing.item.status != WorkItemStatus::Accepted)
        {
            match crate::slices::work_queue::store::system_accept_item(
                conn,
                &state.client_id,
                &item_id,
                "smart_draft",
                input.expected_revision,
                &format!("{}:system_accept", input.idempotency_key),
                now_ms(),
            )? {
                MutationOutcome::Applied { .. } | MutationOutcome::ReplayedIdempotent { .. } => {}
                MutationOutcome::RevisionConflict {
                    current_revision, ..
                } => {
                    let _ = finish_failed_run_conn(
                        conn,
                        &state.client_id,
                        prepared,
                        input,
                        "expected_revision_conflict",
                    );
                    return Err(SmartDraftError::RevisionConflict { current_revision });
                }
            }
        }
        crate::slices::work_queue::store::get_item_unscoped(conn, &state.client_id, &item_id)?
            .ok_or_else(|| StoreError::Domain("work_item_not_found".to_string()))?
            .item
    };

    let mut outcomes = Vec::new();
    for (outcome_index, prepared_kind) in prepared.prepared.iter().enumerate() {
        let parsed_outcome = parsed.outcomes.get(&prepared_kind.packet_kind);
        let Some(parsed_outcome) = parsed_outcome else {
            outcomes.push(PacketProposalKindOutcome {
                packet_kind: prepared_kind.packet_kind.clone(),
                status: PacketProposalKindOutcomeStatus::Unavailable,
                reason_code: Some(PacketProposalReasonCode::KindNotRequested),
                message: None,
                draft_id: None,
            });
            continue;
        };
        if parsed_outcome.status != PacketProposalKindOutcomeStatus::Drafted {
            outcomes.push(PacketProposalKindOutcome {
                packet_kind: prepared_kind.packet_kind.clone(),
                status: parsed_outcome.status,
                reason_code: parsed_outcome.reason_code,
                message: None,
                draft_id: None,
            });
            continue;
        }
        let Some(draft_payload) = parsed_outcome.draft.as_ref() else {
            record_rejected_proposal_evidence(
                conn,
                &state.client_id,
                RejectedProposalEvidence {
                    run_id: &prepared.run_id,
                    outcome_index,
                    packet_kind: &prepared_kind.packet_kind,
                    proposal: &parsed_outcome.raw,
                    error_code: "model_output_invalid",
                    message: Some("draft payload missing or invalid"),
                    actor_id: &input.actor_id,
                    now_ms: now_ms(),
                },
            )?;
            outcomes.push(PacketProposalKindOutcome {
                packet_kind: prepared_kind.packet_kind.clone(),
                status: PacketProposalKindOutcomeStatus::Unavailable,
                reason_code: Some(PacketProposalReasonCode::ModelOutputInvalid),
                message: Some("draft payload missing or invalid".to_string()),
                draft_id: None,
            });
            continue;
        };
        let key = format!(
            "smart_draft:{}:{}",
            prepared.run_id, prepared_kind.packet_kind
        );
        match produce::stage_proposal_for_kind(
            &prepared_kind.packet_kind,
            produce::StageContext {
                conn: &mut *conn,
                client_id: &state.client_id,
                actor_id: &input.actor_id,
                item: &item,
                message: &prepared.message,
                response: draft_payload,
                context: &prepared_kind.context,
                model: &envelope.model,
                attempt: prepared_kind.attempt,
                idempotency_key: &key,
                now_ms: now_ms(),
            },
        ) {
            Ok(Some(draft_id)) => outcomes.push(PacketProposalKindOutcome {
                packet_kind: prepared_kind.packet_kind.clone(),
                status: PacketProposalKindOutcomeStatus::Drafted,
                reason_code: None,
                message: None,
                draft_id: Some(draft_id),
            }),
            Ok(None) => outcomes.push(PacketProposalKindOutcome {
                packet_kind: prepared_kind.packet_kind.clone(),
                status: PacketProposalKindOutcomeStatus::RejectedByGate,
                reason_code: Some(PacketProposalReasonCode::StageFailed),
                message: None,
                draft_id: None,
            }),
            Err(err) if matches!(&err.error, StoreError::Domain(_)) => {
                let error_code = produce::store_error_code(&err.error);
                let message = proposal_stage_message(&err, &error_code);
                record_rejected_proposal_evidence(
                    conn,
                    &state.client_id,
                    RejectedProposalEvidence {
                        run_id: &prepared.run_id,
                        outcome_index,
                        packet_kind: &prepared_kind.packet_kind,
                        proposal: draft_payload,
                        error_code: &error_code,
                        message: message.as_deref(),
                        actor_id: &input.actor_id,
                        now_ms: now_ms(),
                    },
                )?;
                outcomes.push(PacketProposalKindOutcome {
                    packet_kind: prepared_kind.packet_kind.clone(),
                    status: PacketProposalKindOutcomeStatus::RejectedByGate,
                    reason_code: Some(PacketProposalReasonCode::GateRejected),
                    message,
                    draft_id: None,
                })
            }
            Err(err) => {
                let error_code = produce::store_error_code(&err.error);
                let message = proposal_stage_message(&err, &error_code);
                record_rejected_proposal_evidence(
                    conn,
                    &state.client_id,
                    RejectedProposalEvidence {
                        run_id: &prepared.run_id,
                        outcome_index,
                        packet_kind: &prepared_kind.packet_kind,
                        proposal: draft_payload,
                        error_code: &error_code,
                        message: message.as_deref(),
                        actor_id: &input.actor_id,
                        now_ms: now_ms(),
                    },
                )?;
                outcomes.push(PacketProposalKindOutcome {
                    packet_kind: prepared_kind.packet_kind.clone(),
                    status: PacketProposalKindOutcomeStatus::Unavailable,
                    reason_code: Some(PacketProposalReasonCode::StageFailed),
                    message,
                    draft_id: None,
                })
            }
        }
    }
    store::update_run(
        conn,
        &state.client_id,
        RunUpdate {
            run_id: &prepared.run_id,
            item_id: Some(&item.item_id),
            status: PacketProposalRunStatus::Completed,
            outcomes: &outcomes,
            model: Some(&envelope.model),
            confidence: Some(confidence_str(parsed.confidence)),
            error_code: None,
            idempotency_key: &format!("{}:finish", input.idempotency_key),
            actor_id: &input.actor_id,
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )?;
    let run = store::get_run(conn, &state.client_id, &prepared.run_id)?
        .ok_or_else(|| StoreError::Domain("packet_proposal_run_not_found".to_string()))?;
    let item =
        crate::slices::work_queue::store::get_item_unscoped(conn, &state.client_id, &item.item_id)?
            .ok_or_else(|| StoreError::Domain("work_item_not_found".to_string()))?;
    Ok((run, item))
}

fn build_system_accepted_item(
    source_kind: &str,
    source_ref: &str,
    message: &InboundMessageRecord,
    policy: Option<&WorkQueuePolicy>,
) -> WorkItem {
    let packet_kinds = policy
        .map(|policy| policy.packet_kinds.clone())
        .unwrap_or_default();
    WorkItem {
        item_id: format!("wi_{source_kind}_{source_ref}"),
        source_kind: source_kind.to_string(),
        source_ref: source_ref.to_string(),
        category_id: message.resolved_category.clone(),
        title: message
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Smart draft")
            .chars()
            .take(160)
            .collect(),
        summary: message.body_excerpt.chars().take(280).collect(),
        packet_kinds,
        status: WorkItemStatus::Accepted,
        accept_actor: Some(WorkItemAcceptActor::System),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: message.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: message
            .source_user_id
            .iter()
            .filter(|user_id| !user_id.trim().is_empty())
            .cloned()
            .collect(),
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    }
}

fn proposal_stage_message(err: &produce::ProposalStageError, error_code: &str) -> Option<String> {
    err.message
        .clone()
        .or_else(|| Some(error_code.to_string()))
        .map(|message| message.chars().take(500).collect())
}

struct RejectedProposalEvidence<'a> {
    run_id: &'a str,
    outcome_index: usize,
    packet_kind: &'a str,
    proposal: &'a serde_json::Value,
    error_code: &'a str,
    message: Option<&'a str>,
    actor_id: &'a str,
    now_ms: u64,
}

fn record_rejected_proposal_evidence(
    conn: &mut Connection,
    client_id: &str,
    evidence: RejectedProposalEvidence<'_>,
) -> Result<(), SmartDraftError> {
    let evidence_id = format!(
        "ppe_{}_{}_{}",
        evidence.run_id, evidence.outcome_index, evidence.packet_kind
    );
    let tool_args_json = serde_json::json!({ "packet_kind": evidence.packet_kind }).to_string();
    let result_excerpt = compact_json_excerpt(&serde_json::json!({
        "packet_kind": evidence.packet_kind,
        "error_code": evidence.error_code,
        "message": evidence.message,
        "proposed_draft": evidence.proposal,
    }));
    store::append_evidence(
        conn,
        client_id,
        NewEvidence {
            evidence_id: &evidence_id,
            run_id: evidence.run_id,
            turn_index: 10_000 + evidence.outcome_index as u32,
            tool_name: "proposal_stage",
            tool_args_json: &tool_args_json,
            result_ref: evidence.error_code,
            result_excerpt: &result_excerpt,
            idempotency_key: &format!(
                "smart_draft:{}:proposal_stage:{}",
                evidence.run_id, evidence.outcome_index
            ),
            actor_id: evidence.actor_id,
            actor_kind: ActorKindDto::System,
            now_ms: evidence.now_ms,
        },
    )?;
    Ok(())
}

fn compact_json_excerpt(value: &serde_json::Value) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    raw.chars().take(12_000).collect()
}

fn build_proposal_request(client_id: &str, prepared: &PreparedRun) -> TypedLlmTaskRequest {
    let shared_background = shared_background_for_prompt(&prepared.prepared);
    let tool_loop = prepared.execution_mode == PacketProposalExecutionMode::ToolLoopAgentic;
    let mut capabilities = TypedLlmTaskCapabilities::pure_transformation();
    if tool_loop {
        capabilities.tools = true;
        capabilities.multi_step = true;
    }
    let packet_contracts: Vec<_> = prepared
        .prepared
        .iter()
        .map(|kind| {
            let prepared_context = if shared_background.is_some() {
                context_without_background(&kind.context)
            } else {
                kind.context.clone()
            };
            json!({
                "packet_kind": kind.packet_kind,
                "response_key": kind.contract.response_key,
                "schema_ref": kind.contract.schema_ref,
                "instructions": kind.contract.instructions,
                "evidence_requirements": produce::proposal_evidence_requirements(&kind.packet_kind),
                "context_ref": shared_background.as_ref().map(|_| "shared_context"),
                "prepared_context": prepared_context,
            })
        })
        .collect();
    let category_catalog: Vec<_> = prepared
        .category_catalog
        .iter()
        .map(|category| {
            json!({
                "category_id": category.category_id,
                "description": category.description,
            })
        })
        .collect();
    let mut text_blocks = vec![TypedLlmTextBlock {
        block_id: "source".to_string(),
        text: format!(
            "From: {}\nTo: {}\nSubject: {}\n{}\nLabels: {}\n\n{}",
            prepared.message.from_addr.as_deref().unwrap_or("(unknown)"),
            prepared.message.to_addr.as_deref().unwrap_or("(unknown)"),
            prepared
                .message
                .subject
                .as_deref()
                .unwrap_or("(no subject)"),
            crate::slices::datetime_input::email_prompt_datetime_context(&prepared.message),
            prepared.message.labels.join(", "),
            crate::slices::email_triage::service::body_for_ai(&prepared.message)
        ),
    }];
    if let Some(block) = shared_background {
        text_blocks.push(TypedLlmTextBlock {
            block_id: "shared_context".to_string(),
            text: block.text,
        });
    }
    TypedLlmTaskRequest {
        task_id: format!("packet_proposal_{}", prepared.run_id),
        correlation_id: prepared.run_id.clone(),
        idempotency_key: format!("packet_proposal:{}", prepared.run_id),
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: prepared.source_kind.clone(),
            entity_id: prepared.source_ref.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "packet_proposal_bounded_typed".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: PROPOSAL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 96 * 1024,
            max_output_bytes: 32 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities,
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": proposal_instructions(tool_loop),
                "resolved_decision_mode": prepared.decision_mode,
                "current_category": prepared.message.resolved_category,
                "category_catalog": category_catalog,
                "packet_contracts": packet_contracts,
                "available_tools": if tool_loop { json!(packet_proposal_tool_names()) } else { json!([]) },
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
    }
}

fn proposal_instructions(tool_loop: bool) -> &'static str {
    if tool_loop {
        "Smart draft for BusinessOS. Decide and fill existing packet-kind drafts for this ONE source. You may use the read-only email_thread_lookup tool to inspect the current source or local email thread. Respond finally with a single JSON object with fields: suggested_category (category_id from category_catalog or null), confidence (\"high\" | \"medium\" | \"low\"), rationale (one sentence), outcomes (array). Each outcome must have packet_kind from packet_contracts, status (\"drafted\" or \"unavailable\"), optional reason_code, and when status=\"drafted\" a draft object matching that packet kind's schema_ref. In fill_fixed mode, include every packet_contract and do not decline; use unavailable only when the source lacks required facts. In ai_decides mode, include drafted outputs only when warranted; omitted candidates are treated as unavailable. Never invent facts not grounded in the source or tool results; shared_context is tone/context only."
    } else {
        "Smart draft for BusinessOS. Decide and fill existing packet-kind drafts for this ONE source. Respond with a single JSON object with fields: suggested_category (category_id from category_catalog or null), confidence (\"high\" | \"medium\" | \"low\"), rationale (one sentence), outcomes (array). Each outcome must have packet_kind from packet_contracts, status (\"drafted\" or \"unavailable\"), optional reason_code, and when status=\"drafted\" a draft object matching that packet kind's schema_ref. In fill_fixed mode, include every packet_contract and do not decline; use unavailable only when the source lacks required facts. In ai_decides mode, include drafted outputs only when warranted; omitted candidates are treated as unavailable. Never invent facts not grounded in the source; shared_context is tone/context only."
    }
}

fn packet_proposal_running_stale_after_ms() -> u64 {
    #[cfg(test)]
    if let Some(value) = test_stale_running_after_ms() {
        return value;
    }
    env_registry::string(&env_registry::BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS)
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(3_600_000)
}

fn packet_proposal_execution_mode() -> PacketProposalExecutionMode {
    #[cfg(test)]
    if let Some(mode) = test_execution_mode() {
        return mode;
    }
    EXECUTION_MODE_BOUNDED_TYPED
}

fn packet_proposal_tool_loop_enabled(state: &AppState) -> bool {
    #[cfg(test)]
    if let Some(value) = test_tool_loop_enabled() {
        return value;
    }
    let persistence = state.persistence.lock();
    crate::slices::admin_settings::service::flag(
        persistence.connection_ref(),
        &state.client_id,
        &env_registry::BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED,
    )
    .unwrap_or_else(|err| {
        tracing::warn!(error = %err, "packet proposal config read failed");
        false
    })
}

fn execute_packet_proposal_llm(
    state: &AppState,
    request: &TypedLlmTaskRequest,
) -> Result<TypedLlmTaskOutputEnvelope, String> {
    #[cfg(test)]
    if let Some(response_json) = take_test_packet_proposal_response(request) {
        return Ok(TypedLlmTaskOutputEnvelope {
            task_id: request.task_id.clone(),
            execution_route: TypedLlmExecutionRoute::Harness,
            provider_id: "test".to_string(),
            model: "test-model".to_string(),
            schema_ref: request.spec.schema_ref.clone(),
            raw_response_hash: "test".to_string(),
            response_json,
            usage: None,
            finish_reason: Some("stop".to_string()),
            latency_ms: 0,
            retry_count: 0,
            provider_request_id: None,
            correlation_id: request.correlation_id.clone(),
        });
    }
    crate::slices::ai_usage::service::execute_recorded(
        state.persistence.clone(),
        &state.client_id,
        PROPOSAL_PURPOSE,
        request,
    )
    .map_err(|err| err.code().to_string())
}

fn execute_packet_proposal_tool_loop(
    state: &AppState,
    prepared: &PreparedRun,
    request: &TypedLlmTaskRequest,
) -> Result<TypedLlmTaskOutputEnvelope, String> {
    if !packet_proposal_tool_loop_enabled(state) {
        return Err(TOOL_LOOP_UNAVAILABLE_ERROR_CODE.to_string());
    }
    let tools = packet_proposal_tool_definitions();
    let limits = packet_proposal_tool_loop_limits();
    #[cfg(test)]
    if let Some(turns) = take_test_packet_proposal_tool_loop_turns() {
        let client = MockScriptedDirectLlmClient::new(turns);
        return crate::slices::ai_usage::service::execute_tool_loop_recorded_with_client(
            crate::slices::ai_usage::service::ToolLoopRecordedRequest {
                persistence: state.persistence.clone(),
                client_id: &state.client_id,
                purpose: PROPOSAL_PURPOSE,
                request,
                tools: &tools,
                limits,
            },
            &client,
            |turn_index, call| execute_packet_proposal_tool(state, prepared, turn_index, call),
        )
        .map_err(map_tool_loop_error);
    }
    crate::slices::ai_usage::service::execute_tool_loop_recorded(
        state.persistence.clone(),
        &state.client_id,
        PROPOSAL_PURPOSE,
        request,
        &tools,
        limits,
        |turn_index, call| execute_packet_proposal_tool(state, prepared, turn_index, call),
    )
    .map_err(map_tool_loop_error)
}

fn packet_proposal_tool_loop_limits() -> crate::slices::ai_usage::service::ToolLoopLimits {
    #[cfg(test)]
    if let Some(limits) = test_tool_loop_limits() {
        return limits;
    }
    crate::slices::ai_usage::service::ToolLoopLimits {
        max_turns: TOOL_LOOP_MAX_TURNS,
        max_tool_calls: TOOL_LOOP_MAX_TOOL_CALLS,
        max_evidence_bytes: TOOL_LOOP_MAX_EVIDENCE_BYTES,
        wall_clock_ms: TOOL_LOOP_WALL_CLOCK_MS,
    }
}

fn map_tool_loop_error(err: bos_kernel::AppError) -> String {
    match err.code() {
        crate::slices::ai_usage::service::TOOL_LOOP_EXHAUSTED_CODE => {
            TOOL_LOOP_EXHAUSTED_ERROR_CODE.to_string()
        }
        crate::slices::ai_usage::service::TOOL_LOOP_UNAVAILABLE_CODE
        | "direct_llm_tools_unsupported"
        | "direct_llm_task_route_not_direct"
        | "direct_llm_task_requires_harness"
        | "llm_api_not_configured"
        | "llm_api_model_not_configured"
        | "llm_harness_model_not_configured" => TOOL_LOOP_UNAVAILABLE_ERROR_CODE.to_string(),
        other => other.to_string(),
    }
}

fn packet_proposal_tool_definitions() -> Vec<DirectLlmToolDefinition> {
    crate::slices::grounding::grounding_tool_definitions_for(packet_proposal_tool_names())
}

fn packet_proposal_tool_names() -> &'static [&'static str] {
    &[
        crate::slices::grounding::TOOL_EMAIL_THREAD_LOOKUP,
        crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP,
        crate::slices::grounding::TOOL_ORDER_STATUS_LOOKUP,
        crate::slices::grounding::TOOL_PRODUCT_LOOKUP,
        crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP,
        crate::slices::grounding::TOOL_CUSTOMER_INVOICE_HISTORY,
        crate::slices::grounding::TOOL_CALL_TRANSCRIPT_LOOKUP,
    ]
}

#[cfg(test)]
pub(crate) fn test_packet_proposal_tool_names() -> &'static [&'static str] {
    packet_proposal_tool_names()
}

fn execute_packet_proposal_tool(
    state: &AppState,
    prepared: &PreparedRun,
    turn_index: u32,
    call: &DirectLlmToolCall,
) -> bos_kernel::AppResult<DirectLlmToolResult> {
    if !packet_proposal_tool_names().contains(&call.name.as_str()) {
        return record_packet_tool_result(
            state,
            prepared,
            turn_index,
            call,
            "tool_denied",
            "packet_proposal_tool_unknown",
            json!({
                "ok": false,
                "error_code": "packet_proposal_tool_unknown",
                "records": [],
            }),
        );
    }
    let payload = match call.name.as_str() {
        crate::slices::grounding::TOOL_EMAIL_THREAD_LOOKUP => {
            let requested_source = call
                .arguments
                .get("source_ref")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&prepared.source_ref);
            if requested_source != prepared.source_ref {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_source_out_of_scope",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_source_out_of_scope",
                        "records": [],
                    }),
                );
            }
            let scope = call
                .arguments
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("source");
            if scope != "source" && scope != "thread" {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_scope",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_scope",
                        "records": [],
                    }),
                );
            }
            PacketToolPayload::EmailThread {
                tool_scope: scope.to_string(),
            }
        }
        crate::slices::grounding::TOOL_CRM_CONTACT_LOOKUP => {
            let email = optional_tool_string(call, "email");
            let company = optional_tool_string(call, "company");
            if email.is_none() && company.is_none() {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            }
            if !email
                .as_deref()
                .is_none_or(|email| packet_tool_email_in_source_scope(prepared, email))
                || !company
                    .as_deref()
                    .is_none_or(|company| packet_tool_text_in_source_scope(prepared, company))
            {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::CrmContact { email, company }
        }
        crate::slices::grounding::TOOL_ORDER_STATUS_LOOKUP => {
            let Some(query) = optional_tool_string(call, "query") else {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            };
            if !packet_tool_identifier_in_source_scope(prepared, &query) {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::OrderStatus { query }
        }
        crate::slices::grounding::TOOL_PRODUCT_LOOKUP => {
            let Some(query) = optional_tool_string(call, "query") else {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            };
            if !packet_tool_text_in_source_scope(prepared, &query) {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::Product { query }
        }
        crate::slices::grounding::TOOL_PRIOR_CONVERSATION_LOOKUP => {
            let Some(sender_email) = optional_tool_string(call, "sender_email") else {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            };
            if !packet_tool_current_sender(prepared)
                .as_deref()
                .is_some_and(|sender| sender.eq_ignore_ascii_case(&sender_email))
            {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::PriorConversation { sender_email }
        }
        crate::slices::grounding::TOOL_CUSTOMER_INVOICE_HISTORY => {
            let email = optional_tool_string(call, "email");
            let name = optional_tool_string(call, "name");
            if email.is_none() && name.is_none() {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            }
            if !email
                .as_deref()
                .is_none_or(|email| packet_tool_email_in_source_scope(prepared, email))
                || !name
                    .as_deref()
                    .is_none_or(|name| packet_tool_text_in_source_scope(prepared, name))
            {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::CustomerInvoiceHistory { email, name }
        }
        crate::slices::grounding::TOOL_CALL_TRANSCRIPT_LOOKUP => {
            let Some(query) = optional_tool_string(call, "query") else {
                return record_packet_tool_result(
                    state,
                    prepared,
                    turn_index,
                    call,
                    "tool_denied",
                    "packet_proposal_tool_invalid_query",
                    json!({
                        "ok": false,
                        "error_code": "packet_proposal_tool_invalid_query",
                        "records": [],
                    }),
                );
            };
            if !matches!(prepared.scope, OperatorScope::All)
                || !packet_tool_text_in_source_scope(prepared, &query)
            {
                return record_packet_tool_out_of_scope(state, prepared, turn_index, call);
            }
            PacketToolPayload::CallTranscript { query }
        }
        _ => unreachable!("tool allowlist already checked"),
    };
    let (result_json, result_ref, excerpt) = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let (result_json, result_ref, excerpt) = match payload {
            PacketToolPayload::EmailThread { tool_scope } => {
                crate::slices::grounding::email_thread_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    &prepared.source_ref,
                    prepared.message.thread_id.as_deref(),
                    &tool_scope,
                )
            }
            PacketToolPayload::CrmContact { email, company } => {
                crate::slices::grounding::crm_contact_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    email.as_deref(),
                    company.as_deref(),
                )
            }
            PacketToolPayload::OrderStatus { query } => {
                crate::slices::grounding::order_status_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    &query,
                )
            }
            PacketToolPayload::Product { query } => crate::slices::grounding::product_tool_payload(
                conn,
                &state.client_id,
                &prepared.scope,
                &query,
            ),
            PacketToolPayload::PriorConversation { sender_email } => {
                crate::slices::grounding::prior_conversation_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    &sender_email,
                    Some(&prepared.source_ref),
                )
            }
            PacketToolPayload::CustomerInvoiceHistory { email, name } => {
                crate::slices::grounding::customer_invoice_history_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    state.accounting_visibility_policy,
                    email.as_deref(),
                    name.as_deref(),
                    crate::http::now_ms(),
                )
            }
            PacketToolPayload::CallTranscript { query } => {
                crate::slices::grounding::call_transcript_tool_payload(
                    conn,
                    &state.client_id,
                    &prepared.scope,
                    &query,
                )
            }
        }
        .map_err(packet_tool_store_error)?;
        let result_json_text = serde_json::to_string(&result_json).unwrap_or_default();
        append_packet_tool_evidence(
            conn,
            &state.client_id,
            prepared,
            turn_index,
            call,
            &result_ref,
            &excerpt,
        )?;
        let bounded_json = if result_json_text.len() > TOOL_LOOP_MAX_EVIDENCE_BYTES {
            json!({
                "ok": true,
                "result_ref": result_ref,
                "truncated": true,
                "excerpt": excerpt,
            })
        } else {
            result_json
        };
        (bounded_json, result_ref, excerpt)
    };
    Ok(DirectLlmToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
        result_json: json!({
            "result_ref": result_ref,
            "excerpt": excerpt,
            "payload": result_json,
        }),
    })
}

enum PacketToolPayload {
    EmailThread {
        tool_scope: String,
    },
    CrmContact {
        email: Option<String>,
        company: Option<String>,
    },
    OrderStatus {
        query: String,
    },
    Product {
        query: String,
    },
    PriorConversation {
        sender_email: String,
    },
    CustomerInvoiceHistory {
        email: Option<String>,
        name: Option<String>,
    },
    CallTranscript {
        query: String,
    },
}

fn optional_tool_string(call: &DirectLlmToolCall, key: &str) -> Option<String> {
    call.arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn record_packet_tool_out_of_scope(
    state: &AppState,
    prepared: &PreparedRun,
    turn_index: u32,
    call: &DirectLlmToolCall,
) -> bos_kernel::AppResult<DirectLlmToolResult> {
    record_packet_tool_result(
        state,
        prepared,
        turn_index,
        call,
        "tool_denied",
        "packet_proposal_tool_query_out_of_scope",
        json!({
            "ok": false,
            "error_code": "packet_proposal_tool_query_out_of_scope",
            "records": [],
        }),
    )
}

fn packet_tool_current_sender(prepared: &PreparedRun) -> Option<String> {
    packet_tool_normalized_email(prepared.message.from_addr.as_deref()?)
}

fn packet_tool_email_in_source_scope(prepared: &PreparedRun, raw: &str) -> bool {
    let Some(email) = packet_tool_normalized_email(raw) else {
        return false;
    };
    packet_tool_current_sender(prepared)
        .as_deref()
        .is_some_and(|sender| sender == email)
        || packet_tool_source_text(prepared).contains(&email)
}

fn packet_tool_text_in_source_scope(prepared: &PreparedRun, raw: &str) -> bool {
    let value = raw.trim().to_ascii_lowercase();
    if value.len() < 3 {
        return false;
    }
    packet_tool_source_text(prepared).contains(&value)
}

fn packet_tool_identifier_in_source_scope(prepared: &PreparedRun, raw: &str) -> bool {
    let needle = packet_tool_normalize_identifier(raw);
    if needle.len() < 3 {
        return false;
    }
    packet_tool_normalize_identifier(&packet_tool_source_text(prepared)).contains(&needle)
}

fn packet_tool_source_text(prepared: &PreparedRun) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        prepared.message.from_addr.as_deref().unwrap_or(""),
        prepared.message.to_addr.as_deref().unwrap_or(""),
        prepared.message.subject.as_deref().unwrap_or(""),
        prepared.message.labels.join(" "),
        crate::slices::email_triage::service::body_for_ai(&prepared.message)
    )
    .to_ascii_lowercase()
}

fn packet_tool_normalized_email(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    (value.contains('@') && !value.contains(char::is_whitespace)).then_some(value)
}

fn packet_tool_normalize_identifier(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn record_packet_tool_result(
    state: &AppState,
    prepared: &PreparedRun,
    turn_index: u32,
    call: &DirectLlmToolCall,
    result_ref: &str,
    excerpt: &str,
    result_json: serde_json::Value,
) -> bos_kernel::AppResult<DirectLlmToolResult> {
    {
        let mut persistence = state.persistence.lock();
        append_packet_tool_evidence(
            persistence.connection(),
            &state.client_id,
            prepared,
            turn_index,
            call,
            result_ref,
            excerpt,
        )?;
    }
    Ok(DirectLlmToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
        result_json: json!({
            "result_ref": result_ref,
            "excerpt": excerpt,
            "payload": result_json,
        }),
    })
}

fn append_packet_tool_evidence(
    conn: &mut Connection,
    client_id: &str,
    prepared: &PreparedRun,
    turn_index: u32,
    call: &DirectLlmToolCall,
    result_ref: &str,
    result_excerpt: &str,
) -> bos_kernel::AppResult<()> {
    let evidence_id = packet_proposal_evidence_id(&prepared.run_id, turn_index, &call.id);
    let tool_args_json =
        serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
    store::append_evidence(
        conn,
        client_id,
        NewEvidence {
            evidence_id: &evidence_id,
            run_id: &prepared.run_id,
            turn_index,
            tool_name: &call.name,
            tool_args_json: &tool_args_json,
            result_ref,
            result_excerpt,
            idempotency_key: &format!(
                "smart_draft:{}:evidence:{turn_index}:{}",
                prepared.run_id, call.id
            ),
            actor_id: "smart_draft",
            actor_kind: ActorKindDto::System,
            now_ms: now_ms(),
        },
    )
    .map(|_| ())
    .map_err(packet_tool_store_error)
}

fn packet_proposal_evidence_id(run_id: &str, turn_index: u32, call_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(turn_index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(call_id.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("ppre_");
    for byte in digest.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn packet_tool_store_error(err: StoreError) -> bos_kernel::AppError {
    bos_kernel::AppError::unexpected(
        "packet_proposal_tool_store_failed",
        format!("packet proposal tool store failed: {err}"),
        bos_kernel::CorrelationId::generate(),
    )
}

fn parse_proposal_response(
    response: &serde_json::Value,
    decision_mode: PacketProposalDecisionMode,
    candidates: &[String],
    categories: &[CategoryRecord],
) -> Result<ParsedProposal, &'static str> {
    let confidence = proposal_confidence(response);
    let suggested_category = response
        .get("suggested_category")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string);
    if let Some(category) = &suggested_category {
        if !categories
            .iter()
            .any(|record| record.category_id == *category)
        {
            return Err("packet_proposal_category_invalid");
        }
    }
    let candidate_set: BTreeSet<&str> = candidates.iter().map(String::as_str).collect();
    let mut outcomes = BTreeMap::new();
    let Some(rows) = response
        .get("outcomes")
        .and_then(serde_json::Value::as_array)
    else {
        return Err("packet_proposal_output_invalid");
    };
    for row in rows {
        let Some(kind) = row
            .get("packet_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|kind| candidate_set.contains(*kind))
        else {
            continue;
        };
        let status = match row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("drafted")
        {
            "drafted" => PacketProposalKindOutcomeStatus::Drafted,
            "unavailable" => PacketProposalKindOutcomeStatus::Unavailable,
            "rejected_by_gate" => PacketProposalKindOutcomeStatus::RejectedByGate,
            _ => PacketProposalKindOutcomeStatus::Unavailable,
        };
        let reason_code = match row.get("reason_code").filter(|value| !value.is_null()) {
            Some(value) => value.as_str().and_then(parse_reason_code).or_else(|| {
                (status == PacketProposalKindOutcomeStatus::Unavailable)
                    .then_some(PacketProposalReasonCode::ModelOutputInvalid)
            }),
            None => (status == PacketProposalKindOutcomeStatus::Unavailable)
                .then_some(PacketProposalReasonCode::ContextUnavailable),
        };
        let draft = row.get("draft").cloned().or_else(|| {
            produce::proposal_contract_for_kind(kind)
                .and_then(|contract| row.get(contract.response_key).cloned())
        });
        outcomes.insert(
            kind.to_string(),
            ParsedOutcome {
                status,
                reason_code,
                draft,
                raw: row.clone(),
            },
        );
    }
    if decision_mode == PacketProposalDecisionMode::FillFixed {
        for kind in candidates {
            outcomes.entry(kind.clone()).or_insert(ParsedOutcome {
                status: PacketProposalKindOutcomeStatus::Unavailable,
                reason_code: Some(PacketProposalReasonCode::ModelOutputInvalid),
                draft: None,
                raw: serde_json::Value::Null,
            });
        }
    }
    Ok(ParsedProposal {
        suggested_category,
        confidence,
        outcomes,
    })
}

fn proposal_confidence(response: &serde_json::Value) -> AiConfidence {
    response
        .get("confidence")
        .and_then(serde_json::Value::as_str)
        .and_then(AiConfidence::parse)
        .or_else(|| {
            response
                .get("outcomes")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|row| {
                    row.get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("drafted")
                        == "drafted"
                })
                .filter_map(|row| {
                    proposal_draft_payload(row)
                        .and_then(|draft| draft.get("confidence"))
                        .or_else(|| row.get("confidence"))
                        .and_then(serde_json::Value::as_str)
                        .and_then(AiConfidence::parse)
                })
                .min()
        })
        .unwrap_or(AiConfidence::Low)
}

fn proposal_draft_payload(row: &serde_json::Value) -> Option<&serde_json::Value> {
    row.get("draft").or_else(|| {
        row.get("packet_kind")
            .and_then(serde_json::Value::as_str)
            .and_then(produce::proposal_contract_for_kind)
            .and_then(|contract| row.get(contract.response_key))
    })
}

fn confidence_str(confidence: AiConfidence) -> &'static str {
    match confidence {
        AiConfidence::Low => "low",
        AiConfidence::Medium => "medium",
        AiConfidence::High => "high",
    }
}

fn resolve_source_for_smart_draft(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
    scope: &OperatorScope,
) -> Result<Option<InboundMessageRecord>, SmartDraftError> {
    if source_kind == crate::slices::work_queue::SOURCE_KIND_EMAIL {
        let mut messages = crate::slices::email_triage::store::inbound_by_source_keys(
            conn,
            client_id,
            &[source_ref.to_string()],
            scope,
        )?;
        return Ok((!messages.is_empty()).then(|| messages.remove(0)));
    }
    let item = WorkItem {
        item_id: format!("source_probe_{source_kind}_{source_ref}"),
        source_kind: source_kind.to_string(),
        source_ref: source_ref.to_string(),
        category_id: FALLBACK_CATEGORY_ID.to_string(),
        title: String::new(),
        summary: String::new(),
        packet_kinds: Vec::new(),
        status: WorkItemStatus::Accepted,
        accept_actor: Some(WorkItemAcceptActor::System),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: None,
        assignee_user_id: None,
        visible_to_user_ids: Vec::new(),
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    produce::resolve_source(conn, client_id, &item).map_err(|err| match err {
        produce::SourceError::Unsupported => SmartDraftError::SourceUnsupported,
        produce::SourceError::Store(err) => SmartDraftError::Store(err),
    })
}

fn item_visible_to_scope(
    conn: &Connection,
    state: &AppState,
    item: &WorkItem,
    scope: &OperatorScope,
) -> Result<bool, StoreError> {
    Ok(crate::slices::work_queue::store::get_item_scoped(
        conn,
        &state.client_id,
        &item.item_id,
        scope,
    )?
    .is_some())
}

fn resolve_decision_mode(
    message: &InboundMessageRecord,
    policy: Option<&WorkQueuePolicy>,
    candidate_mode: SmartDraftCandidateMode,
) -> PacketProposalDecisionMode {
    if candidate_mode == SmartDraftCandidateMode::AllEnabled {
        return PacketProposalDecisionMode::AiDecides;
    }
    if message.resolved_category == FALLBACK_CATEGORY_ID {
        return PacketProposalDecisionMode::AiDecides;
    }
    let suggest_on = policy.is_some_and(|policy| !policy.ai_suggestible_packet_kinds.is_empty());
    if suggest_on {
        PacketProposalDecisionMode::AiDecides
    } else {
        PacketProposalDecisionMode::FillFixed
    }
}

fn candidate_packet_kinds(
    state: &AppState,
    policy: Option<&WorkQueuePolicy>,
    decision_mode: PacketProposalDecisionMode,
    candidate_mode: SmartDraftCandidateMode,
) -> Vec<String> {
    if candidate_mode == SmartDraftCandidateMode::AllEnabled {
        return enabled_proposal_kinds(state);
    }
    let mut kinds = match decision_mode {
        PacketProposalDecisionMode::FillFixed => policy
            .map(|policy| policy.packet_kinds.clone())
            .unwrap_or_default(),
        PacketProposalDecisionMode::AiDecides => {
            let Some(policy) = policy else {
                return enabled_proposal_kinds(state);
            };
            let allow_all = policy
                .ai_suggestible_packet_kinds
                .iter()
                .any(|kind| kind == AI_SUGGEST_ALL_SENTINEL);
            if allow_all || policy.ai_suggestible_packet_kinds.is_empty() {
                enabled_proposal_kinds(state)
            } else {
                policy.ai_suggestible_packet_kinds.clone()
            }
        }
    };
    kinds.retain(|kind| proposal_kind_enabled_for_client(state, kind));
    kinds.sort();
    kinds.dedup();
    kinds
}

fn enabled_proposal_kinds(state: &AppState) -> Vec<String> {
    PROPOSAL_ENABLED_KINDS
        .iter()
        .filter(|kind| proposal_kind_enabled_for_client(state, kind))
        .map(|kind| (*kind).to_string())
        .collect()
}

fn proposal_kind_enabled_for_client(state: &AppState, kind: &str) -> bool {
    produce::proposal_enabled_packet_kind(kind)
        && crate::slices::work_queue::packet_kind_slice(kind)
            .is_some_and(|slice| state.slice_enabled(slice))
}

fn background_block(context: &serde_json::Value) -> Option<TypedLlmTextBlock> {
    context
        .get("background")
        .and_then(|value| serde_json::from_value::<TypedLlmTextBlock>(value.clone()).ok())
}

pub(crate) fn shared_background_for_prompt(
    prepared: &[PreparedProposalKind],
) -> Option<TypedLlmTextBlock> {
    let mut backgrounds = prepared
        .iter()
        .filter_map(|kind| background_block(&kind.context));
    let first = backgrounds.next()?;
    if backgrounds.all(|block| block.block_id == first.block_id && block.text == first.text) {
        Some(first)
    } else {
        None
    }
}

fn context_without_background(context: &serde_json::Value) -> serde_json::Value {
    let Some(object) = context.as_object() else {
        return context.clone();
    };
    let mut object = object.clone();
    object.remove("background");
    serde_json::Value::Object(object)
}

fn parse_reason_code(raw: &str) -> Option<PacketProposalReasonCode> {
    match raw {
        "active_draft_exists" => Some(PacketProposalReasonCode::ActiveDraftExists),
        "category_invalid" => Some(PacketProposalReasonCode::CategoryInvalid),
        "context_unavailable" => Some(PacketProposalReasonCode::ContextUnavailable),
        "gate_rejected" => Some(PacketProposalReasonCode::GateRejected),
        "kind_not_enabled" => Some(PacketProposalReasonCode::KindNotEnabled),
        "kind_not_requested" => Some(PacketProposalReasonCode::KindNotRequested),
        "model_output_invalid" => Some(PacketProposalReasonCode::ModelOutputInvalid),
        "source_missing" => Some(PacketProposalReasonCode::SourceMissing),
        "source_unsupported" => Some(PacketProposalReasonCode::SourceUnsupported),
        "stage_failed" => Some(PacketProposalReasonCode::StageFailed),
        _ => None,
    }
}

fn same_kinds(left: &[String], right: &[String]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

fn smart_draft_run_id(source_kind: &str, source_ref: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_ref.as_bytes());
    hasher.update(b"\0");
    hasher.update(idempotency_key.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("ppr_");
    for byte in digest.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
pub(crate) fn test_smart_draft_run_id(
    source_kind: &str,
    source_ref: &str,
    idempotency_key: &str,
) -> String {
    smart_draft_run_id(source_kind, source_ref, idempotency_key)
}

#[cfg(test)]
static TEST_PACKET_PROPOSAL_RESPONSE: OnceLock<Mutex<Option<serde_json::Value>>> = OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_REQUESTS: OnceLock<Mutex<Vec<TypedLlmTaskRequest>>> = OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_LLM_GATE: OnceLock<Mutex<Option<Arc<TestPacketProposalLlmGate>>>> =
    OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_EXECUTION_MODE: OnceLock<Mutex<Option<PacketProposalExecutionMode>>> =
    OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_STALE_AFTER_MS: OnceLock<Mutex<Option<u64>>> = OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_TOOL_LOOP_ENABLED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_TOOL_LOOP_TURNS: OnceLock<
    Mutex<Option<Vec<DirectLlmToolTurnResponse>>>,
> = OnceLock::new();
#[cfg(test)]
static TEST_PACKET_PROPOSAL_TOOL_LOOP_LIMITS: OnceLock<
    Mutex<Option<crate::slices::ai_usage::service::ToolLoopLimits>>,
> = OnceLock::new();

#[cfg(test)]
fn test_mutex_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
fn test_condvar_wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar.wait(guard).unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
pub(crate) struct TestPacketProposalLlmGate {
    entered: (Mutex<bool>, Condvar),
    released: (Mutex<bool>, Condvar),
}

#[cfg(test)]
impl TestPacketProposalLlmGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: (Mutex::new(false), Condvar::new()),
            released: (Mutex::new(false), Condvar::new()),
        })
    }

    pub(crate) fn wait_entered(&self) {
        let (lock, condvar) = &self.entered;
        let mut entered = test_mutex_lock(lock);
        while !*entered {
            entered = test_condvar_wait(condvar, entered);
        }
    }

    pub(crate) fn release(&self) {
        let (lock, condvar) = &self.released;
        *test_mutex_lock(lock) = true;
        condvar.notify_all();
    }

    fn enter_and_wait(&self) {
        let (entered_lock, entered_condvar) = &self.entered;
        *test_mutex_lock(entered_lock) = true;
        entered_condvar.notify_all();

        let (released_lock, released_condvar) = &self.released;
        let mut released = test_mutex_lock(released_lock);
        while !*released {
            released = test_condvar_wait(released_condvar, released);
        }
    }
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_llm_gate(gate: Arc<TestPacketProposalLlmGate>) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_LLM_GATE.get_or_init(|| Mutex::new(None))) = Some(gate);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_llm_gate() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_LLM_GATE.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_response(response: serde_json::Value) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_RESPONSE.get_or_init(|| Mutex::new(None))) =
        Some(response);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_response() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_RESPONSE.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
pub(crate) fn reset_test_packet_proposal_state() {
    clear_test_packet_proposal_response();
    let _ = take_test_packet_proposal_requests();
}

#[cfg(test)]
pub(crate) fn test_packet_proposal_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    test_mutex_lock(LOCK.get_or_init(|| Mutex::new(())))
}

#[cfg(test)]
pub(crate) fn take_test_packet_proposal_requests() -> Vec<TypedLlmTaskRequest> {
    std::mem::take(&mut *test_mutex_lock(
        TEST_PACKET_PROPOSAL_REQUESTS.get_or_init(|| Mutex::new(Vec::new())),
    ))
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_execution_mode(mode: PacketProposalExecutionMode) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_EXECUTION_MODE.get_or_init(|| Mutex::new(None))) =
        Some(mode);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_execution_mode() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_EXECUTION_MODE.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_stale_after_ms(value: u64) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_STALE_AFTER_MS.get_or_init(|| Mutex::new(None))) =
        Some(value);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_stale_after_ms() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_STALE_AFTER_MS.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_tool_loop_enabled(value: bool) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_ENABLED.get_or_init(|| Mutex::new(None))) =
        Some(value);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_tool_loop_enabled() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_ENABLED.get_or_init(|| Mutex::new(None))) =
        None;
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_tool_loop_turns(turns: Vec<DirectLlmToolTurnResponse>) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_TURNS.get_or_init(|| Mutex::new(None))) =
        Some(turns);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_tool_loop_turns() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_TURNS.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
pub(crate) fn set_test_packet_proposal_tool_loop_limits(
    limits: crate::slices::ai_usage::service::ToolLoopLimits,
) {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_LIMITS.get_or_init(|| Mutex::new(None))) =
        Some(limits);
}

#[cfg(test)]
pub(crate) fn clear_test_packet_proposal_tool_loop_limits() {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_LIMITS.get_or_init(|| Mutex::new(None))) = None;
}

#[cfg(test)]
fn test_execution_mode() -> Option<PacketProposalExecutionMode> {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_EXECUTION_MODE.get_or_init(|| Mutex::new(None)))
}

#[cfg(test)]
fn test_stale_running_after_ms() -> Option<u64> {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_STALE_AFTER_MS.get_or_init(|| Mutex::new(None)))
}

#[cfg(test)]
fn test_tool_loop_enabled() -> Option<bool> {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_ENABLED.get_or_init(|| Mutex::new(None)))
}

#[cfg(test)]
fn take_test_packet_proposal_tool_loop_turns() -> Option<Vec<DirectLlmToolTurnResponse>> {
    test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_TURNS.get_or_init(|| Mutex::new(None))).take()
}

#[cfg(test)]
fn test_tool_loop_limits() -> Option<crate::slices::ai_usage::service::ToolLoopLimits> {
    *test_mutex_lock(TEST_PACKET_PROPOSAL_TOOL_LOOP_LIMITS.get_or_init(|| Mutex::new(None)))
}

#[cfg(test)]
fn take_test_packet_proposal_response(request: &TypedLlmTaskRequest) -> Option<serde_json::Value> {
    test_mutex_lock(TEST_PACKET_PROPOSAL_REQUESTS.get_or_init(|| Mutex::new(Vec::new())))
        .push(request.clone());
    let gate =
        test_mutex_lock(TEST_PACKET_PROPOSAL_LLM_GATE.get_or_init(|| Mutex::new(None))).clone();
    if let Some(gate) = gate {
        gate.enter_and_wait();
    }
    test_mutex_lock(TEST_PACKET_PROPOSAL_RESPONSE.get_or_init(|| Mutex::new(None))).take()
}
