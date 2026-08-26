//! Emission logic: a classified inbound message becomes a work item when its
//! category's policy says so. Called by the email_triage ingestion pump for
//! every candidate message (new AND already-ingested), so a policy turned on
//! later still backfills items on the next poll — the source-existence check
//! keeps re-emits receipt-quiet.

use bos_contracts::email_identity::{AttentionLevel, AttentionSignal};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::packet_proposals::{
    PacketProposalKindOutcome, PacketProposalKindOutcomeStatus, PacketProposalRunStatus,
};
use bos_contracts::work_queue::{
    LaunchAgentRequest, LaunchAgentResponse, WorkItem, WorkItemAcceptActor,
    WorkItemAttentionSummary, WorkItemFailureNotification, WorkItemSourceBodyFormat,
    WorkItemSourceResponse, WorkItemStatus, WorkItemWithRevision,
};
use rusqlite::Connection;

use super::agent_launch;
use super::store;
use crate::http::OperatorScope;
use crate::outbox::{AttemptOutcome, STATUS_DELIVERED};
use crate::store_core::MutationOutcome;
use crate::store_core::StoreError;

const SUMMARY_MAX_CHARS: usize = 200;
const ATTENTION_LABEL_MAX_CHARS: usize = 48;
const PRODUCE_FAILURE_LOOKBACK_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const LOWER_ATTENTION_DEFER_KINDS: &[&str] = &["follow_up_task", "calendar_event_draft"];
const SOURCE_SCOPED_DRAFT_KINDS: &[&str] = &[
    "calendar_event_draft",
    "crm_activity",
    "crm_sales_intent",
    "email_draft_reply",
    "follow_up_task",
];
type PendingKindsByItem = std::collections::HashMap<String, Vec<String>>;
type FailureNotificationsByItem =
    std::collections::HashMap<String, Vec<WorkItemFailureNotification>>;

/// The queue feed: stored items decorated with the cross-slice signals —
/// staged drafts awaiting a decision ("needs you") and, when the auto-produce
/// pump is running, kinds it is about to draft ("drafting…", so the item
/// stays visible while the operator waits). `auto_produce_running` is the
/// effective runtime setting, including stored admin overrides.
/// `now_ms` is injected by the caller so the failure lookback stays testable.
pub struct FeedOptions<'a> {
    pub now_ms: u64,
    pub auto_produce_running: bool,
    pub debug_enabled: bool,
    pub in_flight: &'a std::collections::HashSet<(String, String)>,
}

pub fn feed(
    conn: &Connection,
    client_id: &str,
    status: Option<WorkItemStatus>,
    limit: usize,
    scope: &OperatorScope,
    options: FeedOptions<'_>,
) -> Result<Vec<WorkItemWithRevision>, StoreError> {
    let mut items = store::list_items(conn, client_id, status, limit, scope)?;
    let mut staged = crate::produce::staged_draft_kinds_by_item(conn, client_id)?;
    let mut pending: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut failures = failure_notifications_by_item(
        conn,
        client_id,
        &items,
        options.now_ms,
        options.debug_enabled,
    )?;
    let (proposal_pending, proposal_failures) =
        packet_proposal_signals_by_item(conn, client_id, &items, options.debug_enabled)?;
    for (item_id, mut kinds) in proposal_pending {
        let entry = pending.entry(item_id).or_default();
        for kind in kinds.drain(..) {
            if !entry.contains(&kind) {
                entry.push(kind);
            }
        }
    }
    for (item_id, mut signals) in proposal_failures {
        failures.entry(item_id).or_default().append(&mut signals);
    }
    // Running produces (manual kickoffs + pump workers) always show.
    for (item_id, kind) in options.in_flight {
        pending
            .entry(item_id.clone())
            .or_default()
            .push(kind.clone());
    }
    if options.auto_produce_running {
        for (item_id, kind) in
            crate::produce::collect_auto_produce_candidates(conn, client_id, usize::MAX)?
        {
            let entry = pending.entry(item_id).or_default();
            if !entry.contains(&kind) {
                entry.push(kind);
            }
        }
    }
    for entry in &mut items {
        decorate_attention(conn, client_id, entry)?;
        if entry.item.status == WorkItemStatus::Accepted {
            if let Some(kinds) = staged.remove(&entry.item.item_id) {
                entry.staged_draft_kinds = kinds
                    .into_iter()
                    .filter(|kind| {
                        !SOURCE_SCOPED_DRAFT_KINDS.contains(&kind.as_str())
                            || scope.matches_source_user(entry.item.source_user_id.as_deref())
                    })
                    .collect();
            }
            if let Some(kinds) = pending.remove(&entry.item.item_id) {
                entry.pending_produce_kinds = kinds;
            }
            if let Some(signals) = failures.remove(&entry.item.item_id) {
                let active: std::collections::HashSet<&str> = entry
                    .staged_draft_kinds
                    .iter()
                    .chain(entry.pending_produce_kinds.iter())
                    .map(String::as_str)
                    .collect();
                entry.failure_notifications = signals
                    .into_iter()
                    .filter(|signal| {
                        signal
                            .packet_kind
                            .as_deref()
                            .is_none_or(|kind| !active.contains(kind))
                    })
                    .collect();
            }
        }
    }
    Ok(items)
}

fn decorate_attention(
    conn: &Connection,
    client_id: &str,
    entry: &mut WorkItemWithRevision,
) -> Result<(), StoreError> {
    if entry.item.source_kind != super::SOURCE_KIND_EMAIL {
        return Ok(());
    }
    let Some(signal) = crate::slices::email_triage::store::strongest_attention_signal(
        conn,
        client_id,
        &entry.item.source_ref,
    )?
    else {
        return Ok(());
    };
    entry.attention = Some(attention_summary(signal));
    Ok(())
}

