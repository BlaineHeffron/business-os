//! Gmail ingestion pump: poll → classify (deterministic rules) → persist
//! through the receipt spine. Off unless BOS_GMAIL_INGEST_ENABLED; runs on a
//! dedicated thread because the provider client is blocking.
//!
//! `ingest_messages` is the testable core; the thread loop around it only
//! fetches and sleeps.

use std::time::Duration;

use bos_contracts::email_triage::{EmailAttachmentRecord, FALLBACK_CATEGORY_ID};
use bos_contracts::packet_proposals::PacketProposalKindOutcomeStatus;
use bos_contracts::work_queue::WorkItemStatus;
use bos_integrations::gmail_inbox_read::{
    GmailAttachmentMeta, GmailFullMessage, GmailFullReadRequest, LiveGmailInboxReadClient,
};
use bos_integrations::{GoogleOAuthConfig, ReqwestGmailHttpClient};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::facts::FactBag;
use super::service::{
    crm_fact_overrides_from_cache, display_body_for_excerpt, raw_body_for_rules,
    resolve_rule_with_fact_bag, rules_need_crm_facts, MessageView,
};
use super::store::{self, safe_headers, InboundMessageRecord};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

pub struct IngestPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentBackfillSummary {
    pub enabled: bool,
    pub examined: usize,
    pub enriched: usize,
    pub represented_refreshed: usize,
}

struct EnrichmentBackfillConfig {
    enabled: bool,
    batch: usize,
}

struct EvidenceCleanupConfig {
    enabled: bool,
    interval: Duration,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<IngestPumpConfig, StoreError> {
    Ok(IngestPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_GMAIL_INGEST_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_GMAIL_INGEST_INTERVAL_SECS,
                120,
            )?
            .max(15) as u64,
        ),
        query: env_registry::string(&env_registry::BOS_GMAIL_INGEST_QUERY)
            .unwrap_or_else(|| "in:inbox newer_than:14d".to_string()),
    })
}

/// Spawn the pump thread. No-op (with a log line) only when disabled — when
/// enabled but not yet connected, the thread keeps polling for a credential so
/// the operator's "connect Gmail" click takes effect on the next cycle without
/// a restart. Credentials resolve per cycle: EVERY connected user's stored
/// credential is polled (mail + work items tagged with the source user); the
/// env refresh token only runs when nothing is stored (single-account mode).
pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("gmail ingest pump not started (email_triage disabled by client overlay)");
        return;
    }
    spawn_evidence_cleanup(state.clone());
    spawn_enrichment_backfill(state.clone());
    std::thread::Builder::new()
        .name("gmail-ingest-pump".to_string())
        .spawn(move || {
            tracing::info!("gmail ingest pump started");
            let mut warned_unconnected = false;
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "gmail ingest config read failed");
                            IngestPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(120),
                                query: "in:inbox newer_than:14d".to_string(),
                            }
                        }
                    }
                };
                if !config.enabled {
                    std::thread::sleep(config.interval);
                    continue;
                }
                let accounts = {
                    let persistence = state.persistence.lock();
                    crate::slices::google_connector::service::list_gmail_accounts(
                        persistence.connection_ref(),
                        &state.client_id,
                    )
                };
                match accounts {
                    Ok(accounts) if accounts.is_empty() => {
                        if !warned_unconnected {
                            tracing::warn!(
                                "gmail ingest pump waiting: no credential (connect via \
                                 /api/connectors/google/connect or set BOS_GMAIL_OAUTH_* env)"
                            );
                            warned_unconnected = true;
                        }
                    }
                    Ok(accounts) => {
                        warned_unconnected = false;
                        for account in accounts {
                            let source_user = account.user_id.as_deref();
                            match poll_once(&state, &account.oauth, source_user, &config.query) {
                                Ok(summary) if summary.ingested > 0 => {
                                    tracing::info!(
                                        ingested = summary.ingested,
                                        skipped_existing = summary.skipped_existing,
                                        source_user = source_user.unwrap_or("(env)"),
                                        "gmail ingest cycle complete"
                                    );
                                }
                                Ok(_) => {}
                                Err(err) => tracing::warn!(
                                    error = %err,
                                    source_user = source_user.unwrap_or("(env)"),
                                    "gmail ingest cycle failed"
                                ),
                            }
                        }
                    }
                    Err(err) => tracing::warn!(error = %err, "credential resolution failed"),
                }
                run_ai_triage_pass(&state);
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn gmail-ingest-pump thread");
}

