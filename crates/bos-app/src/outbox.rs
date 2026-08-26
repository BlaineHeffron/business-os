//! Outbox spine: the ONE path for external (provider) effects.
//!
//! Enqueue happens INSIDE a store_core mutation's domain-write closure, so the
//! job commits atomically with the domain state that authorized it. A slice
//! worker claims due jobs post-commit and delivers them; every attempt outcome
//! is recorded back through [`crate::store_core::mutate`] (entity_kind =
//! `outbox_job`), so delivery history is receipted like any other mutation.
//!
//! Single-process sqlite: "leases" are a `leased_until_ms` column — enough to
//! keep a slow delivery from being double-claimed by overlapping cycles.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use bos_contracts::calendar_drafts::OutboxJobSummary;
use bos_contracts::outbox::OutboxRetryRequest;
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::http::{error_response, mutation_response, now_ms, AppState};
use crate::store_core::{self, MutationRequest, StoreError};

pub const JOB_ENTITY_KIND: &str = "outbox_job";

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DELIVERED: &str = "delivered";
pub const STATUS_FAILED_TERMINAL: &str = "failed_terminal";
pub const STATUS_DELIVERY_OUTCOME_UNKNOWN: &str = "delivery_outcome_unknown";

/// Delivery attempts are abandoned to terminal failure after this many tries.
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxJob {
    pub job_id: String,
    pub provider: String,
    pub capability: String,
    pub payload_json: String,
    pub source_entity_kind: String,
    pub source_entity_id: String,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub idempotency_key: String,
}

/// A claimed job ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub job_id: String,
    pub provider: String,
    pub capability: String,
    pub payload_json: String,
    pub attempts: u32,
    pub source_entity_kind: String,
    pub source_entity_id: String,
    pub correlation_id: Option<String>,
    pub idempotency_key: String,
}

/// Insert a job inside an open domain-write transaction. The caller's
/// store_core mutation provides the receipt; this is the atomic-enqueue half
/// of "domain write + outbox job in one transaction".
pub fn enqueue_within(
    tx: &Transaction<'_>,
    client_id: &str,
    job: &NewOutboxJob,
    now_ms: u64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO outbox_jobs \
         (client_id, job_id, provider, capability, payload_json, status, attempts, \
          next_attempt_at_ms, source_entity_kind, source_entity_id, correlation_id, \
          causation_id, idempotency_key, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?7, ?8, ?9, ?10, ?11, ?6, ?6)",
        params![
            client_id,
            job.job_id,
            job.provider,
            job.capability,
            job.payload_json,
            now_ms as i64,
            job.source_entity_kind,
            job.source_entity_id,
            job.correlation_id,
            job.causation_id,
            job.idempotency_key,
        ],
    )?;
    Ok(())
}

/// Claim up to `limit` due jobs (optionally scoped to one provider): pending,
/// attempt time reached, lease absent or expired. Claiming sets the lease so
/// an overlapping cycle skips them.
pub fn claim_due_jobs(
    conn: &mut Connection,
    client_id: &str,
    provider: Option<&str>,
    lease_ms: u64,
    limit: usize,
    now_ms: u64,
) -> Result<Vec<ClaimedJob>, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let jobs = {
        let mut stmt = tx.prepare(
            "SELECT job_id, provider, capability, payload_json, attempts, \
             source_entity_kind, source_entity_id, correlation_id, idempotency_key \
             FROM outbox_jobs \
             WHERE client_id = ?1 AND (?2 IS NULL OR provider = ?2) AND status = 'pending' \
               AND next_attempt_at_ms <= ?3 \
               AND (leased_until_ms IS NULL OR leased_until_ms <= ?3) \
             ORDER BY next_attempt_at_ms ASC, job_id ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![client_id, provider, now_ms as i64, limit as i64],
            |row| {
                Ok(ClaimedJob {
                    job_id: row.get(0)?,
                    provider: row.get(1)?,
                    capability: row.get(2)?,
                    payload_json: row.get(3)?,
                    attempts: row.get::<_, i64>(4)? as u32,
                    source_entity_kind: row.get(5)?,
                    source_entity_id: row.get(6)?,
                    correlation_id: row.get(7)?,
                    idempotency_key: row.get(8)?,
                })
            },
        )?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        jobs
    };
    for job in &jobs {
        tx.execute(
            "UPDATE outbox_jobs SET leased_until_ms = ?3, updated_at_ms = ?4 \
             WHERE client_id = ?1 AND job_id = ?2",
            params![
                client_id,
                job.job_id,
                (now_ms + lease_ms) as i64,
                now_ms as i64
            ],
        )?;
    }
    tx.commit()?;
    Ok(jobs)
}