fn attention_summary(signal: AttentionSignal) -> WorkItemAttentionSummary {
    let label = signal
        .label
        .as_deref()
        .and_then(|label| {
            let label = label.trim();
            if label.is_empty() {
                None
            } else {
                Some(label.chars().take(ATTENTION_LABEL_MAX_CHARS).collect())
            }
        })
        .unwrap_or_else(|| default_attention_label(signal.level).to_string());
    WorkItemAttentionSummary {
        level: signal.level,
        label,
        detail: signal.detail,
    }
}

fn default_attention_label(level: AttentionLevel) -> &'static str {
    match level {
        AttentionLevel::Lower => "Lower attention",
        AttentionLevel::Normal => "Attention",
        AttentionLevel::Higher => "Needs attention",
    }
}

pub fn attention_feed(
    conn: &Connection,
    client_id: &str,
    status: Option<WorkItemStatus>,
    limit: usize,
    scope: &OperatorScope,
    options: FeedOptions<'_>,
) -> Result<Vec<WorkItemWithRevision>, StoreError> {
    let mut items = feed(conn, client_id, status, i64::MAX as usize, scope, options)?;
    items.retain(needs_attention);
    items.truncate(limit);
    Ok(items)
}

pub fn source_attention_feed(
    conn: &Connection,
    client_id: &str,
    status: Option<WorkItemStatus>,
    level: AttentionLevel,
    limit: usize,
    scope: &OperatorScope,
    options: FeedOptions<'_>,
) -> Result<Vec<WorkItemWithRevision>, StoreError> {
    let mut items = feed(conn, client_id, status, i64::MAX as usize, scope, options)?;
    items.retain(|entry| {
        entry
            .attention
            .as_ref()
            .is_some_and(|attention| attention.level == level)
    });
    items.truncate(limit);
    Ok(items)
}

pub fn needs_attention(entry: &WorkItemWithRevision) -> bool {
    entry.item.status == WorkItemStatus::Open
        || (entry.item.status == WorkItemStatus::Accepted
            && (!entry.staged_draft_kinds.is_empty()
                || !entry.pending_produce_kinds.is_empty()
                || !entry.failure_notifications.is_empty()))
}

#[derive(Debug)]
pub enum LaunchAgentError {
    Disabled,
    MonitorUnconfigured,
    ItemNotFound,
    PayloadBuild,
    AlreadyRequested,
    AttachmentStageFailed(crate::slices::email_triage::service::AttachmentEvidenceError),
    ResultInvalid,
    JobNotClaimable,
    DeliveryFailed,
    JoinFailed,
    Store(StoreError),
}

pub async fn launch_agent_from_item(
    state: crate::http::AppState,
    item_id: String,
    actor_id: String,
    scope: OperatorScope,
    request: LaunchAgentRequest,
) -> Result<LaunchAgentResponse, LaunchAgentError> {
    if !crate::env_registry::flag(&crate::env_registry::BOS_AGENT_LAUNCH_ENABLED) {
        return Err(LaunchAgentError::Disabled);
    }
    let monitor_url =
        crate::env_registry::string(&crate::env_registry::BOS_DEBUG_AGENT_MONITOR_URL)
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .ok_or(LaunchAgentError::MonitorUnconfigured)?;

    let launch_job_id = agent_launch::job_id(&item_id, &request.idempotency_key);
    let (item, source, launch_defaults) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let item = store::get_item_scoped(conn, &state.client_id, &item_id, &scope)
            .map_err(LaunchAgentError::Store)?
            .ok_or(LaunchAgentError::ItemNotFound)?
            .item;
        if let Some(result_json) =
            crate::outbox::job_result_json(conn, &state.client_id, &launch_job_id)
                .map_err(LaunchAgentError::Store)?
        {
            return launch_response_with_staged_paths(
                conn,
                &state.client_id,
                &launch_job_id,
                &result_json,
            );
        }
        if crate::outbox::job_exists(conn, &state.client_id, &launch_job_id)
            .map_err(LaunchAgentError::Store)?
        {
            return Err(LaunchAgentError::AlreadyRequested);
        }
        let source = item_source(conn, &state.client_id, &item_id, &scope).ok();
        let category = crate::slices::email_triage::store::category_by_id(
            conn,
            &state.client_id,
            &item.category_id,
        )
        .map_err(LaunchAgentError::Store)?;
        let launch_defaults = resolve_agent_launch_defaults(category.as_ref(), &request);
        (item, source, launch_defaults)
    };

    let staged_evidence_paths = stage_selected_attachments_for_launch(
        state.clone(),
        &actor_id,
        &scope,
        &item,
        &request,
        &launch_defaults.work_dir,
        &launch_job_id,
    )
    .await?;
    let context = context_with_staged_evidence(&launch_defaults.context, &staged_evidence_paths);

    let job_id = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let display_name = format!("BusinessOS work item {}", item.item_id);
        let prompt = agent_launch::build_prompt(
            &state.client_id,
            &item,
            source.as_ref(),
            &context,
            &launch_defaults.work_dir,
        );
        let job = agent_launch::build_outbox_job(
            &item.item_id,
            &request.idempotency_key,
            &monitor_url,
            &display_name,
            &prompt,
            &launch_defaults.work_dir,
        )
        .map_err(|err| {
            tracing::warn!(error = %err, "work item agent launch payload build failed");
            LaunchAgentError::PayloadBuild
        })?;
        let job_id = job.job_id.clone();
        match store::record_agent_launch_request(
            conn,
            store::AgentLaunchRequestContext {
                client_id: &state.client_id,
                item_id: &item_id,
                actor_id: &actor_id,
                operator_context: &context,
                job: &job,
                idempotency_key: &request.idempotency_key,
                now_ms: crate::http::now_ms(),
            },
        ) {
            Ok(MutationOutcome::Applied { .. }) => {}
            Ok(MutationOutcome::ReplayedIdempotent { .. }) => {
                let result_json = crate::outbox::job_result_json(conn, &state.client_id, &job_id)
                    .map_err(LaunchAgentError::Store)?
                    .ok_or(LaunchAgentError::AlreadyRequested)?;
                return launch_response_with_staged_paths(
                    conn,
                    &state.client_id,
                    &job_id,
                    &result_json,
                );
            }
            Ok(MutationOutcome::RevisionConflict { .. }) => {
                return Err(LaunchAgentError::AlreadyRequested);
            }
            Err(err) => return Err(LaunchAgentError::Store(err)),
        }
        job_id
    };

    tokio::task::spawn_blocking(move || launch_claimed_agent_job(state, job_id))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "work item agent launch task failed");
            LaunchAgentError::JoinFailed
        })
        .map(|result| {
            result.map(|mut response| {
                response.staged_evidence_paths = staged_evidence_paths;
                response
            })
        })?
}