fn enrichment_backfill_config(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<EnrichmentBackfillConfig, StoreError> {
    Ok(EnrichmentBackfillConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED,
        )?,
        batch: crate::slices::admin_settings::service::usize_or(
            conn,
            client_id,
            &env_registry::BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH,
            200,
        )?
        .clamp(1, 10_000),
    })
}

fn spawn_enrichment_backfill(state: AppState) {
    std::thread::Builder::new()
        .name("email-enrichment-backfill".to_string())
        .spawn(move || {
            let interval = Duration::from_secs(300);
            let mut complete = false;
            let mut offset = 0usize;
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match enrichment_backfill_config(persistence.connection_ref(), &state.client_id)
                    {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "email enrichment backfill config read failed"
                            );
                            EnrichmentBackfillConfig {
                                enabled: false,
                                batch: 200,
                            }
                        }
                    }
                };
                if !config.enabled {
                    complete = false;
                    offset = 0;
                    std::thread::sleep(interval);
                    continue;
                }
                if !complete {
                    match run_enrichment_backfill_page(&state, config.batch, offset) {
                        Ok(summary) => {
                            if summary.examined > 0
                                || summary.enriched > 0
                                || summary.represented_refreshed > 0
                            {
                                tracing::info!(
                                    examined = summary.examined,
                                    enriched = summary.enriched,
                                    represented_refreshed = summary.represented_refreshed,
                                    "email enrichment backfill cycle complete"
                                );
                            }
                            offset += summary.examined;
                            complete = summary.examined < config.batch;
                        }
                        Err(err) => tracing::warn!(
                            error = %err,
                            "email enrichment backfill cycle failed"
                        ),
                    }
                }
                std::thread::sleep(interval);
            }
        })
        .expect("spawn email-enrichment-backfill thread");
}

pub fn run_enrichment_backfill_cycle(
    state: &AppState,
) -> Result<EnrichmentBackfillSummary, String> {
    let config = {
        let persistence = state.persistence.lock();
        enrichment_backfill_config(persistence.connection_ref(), &state.client_id)
            .map_err(|err| err.to_string())?
    };
    if !config.enabled {
        return Ok(EnrichmentBackfillSummary::default());
    }
    run_enrichment_backfill_once(state, config.batch)
}

pub fn run_enrichment_backfill_once(
    state: &AppState,
    batch: usize,
) -> Result<EnrichmentBackfillSummary, String> {
    run_enrichment_backfill_page(state, batch, 0)
}

fn run_enrichment_backfill_page(
    state: &AppState,
    batch: usize,
    offset: usize,
) -> Result<EnrichmentBackfillSummary, String> {
    let parser_ids = state.email_triage_overlay.inbound_parser_ids.clone();
    if parser_ids.is_empty() || batch == 0 {
        return Ok(EnrichmentBackfillSummary {
            enabled: true,
            ..Default::default()
        });
    }
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let records = store::list_inbound_messages_for_reprocess(conn, &state.client_id, batch, offset)
        .map_err(|err| err.to_string())?;
    let mut summary = EnrichmentBackfillSummary {
        enabled: true,
        examined: records.len(),
        enriched: 0,
        represented_refreshed: 0,
    };
    let now_ms = now_ms();
    for record in records {
        let view = stored_message_view(&record);
        summary.enriched += run_inbound_parsers(
            conn,
            &state.client_id,
            &parser_ids,
            &record.source_key,
            &view,
            now_ms,
        )
        .map_err(|err| err.to_string())?;
        if store::refresh_represented_identity(conn, &state.client_id, &record.source_key, now_ms)
            .map_err(|err| err.to_string())?
            .is_some()
        {
            summary.represented_refreshed += 1;
        }
    }
    Ok(summary)
}

