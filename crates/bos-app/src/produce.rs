//! The produce stage of the pipeline (ingest → classify → **produce** →
//! approve → write), generic over packet kinds. A kind supplies its store
//! lookups, its typed-LLM request, and its parse+stage step; this module owns
//! the one orchestration every produce route shares:
//!
//! 1. guards under the lock (item accepted, kind suggested, no active draft —
//!    an existing active draft IS the idempotent result),
//! 2. the bounded typed LLM call on the blocking pool with the lock released,
//! 3. stage under the lock (the kind's unique active-draft index resolves
//!    produce races; the loser returns the winner's draft).
//!
//! Adding a produce kind = implementing [`ProduceFlavor`] — never a fourth
//! copy of this flow.
//!
//! This file is allowed to be larger than a typical top-level spine module only
//! for orchestration shared by every produce route/pump: source resolution,
//! in-flight guards, proposal fan-out/staging, auto-produce dispatch, and the
//! lock/unlock/LLM/stage sequence. Slice-specific parsing, validation, and
//! persistence remain in each slice's `service.rs`/`store.rs`.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::produce::ProduceStatusResponse;
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
use bos_integrations::llm_typed_tasks::TypedLlmTaskRequest;
#[cfg(test)]
use bos_integrations::llm_typed_tasks::{TypedLlmExecutionRoute, TypedLlmTaskOutputEnvelope};
use rusqlite::Connection;
use serde::Deserialize;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use crate::http::{error_response, now_ms, store_error_response, AppState, OperatorScope};
use crate::store_core::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalContract {
    pub packet_kind: &'static str,
    pub schema_ref: &'static str,
    pub response_key: &'static str,
    pub instructions: &'static str,
}

/// Arguments available to unlocked context enrichment between deterministic
/// local preparation and the LLM call.
pub struct EnrichContext<'a> {
    pub state: &'a AppState,
    pub item: &'a WorkItem,
    pub message: &'a InboundMessageRecord,
    pub scope: &'a OperatorScope,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub context: serde_json::Value,
    pub attempt: u64,
    pub now_ms: u64,
}

/// Arguments available while a flavor parses and stages the model response
/// under the persistence lock.
pub struct StageContext<'a> {
    pub conn: &'a mut Connection,
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub item: &'a WorkItem,
    pub message: &'a InboundMessageRecord,
    pub response: &'a serde_json::Value,
    pub context: &'a serde_json::Value,
    pub model: &'a str,
    pub attempt: u64,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

/// What a packet kind plugs into the shared produce flow.
pub trait ProduceFlavor {
    /// Wire response carrying the (existing or freshly staged) draft.
    type Response: serde::Serialize;

    /// Catalog kind id this flavor produces (guards check the work item
    /// suggests it).
    fn packet_kind(&self) -> &'static str;
    /// LLM routing purpose for the typed task.
    fn purpose(&self) -> &'static str;
    /// Slice label for store-failure logs.
    fn slice(&self) -> &'static str;
    /// Domain-error code raised by the kind's store when an active draft
    /// already exists (the race-loser signal).
    fn already_active_code(&self) -> &'static str;

    /// Whether this kind can participate in the bounded typed packet proposal
    /// runner. Phase A only enables text-source kinds whose deterministic
    /// `prepare_context()` can be shared between the proposal prompt and
    /// `stage()`.
    fn proposal_enabled(&self) -> bool {
        false
    }

    /// The nested proposal output contract for this kind. Existing produce
    /// routes keep their own task schema; packet proposals ask for one nested
    /// JSON object per enabled kind and pass that object to `stage()`.
    fn proposal_contract(&self) -> Option<ProposalContract> {
        None
    }