async fn stage_selected_attachments_for_launch(
    state: crate::http::AppState,
    actor_id: &str,
    scope: &OperatorScope,
    item: &WorkItem,
    request: &LaunchAgentRequest,
    work_dir: &str,
    launch_job_id: &str,
) -> Result<Vec<String>, LaunchAgentError> {
    if item.source_kind != super::SOURCE_KIND_EMAIL || request.attachment_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for attachment_id in dedupe_ids(&request.attachment_ids) {
        let response = crate::slices::email_triage::service::stage_attachment_evidence_for_launch(
            state.clone(),
            actor_id.to_string(),
            scope.clone(),
            item.source_ref.clone(),
            attachment_id.clone(),
            bos_contracts::email_triage::EmailAttachmentEvidenceRequest {
                session_id: launch_job_id.to_string(),
                item_id: Some(item.item_id.clone()),
                target_dir: Some(agent_launch::effective_work_dir(work_dir).to_string()),
                idempotency_key: format!("{}:attachment:{attachment_id}", request.idempotency_key),
            },
        )
        .await
        .map_err(|err| {
            tracing::warn!(error = ?err, attachment_id = %attachment_id, "agent launch attachment staging failed");
            LaunchAgentError::AttachmentStageFailed(err)
        })?;
        paths.push(response.path);
    }
    Ok(paths)
}

fn launch_response_with_staged_paths(
    conn: &Connection,
    client_id: &str,
    session_id: &str,
    result_json: &str,
) -> Result<LaunchAgentResponse, LaunchAgentError> {
    let mut response: LaunchAgentResponse =
        serde_json::from_str(result_json).map_err(|_| LaunchAgentError::ResultInvalid)?;
    response.staged_evidence_paths =
        crate::slices::email_triage::store::agent_evidence_paths_for_session(
            conn, client_id, session_id,
        )
        .map_err(LaunchAgentError::Store)?;
    Ok(response)
}

fn dedupe_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn context_with_staged_evidence(context: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return context.to_string();
    }
    let mut out = context.trim().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("--- Staged attachment files ---\n");
    for path in paths {
        out.push_str("- ");
        out.push_str(path);
        out.push('\n');
    }
    out
}

struct AgentLaunchDefaults {
    context: String,
    work_dir: String,
}

fn resolve_agent_launch_defaults(
    category: Option<&bos_contracts::email_triage::CategoryRecord>,
    request: &LaunchAgentRequest,
) -> AgentLaunchDefaults {
    let category_context = category
        .map(|category| category.default_agent_context.as_str())
        .unwrap_or("");
    let category_work_dir = category
        .map(|category| category.default_agent_dir.as_str())
        .unwrap_or("");
    let work_dir = agent_launch::effective_work_dir(agent_launch::resolve_work_dir(
        request.work_dir.as_deref(),
        category_work_dir,
    ))
    .to_string();
    AgentLaunchDefaults {
        context: agent_launch::combine_context(category_context, request.context.as_str()),
        work_dir,
    }
}

fn launch_claimed_agent_job(
    state: crate::http::AppState,
    job_id: String,
) -> Result<LaunchAgentResponse, LaunchAgentError> {
    let now = crate::http::now_ms();
    let claimed = {
        let mut persistence = state.persistence.lock();
        crate::outbox::claim_due_job_by_id(
            persistence.connection(),
            &state.client_id,
            &job_id,
            120_000,
            now,
        )
        .map_err(LaunchAgentError::Store)?
    };
    let Some(job) = claimed else {
        return Err(LaunchAgentError::JobNotClaimable);
    };
    let outcome = agent_launch::deliver(&job, crate::http::now_ms());
    let result_json = match &outcome {
        AttemptOutcome::Delivered { result_json } => Some(result_json.clone()),
        AttemptOutcome::Retry { error, .. }
        | AttemptOutcome::Terminal { error, .. }
        | AttemptOutcome::OutcomeUnknown { error, .. } => {
            tracing::warn!(error, "work item agent launch outbox delivery failed");
            None
        }
    };
    let status = {
        let mut persistence = state.persistence.lock();
        crate::outbox::record_attempt(
            persistence.connection(),
            &state.client_id,
            &job,
            &outcome,
            crate::http::now_ms(),
        )
        .map_err(LaunchAgentError::Store)?
    };
    match (status, result_json) {
        (STATUS_DELIVERED, Some(result_json)) => {
            serde_json::from_str(&result_json).map_err(|_| LaunchAgentError::ResultInvalid)
        }
        _ => Err(LaunchAgentError::DeliveryFailed),
    }
}