fn spawn_evidence_cleanup(state: AppState) {
    std::thread::Builder::new()
        .name("agent-evidence-cleanup".to_string())
        .spawn(move || loop {
            let config = {
                let persistence = state.persistence.lock();
                let conn = persistence.connection_ref();
                let enabled = crate::slices::admin_settings::service::flag(
                    conn,
                    &state.client_id,
                    &env_registry::BOS_AGENT_EVIDENCE_CLEANUP_ENABLED,
                );
                let interval = crate::slices::admin_settings::service::usize_or(
                    conn,
                    &state.client_id,
                    &env_registry::BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS,
                    3600,
                );
                match (enabled, interval) {
                    (Ok(enabled), Ok(interval)) => EvidenceCleanupConfig {
                        enabled,
                        interval: Duration::from_secs((interval as u64).max(60)),
                    },
                    (Err(err), _) | (_, Err(err)) => {
                        tracing::warn!(error = %err, "agent evidence cleanup config read failed");
                        EvidenceCleanupConfig {
                            enabled: false,
                            interval: Duration::from_secs(3600),
                        }
                    }
                }
            };
            if config.enabled {
                match run_evidence_cleanup_once(&state, 100) {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(deleted, "agent evidence cleanup complete")
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(error = %err, "agent evidence cleanup failed"),
                }
            }
            std::thread::sleep(config.interval);
        })
        .expect("spawn agent-evidence-cleanup thread");
}

pub fn run_evidence_cleanup_once(state: &AppState, limit: usize) -> Result<usize, String> {
    let due = {
        let persistence = state.persistence.lock();
        store::due_agent_evidence_files(
            persistence.connection_ref(),
            &state.client_id,
            now_ms(),
            limit,
        )
        .map_err(|err| err.to_string())?
    };
    let mut deleted = 0;
    for (evidence_id, path) in due {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(path = %path, error = %err, "agent evidence file delete failed");
                continue;
            }
        }
        let mut persistence = state.persistence.lock();
        store::mark_agent_evidence_deleted(
            persistence.connection(),
            &state.client_id,
            &evidence_id,
            &path,
            now_ms(),
        )
        .map_err(|err| err.to_string())?;
        deleted += 1;
    }
    Ok(deleted)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestSummary {
    pub ingested: usize,
    pub skipped_existing: usize,
}

fn poll_once(
    state: &AppState,
    oauth: &GoogleOAuthConfig,
    source_user_id: Option<&str>,
    query: &str,
) -> Result<IngestSummary, String> {
    const GMAIL_INGEST_PAGE_SIZE: u32 = 500;
    const ENV_ACCOUNT_REF: &str = "__env__";

    let client = match LiveGmailInboxReadClient::from_credentials(
        std::sync::Arc::new(ReqwestGmailHttpClient::default()),
        oauth,
    ) {
        Ok(client) => client,
        Err(err) => {
            if err.code() == "google_oauth_invalid_grant" {
                cleanup_revoked_google_credential(state, source_user_id, err.message());
            }
            return Err(format!("oauth/token: {err}"));
        }
    };
    // Resolve label IDS to display names (user labels arrive as "Label_13");
    // a failed lookup degrades to raw ids rather than blocking ingestion.
    let label_names = client.list_label_names().unwrap_or_else(|err| {
        tracing::warn!(error = ?err, "gmail labels.list failed; storing raw label ids");
        std::collections::HashMap::new()
    });
    let account_ref = source_user_id.unwrap_or(ENV_ACCOUNT_REF);
    let query_hash = store::gmail_ingest_query_hash(query);
    let page_token = {
        let persistence = state.persistence.lock();
        store::get_gmail_ingest_cursor(persistence.connection_ref(), &state.client_id, account_ref)
            .map_err(|err| format!("gmail cursor read: {err}"))?
            .filter(|cursor| cursor.query_hash == query_hash)
            .and_then(|cursor| cursor.next_page_token)
    };
    let page = client
        .read_full_messages_page(&GmailFullReadRequest {
            query: query.to_string(),
            max_messages: GMAIL_INGEST_PAGE_SIZE,
            page_token,
        })
        .map_err(|err| format!("gmail read: {err:?}"))?;
    let mut messages = page.messages;
    for message in &mut messages {
        message.label_ids = message
            .label_ids
            .iter()
            .map(|id| label_names.get(id).unwrap_or(id).clone())
            .collect();
    }
    warm_crm_fact_cache_for_ingest(state, &messages)?;
    let mut persistence = state.persistence.lock();
    let summary = ingest_messages_with_overlay(
        persistence.connection(),
        &state.client_id,
        source_user_id,
        &messages,
        &state.email_triage_overlay,
        &state.work_queue_overlay,
        now_ms(),
    )
    .map_err(|err| err.to_string())?;
    // One-time correction for rows ingested before names were resolved;
    // replays quietly once everything is renamed. Scoped to this account's
    // rows — label ids are per Gmail account.
    match store::relabel_inbound_messages(
        persistence.connection(),
        &state.client_id,
        source_user_id,
        &label_names,
        now_ms(),
    ) {
        Ok(0) => {}
        Ok(updated) => tracing::info!(updated, "resolved stored gmail label ids to names"),
        Err(err) => tracing::warn!(error = %err, "label backfill failed"),
    }
    store::put_gmail_ingest_cursor(
        persistence.connection(),
        &state.client_id,
        account_ref,
        &store::GmailIngestCursor {
            query_hash,
            next_page_token: page.next_page_token,
        },
        now_ms(),
    )
    .map_err(|err| format!("gmail cursor advance: {err}"))?;
    Ok(summary)
}