pub fn claim_due_job_by_id(
    conn: &mut Connection,
    client_id: &str,
    job_id: &str,
    lease_ms: u64,
    now_ms: u64,
) -> Result<Option<ClaimedJob>, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let job = tx
        .query_row(
            "SELECT job_id, provider, capability, payload_json, attempts, \
             source_entity_kind, source_entity_id, correlation_id, idempotency_key \
             FROM outbox_jobs \
             WHERE client_id = ?1 AND job_id = ?2 AND status = 'pending' \
               AND next_attempt_at_ms <= ?3 \
               AND (leased_until_ms IS NULL OR leased_until_ms <= ?3)",
            params![client_id, job_id, now_ms as i64],
            |row| {
                Ok(ClaimedJob {
                    job_id: row.get(0)?,
                    provider: row.get(1)?,
                    capability: row.get(2)?,
                    payload_json: row.get(3)?,
                    attempts: row.get::<_, i64>(4)? as u32,
                    source_entity_kind: row.get(5)?,
                    source_entity_id: row.get(6)?,
                    correlation_id: row.get(7)?,
                    idempotency_key: row.get(8)?,
                })
            },
        )
        .optional()?;
    if let Some(job) = &job {
        tx.execute(
            "UPDATE outbox_jobs SET leased_until_ms = ?3, updated_at_ms = ?4 \
             WHERE client_id = ?1 AND job_id = ?2",
            params![
                client_id,
                job.job_id,
                (now_ms + lease_ms) as i64,
                now_ms as i64
            ],
        )?;
    }
    tx.commit()?;
    Ok(job)
}

pub fn job_result_json(
    conn: &Connection,
    client_id: &str,
    job_id: &str,
) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT result_json FROM outbox_jobs \
             WHERE client_id = ?1 AND job_id = ?2 AND status = 'delivered'",
            params![client_id, job_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

pub fn job_exists(conn: &Connection, client_id: &str, job_id: &str) -> Result<bool, StoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            params![client_id, job_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Delivered (a dry-run execution also counts as delivered — the gate
    /// decision is recorded in `result_json`).
    Delivered { result_json: String },
    /// Transient failure; retried at `retry_at_ms` unless attempts exhausted
    /// (then it becomes terminal).
    Retry { error: String, retry_at_ms: u64 },
    /// Permanent failure; never retried.
    Terminal {
        error: String,
        result_json: Option<String>,
    },
    /// Submission may have reached the provider, but no authoritative result
    /// returned. Never retry automatically or through the generic retry route;
    /// the operator must reconcile the provider state first.
    OutcomeUnknown {
        error: String,
        result_json: Option<String>,
    },
}

const PROVIDER_ERROR_DETAIL_MAX_CHARS: usize = 500;

pub fn provider_error_detail(code: &str, message: &str) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        code.to_string()
    } else {
        let detail = format!("{code}: {normalized}");
        if detail.chars().count() > PROVIDER_ERROR_DETAIL_MAX_CHARS {
            format!(
                "{}...",
                detail
                    .chars()
                    .take(PROVIDER_ERROR_DETAIL_MAX_CHARS)
                    .collect::<String>()
            )
        } else {
            detail
        }
    }
}