fn failure_notifications_by_item(
    conn: &Connection,
    client_id: &str,
    items: &[WorkItemWithRevision],
    now_ms: u64,
    debug_enabled: bool,
) -> Result<std::collections::HashMap<String, Vec<WorkItemFailureNotification>>, StoreError> {
    let mut out =
        produce_failure_notifications_by_item(conn, client_id, items, now_ms, debug_enabled)?;
    for (item_id, mut signals) in
        outbox_failure_notifications_by_item(conn, client_id, items, debug_enabled)?
    {
        out.entry(item_id).or_default().append(&mut signals);
    }
    for signals in out.values_mut() {
        signals.sort_by_key(|signal| std::cmp::Reverse(signal.occurred_at_ms));
    }
    Ok(out)
}

fn produce_failure_notifications_by_item(
    conn: &Connection,
    client_id: &str,
    items: &[WorkItemWithRevision],
    now_ms: u64,
    debug_enabled: bool,
) -> Result<std::collections::HashMap<String, Vec<WorkItemFailureNotification>>, StoreError> {
    let item_ids: Vec<String> = items
        .iter()
        .filter(|entry| entry.item.status == WorkItemStatus::Accepted)
        .map(|entry| entry.item.item_id.clone())
        .collect();
    let rows = crate::slices::ai_usage::store::usage_for_correlations(
        conn,
        client_id,
        &item_ids,
        now_ms.saturating_sub(PRODUCE_FAILURE_LOOKBACK_MS),
    )?;
    let accepted_kinds: std::collections::HashMap<&str, std::collections::HashSet<&str>> = items
        .iter()
        .filter(|entry| entry.item.status == WorkItemStatus::Accepted)
        .map(|entry| {
            (
                entry.item.item_id.as_str(),
                entry.item.packet_kinds.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let mut seen_latest: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut out: std::collections::HashMap<String, Vec<WorkItemFailureNotification>> =
        std::collections::HashMap::new();
    for row in rows {
        let Some(kind) = crate::produce::packet_kind_for_purpose(&row.purpose) else {
            continue;
        };
        let Some(kinds) = accepted_kinds.get(row.correlation_id.as_str()) else {
            continue;
        };
        if !kinds.contains(kind) {
            continue;
        }
        let key = (row.correlation_id.clone(), kind.to_string());
        if !seen_latest.insert(key) {
            continue;
        }
        if row.success {
            continue;
        }
        let diagnostic_id = format!("llm:{}", row.usage_id);
        out.entry(row.correlation_id)
            .or_default()
            .push(WorkItemFailureNotification {
                notification_id: format!(
                    "ai_produce:{kind}:{}",
                    row.error_code.as_deref().unwrap_or("llm_failed")
                ),
                source: "ai_produce".to_string(),
                packet_kind: Some(kind.to_string()),
                title: "Draft generation failed".to_string(),
                message: packet_title(kind),
                next_action: Some("Open Drafts to retry this item.".to_string()),
                diagnostic_href: debug_enabled.then(|| format!("#debug/{diagnostic_id}")),
                diagnostic_id: debug_enabled.then_some(diagnostic_id),
                error_code: row.error_code,
                occurred_at_ms: row.recorded_at_ms,
            });
    }
    Ok(out)
}

fn outbox_failure_notifications_by_item(
    conn: &Connection,
    client_id: &str,
    items: &[WorkItemWithRevision],
    debug_enabled: bool,
) -> Result<std::collections::HashMap<String, Vec<WorkItemFailureNotification>>, StoreError> {
    let item_ids: Vec<String> = items
        .iter()
        .filter(|entry| entry.item.status == WorkItemStatus::Accepted)
        .map(|entry| entry.item.item_id.clone())
        .collect();
    if item_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = vec!["?"; item_ids.len()].join(", ");
    let sql = format!(
        "SELECT d.item_id, d.packet_kind, j.job_id, j.provider, j.capability, j.status, \
         j.attempts, j.last_error, j.updated_at_ms \
         FROM outbox_jobs j \
         JOIN ( \
           SELECT client_id, draft_id, item_id, '{}' AS source_entity_kind, 'calendar_event_draft' AS packet_kind FROM calendar_event_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'crm_activity' FROM crm_note_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'crm_record_create' FROM crm_record_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'email_draft_reply' FROM email_reply_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'invoice_draft' FROM invoice_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'ledger_entry' FROM ledger_entry_drafts \
           UNION ALL SELECT client_id, draft_id, item_id, '{}', 'claim_draft' FROM claim_drafts \
         ) d ON d.client_id = j.client_id \
           AND d.source_entity_kind = j.source_entity_kind \
           AND d.draft_id = j.source_entity_id \
         JOIN work_items w ON w.client_id = j.client_id AND w.item_id = d.item_id \
         WHERE j.client_id = ? AND w.status = 'accepted' AND j.last_error IS NOT NULL \
           AND d.item_id IN ({placeholders}) \
         ORDER BY j.updated_at_ms DESC, j.job_id DESC",
        crate::slices::calendar_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::crm_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::crm_record_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::email_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::invoice_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::ledger_drafts::store::DRAFT_ENTITY_KIND,
        crate::slices::claim_drafts::store::DRAFT_ENTITY_KIND,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(item_ids.len() + 1);
    params.push(&client_id);
    for id in &item_ids {
        params.push(id);
    }
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, i64>(8)? as u64,
        ))
    })?;
    let mut seen = std::collections::HashSet::new();
    let mut out: std::collections::HashMap<String, Vec<WorkItemFailureNotification>> =
        std::collections::HashMap::new();
    for row in rows {
        let (
            item_id,
            packet_kind,
            job_id,
            _provider,
            _capability,
            status,
            _attempts,
            _last_error,
            occurred_at_ms,
        ) = row?;
        if !seen.insert(job_id.clone()) {
            continue;
        }
        let diagnostic_id = format!("outbox:{job_id}");
        let message = format!(
            "We couldn't deliver your {} — open the draft panel to see what happened or try again.",
            packet_label(&packet_kind),
        );
        out.entry(item_id)
            .or_default()
            .push(WorkItemFailureNotification {
                notification_id: format!("provider_delivery:{job_id}:{status}"),
                source: "provider_delivery".to_string(),
                packet_kind: Some(packet_kind),
                title: "Couldn't deliver draft".to_string(),
                message,
                next_action: Some(
                    "Open the draft panel to retry or see what went wrong.".to_string(),
                ),
                diagnostic_href: debug_enabled.then(|| format!("#debug/{diagnostic_id}")),
                diagnostic_id: debug_enabled.then_some(diagnostic_id),
                error_code: Some(status),
                occurred_at_ms,
            });
    }
    Ok(out)
}