fn warm_crm_fact_cache_for_ingest(
    state: &AppState,
    messages: &[GmailFullMessage],
) -> Result<(), String> {
    let persistence = state.persistence.lock();
    let rules: Vec<_> = store::list_active(persistence.connection_ref(), &state.client_id)
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|stored| stored.rule)
        .collect();
    if !rules_need_crm_facts(&rules) {
        return Ok(());
    }
    let plans: Vec<_> = messages
        .iter()
        .map(|message| {
            let source_key = store::source_key_for(None, &message.message_id);
            let view = message_view(message, source_key, None, safe_headers(&message.headers));
            crm_fact_overrides_from_cache(
                persistence.connection_ref(),
                &state.client_id,
                &view,
                now_ms(),
            )
        })
        .collect();
    drop(persistence);

    let mut lookup = super::service::EnvCrmLiveLookup;
    let mut writes = Vec::new();
    let ttls = super::service::CrmFactTtls::from_env();
    for (_, misses) in plans {
        let mut budget = super::service::crm_provider_budget_per_message();
        let (_, mut message_writes) = super::service::resolve_crm_fact_misses(
            &misses,
            &mut budget,
            &mut lookup,
            now_ms(),
            ttls,
        );
        writes.append(&mut message_writes);
    }
    if writes.is_empty() {
        return Ok(());
    }
    let mut persistence = state.persistence.lock();
    super::service::persist_crm_fact_cache_writes(
        persistence.connection(),
        &state.client_id,
        &writes,
    )
    .map_err(|err| err.to_string())
}

fn message_view(
    message: &GmailFullMessage,
    source_key: String,
    source_user_id: Option<&str>,
    headers: Vec<(String, String)>,
) -> MessageView {
    MessageView {
        message_id: Some(source_key),
        source_user_id: source_user_id.map(str::to_string),
        subject: message.subject.clone(),
        from: message.from.clone(),
        to: message.to.clone(),
        body: Some(message.plain_text_body.clone()),
        labels: message.label_ids.clone(),
        headers,
    }
}

fn stored_message_view(message: &InboundMessageRecord) -> MessageView {
    MessageView {
        message_id: Some(message.source_key.clone()),
        source_user_id: message.source_user_id.clone(),
        subject: message.subject.clone(),
        from: message.from_addr.clone(),
        to: message.to_addr.clone(),
        body: Some(raw_body_for_rules(message)),
        labels: message.labels.clone(),
        headers: message.headers.clone(),
    }
}

fn run_inbound_parsers(
    conn: &mut Connection,
    client_id: &str,
    parser_ids: &[String],
    source_key: &str,
    view: &MessageView,
    now_ms: u64,
) -> Result<usize, StoreError> {
    if parser_ids.is_empty() {
        return Ok(0);
    }
    let parsers = super::service::select_inbound_parsers(parser_ids);
    if parsers.is_empty() {
        return Ok(0);
    }
    let mut input = super::service::parser_input_for_message(view);
    input.source_key = source_key.to_string();
    let mut enriched = 0;
    for parser in parsers {
        let Some(parsed) = parser.parse(&input) else {
            continue;
        };
        store::upsert_inbound_enrichment(
            conn,
            store::InboundEnrichmentWrite {
                client_id,
                source_key,
                parser_id: parser.parser_id(),
                parser_version: parser.parser_version(),
                parsed: &parsed,
                now_ms,
            },
        )?;
        enriched += 1;
    }
    Ok(enriched)
}