/// Record one delivery attempt through the receipt spine and update the job
/// row accordingly. Returns the job's final status string.
pub fn record_attempt(
    conn: &mut Connection,
    client_id: &str,
    job: &ClaimedJob,
    outcome: &AttemptOutcome,
    now_ms: u64,
) -> Result<&'static str, StoreError> {
    let attempts = job.attempts + 1;
    let (status, change_kind, error_detail, result_json, next_attempt_at_ms): (
        &'static str,
        &'static str,
        Option<String>,
        Option<String>,
        Option<u64>,
    ) = match outcome {
        AttemptOutcome::Delivered { result_json } => (
            STATUS_DELIVERED,
            "deliver_succeeded",
            None,
            Some(result_json.clone()),
            None,
        ),
        AttemptOutcome::Retry { error, retry_at_ms } => {
            if attempts >= MAX_ATTEMPTS {
                (
                    STATUS_FAILED_TERMINAL,
                    "deliver_attempts_exhausted",
                    Some(error.clone()),
                    None,
                    None,
                )
            } else {
                (
                    STATUS_PENDING,
                    "deliver_retry_scheduled",
                    Some(error.clone()),
                    None,
                    Some(*retry_at_ms),
                )
            }
        }
        AttemptOutcome::Terminal { error, result_json } => (
            STATUS_FAILED_TERMINAL,
            "deliver_failed_terminal",
            Some(error.clone()),
            result_json.clone(),
            None,
        ),
        AttemptOutcome::OutcomeUnknown { error, result_json } => (
            STATUS_DELIVERY_OUTCOME_UNKNOWN,
            "deliver_outcome_unknown",
            Some(error.clone()),
            result_json.clone(),
            None,
        ),
    };
    let after = serde_json::json!({
        "status": status,
        "attempts": attempts,
        "error": error_detail,
    })
    .to_string();
    let retry_cycle = conn
        .query_row(
            "SELECT receipt_id FROM receipts \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
               AND change_kind = 'retry_requested' AND outcome = 'applied' \
             ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
            params![client_id, JOB_ENTITY_KIND, job.job_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let idempotency_key = format!(
        "outbox_attempt:{}:{}:{}",
        job.job_id,
        retry_cycle.as_deref().unwrap_or("initial"),
        attempts
    );
    let owned_client = client_id.to_string();
    let owned_job_id = job.job_id.clone();
    let owned_error = error_detail.clone();
    let owned_result = result_json.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: JOB_ENTITY_KIND,
            entity_id: &job.job_id,
            change_kind,
            actor_id: "outbox_delivery_worker",
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: job.correlation_id.as_deref(),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE outbox_jobs SET status = ?3, attempts = ?4, leased_until_ms = NULL, \
                 last_error = ?5, result_json = COALESCE(?6, result_json), \
                 next_attempt_at_ms = COALESCE(?7, next_attempt_at_ms), updated_at_ms = ?8 \
                 WHERE client_id = ?1 AND job_id = ?2",
                params![
                    owned_client,
                    owned_job_id,
                    status,
                    attempts as i64,
                    owned_error,
                    owned_result,
                    next_attempt_at_ms.map(|v| v as i64),
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(status)
}

/// 30s doubling per attempt, capped at 15 minutes — the shared retry curve
/// for provider deliveries.
pub fn retry_backoff_ms(attempts: u32) -> u64 {
    (30_000u64 << attempts.min(5)).min(900_000)
}

/// Operator retry for a terminal failed provider delivery. This keeps the same
/// authorized outbox job/payload, records the recovery action in receipts, and
/// gives the delivery pump a fresh retry budget.
pub fn retry_terminal_job(
    conn: &mut Connection,
    client_id: &str,
    job_id: &str,
    actor_id: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<store_core::MutationOutcome, StoreError> {
    let before_json: Option<String> = conn
        .query_row(
            "SELECT status, attempts FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            params![client_id, job_id],
            |row| {
                Ok(serde_json::json!({
                    "status": row.get::<_, String>(0)?,
                    "attempts": row.get::<_, i64>(1)? as u32,
                })
                .to_string())
            },
        )
        .optional()?;
    let owned_client = client_id.to_string();
    let owned_job = job_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: JOB_ENTITY_KIND,
            entity_id: job_id,
            change_kind: "retry_requested",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(job_id),
            causation_id: None,
            before_json,
            after_json: Some(
                serde_json::json!({
                    "status": STATUS_PENDING,
                    "attempts": 0,
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE outbox_jobs SET status = 'pending', attempts = 0, leased_until_ms = NULL, \
                 last_error = NULL, result_json = NULL, next_attempt_at_ms = ?3, updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND job_id = ?2 AND status = 'failed_terminal'",
                params![owned_client, owned_job, now_ms as i64],
            )?;
            if changed == 0 {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
                        params![owned_client, owned_job],
                        |row| row.get(0),
                    )
                    .optional()?;
                return Err(StoreError::Domain(match status {
                    Some(status) => format!("outbox_retry_not_failed:{status}"),
                    None => "outbox_job_not_found".to_string(),
                }));
            }
            Ok(())
        },
    )
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/outbox-jobs/{job_id}/retry", post(retry_job))
}

async fn retry_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OutboxRetryRequest>,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    if let Err(denied) = auth.require_all_scope() {
        return *denied;
    }
    if request.idempotency_key.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let actor_id = auth.actor_or(request.actor_id.as_deref());
    let mut persistence = state.persistence.lock();
    match retry_terminal_job(
        persistence.connection(),
        &state.client_id,
        &job_id,
        &actor_id,
        &request.idempotency_key,
        now_ms(),
    ) {
        Ok(outcome) => mutation_response(outcome),
        Err(err) => crate::http::store_error_response("outbox", err),
    }
}

/// How long a delivery claim holds before an overlapping cycle may re-claim.
const LEASE_MS: u64 = 120_000;
const CLAIM_BATCH: usize = 5;

pub struct DeliveryPumpConfig {
    pub enabled: bool,
    pub interval: std::time::Duration,
}

pub fn pump_config_from_settings(
    conn: &Connection,
    client_id: &str,
) -> Result<DeliveryPumpConfig, StoreError> {
    Ok(DeliveryPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &crate::env_registry::BOS_OUTBOX_DELIVERY_ENABLED,
        )?,
        interval: std::time::Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &crate::env_registry::BOS_OUTBOX_DELIVERY_INTERVAL_SECS,
                15,
            )?
            .max(5) as u64,
        ),
    })
}