fn packet_proposal_signals_by_item(
    conn: &Connection,
    client_id: &str,
    items: &[WorkItemWithRevision],
    debug_enabled: bool,
) -> Result<(PendingKindsByItem, FailureNotificationsByItem), StoreError> {
    let item_ids: Vec<String> = items
        .iter()
        .filter(|entry| entry.item.status == WorkItemStatus::Accepted)
        .map(|entry| entry.item.item_id.clone())
        .collect();
    if item_ids.is_empty() {
        return Ok((
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        ));
    }
    let placeholders = vec!["?"; item_ids.len()].join(", ");
    let sql = format!(
        "SELECT p.run_id, p.item_id, p.status, p.candidate_packet_kinds_json, p.outcomes_json, \
                p.error_code, p.updated_at_ms, \
                (SELECT r.actor_id FROM receipts r \
                 WHERE r.client_id = p.client_id \
                   AND r.entity_kind = 'packet_proposal_run' \
                   AND r.entity_id = p.run_id \
                   AND r.change_kind = 'start' \
                 ORDER BY r.created_at_ms ASC, r.receipt_id ASC LIMIT 1) AS start_actor_id \
         FROM packet_proposal_runs p \
         WHERE p.client_id = ? AND p.item_id IN ({placeholders}) \
           AND p.status IN ('running', 'completed', 'failed') \
         ORDER BY p.updated_at_ms DESC, p.run_id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(item_ids.len() + 1);
    params.push(&client_id);
    for id in &item_ids {
        params.push(id);
    }
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)? as u64,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;

    let mut pending: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut failures: std::collections::HashMap<String, Vec<WorkItemFailureNotification>> =
        std::collections::HashMap::new();
    let mut terminal_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut newer_running_seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for row in rows {
        let (
            run_id,
            item_id,
            status_raw,
            candidate_packet_kinds_json,
            outcomes_json,
            error_code,
            occurred_at_ms,
            start_actor_id,
        ) = row?;
        let status = packet_proposal_status_from_str(&status_raw);
        let candidates: Vec<String> =
            serde_json::from_str(&candidate_packet_kinds_json).unwrap_or_default();
        if status == PacketProposalRunStatus::Running {
            newer_running_seen.insert(item_id.clone());
            let entry = pending.entry(item_id).or_default();
            for kind in candidates {
                if !entry.contains(&kind) {
                    entry.push(kind);
                }
            }
            continue;
        }
        if newer_running_seen.contains(&item_id) {
            continue;
        }
        if !terminal_seen.insert(item_id.clone()) {
            continue;
        }
        let outcomes: Vec<PacketProposalKindOutcome> =
            serde_json::from_str(&outcomes_json).unwrap_or_default();
        if status == PacketProposalRunStatus::Completed
            && (outcomes
                .iter()
                .any(|outcome| outcome.status == PacketProposalKindOutcomeStatus::Drafted)
                || is_automatic_packet_proposal_actor(start_actor_id.as_deref()))
        {
            continue;
        }
        let diagnostic_id = format!("packet_proposal:{run_id}");
        failures
            .entry(item_id)
            .or_default()
            .push(WorkItemFailureNotification {
                notification_id: format!("smart_draft:{run_id}:{status_raw}"),
                source: "smart_draft".to_string(),
                packet_kind: None,
                title: if status == PacketProposalRunStatus::Failed {
                    "Smart draft failed".to_string()
                } else {
                    "Smart draft produced no drafts".to_string()
                },
                message: packet_proposal_failure_message(status, &outcomes, error_code.as_deref()),
                next_action: Some(
                    "Open Smart draft again after adjusting the item or source.".to_string(),
                ),
                diagnostic_href: debug_enabled.then(|| format!("#debug/{diagnostic_id}")),
                diagnostic_id: debug_enabled.then_some(diagnostic_id),
                error_code,
                occurred_at_ms,
            });
    }
    Ok((pending, failures))
}

fn is_automatic_packet_proposal_actor(actor_id: Option<&str>) -> bool {
    matches!(actor_id, Some("email_ai_triage" | "smart_draft"))
}

fn packet_proposal_status_from_str(raw: &str) -> PacketProposalRunStatus {
    match raw {
        "completed" => PacketProposalRunStatus::Completed,
        "failed" => PacketProposalRunStatus::Failed,
        _ => PacketProposalRunStatus::Running,
    }
}

fn packet_proposal_failure_message(
    status: PacketProposalRunStatus,
    outcomes: &[PacketProposalKindOutcome],
    error_code: Option<&str>,
) -> String {
    if status == PacketProposalRunStatus::Failed {
        return error_code
            .map(|code| format!("Smart draft stopped before drafts were ready: {code}."))
            .unwrap_or_else(|| "Smart draft stopped before drafts were ready.".to_string());
    }
    let details: Vec<String> = outcomes
        .iter()
        .filter(|outcome| outcome.status != PacketProposalKindOutcomeStatus::Drafted)
        .map(|outcome| {
            let reason = outcome
                .message
                .as_deref()
                .filter(|message| !message.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    outcome
                        .reason_code
                        .map(|reason| reason_code_label(reason).to_string())
                })
                .unwrap_or_else(|| "not available".to_string());
            format!("{}: {}", packet_label(&outcome.packet_kind), reason)
        })
        .take(3)
        .collect();
    if details.is_empty() {
        "Smart draft finished, but no reviewable draft passed the gates.".to_string()
    } else {
        format!(
            "Smart draft finished, but no reviewable draft passed the gates. {}",
            details.join("; ")
        )
    }
}