fn cleanup_revoked_google_credential(state: &AppState, source_user_id: Option<&str>, reason: &str) {
    let Some(user_id) = source_user_id else {
        tracing::warn!(
            "gmail env credential refresh token is revoked/expired; reconnect or rotate env token"
        );
        return;
    };
    let mut persistence = state.persistence.lock();
    match crate::slices::google_connector::store::mark_credential_revoked(
        persistence.connection(),
        &state.client_id,
        user_id,
        crate::slices::google_connector::SERVICE_GMAIL,
        reason,
        now_ms(),
    ) {
        Ok(_) => tracing::warn!(
            source_user = user_id,
            "gmail credential revoked/expired; removed stored credential so operator can reconnect"
        ),
        Err(err) => tracing::warn!(
            source_user = user_id,
            error = %err,
            "failed to remove revoked gmail credential"
        ),
    }
}

/// Classify and persist fetched messages, tagged with the connected account
/// (operator user) they came from. Already-ingested ids are skipped BEFORE
/// mutation so steady-state polls write no receipts.
pub fn ingest_messages(
    conn: &mut Connection,
    client_id: &str,
    source_user_id: Option<&str>,
    messages: &[GmailFullMessage],
    now_ms: u64,
) -> Result<IngestSummary, StoreError> {
    ingest_messages_with_overlay(
        conn,
        client_id,
        source_user_id,
        messages,
        &crate::overlay::EmailTriageOverlay::default(),
        &crate::overlay::WorkQueueOverlay::default(),
        now_ms,
    )
}

pub fn ingest_messages_with_overlay(
    conn: &mut Connection,
    client_id: &str,
    source_user_id: Option<&str>,
    messages: &[GmailFullMessage],
    email_triage_overlay: &crate::overlay::EmailTriageOverlay,
    work_queue_overlay: &crate::overlay::WorkQueueOverlay,
    now_ms: u64,
) -> Result<IngestSummary, StoreError> {
    let candidates: Vec<(String, String, Option<String>)> = messages
        .iter()
        .map(|m| {
            (
                store::source_key_for(source_user_id, &m.message_id),
                m.message_id.clone(),
                source_user_id.map(str::to_string),
            )
        })
        .collect();
    let existing = store::existing_source_key_matches(conn, client_id, &candidates)?;
    let rules: Vec<_> = store::list_active(conn, client_id)?
        .into_iter()
        .map(|stored| stored.rule)
        .collect();
    let need_crm_facts = rules_need_crm_facts(&rules);
    let parser_ids = &email_triage_overlay.inbound_parser_ids;

    let mut summary = IngestSummary::default();
    for message in messages {
        let source_key = store::source_key_for(source_user_id, &message.message_id);
        let headers = safe_headers(&message.headers);
        let view = message_view(message, source_key.clone(), source_user_id, headers.clone());
        if let Some(stored_source_key) = existing.get(&source_key) {
            run_inbound_parsers(
                conn,
                client_id,
                parser_ids,
                stored_source_key,
                &view,
                now_ms,
            )?;
            store::refresh_represented_identity(conn, client_id, stored_source_key, now_ms)?;
            let attachments = attachment_records(&message.attachments);
            if !attachments.is_empty() {
                let idempotency_key =
                    attachment_update_idempotency_key(stored_source_key, &attachments);
                store::update_inbound_attachments(
                    conn,
                    client_id,
                    stored_source_key,
                    &attachments,
                    &idempotency_key,
                    now_ms,
                )?;
            }
            summary.skipped_existing += 1;
            continue;
        }
        run_inbound_parsers(conn, client_id, parser_ids, &source_key, &view, now_ms)?;
        let crm = if need_crm_facts {
            crm_fact_overrides_from_cache(conn, client_id, &view, now_ms).0
        } else {
            Default::default()
        };
        let mut bag = FactBag::new(
            Some(conn),
            client_id,
            &view,
            Some(&source_key),
            source_user_id,
            crm,
        );
        let matched_rule = resolve_rule_with_fact_bag(&rules, &mut bag);
        let resolved_category = matched_rule
            .map(|rule| rule.pinned_category.clone())
            .unwrap_or_else(|| FALLBACK_CATEGORY_ID.to_string());
        let matched_rule_id = matched_rule.map(|rule| rule.rule_id.clone());
        drop(bag);
        let display_body = display_body_for_excerpt(&message.plain_text_body);
        let record = InboundMessageRecord {
            source_key,
            message_id: message.message_id.clone(),
            thread_id: message.thread_id.clone(),
            internal_date_ms: message.internal_date_epoch_ms,
            from_addr: message.from.clone(),
            to_addr: message.to.clone(),
            subject: message.subject.clone(),
            body_excerpt: display_body,
            body_full: message.plain_text_body.clone(),
            headers,
            labels: message.label_ids.clone(),
            resolved_category,
            matched_rule_id,
            ingested_at_ms: now_ms,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: attachment_records(&message.attachments),
            source_user_id: source_user_id.map(str::to_string),
        };
        store::record_inbound_message_with_body_html(
            conn,
            client_id,
            &record,
            message.html_body.as_deref(),
        )?;
        store::refresh_represented_identity(conn, client_id, &record.source_key, now_ms)?;
        crate::slices::work_queue::service::emit_for_inbound_message_with_overlay(
            conn,
            client_id,
            &record,
            work_queue_overlay,
            now_ms,
        )?;
        summary.ingested += 1;
    }

    // Re-run emission over already-ingested candidates so a policy enabled
    // AFTER ingestion still produces items (existence check keeps it quiet).
    let existing_keys: Vec<String> = existing.into_values().collect();
    for record in store::inbound_by_source_keys(
        conn,
        client_id,
        &existing_keys,
        &crate::http::OperatorScope::All,
    )? {
        crate::slices::work_queue::service::emit_for_inbound_message_with_overlay(
            conn,
            client_id,
            &record,
            work_queue_overlay,
            now_ms,
        )?;
    }
    Ok(summary)
}