    /// Human-readable evidence requirements for the packet proposal prompt.
    fn evidence_requirements(&self) -> &'static [&'static str] {
        &[]
    }

    /// The staged-or-approved draft for the item, if any.
    fn active_draft(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, StoreError>;

    /// How many drafts (any status) exist for the item — the next attempt
    /// number is count + 1.
    fn draft_attempts(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<u64, StoreError>;

    /// Deterministic locked-phase context assembled from local stores BEFORE
    /// the LLM call (e.g. BM25 retrieval over the drive corpus). The same
    /// value rides into build_request (what the model sees) and stage (what
    /// validation gates against), so they can never drift. Kinds that need
    /// none keep the default. Domain errors surface as 422 produce guards
    /// (e.g. "no evidence found for this brief").
    fn prepare_context(
        &self,
        _conn: &Connection,
        _client_id: &str,
        _item: &WorkItem,
        _message: &InboundMessageRecord,
        _scope: &OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::Value::Null)
    }

    /// Optional unlocked context enrichment between deterministic local
    /// context assembly and the LLM request. This is for best-effort evidence
    /// fetches that must not run under the persistence lock. The default is
    /// behavior-identical for existing produce kinds.
    fn enrich_context_unlocked(&self, ctx: EnrichContext<'_>) -> serde_json::Value {
        ctx.context
    }

    /// Build the bounded typed transform request for this item + source.
    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> TypedLlmTaskRequest;

    /// Parse + domain-validate the model response and insert the staged
    /// draft. Invalid responses surface as `StoreError::Domain(code)` (422).
    /// `message` is the source the produce ran over (kinds that ground
    /// deterministic fields — e.g. occurred-at from the email date — read it;
    /// others ignore it); `context` is prepare_context's value.
    fn stage(&self, ctx: StageContext<'_>) -> Result<(), StoreError>;

    /// Optional operator-facing explanation for a failed stage, extracted from
    /// the same bounded model response that failed validation.
    fn stage_failure_message(
        &self,
        _response: &serde_json::Value,
        _error_code: &str,
    ) -> Option<String> {
        None
    }

    /// Cross-kind orchestration after a draft is staged, with the persistence
    /// lock RELEASED and [`AppState`] available (so the hook may spawn other
    /// produces). Runs once per successful stage. Default no-op; the CRM-note
    /// kind uses it to add + kick the `crm_record_create` kind when the note's
    /// contact is missing from the CRM. Best-effort — failures only log.
    fn after_stage(&self, _state: &AppState, _item: &WorkItem, _actor_id: &str) {}
}

/// Epoch milliseconds → RFC3339 UTC ("2026-06-10T14:00:00Z").
/// Used by approval-payload builders (approved_at, occurred_at).
pub fn epoch_ms_to_rfc3339_utc(epoch_ms: u64) -> String {
    crate::slices::datetime_input::epoch_ms_to_rfc3339_utc(epoch_ms)
}

/// Epoch milliseconds -> UTC civil date ("2026-06-10"). Prompt builders use
/// this next to the raw epoch so models do not have to do calendar math.
pub fn epoch_ms_to_utc_date(epoch_ms: u64) -> String {
    crate::slices::datetime_input::epoch_ms_to_utc_date(epoch_ms)
}

/// Reusable policy for background draft enrichment. Later evidence may replace
/// a current value only when the current value is still a weak model prefill;
/// operator/provider/deterministic values must win.
pub mod draft_field_policy {
    use bos_contracts::calendar_drafts::DraftFieldProvenance;

    /// A display name that is really just a domain/URL is a weak identity
    /// placeholder when stronger website evidence names the business.
    pub fn is_domain_like_display_name(value: &str) -> bool {
        let value = value
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.")
            .trim_end_matches('/')
            .trim();
        if value.is_empty() || value.contains(char::is_whitespace) {
            return false;
        }
        let labels: Vec<&str> = value.split('.').collect();
        labels.len() >= 2
            && labels.iter().all(|label| {
                !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            && labels.last().is_some_and(|tld| {
                (2..=24).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic())
            })
    }

    /// True when `current` still appears to be the AI-extracted value for
    /// `field`. If the operator edited the field, the old AI quote usually no
    /// longer grounds the current value, so enrichment leaves it alone.
    pub fn still_ai_prefill(
        provenance: &[DraftFieldProvenance],
        field: &str,
        current: &str,
    ) -> bool {
        let current = current.trim().to_lowercase();
        !current.is_empty()
            && provenance.iter().any(|entry| {
                entry.field == field
                    && !entry.quote.starts_with("page:")
                    && !matches!(entry.quote.as_str(), "crm_match" | "provider_match")
                    && entry.quote.to_lowercase().contains(&current)
            })
    }

    /// Company/display names can be revised by enrichment only when the current
    /// value is a domain-like AI placeholder and the replacement is richer.
    pub fn may_replace_weak_company_name(
        current: Option<&str>,
        replacement: &str,
        provenance: &[DraftFieldProvenance],
    ) -> bool {
        let Some(current) = current.map(str::trim).filter(|v| !v.is_empty()) else {
            return true;
        };
        let replacement = replacement.trim();
        !replacement.is_empty()
            && !replacement.eq_ignore_ascii_case(current)
            && !is_domain_like_display_name(replacement)
            && is_domain_like_display_name(current)
            && still_ai_prefill(provenance, "company_name", current)
    }
}

fn push_profile_line(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
        lines.push(format!("{label}: {value}"));
    }
}

/// The client's company-background block for grounding outward-facing tasks, or
/// None when no profile (or an all-empty one) is seeded. Read in a slice's
/// `prepare_context` and folded into the LLM request's text blocks — tone/
/// context only; never a source of facts the task output may assert.
pub fn background_text_block(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<bos_integrations::llm_typed_tasks::TypedLlmTextBlock>, StoreError> {
    let Some(profile) = crate::slices::client_profile::store::load_profile(conn, client_id)? else {
        return Ok(None);
    };
    let mut lines = vec![
        "Company background (use for tone/context only; never state facts not supported by the task source):"
            .to_string(),
    ];
    push_profile_line(&mut lines, "Company", profile.company_name.as_deref());
    push_profile_line(&mut lines, "About", profile.bio.as_deref());
    push_profile_line(&mut lines, "Industry", profile.industry.as_deref());
    push_profile_line(&mut lines, "Website", profile.website.as_deref());
    push_profile_line(&mut lines, "Voice", profile.persona.as_deref());
    if lines.len() == 1 {
        return Ok(None);
    }
    Ok(Some(bos_integrations::llm_typed_tasks::TypedLlmTextBlock {
        block_id: "background".to_string(),
        text: lines.join("\n"),
    }))
}

pub(crate) enum SourceError {
    Unsupported,
    Store(StoreError),
}

/// Resolve a work item's source into the message view produce kinds consume.
/// Each source family contributes one arm; an item from a family nothing can
/// resolve never reaches the LLM. Also serves the queue's source-peek route —
/// the operator reads exactly what the produce stage would.
pub(crate) fn resolve_source(
    conn: &Connection,
    client_id: &str,
    item: &WorkItem,
) -> Result<Option<InboundMessageRecord>, SourceError> {
    match item.source_kind.as_str() {
        crate::slices::work_queue::SOURCE_KIND_EMAIL => {
            let mut messages = crate::slices::email_triage::store::inbound_by_source_keys(
                conn,
                client_id,
                std::slice::from_ref(&item.source_ref),
                &OperatorScope::All,
            )
            .map_err(SourceError::Store)?;
            Ok((!messages.is_empty()).then(|| messages.remove(0)))
        }
        crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE => Ok(
            crate::slices::operator_notes::store::get_note(conn, client_id, &item.source_ref)
                .map_err(SourceError::Store)?
                .map(|note| crate::slices::operator_notes::service::produce_source_view(&note)),
        ),
        crate::slices::work_queue::SOURCE_KIND_STOCKFORGE_DAMAGE => {
            Ok(crate::slices::claim_drafts::store::get_damage_snapshot(
                conn,
                client_id,
                &item.source_ref,
            )
            .map_err(SourceError::Store)?
            .map(|snapshot| crate::slices::claim_drafts::service::produce_source_view(&snapshot)))
        }
        crate::slices::content_plans::SOURCE_KIND_CONTENT_PLAN_ITEM => Ok(
            crate::slices::content_plans::store::get_item(conn, client_id, &item.source_ref)
                .map_err(SourceError::Store)?
                .map(|entry| crate::slices::content_plans::service::source_view(&entry.item)),
        ),
        crate::slices::lead_discovery::SOURCE_KIND_LEAD_FINDING => Ok(
            crate::slices::lead_discovery::store::get_finding(conn, client_id, &item.source_ref)
                .map_err(SourceError::Store)?
                .map(|finding| {
                    crate::slices::lead_discovery::service::source_view(&finding.finding)
                }),
        ),
        crate::slices::call_inputs::SOURCE_KIND_CALL_INPUT => Ok(
            crate::slices::call_inputs::store::get_input(conn, client_id, &item.source_ref)
                .map_err(SourceError::Store)?
                .map(|input| crate::slices::call_inputs::service::source_view(&input.input, None)),
        ),
        crate::slices::email_drafts::store::SOURCE_KIND_EMAIL_FOLLOW_UP => {
            crate::slices::email_drafts::store::source_view_for_follow_up(
                conn,
                client_id,
                &item.source_ref,
            )
            .map_err(SourceError::Store)
        }
        _ => Err(SourceError::Unsupported),
    }
}

/// A produce slice's staged-item-ids store lookup.
type StagedItemIdsFn = fn(&Connection, &str) -> Result<Vec<String>, StoreError>;

/// Packet kinds with a STAGED draft per work item — the queue feed's
/// "needs you" decoration. One arm per produce kind, like [`resolve_source`]:
/// a new flavor adds its store lookup here.
pub(crate) fn staged_draft_kinds_by_item(
    conn: &Connection,
    client_id: &str,
) -> Result<std::collections::HashMap<String, Vec<String>>, StoreError> {
    let sources: [(&str, StagedItemIdsFn); 10] = [
        (
            "calendar_event_draft",
            crate::slices::calendar_drafts::store::staged_item_ids,
        ),
        (
            "claim_draft",
            crate::slices::claim_drafts::store::staged_item_ids,
        ),
        (
            "content_draft",
            crate::slices::content_drafts::store::staged_item_ids,
        ),
        (
            "follow_up_task",
            crate::slices::follow_up_tasks::store::staged_item_ids,
        ),
        (
            "crm_activity",
            crate::slices::crm_drafts::store::staged_item_ids,
        ),
        (
            "crm_record_create",
            crate::slices::crm_record_drafts::store::staged_item_ids,
        ),
        (
            "crm_sales_intent",
            crate::slices::crm_sales_intent::store::staged_item_ids,
        ),
        (
            "email_draft_reply",
            crate::slices::email_drafts::store::staged_item_ids,
        ),
        (
            "ledger_entry",
            crate::slices::ledger_drafts::store::staged_item_ids,
        ),
        (
            "invoice_draft",
            crate::slices::invoice_drafts::store::staged_item_ids,
        ),
    ];
    let mut by_item: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (kind, staged_ids) in sources {
        for item_id in staged_ids(conn, client_id)? {
            by_item.entry(item_id).or_default().push(kind.to_string());
        }
    }
    Ok(by_item)
}

/// Actor stamped on pump-produced drafts and their receipts.
const AUTO_PRODUCE_ACTOR: &str = "auto_produce_pump";

/// Claim the (item, kind) produce slot. False = one is already running —
/// the caller skips instead of double-spending the LLM. Process-local on
/// purpose: a crash just means a re-click, and the one-active-draft index
/// already prevents duplicate drafts.
pub(crate) fn begin_produce(state: &AppState, item_id: &str, kind: &str) -> bool {
    state
        .produce_in_flight
        .lock()
        .insert((item_id.to_string(), kind.to_string()))
}

pub(crate) fn end_produce(state: &AppState, item_id: &str, kind: &str) {
    state
        .produce_in_flight
        .lock()
        .remove(&(item_id.to_string(), kind.to_string()));
}

/// Snapshot of running produces, for feed decoration ("drafting…").
pub(crate) fn produce_in_flight_snapshot(
    state: &AppState,
) -> std::collections::HashSet<(String, String)> {
    state.produce_in_flight.lock().clone()
}

/// Draft attempts (any status) for an item+kind, dispatched by kind string.
/// `None` = the kind has no produce flavor, so the pump skips it.
fn kind_draft_attempts(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    kind: &str,
) -> Result<Option<u64>, StoreError> {
    match kind {
        "calendar_event_draft" => {
            crate::slices::calendar_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "follow_up_task" => {
            crate::slices::follow_up_tasks::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "crm_activity" => {
            crate::slices::crm_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "crm_record_create" => {
            crate::slices::crm_record_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "crm_sales_intent" => {
            crate::slices::crm_sales_intent::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "email_draft_reply" => {
            crate::slices::email_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "ledger_entry" => {
            crate::slices::ledger_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "content_draft" => {
            crate::slices::content_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "claim_draft" => {
            crate::slices::claim_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        "invoice_draft" => {
            crate::slices::invoice_drafts::store::count_drafts_for_item(conn, client_id, item_id)
                .map(Some)
        }
        _ => Ok(None),
    }
}

/// Map typed-LLM purpose ids back to the packet kind whose produce flow owns
/// them. Used by queue diagnostics to turn `ai_usage_log` failures into an
/// operator-visible retry/debug signal.
pub(crate) fn packet_kind_for_purpose(purpose: &str) -> Option<&'static str> {
    match purpose {
        crate::slices::calendar_drafts::service::EXTRACT_PURPOSE => Some("calendar_event_draft"),
        crate::slices::claim_drafts::service::FILL_PURPOSE => Some("claim_draft"),
        crate::slices::content_drafts::service::FILL_PURPOSE => Some("content_draft"),
        crate::slices::crm_drafts::service::FILL_PURPOSE => Some("crm_activity"),
        crate::slices::crm_record_drafts::service::FILL_PURPOSE => Some("crm_record_create"),
        crate::slices::crm_sales_intent::service::FILL_PURPOSE => Some("crm_sales_intent"),
        crate::slices::email_drafts::service::FILL_PURPOSE => Some("email_draft_reply"),
        crate::slices::follow_up_tasks::service::FILL_PURPOSE => Some("follow_up_task"),
        crate::slices::invoice_drafts::service::FILL_PURPOSE => Some("invoice_draft"),
        crate::slices::ledger_drafts::service::FILL_PURPOSE => Some("ledger_entry"),
        _ => None,
    }
}

pub(crate) fn proposal_enabled_packet_kind(kind: &str) -> bool {
    match kind {
        "calendar_event_draft" => {
            crate::slices::calendar_drafts::service::Produce.proposal_enabled()
        }
        "follow_up_task" => crate::slices::follow_up_tasks::service::Produce.proposal_enabled(),
        "crm_activity" => crate::slices::crm_drafts::service::Produce.proposal_enabled(),
        "crm_sales_intent" => crate::slices::crm_sales_intent::service::Produce.proposal_enabled(),
        "email_draft_reply" => crate::slices::email_drafts::service::Produce.proposal_enabled(),
        _ => false,
    }
}

pub(crate) fn proposal_contract_for_kind(kind: &str) -> Option<ProposalContract> {
    match kind {
        "calendar_event_draft" => {
            crate::slices::calendar_drafts::service::Produce.proposal_contract()
        }
        "follow_up_task" => crate::slices::follow_up_tasks::service::Produce.proposal_contract(),
        "crm_activity" => crate::slices::crm_drafts::service::Produce.proposal_contract(),
        "crm_sales_intent" => crate::slices::crm_sales_intent::service::Produce.proposal_contract(),
        "email_draft_reply" => crate::slices::email_drafts::service::Produce.proposal_contract(),
        _ => None,
    }
}

pub(crate) fn proposal_evidence_requirements(kind: &str) -> &'static [&'static str] {
    match kind {
        "calendar_event_draft" => {
            crate::slices::calendar_drafts::service::Produce.evidence_requirements()
        }
        "follow_up_task" => {
            crate::slices::follow_up_tasks::service::Produce.evidence_requirements()
        }
        "crm_activity" => crate::slices::crm_drafts::service::Produce.evidence_requirements(),
        "crm_sales_intent" => {
            crate::slices::crm_sales_intent::service::Produce.evidence_requirements()
        }
        "email_draft_reply" => {
            crate::slices::email_drafts::service::Produce.evidence_requirements()
        }
        _ => &[],
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedProposalKind {
    pub packet_kind: String,
    pub contract: ProposalContract,
    pub context: serde_json::Value,
    pub attempt: u64,
}

pub(crate) fn prepare_proposal_kind(
    conn: &Connection,
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    scope: &OperatorScope,
    actor_id: &str,
    kind: &str,
) -> Result<Option<PreparedProposalKind>, StoreError> {
    match kind {
        "calendar_event_draft" => prepare_proposal_kind_with_flavor(
            conn,
            client_id,
            item,
            message,
            scope,
            actor_id,
            &crate::slices::calendar_drafts::service::Produce,
        )
        .map(Some),
        "follow_up_task" => prepare_proposal_kind_with_flavor(
            conn,
            client_id,
            item,
            message,
            scope,
            actor_id,
            &crate::slices::follow_up_tasks::service::Produce,
        )
        .map(Some),
        "crm_activity" => prepare_proposal_kind_with_flavor(
            conn,
            client_id,
            item,
            message,
            scope,
            actor_id,
            &crate::slices::crm_drafts::service::Produce,
        )
        .map(Some),
        "crm_sales_intent" => prepare_proposal_kind_with_flavor(
            conn,
            client_id,
            item,
            message,
            scope,
            actor_id,
            &crate::slices::crm_sales_intent::service::Produce,
        )
        .map(Some),
        "email_draft_reply" => prepare_proposal_kind_with_flavor(
            conn,
            client_id,
            item,
            message,
            scope,
            actor_id,
            &crate::slices::email_drafts::service::Produce,
        )
        .map(Some),
        _ => Ok(None),
    }
}

fn prepare_proposal_kind_with_flavor<F: ProduceFlavor>(
    conn: &Connection,
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    scope: &OperatorScope,
    actor_id: &str,
    flavor: &F,
) -> Result<PreparedProposalKind, StoreError> {
    let Some(contract) = flavor.proposal_contract() else {
        return Err(StoreError::Domain(
            "packet_proposal_kind_not_enabled".to_string(),
        ));
    };
    let context = flavor.prepare_context(conn, client_id, item, message, scope, actor_id)?;
    let attempt = flavor.draft_attempts(conn, client_id, &item.item_id)? + 1;
    Ok(PreparedProposalKind {
        packet_kind: flavor.packet_kind().to_string(),
        contract,
        context,
        attempt,
    })
}

pub(crate) struct ProposalStageError {
    pub error: StoreError,
    pub message: Option<String>,
}

pub(crate) fn stage_proposal_for_kind(
    kind: &str,
    ctx: StageContext<'_>,
) -> Result<Option<String>, ProposalStageError> {
    match kind {
        "calendar_event_draft" => {
            stage_proposal_with_flavor(&crate::slices::calendar_drafts::service::Produce, ctx)
        }
        "follow_up_task" => {
            stage_proposal_with_flavor(&crate::slices::follow_up_tasks::service::Produce, ctx)
        }
        "crm_activity" => {
            stage_proposal_with_flavor(&crate::slices::crm_drafts::service::Produce, ctx)
        }
        "crm_sales_intent" => {
            stage_proposal_with_flavor(&crate::slices::crm_sales_intent::service::Produce, ctx)
        }
        "email_draft_reply" => {
            stage_proposal_with_flavor(&crate::slices::email_drafts::service::Produce, ctx)
        }
        _ => Err(ProposalStageError {
            error: StoreError::Domain("packet_proposal_kind_not_enabled".to_string()),
            message: None,
        }),
    }
}

fn stage_proposal_with_flavor<F: ProduceFlavor>(
    flavor: &F,
    ctx: StageContext<'_>,
) -> Result<Option<String>, ProposalStageError> {
    let StageContext {
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
    if let Err(error) = flavor.stage(StageContext {
        conn: &mut *conn,
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
    }) {
        let error_code = store_error_code(&error);
        return Err(ProposalStageError {
            message: flavor.stage_failure_message(response, &error_code),
            error,
        });
    }
    active_draft_id_for_kind(conn, client_id, &item.item_id, flavor.packet_kind()).map_err(
        |error| ProposalStageError {
            error,
            message: None,
        },
    )
}

pub(crate) fn active_draft_id_for_kind(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    kind: &str,
) -> Result<Option<String>, StoreError> {
    match kind {
        "calendar_event_draft" => Ok(
            crate::slices::calendar_drafts::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|entry| entry.draft.draft_id),
        ),
        "follow_up_task" => Ok(
            crate::slices::follow_up_tasks::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|entry| entry.draft.draft_id),
        ),
        "crm_activity" => Ok(crate::slices::crm_drafts::store::active_draft_for_item(
            conn, client_id, item_id,
        )?
        .map(|entry| entry.draft.draft_id)),
        "crm_sales_intent" => Ok(
            crate::slices::crm_sales_intent::store::active_draft_for_item(
                conn, client_id, item_id,
            )?
            .map(|entry| entry.draft.draft_id),
        ),
        "email_draft_reply" => Ok(crate::slices::email_drafts::store::active_draft_for_item(
            conn, client_id, item_id,
        )?
        .map(|entry| entry.draft.draft_id)),
        _ => Ok(None),
    }
}

/// One produce, dispatched by kind string to the slice's flavor. Shared by the
/// auto-produce pump and the operator-note action kickoff (the actor differs:
/// the pump stamps its own, note actions stamp the note author).
fn produce_blocking_by_kind(
    state: &AppState,
    item_id: &str,
    kind: &str,
    idempotency_key: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
) -> Result<(), ProduceError> {
    let system_scope = OperatorScope::All;
    match kind {
        "calendar_event_draft" => produce_blocking(
            state,
            &crate::slices::calendar_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "follow_up_task" => produce_blocking(
            state,
            &crate::slices::follow_up_tasks::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "crm_activity" => produce_blocking(
            state,
            &crate::slices::crm_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "crm_record_create" => produce_blocking(
            state,
            &crate::slices::crm_record_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "crm_sales_intent" => produce_blocking(
            state,
            &crate::slices::crm_sales_intent::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "email_draft_reply" => produce_blocking(
            state,
            &crate::slices::email_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "ledger_entry" => produce_blocking(
            state,
            &crate::slices::ledger_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "content_draft" => produce_blocking(
            state,
            &crate::slices::content_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "claim_draft" => produce_blocking(
            state,
            &crate::slices::claim_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        "invoice_draft" => produce_blocking(
            state,
            &crate::slices::invoice_drafts::service::Produce,
            item_id,
            idempotency_key,
            actor_id,
            actor_kind,
            &system_scope,
        )
        .map(|_| ()),
        _ => Err(ProduceError::Guard("produce_kind_unsupported")),
    }
}

/// Kick background produce for a kind from a non-route context (operator-note
/// actions). Mirrors [`run`]'s in-flight guard + detached thread, minus the
/// HTTP response — the originating route has already answered. Already-running
/// slots and unknown kinds are quietly skipped (the latter logged at info).
pub fn kick_produce_for_kind(
    state: AppState,
    item_id: String,
    kind: String,
    idempotency_key: String,
    actor_id: String,
    actor_kind: ActorKindDto,
) {
    if !begin_produce(&state, &item_id, &kind) {
        return;
    }
    std::thread::Builder::new()
        .name(format!("produce-{kind}"))
        .spawn(move || {
            let result = produce_blocking_by_kind(
                &state,
                &item_id,
                &kind,
                &idempotency_key,
                &actor_id,
                actor_kind,
            );
            end_produce(&state, &item_id, &kind);
            match result {
                Ok(()) => {
                    tracing::info!(%item_id, %kind, "note-action produce staged a draft")
                }
                Err(ProduceError::Llm(code)) => {
                    tracing::warn!(%item_id, %kind, code, "note-action produce llm failed")
                }
                Err(ProduceError::Store(err)) => {
                    tracing::warn!(%item_id, %kind, error = %err, "note-action produce store failed")
                }
                Err(ProduceError::Guard(code)) => {
                    tracing::info!(%item_id, %kind, code, "note-action produce stopped by guard")
                }
                Err(_) => {
                    tracing::info!(%item_id, %kind, "note-action produce stopped (item/source gone)")
                }
            }
        })
        .expect("spawn produce thread");
}

/// (item_id, kind) pairs the pump should produce this cycle: accepted items
/// whose category policy opted into auto_produce, kinds with a produce flavor
/// and ZERO prior drafts. The zero-attempts gate means the pump only fills
/// the FIRST draft — after an operator rejects one, re-producing is a manual
/// decision, never an automatic LLM-spend loop.
pub(crate) fn collect_auto_produce_candidates(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let auto_categories: std::collections::HashSet<String> =
        crate::slices::work_queue::store::list_policies(conn, client_id)?
            .into_iter()
            .filter(|policy| policy.auto_produce)
            .map(|policy| policy.category_id)
            .collect();
    if auto_categories.is_empty() {
        return Ok(Vec::new());
    }
    let scan_limit = if limit == usize::MAX {
        i64::MAX as usize
    } else {
        limit.max(200)
    };
    let items = crate::slices::work_queue::store::list_items(
        conn,
        client_id,
        Some(bos_contracts::work_queue::WorkItemStatus::Accepted),
        scan_limit,
        &crate::http::OperatorScope::All,
    )?;
    let mut candidates = Vec::new();
    'items: for entry in items {
        if !auto_categories.contains(&entry.item.category_id) {
            continue;
        }
        for kind in &entry.item.packet_kinds {
            if proposal_enabled_packet_kind(kind)
                && crate::slices::packet_proposals::store::has_running_or_terminal_run_covering_kind(
                    conn,
                    client_id,
                    &entry.item.item_id,
                    kind,
                )?
            {
                continue;
            }
            if kind_draft_attempts(conn, client_id, &entry.item.item_id, kind)? != Some(0) {
                continue;
            }
            candidates.push((entry.item.item_id.clone(), kind.clone()));
            if candidates.len() >= limit {
                break 'items;
            }
        }
    }
    Ok(candidates)
}

fn run_auto_produce_cycle(state: &AppState, max_per_cycle: usize) {
    let candidates = {
        let persistence = state.persistence.lock();
        match collect_auto_produce_candidates(
            persistence.connection_ref(),
            &state.client_id,
            max_per_cycle,
        ) {
            Ok(candidates) => candidates,
            Err(err) => {
                tracing::warn!(error = %err, "auto-produce candidate scan failed");
                return;
            }
        }
    };
    for (item_id, kind) in candidates {
        // Skip slots a manual kickoff already claimed (and claim ours so a
        // manual click during this produce no-ops instead of double-spending).
        if !begin_produce(state, &item_id, &kind) {
            continue;
        }
        // Deterministic key: with the zero-attempts gate there is exactly one
        // automatic attempt per item+kind, so a crash-replay stays quiet.
        let idempotency_key = format!("auto_produce:{item_id}:{kind}");
        let outcome = produce_blocking_by_kind(
            state,
            &item_id,
            &kind,
            &idempotency_key,
            AUTO_PRODUCE_ACTOR,
            ActorKindDto::System,
        );
        end_produce(state, &item_id, &kind);
        match outcome {
            Ok(()) => tracing::info!(%item_id, %kind, "auto-produce staged a draft"),
            Err(ProduceError::Llm(code)) => {
                tracing::warn!(%item_id, %kind, code, "auto-produce llm failed")
            }
            Err(ProduceError::Store(err)) => {
                tracing::warn!(%item_id, %kind, error = %err, "auto-produce store failed")
            }
            // Guards (item reopened/dismissed meanwhile, kind toggled off,
            // source gone) are normal pump races — quiet at info.
            Err(ProduceError::Guard(code)) => {
                tracing::info!(%item_id, %kind, code, "auto-produce skipped by guard")
            }
            Err(_) => tracing::info!(%item_id, %kind, "auto-produce skipped (item/source gone)"),
        }
    }
}

/// The auto-produce pump: accepted items in categories whose policy opted in
/// get their first drafts produced automatically, bounded by
/// BOS_AUTO_PRODUCE_MAX_PER_CYCLE LLM calls per cycle. Off unless
/// BOS_AUTO_PRODUCE_ENABLED — every accept then spends LLM calls, which is an
/// operator cost decision.
pub fn spawn_auto_produce_pump(state: AppState) {
    std::thread::Builder::new()
        .name("auto-produce-pump".to_string())
        .spawn(move || {
            tracing::info!("auto-produce pump started");
            loop {
                let (enabled, interval, max_per_cycle) = {
                    let persistence = state.persistence.lock();
                    let conn = persistence.connection_ref();
                    let enabled = crate::slices::admin_settings::service::flag(
                        conn,
                        &state.client_id,
                        &crate::env_registry::BOS_AUTO_PRODUCE_ENABLED,
                    )
                    .unwrap_or_else(|err| {
                        tracing::warn!(error = %err, "auto-produce config read failed");
                        false
                    });
                    let interval_secs = crate::slices::admin_settings::service::usize_or(
                        conn,
                        &state.client_id,
                        &crate::env_registry::BOS_AUTO_PRODUCE_INTERVAL_SECS,
                        30,
                    )
                    .unwrap_or(30)
                    .max(5) as u64;
                    let max_per_cycle = crate::slices::admin_settings::service::usize_or(
                        conn,
                        &state.client_id,
                        &crate::env_registry::BOS_AUTO_PRODUCE_MAX_PER_CYCLE,
                        3,
                    )
                    .unwrap_or(3);
                    (
                        enabled,
                        std::time::Duration::from_secs(interval_secs),
                        max_per_cycle,
                    )
                };
                if enabled {
                    run_auto_produce_cycle(&state, max_per_cycle);
                }
                std::thread::sleep(interval);
            }
        })
        .expect("spawn auto-produce-pump thread");
}

/// Guards shared by every produce kind: only ACCEPTED items with the kind in
/// their suggested set may produce. Returns the wire error code.
pub fn validate_item_for_kind(item: &WorkItem, packet_kind: &str) -> Result<(), &'static str> {
    if item.status != WorkItemStatus::Accepted {
        return Err("produce_item_not_accepted");
    }
    if !item.packet_kinds.iter().any(|kind| kind == packet_kind) {
        return Err("produce_kind_not_suggested");
    }
    Ok(())
}

/// Why a produce attempt yielded no staged draft. Route guards surface the
/// 4xx arms inline before kickoff; background workers (manual threads + the
/// pump) log the rest and move on.
#[derive(Debug)]
pub(crate) enum ProduceError {
    ItemNotFound,
    Guard(&'static str),
    SourceMissing,
    SourceUnsupported,
    Llm(String),
    Store(StoreError),
    DraftVanished,
}

pub(crate) fn store_error_code(err: &StoreError) -> String {
    match err {
        StoreError::Domain(code) => code.clone(),
        StoreError::Sqlite(_) => "produce_store_sqlite_failed".to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct ProduceStatusQuery {
    item_id: String,
    kind: String,
    idempotency_key: String,
}

pub(crate) fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/produce/status", get(produce_status))
}

async fn produce_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProduceStatusQuery>,
) -> Response {
    if query.item_id.trim().is_empty()
        || query.kind.trim().is_empty()
        || query.idempotency_key.trim().is_empty()
    {
        return error_response(StatusCode::BAD_REQUEST, "produce_status_query_required");
    }
    let scope = match state.require_scope(&headers) {
        Ok(scope) => scope,
        Err(denied) => return *denied,
    };
    let persistence = state.persistence.lock();
    match crate::slices::work_queue::store::get_item_scoped(
        persistence.connection_ref(),
        &state.client_id,
        &query.item_id,
        &scope,
    ) {
        Ok(Some(_)) => {}
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "work_item_not_found"),
        Err(err) => return store_error_response("produce", err),
    }
    drop(persistence);
    if state
        .produce_in_flight
        .lock()
        .contains(&(query.item_id.clone(), query.kind.clone()))
    {
        return Json(ProduceStatusResponse::Producing).into_response();
    }
    let persistence = state.persistence.lock();
    match latest_produce_failure_receipt(
        persistence.connection_ref(),
        &state.client_id,
        &query.item_id,
        &query.kind,
        &query.idempotency_key,
    ) {
        Ok(Some(failure)) => Json(ProduceStatusResponse::Failed {
            error_code: failure.error_code,
            message: failure.message,
            receipt_id: failure.receipt_id,
            created_at_ms: failure.created_at_ms,
        })
        .into_response(),
        Ok(None) => Json(ProduceStatusResponse::Idle).into_response(),
        Err(err) => store_error_response("produce", err),
    }
}

struct ProduceFailureReceipt {
    error_code: String,
    message: Option<String>,
    receipt_id: String,
    created_at_ms: u64,
}

fn latest_produce_failure_receipt(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    kind: &str,
    idempotency_key: &str,
) -> Result<Option<ProduceFailureReceipt>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT receipt_id, error_class, created_at_ms, after_json \
         FROM receipts \
         WHERE client_id = ?1 AND entity_kind = 'produce' AND entity_id = ?2 \
           AND change_kind = 'stage' AND outcome = 'failed' \
           AND causation_id = ?4 \
           AND json_extract(after_json, '$.packet_kind') = ?3 \
         ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![client_id, item_id, kind, idempotency_key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(ProduceFailureReceipt {
        receipt_id: row.get(0)?,
        error_code: row
            .get::<_, Option<String>>(1)?
            .unwrap_or_else(|| "produce_stage_failed".to_string()),
        created_at_ms: row.get(2)?,
        message: produce_failure_message(row.get::<_, Option<String>>(3)?.as_deref()),
    }))
}

fn produce_failure_message(after_json: Option<&str>) -> Option<String> {
    let value = after_json.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())?;
    value
        .get("message")
        .or_else(|| value.get("reason"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(|message| message.chars().take(1_000).collect())
}

#[allow(clippy::too_many_arguments)]
fn record_produce_failure_receipt(
    conn: &mut Connection,
    client_id: &str,
    item_id: &str,
    kind: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    idempotency_key: &str,
    error_code: &str,
    message: Option<&str>,
    now_ms: u64,
) {
    let mut details = serde_json::json!({
        "packet_kind": kind,
        "error_code": error_code,
    });
    if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
        details["message"] = serde_json::Value::String(message.to_string());
    }
    let after_json = serde_json::to_string(&details).ok();
    let receipt_key = format!("produce_failure:{kind}:{idempotency_key}:{error_code}");
    if let Err(receipt_err) = crate::store_core::record_failed_receipt(
        conn,
        crate::store_core::MutationRequest {
            client_id,
            entity_kind: "produce",
            entity_id: item_id,
            change_kind: "stage",
            actor_id,
            actor_kind,
            expected_revision: None,
            idempotency_key: &receipt_key,
            correlation_id: Some(item_id),
            causation_id: Some(idempotency_key),
            before_json: None,
            after_json,
            now_ms,
        },
        error_code,
    ) {
        tracing::warn!(
            %item_id,
            %kind,
            error_code,
            error = %receipt_err,
            "failed to record produce failure receipt"
        );
    }
}

#[cfg(test)]
static TEST_PRODUCE_LLM_RESPONSE: OnceLock<Mutex<Option<serde_json::Value>>> = OnceLock::new();
#[cfg(test)]
static TEST_PRODUCE_LLM_RESPONSES_BY_TASK: OnceLock<Mutex<HashMap<String, serde_json::Value>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_produce_llm_response(response: serde_json::Value) {
    *TEST_PRODUCE_LLM_RESPONSE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("test produce llm mutex poisoned") = Some(response);
}

#[cfg(test)]
pub(crate) fn set_test_produce_llm_response_for_task(
    task_id: impl Into<String>,
    response: serde_json::Value,
) {
    TEST_PRODUCE_LLM_RESPONSES_BY_TASK
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("test produce llm task mutex poisoned")
        .insert(task_id.into(), response);
}

#[cfg(test)]
fn take_test_produce_llm_response(task_id: &str) -> Option<serde_json::Value> {
    if let Some(response) = TEST_PRODUCE_LLM_RESPONSES_BY_TASK
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("test produce llm task mutex poisoned")
        .remove(task_id)
    {
        return Some(response);
    }
    TEST_PRODUCE_LLM_RESPONSE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("test produce llm mutex poisoned")
        .take()
}

fn execute_produce_llm(
    state: &AppState,
    purpose: &str,
    request: &TypedLlmTaskRequest,
) -> Result<bos_integrations::llm_typed_tasks::TypedLlmTaskOutputEnvelope, ProduceError> {
    #[cfg(test)]
    if let Some(response_json) = take_test_produce_llm_response(&request.task_id) {
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
        purpose,
        request,
    )
    .map_err(|err| ProduceError::Llm(err.code().to_string()))
}

/// The shared produce flow, blocking (the LLM call is synchronous). Both
/// callers — the manual-kickoff worker thread and the auto-produce pump —
/// go through here.
pub(crate) fn produce_blocking<F: ProduceFlavor>(
    state: &AppState,
    flavor: &F,
    item_id: &str,
    idempotency_key: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    scope: &OperatorScope,
) -> Result<F::Response, ProduceError> {
    let produce_now_ms = now_ms();
    // Phase 1 (locked): load + guard + deterministic context, then release
    // the lock for the LLM call.
    let (item, message, context, attempt) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let item = match crate::slices::work_queue::store::get_item_scoped(
            conn,
            &state.client_id,
            item_id,
            scope,
        ) {
            Ok(Some(found)) => found.item,
            Ok(None) => return Err(ProduceError::ItemNotFound),
            Err(err) => return Err(ProduceError::Store(err)),
        };
        validate_item_for_kind(&item, flavor.packet_kind()).map_err(ProduceError::Guard)?;
        match flavor.active_draft(conn, &state.client_id, &item.item_id) {
            // Idempotent re-produce: the existing active draft IS the result.
            Ok(Some(existing)) => return Ok(existing),
            Ok(None) => {}
            Err(err) => return Err(ProduceError::Store(err)),
        }
        let message = match resolve_source(conn, &state.client_id, &item) {
            Ok(Some(message)) => message,
            Ok(None) => return Err(ProduceError::SourceMissing),
            Err(SourceError::Unsupported) => return Err(ProduceError::SourceUnsupported),
            Err(SourceError::Store(err)) => return Err(ProduceError::Store(err)),
        };
        let context = flavor
            .prepare_context(conn, &state.client_id, &item, &message, scope, actor_id)
            .map_err(ProduceError::Store)?;
        let attempt = flavor
            .draft_attempts(conn, &state.client_id, &item.item_id)
            .map_err(ProduceError::Store)?
            + 1;
        (item, message, context, attempt)
    };

    // Phase 2 (unlocked): optional best-effort context enrichment, then the
    // bounded typed transform, usage-recorded.
    let context = flavor.enrich_context_unlocked(EnrichContext {
        state,
        item: &item,
        message: &message,
        scope,
        actor_id,
        actor_kind,
        context,
        attempt,
        now_ms: produce_now_ms,
    });
    let mut request = flavor.build_request(&state.client_id, &item, &message, &context, attempt);
    apply_operator_guidance(&mut request, &item);
    let purpose = flavor.purpose();
    let envelope = execute_produce_llm(state, purpose, &request).inspect_err(|err| {
        if let ProduceError::Llm(code) = &err {
            tracing::warn!(item_id = %item.item_id, purpose, code, "produce llm failed");
        }
    })?;

    // Phase 3 (locked): stage; the unique active-draft index resolves races.
    // The lock is scoped to this block so the after_stage hook (which may take
    // the lock again to spawn other produces) runs unlocked.
    let staged = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        if let Err(err) = flavor.stage(StageContext {
            conn: &mut *conn,
            client_id: &state.client_id,
            actor_id,
            item: &item,
            message: &message,
            response: &envelope.response_json,
            context: &context,
            model: &envelope.model,
            attempt,
            idempotency_key,
            now_ms: produce_now_ms,
        }) {
            if let StoreError::Domain(code) = &err {
                if code == flavor.already_active_code() {
                    if let Ok(Some(existing)) =
                        flavor.active_draft(conn, &state.client_id, &item.item_id)
                    {
                        // A race-loser returns the winner's draft; the winner
                        // already ran after_stage, so we don't re-run it.
                        return Ok(existing);
                    }
                }
            }
            let error_code = store_error_code(&err);
            let message = flavor.stage_failure_message(&envelope.response_json, &error_code);
            record_produce_failure_receipt(
                conn,
                &state.client_id,
                &item.item_id,
                flavor.packet_kind(),
                actor_id,
                actor_kind,
                idempotency_key,
                &error_code,
                message.as_deref(),
                produce_now_ms,
            );
            return Err(ProduceError::Store(err));
        }
        match flavor.active_draft(conn, &state.client_id, &item.item_id) {
            Ok(Some(staged)) => staged,
            Ok(None) => return Err(ProduceError::DraftVanished),
            Err(err) => return Err(ProduceError::Store(err)),
        }
    };
    flavor.after_stage(state, &item, actor_id);
    Ok(staged)
}

/// The shared produce flow for HTTP routes — async kickoff. Guards run
/// inline so the operator gets immediate 4xx feedback (and an existing
/// active draft returns 200 with the draft, idempotently); the LLM call
/// itself runs on a detached thread and the route answers 202
/// {"producing": true} right away. The queue feed shows "drafting…" via the
/// in-flight registry and panels poll draft/status read models.
pub async fn run<F>(
    state: AppState,
    flavor: F,
    item_id: &str,
    idempotency_key: &str,
    actor_id: &str,
    scope: OperatorScope,
) -> Response
where
    F: ProduceFlavor + Send + 'static,
    F::Response: Send + 'static,
{
    // Inline guards (mirrors produce_blocking's phase 1, which re-checks —
    // these exist purely for immediate operator feedback).
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let item = match crate::slices::work_queue::store::get_item_scoped(
            conn,
            &state.client_id,
            item_id,
            &scope,
        ) {
            Ok(Some(found)) => found.item,
            Ok(None) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "work_item_not_found")
            }
            Err(err) => return store_error_response(flavor.slice(), err),
        };
        if let Err(code) = validate_item_for_kind(&item, flavor.packet_kind()) {
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, code);
        }
        match flavor.active_draft(conn, &state.client_id, &item.item_id) {
            // Idempotent re-produce: the existing active draft IS the result.
            Ok(Some(existing)) => return Json(existing).into_response(),
            Ok(None) => {}
            Err(err) => return store_error_response(flavor.slice(), err),
        }
        let message = match resolve_source(conn, &state.client_id, &item) {
            Ok(Some(message)) => message,
            Ok(None) => {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "produce_source_missing")
            }
            Err(SourceError::Unsupported) => {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "produce_source_unsupported",
                )
            }
            Err(SourceError::Store(err)) => return store_error_response(flavor.slice(), err),
        };
        // Context preparation failures (e.g. no corpus evidence for the
        // brief) are operator-actionable — surface them before kickoff.
        if let Err(err) =
            flavor.prepare_context(conn, &state.client_id, &item, &message, &scope, actor_id)
        {
            return store_error_response(flavor.slice(), err);
        }
    }

    let kind = flavor.packet_kind();
    if !begin_produce(&state, item_id, kind) {
        // Already drafting (second click, or the pump beat us) — no-op.
        return (
            StatusCode::ACCEPTED,
            Json(bos_contracts::produce::ProduceKickoffResponse { producing: true }),
        )
            .into_response();
    }
    let owned_item = item_id.to_string();
    let owned_key = idempotency_key.to_string();
    let owned_actor = actor_id.to_string();
    let owned_scope = scope;
    let slice = flavor.slice();
    std::thread::Builder::new()
        .name(format!("produce-{kind}"))
        .spawn(move || {
            let result = produce_blocking(
                &state,
                &flavor,
                &owned_item,
                &owned_key,
                &owned_actor,
                ActorKindDto::Operator,
                &owned_scope,
            );
            end_produce(&state, &owned_item, kind);
            match result {
                Ok(_) => {
                    tracing::info!(item_id = %owned_item, kind, "manual produce staged a draft")
                }
                Err(ProduceError::Llm(code)) => {
                    tracing::warn!(item_id = %owned_item, kind, code, "manual produce llm failed")
                }
                Err(ProduceError::Store(err)) => {
                    tracing::warn!(item_id = %owned_item, kind, error = %err, slice, "manual produce store failed")
                }
                Err(ProduceError::Guard(code)) => {
                    tracing::info!(item_id = %owned_item, kind, code, "manual produce stopped by guard")
                }
                Err(_) => {
                    tracing::info!(item_id = %owned_item, kind, "manual produce stopped (item/source gone)")
                }
            }
        })
        .expect("spawn produce thread");
    (
        StatusCode::ACCEPTED,
        Json(bos_contracts::produce::ProduceKickoffResponse { producing: true }),
    )
        .into_response()
}

pub(crate) fn apply_operator_guidance(request: &mut TypedLlmTaskRequest, item: &WorkItem) {
    let guidance = item.produce_guidance.trim();
    if guidance.is_empty() {
        return;
    }
    request
        .input
        .text_blocks
        .push(bos_integrations::llm_typed_tasks::TypedLlmTextBlock {
            block_id: "operator_produce_guidance".to_string(),
            text: format!(
                "Operator guidance for this draft attempt. Follow it when it does not conflict \
                 with the task instructions, source grounding rules, or approval gates:\n{guidance}"
            ),
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    struct NoopFlavor;

    impl ProduceFlavor for NoopFlavor {
        type Response = ();

        fn packet_kind(&self) -> &'static str {
            "noop"
        }

        fn purpose(&self) -> &'static str {
            "noop"
        }

        fn slice(&self) -> &'static str {
            "noop"
        }

        fn already_active_code(&self) -> &'static str {
            "noop_already_active"
        }

        fn active_draft(
            &self,
            _conn: &Connection,
            _client_id: &str,
            _item_id: &str,
        ) -> Result<Option<Self::Response>, StoreError> {
            unimplemented!("not used by this unit test")
        }

        fn draft_attempts(
            &self,
            _conn: &Connection,
            _client_id: &str,
            _item_id: &str,
        ) -> Result<u64, StoreError> {
            unimplemented!("not used by this unit test")
        }

        fn build_request(
            &self,
            _client_id: &str,
            _item: &WorkItem,
            _message: &InboundMessageRecord,
            _context: &serde_json::Value,
            _attempt: u64,
        ) -> TypedLlmTaskRequest {
            unimplemented!("not used by this unit test")
        }

        fn stage(&self, _ctx: StageContext<'_>) -> Result<(), StoreError> {
            unimplemented!("not used by this unit test")
        }
    }

    fn item(status: WorkItemStatus, kinds: &[&str]) -> WorkItem {
        WorkItem {
            item_id: "wi_email_m1".to_string(),
            source_kind: "email".to_string(),
            source_ref: "m1".to_string(),
            category_id: "events".to_string(),
            title: "t".to_string(),
            summary: String::new(),
            packet_kinds: kinds.iter().map(|k| k.to_string()).collect(),
            status,
            accept_actor: (status == WorkItemStatus::Accepted)
                .then_some(bos_contracts::work_queue::WorkItemAcceptActor::Operator),
            ai_suggested: false,
            rationale: String::new(),
            produce_guidance: String::new(),
            source_user_id: None,
            assignee_user_id: None,
            visible_to_user_ids: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn message() -> InboundMessageRecord {
        InboundMessageRecord {
            source_key: "m1".to_string(),
            message_id: "m1".to_string(),
            thread_id: None,
            internal_date_ms: Some(1),
            from_addr: None,
            to_addr: None,
            subject: Some("Subject".to_string()),
            body_excerpt: "Body".to_string(),
            body_full: "Body".to_string(),
            headers: Vec::new(),
            labels: Vec::new(),
            resolved_category: "events".to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: None,
        }
    }

    fn email_fill_response() -> serde_json::Value {
        serde_json::json!({
            "body_text": "Thanks for reaching out. Could you send the haul-out date?",
            "confidence": "high",
            "provenance": [
                { "field": "body_text", "quote": "Need a quote" }
            ]
        })
    }

    fn state_with_email_item(item_id: &str, source_user_id: Option<&str>) -> AppState {
        let state = crate::http::test_support::test_state();
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let message_id = format!("msg_{item_id}");
        let mut record = message();
        record.source_key = message_id.clone();
        record.message_id = message_id.clone();
        record.thread_id = Some(format!("thread_{item_id}"));
        record.from_addr = Some("customer@example.com".to_string());
        record.to_addr = Some("ops@example.com".to_string());
        record.subject = Some("Need a quote".to_string());
        record.source_user_id = source_user_id.map(str::to_string);
        crate::slices::email_triage::store::record_inbound_message(conn, &state.client_id, &record)
            .expect("record inbound message");

        let mut work_item = item(WorkItemStatus::Accepted, &["email_draft_reply"]);
        work_item.item_id = item_id.to_string();
        work_item.source_ref = message_id;
        work_item.source_user_id = source_user_id.map(str::to_string);
        crate::slices::work_queue::store::insert_item(conn, &state.client_id, &work_item)
            .expect("insert work item");
        drop(persistence);
        state
    }

    fn state_with_calendar_item(item_id: &str, source_user_id: Option<&str>) -> AppState {
        let state = crate::http::test_support::test_state();
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let message_id = format!("msg_{item_id}");
        let mut record = message();
        record.source_key = message_id.clone();
        record.message_id = message_id.clone();
        record.thread_id = Some(format!("thread_{item_id}"));
        record.from_addr = Some("coach@example.com".to_string());
        record.to_addr = Some("ops@example.com".to_string());
        record.subject = Some("Team newsletter".to_string());
        record.body_excerpt = "Weekly team update with no date yet.".to_string();
        record.body_full = "Weekly team update with no date yet.".to_string();
        record.source_user_id = source_user_id.map(str::to_string);
        crate::slices::email_triage::store::record_inbound_message(conn, &state.client_id, &record)
            .expect("record inbound message");

        let mut work_item = item(WorkItemStatus::Accepted, &["calendar_event_draft"]);
        work_item.item_id = item_id.to_string();
        work_item.source_ref = message_id;
        work_item.source_user_id = source_user_id.map(str::to_string);
        crate::slices::work_queue::store::insert_item(conn, &state.client_id, &work_item)
            .expect("insert work item");
        drop(persistence);
        state
    }

    fn email_draft_exists(state: &AppState, item_id: &str) -> bool {
        let persistence = state.persistence.lock();
        crate::slices::email_drafts::store::active_draft_for_item(
            persistence.connection_ref(),
            &state.client_id,
            item_id,
        )
        .expect("query active draft")
        .is_some()
    }

    fn calendar_draft_exists(state: &AppState, item_id: &str) -> bool {
        let persistence = state.persistence.lock();
        crate::slices::calendar_drafts::store::active_draft_for_item(
            persistence.connection_ref(),
            &state.client_id,
            item_id,
        )
        .expect("query active draft")
        .is_some()
    }

    async fn wait_for_email_draft(state: &AppState, item_id: &str) {
        for _ in 0..100 {
            if email_draft_exists(state, item_id) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("email draft was not staged for {item_id}");
    }

    async fn wait_for_produce_failure(
        state: &AppState,
        item_id: &str,
        kind: &str,
        idempotency_key: &str,
    ) -> ProduceFailureReceipt {
        for _ in 0..100 {
            let found = {
                let persistence = state.persistence.lock();
                latest_produce_failure_receipt(
                    persistence.connection_ref(),
                    &state.client_id,
                    item_id,
                    kind,
                    idempotency_key,
                )
                .expect("query produce failure")
            };
            if let Some(failure) = found {
                return failure;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("produce failure receipt was not recorded for {item_id}");
    }

    async fn response_error(response: Response) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .expect("error code")
            .to_string()
    }

    #[test]
    fn guards_require_accepted_item_with_the_kind() {
        assert_eq!(
            validate_item_for_kind(
                &item(WorkItemStatus::Open, &["calendar_event_draft"]),
                "calendar_event_draft"
            ),
            Err("produce_item_not_accepted")
        );
        assert_eq!(
            validate_item_for_kind(
                &item(WorkItemStatus::Accepted, &["follow_up_task"]),
                "calendar_event_draft"
            ),
            Err("produce_kind_not_suggested")
        );
        assert!(validate_item_for_kind(
            &item(WorkItemStatus::Accepted, &["calendar_event_draft"]),
            "calendar_event_draft"
        )
        .is_ok());
    }

    #[test]
    fn default_unlocked_context_hook_is_noop() {
        let state = crate::http::test_support::test_state();
        let item = item(WorkItemStatus::Accepted, &["noop"]);
        let message = message();
        let context = serde_json::json!({ "evidence": ["local"], "attempt": 1 });

        let enriched = NoopFlavor.enrich_context_unlocked(EnrichContext {
            state: &state,
            item: &item,
            message: &message,
            scope: &OperatorScope::All,
            actor_id: "operator",
            actor_kind: ActorKindDto::Operator,
            context: context.clone(),
            attempt: 1,
            now_ms: 123,
        });

        assert_eq!(enriched, context);
    }

    #[test]
    fn operator_guidance_is_added_as_llm_text_block() {
        let mut item = item(WorkItemStatus::Accepted, &["noop"]);
        item.produce_guidance = "Keep the reply short and ask for the haul-out date.".to_string();
        let mut request = TypedLlmTaskRequest {
            task_id: "task".to_string(),
            correlation_id: item.item_id.clone(),
            idempotency_key: "key".to_string(),
            tenant_or_project_scope: "test-client".to_string(),
            source_entity: None,
            spec: bos_integrations::llm_typed_tasks::TypedLlmTaskSpec {
                task_class: bos_integrations::llm_typed_tasks::TypedLlmTaskClass::Draft,
                prompt_template_id: "noop".to_string(),
                prompt_template_version: "v1".to_string(),
                prompt_template_hash: "hash".to_string(),
                schema_ref: "noop".to_string(),
                response_format:
                    bos_integrations::llm_typed_tasks::TypedLlmResponseFormat::JsonObject,
                max_input_bytes: 1024,
                max_output_bytes: 1024,
                max_tokens: 256,
                timeout_ms: 1_000,
                capabilities:
                    bos_integrations::llm_typed_tasks::TypedLlmTaskCapabilities::pure_transformation(
                    ),
                authority: bos_integrations::llm_typed_tasks::TypedLlmAuthority::no_side_effects(),
            },
            input: bos_integrations::llm_typed_tasks::TypedLlmTaskInput {
                json: serde_json::json!({}),
                text_blocks: Vec::new(),
            },
            execution_policy: bos_integrations::llm_typed_tasks::TypedLlmExecutionPolicy {
                default_route: bos_integrations::llm_typed_tasks::TypedLlmExecutionRoute::Harness,
                fallback_policy:
                    bos_integrations::llm_typed_tasks::TypedLlmFallbackPolicy::NoFallback,
                retry_policy: bos_integrations::llm_typed_tasks::TypedLlmRetryPolicy {
                    max_attempts: 1,
                    backoff_ms: 0,
                    max_elapsed_ms: 1_000,
                },
            },
            provider_policy: bos_integrations::llm_typed_tasks::TypedLlmProviderPolicy {
                preferred_provider: String::new(),
                preferred_model: String::new(),
                fallback_provider: None,
                fallback_model: None,
            },
            safety_policy: bos_integrations::llm_typed_tasks::TypedLlmSafetyPolicy {
                redaction_policy:
                    bos_integrations::llm_typed_tasks::TypedLlmRedactionPolicy::PreSubmit,
                raw_output_retention:
                    bos_integrations::llm_typed_tasks::TypedLlmRawOutputRetention::None,
            },
        };

        apply_operator_guidance(&mut request, &item);

        assert_eq!(request.input.text_blocks.len(), 1);
        assert_eq!(
            request.input.text_blocks[0].block_id,
            "operator_produce_guidance"
        );
        assert!(request.input.text_blocks[0]
            .text
            .contains("ask for the haul-out date"));
    }

    #[tokio::test]
    async fn manual_produce_hides_cross_scope_and_legacy_null_items() {
        for (item_id, source_user_id) in [
            ("wi_email_scope_u2", Some("u2")),
            ("wi_email_scope_null", None),
        ] {
            let state = state_with_email_item(item_id, source_user_id);

            let response = run(
                state.clone(),
                crate::slices::email_drafts::service::Produce,
                item_id,
                &format!("produce:{item_id}"),
                "u1",
                OperatorScope::User("u1".to_string()),
            )
            .await;

            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(response_error(response).await, "work_item_not_found");
            assert!(
                !email_draft_exists(&state, item_id),
                "hidden item must not stage a draft"
            );
        }
    }

    #[tokio::test]
    async fn produce_stages_owned_manual_and_all_scoped_system_items() {
        let owned_item_id = "wi_email_scope_u1";
        let owned_state = state_with_email_item(owned_item_id, Some("u1"));
        set_test_produce_llm_response_for_task(
            "email_fill_wi_email_scope_u1_1",
            email_fill_response(),
        );

        let response = run(
            owned_state.clone(),
            crate::slices::email_drafts::service::Produce,
            owned_item_id,
            "produce:owned",
            "u1",
            OperatorScope::User("u1".to_string()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        wait_for_email_draft(&owned_state, owned_item_id).await;

        let legacy_item_id = "wi_email_scope_legacy";
        let legacy_state = state_with_email_item(legacy_item_id, None);
        set_test_produce_llm_response(email_fill_response());

        produce_blocking_by_kind(
            &legacy_state,
            legacy_item_id,
            "email_draft_reply",
            "produce:legacy",
            "auto_produce_pump",
            ActorKindDto::System,
        )
        .expect("system produce should stage legacy item");
        assert!(email_draft_exists(&legacy_state, legacy_item_id));
    }

    #[tokio::test]
    async fn manual_produce_stage_failure_returns_accepted_and_receipts_failure() {
        let item_id = "wi_email_stage_failure";
        let idempotency_key = "produce:stage-failure";
        let state = state_with_email_item(item_id, Some("u1"));
        set_test_produce_llm_response_for_task(
            "email_fill_wi_email_stage_failure_1",
            serde_json::json!({
                "not": "a reply fill"
            }),
        );

        let response = run(
            state.clone(),
            crate::slices::email_drafts::service::Produce,
            item_id,
            idempotency_key,
            "u1",
            OperatorScope::User("u1".to_string()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let failure =
            wait_for_produce_failure(&state, item_id, "email_draft_reply", idempotency_key).await;
        assert_eq!(failure.error_code, "email_fill_invalid_response");
        assert!(
            !email_draft_exists(&state, item_id),
            "invalid fill must not stage a draft"
        );
    }

    #[tokio::test]
    async fn manual_calendar_no_event_status_includes_reason_and_receipts_failure() {
        let item_id = "wi_email_calendar_no_event";
        let idempotency_key = "produce:calendar-no-event";
        let state = state_with_calendar_item(item_id, Some("u1"));
        set_test_produce_llm_response_for_task(
            "cal_extract_wi_email_calendar_no_event_1",
            serde_json::json!({
                "extractable": false,
                "reason": "newsletter with no concrete dated event"
            }),
        );

        let response = run(
            state.clone(),
            crate::slices::calendar_drafts::service::Produce,
            item_id,
            idempotency_key,
            "u1",
            OperatorScope::User("u1".to_string()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let failure =
            wait_for_produce_failure(&state, item_id, "calendar_event_draft", idempotency_key)
                .await;
        assert_eq!(failure.error_code, "calendar_extract_no_event");
        assert_eq!(
            failure.message.as_deref(),
            Some("newsletter with no concrete dated event")
        );
        assert!(
            !calendar_draft_exists(&state, item_id),
            "unextractable calendar item must not stage a draft"
        );
    }

    fn add_operator_user(state: &AppState, user_id: &str, token: &str) {
        let mut persistence = state.persistence.lock();
        crate::slices::operator_users::store::create_user(
            persistence.connection(),
            &state.client_id,
            "operator",
            &bos_contracts::operator_users::OperatorUser {
                user_id: user_id.to_string(),
                display_name: user_id.to_string(),
                active: true,
                archived_at_ms: None,
                default_calendar_id: None,
                created_at_ms: 1_000,
                updated_at_ms: 1_000,
            },
            token,
            &format!("create_{user_id}"),
        )
        .expect("create operator user");
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("header value"),
        );
        headers
    }

    #[tokio::test]
    async fn produce_status_respects_operator_scope() {
        let item_id = "wi_email_status_scope";
        let idempotency_key = "produce:status-scope";
        let state = state_with_calendar_item(item_id, Some("u1"));
        add_operator_user(&state, "u1", "tok_u1");
        add_operator_user(&state, "u2", "tok_u2");
        set_test_produce_llm_response(serde_json::json!({
            "extractable": false,
            "reason": "no concrete dated event"
        }));

        let response = run(
            state.clone(),
            crate::slices::calendar_drafts::service::Produce,
            item_id,
            idempotency_key,
            "u1",
            OperatorScope::User("u1".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        wait_for_produce_failure(&state, item_id, "calendar_event_draft", idempotency_key).await;

        let owner = produce_status(
            State(state.clone()),
            bearer_headers("tok_u1"),
            Query(ProduceStatusQuery {
                item_id: item_id.to_string(),
                kind: "calendar_event_draft".to_string(),
                idempotency_key: idempotency_key.to_string(),
            }),
        )
        .await;
        assert_eq!(owner.status(), StatusCode::OK);

        let other = produce_status(
            State(state),
            bearer_headers("tok_u2"),
            Query(ProduceStatusQuery {
                item_id: item_id.to_string(),
                kind: "calendar_event_draft".to_string(),
                idempotency_key: idempotency_key.to_string(),
            }),
        )
        .await;
        assert_eq!(other.status(), StatusCode::NOT_FOUND);
        assert_eq!(response_error(other).await, "work_item_not_found");
    }
}