fn reason_code_label(
    reason: bos_contracts::packet_proposals::PacketProposalReasonCode,
) -> &'static str {
    match reason {
        bos_contracts::packet_proposals::PacketProposalReasonCode::ActiveDraftExists => {
            "active draft exists"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::CategoryInvalid => {
            "category invalid"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::ContextUnavailable => {
            "context unavailable"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::GateRejected => "gate rejected",
        bos_contracts::packet_proposals::PacketProposalReasonCode::KindNotEnabled => {
            "kind not enabled"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::KindNotRequested => {
            "kind not requested"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::LowConfidence => {
            "low confidence"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::ModelOutputInvalid => {
            "model output invalid"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::SourceMissing => {
            "source missing"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::SourceUnsupported => {
            "source unsupported"
        }
        bos_contracts::packet_proposals::PacketProposalReasonCode::StageFailed => "stage failed",
    }
}

fn packet_title(kind: &str) -> String {
    format!("{} could not be generated.", packet_label(kind))
}

fn packet_label(kind: &str) -> String {
    let label = super::packet_kind_catalog()
        .iter()
        .find(|record| record.kind_id == kind)
        .map(|record| record.title.as_str())
        .unwrap_or(kind);
    label.to_string()
}

/// Error surface for the source-peek route.
pub enum ItemSourceError {
    ItemNotFound,
    SourceMissing,
    SourceUnsupported,
    Store(StoreError),
}

/// The full source behind a work item, through the same resolution the
/// produce stage uses — the operator peeks at exactly what the LLM would see.
pub fn item_source(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    scope: &OperatorScope,
) -> Result<WorkItemSourceResponse, ItemSourceError> {
    let item = store::get_item_scoped(conn, client_id, item_id, scope)
        .map_err(ItemSourceError::Store)?
        .ok_or(ItemSourceError::ItemNotFound)?
        .item;
    match crate::produce::resolve_source(conn, client_id, &item) {
        Ok(Some(message)) => {
            let (source_body, source_body_format) =
                source_body_for_display(conn, client_id, &message)?;
            Ok(WorkItemSourceResponse {
                source_kind: item.source_kind,
                message,
                source_body,
                source_body_format,
            })
        }
        Ok(None) => Err(ItemSourceError::SourceMissing),
        Err(crate::produce::SourceError::Unsupported) => Err(ItemSourceError::SourceUnsupported),
        Err(crate::produce::SourceError::Store(err)) => Err(ItemSourceError::Store(err)),
    }
}

fn source_body_for_display(
    conn: &Connection,
    client_id: &str,
    message: &InboundMessageRecord,
) -> Result<(String, WorkItemSourceBodyFormat), ItemSourceError> {
    if let Some(html) =
        crate::slices::email_triage::store::inbound_body_html(conn, client_id, &message.source_key)
            .map_err(ItemSourceError::Store)?
    {
        return Ok((html, WorkItemSourceBodyFormat::Html));
    }

    let body = if message.body_full.trim().is_empty() {
        message.body_excerpt.clone()
    } else {
        message.body_full.clone()
    };
    let format = if looks_like_html(&body) {
        WorkItemSourceBodyFormat::Html
    } else {
        WorkItemSourceBodyFormat::PlainText
    };
    Ok((body, format))
}

fn looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start();
    let lower_head: String = trimmed
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    lower_head.starts_with("<!doctype html")
        || lower_head.starts_with("<html")
        || lower_head.starts_with("<body")
        || lower_head.contains("<div")
        || lower_head.contains("<table")
        || lower_head.contains("<p")
        || lower_head.contains("<br")
        || lower_head.contains("<span")
}

/// Emit a work item for a classified message if policy applies and none
/// exists yet. Returns true when an item was created.
///
/// Emit a work item from generic category policy. Client-specific email
/// parsers may have attached represented-party or attention metadata, but this
/// path only consumes the neutral persisted contract.
pub fn emit_for_inbound_message(
    conn: &mut Connection,
    client_id: &str,
    message: &InboundMessageRecord,
    now_ms: u64,
) -> Result<bool, StoreError> {
    emit_for_inbound_message_with_overlay(
        conn,
        client_id,
        message,
        &crate::overlay::WorkQueueOverlay::default(),
        now_ms,
    )
}

pub fn emit_for_inbound_message_with_overlay(
    conn: &mut Connection,
    client_id: &str,
    message: &InboundMessageRecord,
    overlay: &crate::overlay::WorkQueueOverlay,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let existing = store::get_item_for_source(
        conn,
        client_id,
        super::SOURCE_KIND_EMAIL,
        &message.source_key,
    )?;
    let existing = match existing {
        Some(existing) => Some(existing),
        None if message.source_key != message.message_id => store::get_item_for_source_user(
            conn,
            client_id,
            super::SOURCE_KIND_EMAIL,
            &message.message_id,
            message.source_user_id.as_deref(),
        )?,
        None => None,
    };
    if let Some(existing) = existing {
        let visible = visible_users_for_message(message, overlay);
        let idempotency_key = format!(
            "visibility:{}:{}:{}",
            existing.item.source_kind,
            existing.item.source_ref,
            stable_visibility_key(&visible)
        );
        store::ensure_visibility_user_ids(
            conn,
            client_id,
            &existing.item.item_id,
            &visible,
            "work_queue_visibility_sync",
            &idempotency_key,
            now_ms,
        )?;
        return Ok(false);
    }
    let policy = match store::policy_for_category(conn, client_id, &message.resolved_category)? {
        Some(policy) => policy,
        None => return Ok(false),
    };
    if !policy.create_work_item {
        return Ok(false);
    }
    let (title_hint, summary_hint) = crate::slices::email_triage::store::enrichment_display_hints(
        conn,
        client_id,
        &message.source_key,
    )?;
    let attention = crate::slices::email_triage::store::strongest_attention_signal(
        conn,
        client_id,
        &message.source_key,
    )?;
    let title = title_hint.unwrap_or_else(|| item_title(message));
    let summary = summary_hint.unwrap_or_else(|| item_summary(message));
    let packet_kinds = gate_packet_kinds_for_attention(policy.packet_kinds, attention.as_ref());
    if packet_kinds.is_empty() {
        return Ok(false);
    }
    let item = WorkItem {
        item_id: format!("wi_email_{}", message.source_key),
        source_kind: super::SOURCE_KIND_EMAIL.to_string(),
        source_ref: message.source_key.clone(),
        category_id: message.resolved_category.clone(),
        title,
        summary,
        packet_kinds,
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: message.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: visible_users_for_message(message, overlay),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    store::insert_item(conn, client_id, &item)?;
    Ok(true)
}

pub fn visible_users_for_message(
    message: &InboundMessageRecord,
    overlay: &crate::overlay::WorkQueueOverlay,
) -> Vec<String> {
    let to_addresses = crate::slices::email_triage::subjects::normalized_email_addresses(
        message.to_addr.as_deref(),
    );
    let mut visible = Vec::new();
    for shared in overlay.shared_inboxes.values() {
        let matches = shared
            .match_to
            .iter()
            .flat_map(|raw| {
                crate::slices::email_triage::subjects::normalized_email_addresses(Some(raw))
            })
            .any(|candidate| to_addresses.iter().any(|addr| addr == &candidate));
        if matches {
            for user_id in &shared.visible_to_user_ids {
                let user_id = user_id.trim();
                if !user_id.is_empty() && !visible.iter().any(|existing| existing == user_id) {
                    visible.push(user_id.to_string());
                }
            }
        }
    }
    if visible.is_empty() {
        if let Some(source_user_id) = message
            .source_user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            visible.push(source_user_id.to_string());
        }
    }
    visible
}

fn stable_visibility_key(user_ids: &[String]) -> String {
    let mut normalized: Vec<String> = user_ids
        .iter()
        .map(|user_id| user_id.trim())
        .filter(|user_id| !user_id.is_empty())
        .map(str::to_string)
        .collect();
    normalized.sort_by_key(|value| value.to_ascii_lowercase());
    normalized.dedup();
    normalized.join(",")
}

fn gate_packet_kinds_for_attention(
    kinds: Vec<String>,
    attention: Option<&bos_contracts::email_identity::AttentionSignal>,
) -> Vec<String> {
    use bos_contracts::email_identity::AttentionLevel;
    let Some(attention) = attention else {
        return kinds;
    };
    match attention.level {
        AttentionLevel::Lower => kinds
            .into_iter()
            .filter(|kind| !LOWER_ATTENTION_DEFER_KINDS.contains(&kind.as_str()))
            .collect(),
        AttentionLevel::Normal => kinds,
        AttentionLevel::Higher => kinds,
    }
}

/// Spec for an unconditional emission (operator-initiated sources).
pub struct UnconditionalEmit<'a> {
    pub source_kind: &'a str,
    pub source_ref: &'a str,
    pub category_id: &'a str,
    pub title: &'a str,
    pub summary: &'a str,
    /// Suggested kinds when the category has no (non-empty) policy row.
    pub default_kinds: Vec<String>,
    /// When true, a stored category policy can replace `default_kinds`.
    /// Operator-note actions set this false because the submitted checkboxes
    /// are the operator's explicit selection for that item.
    pub allow_policy_kinds: bool,
    /// Operator user whose input sourced the item (None = shared identity).
    pub source_user_id: Option<String>,
    /// Status the item lands in. Most sources open (operator decides);
    /// operator-note actions emit ACCEPTED — selecting an action accepts the
    /// self-authored note implicitly.
    pub status: WorkItemStatus,
}

/// Emit a work item unconditionally (operator-initiated sources: the
/// operator created the input BECAUSE they want work from it, so the
/// quiet-by-default policy gate does not apply). The category's policy, when
/// present, still supplies the suggested packet kinds. One-item-per-source
/// still holds.
pub fn emit_unconditional(
    conn: &mut Connection,
    client_id: &str,
    spec: UnconditionalEmit<'_>,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if store::item_exists_for_source(conn, client_id, spec.source_kind, spec.source_ref)? {
        return Ok(false);
    }
    let packet_kinds = if spec.allow_policy_kinds {
        store::policy_for_category(conn, client_id, spec.category_id)?
            .map(|policy| policy.packet_kinds)
            .filter(|kinds| !kinds.is_empty())
            .unwrap_or(spec.default_kinds)
    } else {
        spec.default_kinds
    };
    let item = WorkItem {
        item_id: format!("wi_{}_{}", spec.source_kind, spec.source_ref),
        source_kind: spec.source_kind.to_string(),
        source_ref: spec.source_ref.to_string(),
        category_id: spec.category_id.to_string(),
        title: spec.title.to_string(),
        summary: spec.summary.chars().take(SUMMARY_MAX_CHARS).collect(),
        packet_kinds,
        status: spec.status,
        accept_actor: (spec.status == WorkItemStatus::Accepted)
            .then_some(WorkItemAcceptActor::Operator),
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: spec.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: spec
            .source_user_id
            .iter()
            .filter(|user_id| !user_id.trim().is_empty())
            .cloned()
            .collect(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    store::insert_item(conn, client_id, &item)?;
    Ok(true)
}

/// Operator-requested follow-up from the inbox. If the email already has a
/// work item, append `follow_up_task` through the normal revisioned item
/// mutation. Otherwise create one open item for that source email.
pub fn add_manual_follow_up_for_email(
    conn: &mut Connection,
    ctx: store::ItemActionContext<'_>,
    message: &InboundMessageRecord,
    overlay: &crate::overlay::WorkQueueOverlay,
) -> Result<crate::store_core::MutationOutcome, StoreError> {
    const KIND: &str = "follow_up_task";
    if let Some(existing) = store::get_item_for_source(
        conn,
        ctx.client_id,
        super::SOURCE_KIND_EMAIL,
        &message.source_key,
    )? {
        let mut kinds = existing.item.packet_kinds;
        if !kinds.iter().any(|kind| kind == KIND) {
            kinds.push(KIND.to_string());
        }
        return store::update_packet_kinds(conn, ctx, &existing.item.item_id, &kinds);
    }

    let item = WorkItem {
        item_id: format!("wi_email_{}", message.source_key),
        source_kind: super::SOURCE_KIND_EMAIL.to_string(),
        source_ref: message.source_key.clone(),
        category_id: message.resolved_category.clone(),
        title: item_title(message),
        summary: item_summary(message),
        packet_kinds: vec![KIND.to_string()],
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: false,
        rationale: String::new(),
        produce_guidance: String::new(),
        source_user_id: message.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: visible_users_for_message(message, overlay),
        created_at_ms: ctx.now_ms,
        updated_at_ms: ctx.now_ms,
    };
    store::insert_item_with_actor(
        conn,
        ctx.client_id,
        &item,
        ctx.actor_id,
        bos_contracts::receipt::ActorKindDto::Operator,
        ctx.idempotency_key,
    )
}

/// Emit or extend an AI-suggested work item (tier-2 triage). Bypasses
/// deterministic packet policy — the AI's confidence gate already decided —
/// but preserves the one-item-per-source invariant by appending missing kinds
/// to an existing item.
pub fn emit_ai_suggested_item(
    conn: &mut Connection,
    client_id: &str,
    message: &InboundMessageRecord,
    overlay: &crate::overlay::WorkQueueOverlay,
    packet_kinds: Vec<String>,
    rationale: &str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if let Some(existing) = store::get_item_for_source(
        conn,
        client_id,
        super::SOURCE_KIND_EMAIL,
        &message.source_key,
    )? {
        return store::append_ai_packet_kinds(
            conn,
            client_id,
            &existing.item.item_id,
            &packet_kinds,
            rationale,
            now_ms,
        );
    }
    let item = WorkItem {
        item_id: format!("wi_email_{}", message.source_key),
        source_kind: super::SOURCE_KIND_EMAIL.to_string(),
        source_ref: message.source_key.clone(),
        category_id: message.resolved_category.clone(),
        title: item_title(message),
        summary: item_summary(message),
        packet_kinds,
        status: WorkItemStatus::Open,
        accept_actor: None,
        ai_suggested: true,
        rationale: rationale.to_string(),
        produce_guidance: String::new(),
        source_user_id: message.source_user_id.clone(),
        assignee_user_id: None,
        visible_to_user_ids: visible_users_for_message(message, overlay),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    store::insert_item(conn, client_id, &item)?;
    Ok(true)
}

fn item_title(message: &InboundMessageRecord) -> String {
    match message.subject.as_deref().map(str::trim) {
        Some(subject) if !subject.is_empty() => subject.to_string(),
        _ => format!(
            "Email from {}",
            message.from_addr.as_deref().unwrap_or("unknown sender")
        ),
    }
}

fn item_summary(message: &InboundMessageRecord) -> String {
    let mut summary = String::new();
    if let Some(from) = message.from_addr.as_deref() {
        summary.push_str("From ");
        summary.push_str(from);
        summary.push_str(" — ");
    }
    summary.extend(message.body_excerpt.chars().take(SUMMARY_MAX_CHARS));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn category() -> bos_contracts::email_triage::CategoryRecord {
        bos_contracts::email_triage::CategoryRecord {
            category_id: "billing".to_string(),
            display_name: "Billing".to_string(),
            description: "Invoices and payments".to_string(),
            color: "#10b981".to_string(),
            sort: 40,
            is_system: false,
            default_agent_dir: "/home/example/projects/billing-client".to_string(),
            default_agent_context: "Use the billing-client runbook.".to_string(),
        }
    }

    #[test]
    fn launch_defaults_apply_category_context_and_workdir_server_side() {
        let request = LaunchAgentRequest {
            context: "Focus overdue invoices.".to_string(),
            work_dir: None,
            attachment_ids: Vec::new(),
            idempotency_key: "launch-1".to_string(),
        };

        let resolved = resolve_agent_launch_defaults(Some(&category()), &request);

        assert_eq!(resolved.work_dir, "/home/example/projects/billing-client");
        assert!(resolved.context.contains("Category default context"));
        assert!(resolved.context.contains("Use the billing-client runbook."));
        assert!(resolved.context.contains("Launch override context"));
        assert!(resolved.context.contains("Focus overdue invoices."));
    }

    #[test]
    fn launch_defaults_allow_request_workdir_override() {
        let request = LaunchAgentRequest {
            context: String::new(),
            work_dir: Some("/tmp/one-off".to_string()),
            attachment_ids: Vec::new(),
            idempotency_key: "launch-1".to_string(),
        };

        let resolved = resolve_agent_launch_defaults(Some(&category()), &request);

        assert_eq!(resolved.work_dir, "/tmp/one-off");
        assert_eq!(resolved.context, "Use the billing-client runbook.");
    }
}