fn attachment_records(attachments: &[GmailAttachmentMeta]) -> Vec<EmailAttachmentRecord> {
    attachments
        .iter()
        .map(|attachment| EmailAttachmentRecord {
            attachment_id: attachment.attachment_id.clone(),
            part_id: attachment.part_id.clone(),
            filename: attachment.filename.clone(),
            mime_type: attachment.mime_type.clone(),
            size_bytes: attachment.size_bytes,
            inline: attachment.inline,
            content_id: attachment.content_id.clone(),
        })
        .collect()
}

fn attachment_update_idempotency_key(
    message_id: &str,
    attachments: &[EmailAttachmentRecord],
) -> String {
    let raw = serde_json::to_string(attachments).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(message_id.as_bytes());
    hasher.update([0]);
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hash.push_str(&format!("{byte:02x}"));
    }
    format!("ingest-attachments:{message_id}:{hash}")
}

/// AI work-packet suggestion pass: examine up to BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE
/// messages whose category policy allows AI suggestions and that have not been
/// examined by this pass. The LLM call runs WITHOUT the persistence lock (a
/// harness execution can take a minute); only the batch fetch and the result
/// write hold it.
pub fn run_ai_triage_pass(state: &AppState) {
    // Snapshot batch + catalogs under the lock, then release it for LLM work.
    let (batch, categories, min_confidence) = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        let enabled = match crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &env_registry::BOS_AI_TRIAGE_ENABLED,
        ) {
            Ok(enabled) => enabled,
            Err(err) => {
                tracing::warn!(error = %err, "ai suggestions: config read failed");
                return;
            }
        };
        if !enabled {
            return;
        }
        let min_confidence = match crate::slices::admin_settings::service::value(
            conn,
            &state.client_id,
            &env_registry::BOS_AI_TRIAGE_MIN_CONFIDENCE,
        ) {
            Ok(raw) => raw
                .as_deref()
                .and_then(super::service::AiConfidence::parse)
                .unwrap_or(super::service::AiConfidence::High),
            Err(err) => {
                tracing::warn!(error = %err, "ai suggestions: config read failed");
                return;
            }
        };
        let batch_limit = crate::slices::admin_settings::service::usize_or(
            conn,
            &state.client_id,
            &env_registry::BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE,
            5,
        )
        .unwrap_or(5)
        .clamp(1, 25);
        let categories = match store::list_categories(conn, &state.client_id, now_ms()) {
            Ok(categories) => categories,
            Err(err) => {
                tracing::warn!(error = %err, "ai suggestions: category load failed");
                return;
            }
        };
        let batch = match store::list_unexamined_ai_suggestible(conn, &state.client_id, batch_limit)
        {
            Ok(batch) => batch,
            Err(err) => {
                tracing::warn!(error = %err, "ai suggestions: batch load failed");
                return;
            }
        };
        (batch, categories, min_confidence)
    };
    if batch.is_empty() {
        return;
    }
    let enabled_kinds = super::service::ai_suggestible_kinds_for_enabled(
        crate::slices::work_queue::packet_kind_catalog(),
        |slice_id| state.slice_enabled(slice_id),
    );
    tracing::info!(
        count = batch.len(),
        "ai work-packet suggestion pass examining messages"
    );

    for message in batch {
        let policy = {
            let persistence = state.persistence.lock();
            match crate::slices::work_queue::store::policy_for_category(
                persistence.connection_ref(),
                &state.client_id,
                &message.resolved_category,
            ) {
                Ok(policy) => policy,
                Err(err) => {
                    tracing::warn!(error = %err, "ai suggestions: policy load failed");
                    None
                }
            }
        };
        let kinds =
            super::service::ai_suggestible_kinds_for_policy(&enabled_kinds, policy.as_ref());
        if kinds.is_empty() {
            let mut persistence = state.persistence.lock();
            if let Err(err) = store::set_ai_triage_result(
                persistence.connection(),
                &state.client_id,
                &message.source_key,
                "no_suggestion",
                Some("No AI-suggestible packet kinds are enabled for this category."),
                Some(
                    serde_json::json!({
                        "suggested_packet_kinds": [],
                        "suggested_category": null,
                        "confidence": "low",
                        "rationale": "No AI-suggestible packet kinds are enabled for this category.",
                        "actionable": false,
                        "model": null,
                    })
                    .to_string(),
                ),
                now_ms(),
            ) {
                tracing::warn!(error = %err, "ai suggestions: result write failed");
            }
            continue;
        }
        let packet_proposals_enabled = {
            let persistence = state.persistence.lock();
            match crate::slices::admin_settings::service::flag(
                persistence.connection_ref(),
                &state.client_id,
                &env_registry::BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
            ) {
                Ok(enabled) => enabled,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "ai suggestions: packet proposal config read failed"
                    );
                    false
                }
            }
        };
        if packet_proposals_enabled {
            run_packet_proposal_ai_triage(state, &message, min_confidence);
            continue;
        }
        let allowed_policy_kinds: std::collections::HashSet<String> =
            kinds.iter().map(|kind| kind.kind_id.clone()).collect();
        let request = super::service::build_ai_triage_request(
            &state.client_id,
            &message,
            &categories,
            &kinds,
        );
        let outcome = crate::slices::ai_usage::service::execute_recorded(
            state.persistence.clone(),
            &state.client_id,
            super::service::AI_TRIAGE_PURPOSE,
            &request,
        );
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        match outcome {
            Ok(envelope) => {
                match super::service::parse_ai_triage_response(&envelope.response_json, &categories)
                {
                    Ok(suggestion) => {
                        let mut suggestion = suggestion;
                        super::service::retain_enabled_ai_suggestions(
                            &mut suggestion.suggested_packet_kinds,
                            |slice_id| state.slice_enabled(slice_id),
                        );
                        suggestion
                            .suggested_packet_kinds
                            .retain(|kind| allowed_policy_kinds.contains(kind));
                        let actionable = !suggestion.suggested_packet_kinds.is_empty()
                            && suggestion.confidence >= min_confidence;
                        let status = if actionable {
                            "suggested"
                        } else {
                            "no_suggestion"
                        };
                        let payload = serde_json::json!({
                            "suggested_packet_kinds": suggestion.suggested_packet_kinds,
                            "suggested_category": suggestion.suggested_category,
                            "confidence": format!("{:?}", suggestion.confidence).to_lowercase(),
                            "rationale": suggestion.rationale,
                            "actionable": actionable,
                            "model": envelope.model,
                        });
                        if let Err(err) = store::set_ai_triage_result(
                            conn,
                            &state.client_id,
                            &message.source_key,
                            status,
                            Some(&suggestion.rationale),
                            Some(payload.to_string()),
                            now_ms(),
                        ) {
                            tracing::warn!(error = %err, "ai suggestions: result write failed");
                            continue;
                        }
                        if actionable {
                            match crate::slices::work_queue::service::emit_ai_suggested_item(
                                conn,
                                &state.client_id,
                                &message,
                                &state.work_queue_overlay,
                                suggestion.suggested_packet_kinds.clone(),
                                &suggestion.rationale,
                                now_ms(),
                            ) {
                                Ok(true) => tracing::info!(
                                    message_id = %message.message_id,
                                    kinds = ?suggestion.suggested_packet_kinds,
                                    "ai suggestions created work item"
                                ),
                                Ok(false) => {}
                                Err(err) => {
                                    tracing::warn!(error = %err, "ai suggestions: item emit failed")
                                }
                            }
                        }
                    }
                    Err(parse_err) => {
                        let _ = store::set_ai_triage_result(
                            conn,
                            &state.client_id,
                            &message.source_key,
                            "error",
                            Some(&format!("unparseable response: {parse_err}")),
                            None,
                            now_ms(),
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    message_id = %message.message_id,
                    error = ?err,
                    "ai suggestions: llm execution failed"
                );
                let _ = store::set_ai_triage_result(
                    conn,
                    &state.client_id,
                    &message.source_key,
                    "error",
                    Some("llm execution failed"),
                    None,
                    now_ms(),
                );
            }
        }
    }
}