/// The ONE delivery pump: claims due jobs across ALL providers and dispatches
/// each to its slice's executor. Provider calls run WITHOUT the persistence
/// lock; only claim and record-attempt hold it.
pub fn spawn_delivery_pump(state: crate::http::AppState) {
    std::thread::Builder::new()
        .name("outbox-delivery-pump".to_string())
        .spawn(move || {
            tracing::info!("outbox delivery pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match pump_config_from_settings(persistence.connection_ref(), &state.client_id)
                    {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "outbox delivery config read failed");
                            DeliveryPumpConfig {
                                enabled: false,
                                interval: std::time::Duration::from_secs(15),
                            }
                        }
                    }
                };
                if config.enabled {
                    run_delivery_cycle(&state);
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn outbox-delivery-pump thread");
}

fn run_delivery_cycle(state: &crate::http::AppState) {
    let now = crate::http::now_ms();
    let claimed = {
        let mut persistence = state.persistence.lock();
        match claim_due_jobs(
            persistence.connection(),
            &state.client_id,
            None,
            LEASE_MS,
            CLAIM_BATCH,
            now,
        ) {
            Ok(jobs) => jobs,
            Err(err) => {
                tracing::warn!(error = %err, "outbox claim failed");
                return;
            }
        }
    };
    for job in claimed {
        let outcome = dispatch(state, &job, crate::http::now_ms());
        let mut persistence = state.persistence.lock();
        match record_attempt(
            persistence.connection(),
            &state.client_id,
            &job,
            &outcome,
            crate::http::now_ms(),
        ) {
            Ok(status) => tracing::info!(
                job_id = %job.job_id,
                provider = %job.provider,
                status,
                "outbox delivery attempt recorded"
            ),
            Err(err) => tracing::warn!(
                job_id = %job.job_id,
                error = %err,
                "outbox attempt record failed"
            ),
        }
    }
    let mut persistence = state.persistence.lock();
    match crate::slices::content_plans::service::reconcile_campaign_publications(
        persistence.connection(),
        &state.client_id,
        crate::http::now_ms(),
    ) {
        Ok(settled) if settled > 0 => {
            tracing::info!(settled, "content campaign dependencies reconciled")
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "content campaign reconciliation failed"),
    }
}

/// Provider routing: each write-capable slice contributes one executor. An
/// unknown provider is terminal (a job nothing can deliver must not retry
/// forever).
fn dispatch(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    match job.provider.as_str() {
        crate::slices::calendar_drafts::service::PROVIDER_GOOGLE_CALENDAR => {
            crate::slices::calendar_drafts::worker::deliver(state, job, now_ms)
        }
        // HubSpot carries two capabilities from two slices: note-create
        // (crm_drafts) and record-create (crm_record_drafts).
        crate::slices::crm_drafts::service::PROVIDER_HUBSPOT => {
            if job.capability
                == crate::slices::crm_record_drafts::service::CAPABILITY_CREATE_RECORDS
            {
                crate::slices::crm_record_drafts::service::deliver_hubspot_records(
                    state, job, now_ms,
                )
            } else {
                crate::slices::crm_drafts::service::deliver(state, job, now_ms)
            }
        }
        // EspoCRM carries two capabilities from two slices: note-create
        // (crm_drafts) and record-create (crm_record_drafts).
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM => {
            if job.capability
                == crate::slices::crm_record_drafts::service::CAPABILITY_CREATE_RECORDS
            {
                crate::slices::crm_record_drafts::service::deliver_espocrm_records(
                    state, job, now_ms,
                )
            } else if job.capability
                == crate::slices::crm_sales_intent::service::CAPABILITY_CREATE_LEAD
            {
                crate::slices::crm_sales_intent::service::deliver_espocrm(state, job, now_ms)
            } else {
                crate::slices::crm_drafts::service::deliver_espocrm(state, job, now_ms)
            }
        }
        // Invoice Ninja carries two capabilities from two slices: inbound
        // receipts (ledger_drafts) and outbound invoice drafts.
        crate::slices::ledger_drafts::service::PROVIDER_INVOICE_NINJA => {
            if job.capability
                == crate::slices::invoice_drafts::service::CAPABILITY_CREATE_INVOICE_DRAFT
            {
                crate::slices::invoice_drafts::service::deliver_invoice_ninja(state, job, now_ms)
            } else {
                crate::slices::ledger_drafts::service::deliver(state, job, now_ms)
            }
        }
        crate::slices::invoice_drafts::service::PROVIDER_STRIPE => {
            crate::slices::invoice_drafts::service::deliver(state, job, now_ms)
        }
        crate::slices::ledger_drafts::service::PROVIDER_QBO => {
            crate::slices::ledger_drafts::service::deliver_qbo(state, job, now_ms)
        }
        crate::slices::customer_tier_sync::service::PROVIDER_SHOPIFY => {
            crate::slices::customer_tier_sync::service::deliver(state, job, now_ms)
        }
        crate::slices::email_drafts::service::PROVIDER_GMAIL => {
            if job.capability == crate::slices::email_triage::store::GMAIL_TRASH_CAPABILITY {
                crate::slices::email_triage::service::deliver_gmail_trash(state, job, now_ms)
            } else {
                crate::slices::email_drafts::service::deliver(state, job, now_ms)
            }
        }
        crate::slices::work_queue::agent_launch::PROVIDER_AGENT_MONITOR => {
            crate::slices::work_queue::agent_launch::deliver(job, now_ms)
        }
        crate::slices::quote_workflows::store::PROVIDER_QUOTE_WORKFLOW => {
            crate::slices::quote_workflows::service::deliver(job, now_ms)
        }
        crate::slices::content_drafts::service::PROVIDER_CONTENT_PUBLISH_ADAPTER => {
            crate::slices::content_drafts::service::deliver(state, job, now_ms)
        }
        crate::slices::social_publishing::service::PROVIDER_BUFFER => {
            crate::slices::social_publishing::service::deliver(state, job, now_ms)
        }
        other => AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_provider:{other}"),
            result_json: None,
        },
    }
}