fn run_packet_proposal_ai_triage(
    state: &AppState,
    message: &InboundMessageRecord,
    min_confidence: super::service::AiConfidence,
) {
    let expected_revision = {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        match crate::slices::work_queue::store::get_item_for_source(
            conn,
            &state.client_id,
            crate::slices::work_queue::SOURCE_KIND_EMAIL,
            &message.source_key,
        ) {
            Ok(item) => item
                .filter(|entry| entry.item.status != WorkItemStatus::Accepted)
                .map(|entry| entry.revision),
            Err(err) => {
                tracing::warn!(
                    message_id = %message.message_id,
                    error = ?err,
                    "ai suggestions: failed to load work item revision"
                );
                let _ = store::set_ai_triage_result(
                    conn,
                    &state.client_id,
                    &message.source_key,
                    "error",
                    Some("work item revision unavailable"),
                    None,
                    now_ms(),
                );
                return;
            }
        }
    };
    let response = crate::slices::packet_proposals::service::run_smart_draft(
        state.clone(),
        crate::slices::packet_proposals::service::SmartDraftInput {
            source_kind: crate::slices::work_queue::SOURCE_KIND_EMAIL.to_string(),
            source_ref: message.source_key.clone(),
            idempotency_key: format!("ai_triage_packet_proposal:{}", message.source_key),
            expected_revision,
            min_confidence: Some(min_confidence),
            candidate_mode:
                crate::slices::packet_proposals::service::SmartDraftCandidateMode::Policy,
            actor_id: "email_ai_triage".to_string(),
            scope: crate::http::OperatorScope::All,
        },
    );

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    match response {
        Ok(response) => {
            let suggested_packet_kinds = response
                .run
                .outcomes
                .iter()
                .filter(|outcome| outcome.status == PacketProposalKindOutcomeStatus::Drafted)
                .map(|outcome| outcome.packet_kind.clone())
                .collect::<Vec<_>>();
            let confidence = response
                .run
                .confidence
                .as_deref()
                .unwrap_or("low")
                .to_string();
            let actionable = !suggested_packet_kinds.is_empty();
            let suggested_category = response
                .item
                .as_ref()
                .map(|entry| entry.item.category_id.clone())
                .filter(|category| category != &message.resolved_category);
            let rationale = if suggested_packet_kinds.is_empty() {
                "Smart draft found no draftable outputs.".to_string()
            } else if suggested_packet_kinds.len() == 1 {
                "Smart draft staged 1 draft.".to_string()
            } else {
                format!(
                    "Smart draft staged {} drafts.",
                    suggested_packet_kinds.len()
                )
            };
            let payload = serde_json::json!({
                "suggested_packet_kinds": suggested_packet_kinds,
                "suggested_category": suggested_category,
                "confidence": confidence,
                "rationale": rationale,
                "actionable": actionable,
                "model": response.run.model,
                "packet_proposal_run_id": response.run.run_id,
                "packet_proposal_status": response.run.status,
                "packet_proposal_error_code": response.run.error_code,
                "proposal_outcomes": response.run.outcomes,
            });
            if let Err(err) = store::set_ai_triage_result(
                conn,
                &state.client_id,
                &message.source_key,
                if actionable {
                    "suggested"
                } else {
                    "no_suggestion"
                },
                Some(&rationale),
                Some(payload.to_string()),
                now_ms(),
            ) {
                tracing::warn!(error = %err, "ai suggestions: packet proposal result write failed");
            }
        }
        Err(err) => {
            tracing::warn!(
                message_id = %message.message_id,
                error = ?err,
                "ai suggestions: packet proposal execution failed"
            );
            let _ = store::set_ai_triage_result(
                conn,
                &state.client_id,
                &message.source_key,
                "error",
                Some("packet proposal execution failed"),
                None,
                now_ms(),
            );
        }
    }
}