/// Operator-facing summary of one job (joined into slice read models).
pub fn job_summary(
    conn: &Connection,
    client_id: &str,
    job_id: &str,
) -> Result<Option<OutboxJobSummary>, StoreError> {
    let row = conn
        .query_row(
            "SELECT job_id, status, attempts, last_error, result_json \
             FROM outbox_jobs WHERE client_id = ?1 AND job_id = ?2",
            params![client_id, job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(
        row.map(|(job_id, status, attempts, last_error, result_json)| {
            let result: Option<serde_json::Value> =
                result_json.and_then(|raw| serde_json::from_str(&raw).ok());
            let dry_run = result
                .as_ref()
                .and_then(|v| v.get("dry_run"))
                .and_then(serde_json::Value::as_bool);
            let provider_object_id = result
                .as_ref()
                .and_then(|v| v.get("provider_object_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            OutboxJobSummary {
                job_id,
                status,
                attempts: attempts as u32,
                last_error,
                dry_run,
                provider_object_id,
            }
        }),
    )
}

pub fn jobs_by_correlation(
    conn: &Connection,
    client_id: &str,
    correlation_ids: &[String],
    limit: usize,
) -> Result<Vec<OutboxJobSummary>, StoreError> {
    if correlation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; correlation_ids.len()].join(", ");
    let sql = format!(
        "SELECT job_id, status, attempts, last_error, result_json \
         FROM outbox_jobs \
         WHERE client_id = ? AND correlation_id IN ({placeholders}) \
         ORDER BY created_at_ms DESC, job_id DESC LIMIT ?"
    );
    let limit_i64 = limit as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(correlation_ids.len() + 2);
    params.push(&client_id);
    for id in correlation_ids {
        params.push(id);
    }
    params.push(&limit_i64);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        let result_json: Option<String> = row.get(4)?;
        let result: Option<serde_json::Value> =
            result_json.and_then(|raw| serde_json::from_str(&raw).ok());
        let dry_run = result
            .as_ref()
            .and_then(|v| v.get("dry_run"))
            .and_then(serde_json::Value::as_bool);
        let provider_object_id = result
            .as_ref()
            .and_then(|v| v.get("provider_object_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok(OutboxJobSummary {
            job_id: row.get(0)?,
            status: row.get(1)?,
            attempts: row.get::<_, i64>(2)? as u32,
            last_error: row.get(3)?,
            dry_run,
            provider_object_id,
        })
    })?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row?);
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use bos_contracts::operator_users::OperatorUser;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::http::{build_router, test_support::test_state_configured};
    use crate::persistence::{Persistence, PersistencePool};
    use std::sync::{mpsc, Arc, Barrier};

    fn new_job(id: &str) -> NewOutboxJob {
        NewOutboxJob {
            job_id: id.to_string(),
            provider: "google_calendar".to_string(),
            capability: "create_event".to_string(),
            payload_json: "{}".to_string(),
            source_entity_kind: "calendar_event_draft".to_string(),
            source_entity_id: "ced_1".to_string(),
            correlation_id: Some("corr_1".to_string()),
            causation_id: None,
            idempotency_key: format!("enqueue:{id}"),
        }
    }

    fn enqueue_via_mutation(conn: &mut Connection, job: &NewOutboxJob) {
        let job = job.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id: "test-client",
                entity_kind: "calendar_event_draft",
                entity_id: "ced_1",
                change_kind: "approve",
                actor_id: "op_test",
                actor_kind: ActorKindDto::Operator,
                expected_revision: None,
                idempotency_key: &format!("approve:{}", job.job_id),
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: None,
                now_ms: 1_000,
            },
            move |tx| enqueue_within(tx, "test-client", &job, 1_000),
        )
        .expect("enqueue mutation");
    }

    fn make_terminal_job(conn: &mut Connection, job_id: &str) {
        enqueue_via_mutation(conn, &new_job(job_id));
        let claimed = claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000).expect("claim");
        record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Terminal {
                error: "auth_failed".to_string(),
                result_json: Some("{\"message\":\"status 403\"}".to_string()),
            },
            3_000,
        )
        .expect("record terminal");
    }

    fn personal_operator() -> OperatorUser {
        OperatorUser {
            user_id: "user_jordan".to_string(),
            display_name: "Jordan".to_string(),
            active: true,
            archived_at_ms: None,
            default_calendar_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    async fn response_error(response: axum::response::Response) -> String {
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

    fn retry_request(job_id: &str, idempotency_key: &str) -> Request<Body> {
        Request::post(format!("/api/outbox-jobs/{job_id}/retry"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "actor_id": "op_test",
                    "idempotency_key": idempotency_key
                })
                .to_string(),
            ))
            .expect("request")
    }

    #[test]
    fn provider_error_detail_normalizes_and_bounds_messages() {
        assert_eq!(provider_error_detail("code", "  \n\t  "), "code");
        assert_eq!(
            provider_error_detail("code", " first line\nsecond\tline "),
            "code: first line second line"
        );

        let long = "x".repeat(1_000);
        let detail = provider_error_detail("code", &long);
        assert_eq!(detail.chars().count(), PROVIDER_ERROR_DETAIL_MAX_CHARS + 3);
        assert!(detail.ends_with("..."));
    }

    #[test]
    fn enqueue_commits_with_domain_write_and_claim_leases() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        enqueue_via_mutation(conn, &new_job("job_1"));

        let claimed = claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000).expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].job_id, "job_1");

        // Leased: a second overlapping claim sees nothing.
        let again =
            claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_500).expect("claim again");
        assert!(again.is_empty(), "leased job must not be double-claimed");

        // Lease expiry frees it.
        let later =
            claim_due_jobs(conn, "test-client", None, 60_000, 10, 70_000).expect("claim later");
        assert_eq!(later.len(), 1);
    }

    #[test]
    fn concurrent_claimers_do_not_lease_the_same_job() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let state_dir =
            std::env::temp_dir().join(format!("bos-outbox-claim-{}-{unique}", std::process::id()));
        let pool = PersistencePool::open_at(&state_dir).expect("db");
        {
            let mut conn = pool.lock();
            enqueue_via_mutation(conn.connection(), &new_job("job_race"));
        }

        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel();
        for _ in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut conn = pool.lock();
                let claimed =
                    claim_due_jobs(conn.connection(), "test-client", None, 60_000, 10, 2_000)
                        .expect("claim");
                tx.send(claimed).expect("send claimed jobs");
            });
        }
        drop(tx);

        let claimed_by_thread: Vec<Vec<ClaimedJob>> = rx.iter().take(2).collect();
        let total_claimed: usize = claimed_by_thread.iter().map(Vec::len).sum();
        assert_eq!(total_claimed, 1, "one due job may be leased once");
        assert!(claimed_by_thread
            .iter()
            .flatten()
            .all(|job| job.job_id == "job_race"));
        std::fs::remove_dir_all(state_dir).ok();
    }

    #[test]
    fn failed_domain_write_rolls_back_the_enqueue() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        let job = new_job("job_rollback");
        let result = store_core::mutate(
            conn,
            MutationRequest {
                client_id: "test-client",
                entity_kind: "calendar_event_draft",
                entity_id: "ced_1",
                change_kind: "approve",
                actor_id: "op_test",
                actor_kind: ActorKindDto::Operator,
                expected_revision: None,
                idempotency_key: "approve:rollback",
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: None,
                now_ms: 1_000,
            },
            move |tx| {
                enqueue_within(tx, "test-client", &job, 1_000)?;
                Err(StoreError::Domain("late validation failure".to_string()))
            },
        );
        assert!(result.is_err());
        let count: i64 = persistence
            .connection()
            .query_row("SELECT COUNT(*) FROM outbox_jobs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(
            count, 0,
            "outbox enqueue must roll back with the domain write"
        );
    }

    #[test]
    fn delivered_attempt_finalizes_job_with_receipt() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        enqueue_via_mutation(conn, &new_job("job_ok"));
        let claimed = claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000).expect("claim");
        let status = record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Delivered {
                result_json: serde_json::json!({
                    "dry_run": true,
                    "provider_object_id": "evt-1",
                })
                .to_string(),
            },
            3_000,
        )
        .expect("record");
        assert_eq!(status, STATUS_DELIVERED);

        let summary = job_summary(persistence.connection_ref(), "test-client", "job_ok")
            .expect("summary")
            .expect("job exists");
        assert_eq!(summary.status, STATUS_DELIVERED);
        assert_eq!(summary.attempts, 1);
        assert_eq!(summary.dry_run, Some(true));
        assert_eq!(summary.provider_object_id.as_deref(), Some("evt-1"));

        let receipts = store_core::receipts_for_entity(
            persistence.connection_ref(),
            "test-client",
            JOB_ENTITY_KIND,
            "job_ok",
            10,
        )
        .expect("receipts");
        assert_eq!(receipts.len(), 1, "delivery attempt must be receipted");
    }

    #[test]
    fn retry_schedules_until_attempts_exhaust_into_terminal() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        enqueue_via_mutation(conn, &new_job("job_retry"));

        let mut now = 2_000u64;
        for attempt in 1..MAX_ATTEMPTS {
            let claimed = claim_due_jobs(conn, "test-client", None, 1_000, 10, now).expect("claim");
            assert_eq!(claimed.len(), 1, "attempt {attempt} should be claimable");
            let status = record_attempt(
                conn,
                "test-client",
                &claimed[0],
                &AttemptOutcome::Retry {
                    error: "transient".to_string(),
                    retry_at_ms: now + 10,
                },
                now,
            )
            .expect("record");
            assert_eq!(status, STATUS_PENDING, "attempt {attempt} stays pending");
            now += 100;
        }
        let claimed =
            claim_due_jobs(conn, "test-client", None, 1_000, 10, now).expect("claim final");
        let status = record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Retry {
                error: "still transient".to_string(),
                retry_at_ms: now + 10,
            },
            now,
        )
        .expect("record final");
        assert_eq!(status, STATUS_FAILED_TERMINAL, "attempts must exhaust");
    }

    #[test]
    fn operator_retry_resets_terminal_job_to_pending_with_receipt() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        enqueue_via_mutation(conn, &new_job("job_terminal"));
        let claimed = claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000).expect("claim");
        record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Terminal {
                error: "auth_failed".to_string(),
                result_json: Some("{\"message\":\"status 403\"}".to_string()),
            },
            3_000,
        )
        .expect("record terminal");

        retry_terminal_job(
            conn,
            "test-client",
            "job_terminal",
            "op_test",
            "retry:job_terminal",
            4_000,
        )
        .expect("retry");

        let summary = job_summary(conn, "test-client", "job_terminal")
            .expect("summary")
            .expect("job");
        assert_eq!(summary.status, STATUS_PENDING);
        assert_eq!(summary.attempts, 0);
        assert_eq!(summary.last_error, None);

        let claimed =
            claim_due_jobs(conn, "test-client", None, 60_000, 10, 4_000).expect("claim retried");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].job_id, "job_terminal");
        record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Delivered {
                result_json: "{\"dry_run\":false,\"provider_object_id\":\"recovered\"}".to_string(),
            },
            5_000,
        )
        .expect("record recovered delivery");
        let recovered = job_summary(conn, "test-client", "job_terminal")
            .expect("summary")
            .expect("job");
        assert_eq!(recovered.status, STATUS_DELIVERED);
        assert_eq!(recovered.attempts, 1);
        assert_eq!(recovered.provider_object_id.as_deref(), Some("recovered"));

        let receipts = store_core::receipts_for_entity(
            conn,
            "test-client",
            JOB_ENTITY_KIND,
            "job_terminal",
            10,
        )
        .expect("receipts");
        assert!(
            receipts
                .iter()
                .any(|receipt| receipt.change_kind == "retry_requested"),
            "retry request is auditable"
        );
        let before_json: String = conn
            .query_row(
                "SELECT before_json FROM receipts \
                 WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
                   AND change_kind = 'retry_requested' AND outcome = 'applied'",
                params!["test-client", JOB_ENTITY_KIND, "job_terminal"],
                |row| row.get(0),
            )
            .expect("retry receipt before_json");
        let before: serde_json::Value = serde_json::from_str(&before_json).expect("json");
        assert_eq!(before["status"], STATUS_FAILED_TERMINAL);
        assert_eq!(before["attempts"], 1);
    }

    #[test]
    fn operator_retry_replays_same_idempotency_key_after_status_changes() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        enqueue_via_mutation(conn, &new_job("job_terminal_replay"));
        let claimed = claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000).expect("claim");
        record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &AttemptOutcome::Terminal {
                error: "auth_failed".to_string(),
                result_json: None,
            },
            3_000,
        )
        .expect("record terminal");

        retry_terminal_job(
            conn,
            "test-client",
            "job_terminal_replay",
            "op_test",
            "retry:job_terminal_replay",
            4_000,
        )
        .expect("first retry");
        let replay = retry_terminal_job(
            conn,
            "test-client",
            "job_terminal_replay",
            "op_test",
            "retry:job_terminal_replay",
            4_100,
        )
        .expect("same retry key replays");
        assert!(
            matches!(
                replay,
                store_core::MutationOutcome::ReplayedIdempotent { .. }
            ),
            "same retry key should replay even though the job is now pending"
        );

        let summary = job_summary(conn, "test-client", "job_terminal_replay")
            .expect("summary")
            .expect("job");
        assert_eq!(summary.status, STATUS_PENDING);
        assert_eq!(summary.attempts, 0);
    }

    #[tokio::test]
    async fn retry_route_rejects_named_user_and_leaves_terminal_job_unchanged() {
        let state = test_state_configured(None, &[]);
        {
            let mut persistence = state.persistence.lock();
            let conn = persistence.connection();
            crate::slices::operator_users::store::create_user(
                conn,
                "test-client",
                "operator",
                &personal_operator(),
                "bosu_tok_jordan",
                "create_user_jordan",
            )
            .expect("operator user");
            make_terminal_job(conn, "job_retry_named");
        }
        let router = build_router(state.clone());

        let mut request = retry_request("job_retry_named", "retry_named");
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer bosu_tok_jordan"),
        );
        let response = router.oneshot(request).await.expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response_error(response).await, "scope_forbidden");

        let persistence = state.persistence.lock();
        let summary = job_summary(
            persistence.connection_ref(),
            "test-client",
            "job_retry_named",
        )
        .expect("summary")
        .expect("job");
        assert_eq!(summary.status, STATUS_FAILED_TERMINAL);
        assert_eq!(summary.attempts, 1);
    }

    #[tokio::test]
    async fn retry_route_all_scope_resets_terminal_job_to_pending() {
        let state = test_state_configured(None, &[]);
        {
            let mut persistence = state.persistence.lock();
            make_terminal_job(persistence.connection(), "job_retry_all");
        }
        let router = build_router(state.clone());

        let response = router
            .oneshot(retry_request("job_retry_all", "retry_all"))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let persistence = state.persistence.lock();
        let summary = job_summary(persistence.connection_ref(), "test-client", "job_retry_all")
            .expect("summary")
            .expect("job");
        assert_eq!(summary.status, STATUS_PENDING);
        assert_eq!(summary.attempts, 0);
        assert_eq!(summary.last_error, None);
    }
}
