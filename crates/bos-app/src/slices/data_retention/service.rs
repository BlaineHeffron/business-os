//! Retention policy, bounded cycle orchestration, and status assembly.

use std::time::Duration;

use bos_contracts::data_retention::{DataRetentionStatus, SqliteAutoVacuumMode};
use bos_contracts::receipt::ActorKindDto;
use sha2::{Digest, Sha256};

use super::store;
use crate::env_registry;
use crate::http::{AppState, SyncGuard};
use crate::slices::email_triage::store as email_store;
use crate::store_core::{self, MutationOutcome, StoreError};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_INTERVAL_SECS: usize = 21_600;
const DEFAULT_RETENTION_DAYS: usize = 90;
const DEFAULT_BATCH_SIZE: usize = 200;
const DEFAULT_MAX_ROWS_PER_CYCLE: usize = 5_000;
const DEFAULT_INCREMENTAL_VACUUM_PAGES: usize = 256;

/// Audit-safe initial policy. Only high-volume provider/source mirrors whose
/// canonical data remains in the provider are eligible. Everything else keeps
/// payloads indefinitely until explicitly reviewed and added here.
pub const RECEIPT_PAYLOAD_ENTITY_KINDS: &[&str] = &[
    crate::slices::accounting::store::BALANCE_SHEET_ENTITY_KIND,
    crate::slices::accounting::store::BILL_ENTITY_KIND,
    crate::slices::accounting::store::CUSTOMER_ENTITY_KIND,
    crate::slices::accounting::store::INVOICE_ENTITY_KIND,
    crate::slices::accounting::store::PNL_ENTITY_KIND,
    crate::slices::crm_cache::store::CONTACT_ENTITY_KIND,
    crate::slices::crm_cache::store::DEAL_ENTITY_KIND,
    crate::slices::email_triage::store::INBOUND_ENTITY_KIND,
    crate::slices::inventory::store::ALERT_ENTITY_KIND,
    crate::slices::inventory::store::MATERIAL_ENTITY_KIND,
    crate::slices::inventory::store::ORDER_ENTITY_KIND,
    crate::slices::inventory::store::PO_ENTITY_KIND,
    crate::slices::inventory::store::REORDER_ENTITY_KIND,
    crate::outbox::JOB_ENTITY_KIND,
    crate::slices::shopify_sales::store::CUSTOMER_ENTITY_KIND,
    crate::slices::shopify_sales::store::ORDER_ENTITY_KIND,
];

pub const RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS: &[(&str, &[&str])] = &[(
    crate::outbox::JOB_ENTITY_KIND,
    &[
        "deliver_succeeded",
        "deliver_attempts_exhausted",
        "deliver_retry_scheduled",
        "deliver_failed_terminal",
    ],
)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub email_body_days: u64,
    pub receipt_payload_days: u64,
    pub batch_size: usize,
    pub max_rows_per_cycle: usize,
    pub incremental_vacuum_pages: usize,
}

#[derive(Debug, Clone)]
pub struct RunActor {
    pub actor_id: String,
    pub actor_kind: ActorKindDto,
}

impl RunActor {
    pub fn system() -> Self {
        Self {
            actor_id: "data_retention_pump".to_string(),
            actor_kind: ActorKindDto::System,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleSummary {
    pub email_bodies_compacted: usize,
    pub receipt_payloads_compacted: usize,
}

impl CycleSummary {
    pub fn total(&self) -> usize {
        self.email_bodies_compacted
            .saturating_add(self.receipt_payloads_compacted)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleReport {
    pub summary: CycleSummary,
    pub errors: Vec<String>,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<RetentionConfig, StoreError> {
    let interval_secs = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_INTERVAL_SECS,
        DEFAULT_INTERVAL_SECS,
    )?
    .clamp(900, 86_400);
    let email_body_days = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_EMAIL_BODY_DAYS,
        DEFAULT_RETENTION_DAYS,
    )?
    .clamp(7, 3_650);
    let receipt_payload_days = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS,
        DEFAULT_RETENTION_DAYS,
    )?
    .clamp(7, 3_650);
    let batch_size = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_BATCH_SIZE,
        DEFAULT_BATCH_SIZE,
    )?
    .clamp(1, 1_000);
    let max_rows_per_cycle = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE,
        DEFAULT_MAX_ROWS_PER_CYCLE,
    )?
    .clamp(batch_size, 50_000);
    let incremental_vacuum_pages = setting_usize(
        conn,
        client_id,
        &env_registry::BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES,
        DEFAULT_INCREMENTAL_VACUUM_PAGES,
    )?
    .min(4_096);
    Ok(RetentionConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_DATA_RETENTION_ENABLED,
        )?,
        interval: Duration::from_secs(interval_secs as u64),
        email_body_days: email_body_days as u64,
        receipt_payload_days: receipt_payload_days as u64,
        batch_size,
        max_rows_per_cycle,
        incremental_vacuum_pages,
    })
}

pub fn status(
    state: &AppState,
    guard: &SyncGuard,
    now_ms: u64,
) -> Result<DataRetentionStatus, StoreError> {
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let config = config_from_settings(conn, &state.client_id)?;
    let email_body_cutoff_ms = cutoff_ms(now_ms, config.email_body_days);
    let receipt_payload_cutoff_ms = cutoff_ms(now_ms, config.receipt_payload_days);
    let storage = store::sqlite_storage_stats(conn)?;
    let eligible_email_bodies =
        email_store::eligible_email_body_count(conn, &state.client_id, email_body_cutoff_ms)?;
    let eligible_receipt_payloads = store_core::eligible_receipt_payload_count(
        conn,
        &state.client_id,
        receipt_payload_cutoff_ms,
        RECEIPT_PAYLOAD_ENTITY_KINDS,
        RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS,
    )?;
    let latest = store::latest_retention_receipt(conn, &state.client_id)?;
    let auto_vacuum_mode = auto_vacuum_mode(storage.auto_vacuum_mode);
    Ok(DataRetentionStatus {
        enabled: config.enabled,
        interval_secs: config.interval.as_secs(),
        email_body_retention_days: config.email_body_days,
        receipt_payload_retention_days: config.receipt_payload_days,
        batch_size: config.batch_size as u64,
        max_rows_per_cycle: config.max_rows_per_cycle as u64,
        incremental_vacuum_pages: config.incremental_vacuum_pages as u64,
        email_body_cutoff_ms,
        receipt_payload_cutoff_ms,
        eligible_email_bodies,
        eligible_receipt_payloads,
        database_bytes: storage.database_bytes,
        page_size_bytes: storage.page_size_bytes,
        page_count: storage.page_count,
        freelist_pages: storage.freelist_pages,
        freelist_bytes: storage
            .freelist_pages
            .saturating_mul(storage.page_size_bytes),
        wal_bytes: storage.wal_bytes,
        auto_vacuum_mode,
        attended_full_vacuum_required: auto_vacuum_mode != SqliteAutoVacuumMode::Incremental,
        in_flight: guard.in_flight,
        last_attempt_ms: guard.last_attempt_ms,
        last_outcome: guard.last_outcome.clone(),
        last_duration_ms: guard.last_duration_ms,
        last_units_compacted: guard.units_used as u64,
        next_allowed_at_ms: guard.next_allowed_at_ms,
        last_retention_receipt_at_ms: latest.as_ref().map(|receipt| receipt.created_at_ms),
        last_retention_receipt_outcome: latest.map(|receipt| receipt.outcome),
    })
}

pub fn run_cycle(
    state: &AppState,
    config: &RetentionConfig,
    actor: &RunActor,
    run_id: &str,
    now_ms: u64,
) -> CycleReport {
    let mut report = CycleReport::default();
    run_storage_step(state, "passive_checkpoint", &mut report.errors, |conn| {
        store::checkpoint_passive(conn).map(|_| ())
    });

    let email_cutoff = cutoff_ms(now_ms, config.email_body_days);
    compact_email_body_batches(
        state,
        config,
        actor,
        run_id,
        now_ms,
        email_cutoff,
        &mut report,
    );

    let remaining = config
        .max_rows_per_cycle
        .saturating_sub(report.summary.total());
    if remaining > 0 {
        let receipt_cutoff = cutoff_ms(now_ms, config.receipt_payload_days);
        compact_receipt_payload_batches(
            state,
            config,
            actor,
            run_id,
            now_ms,
            receipt_cutoff,
            remaining,
            &mut report,
        );
    }

    run_storage_step(state, "optimize", &mut report.errors, store::optimize);
    run_storage_step(state, "incremental_vacuum", &mut report.errors, |conn| {
        store::incremental_vacuum(conn, config.incremental_vacuum_pages)
    });
    run_storage_step(state, "truncate_checkpoint", &mut report.errors, |conn| {
        store::checkpoint_truncate(conn).map(|_| ())
    });
    report
}

pub fn manual_run_id(idempotency_key: &str) -> String {
    let digest = Sha256::digest(idempotency_key.as_bytes());
    format!("retention_{:x}", digest)[..26].to_string()
}

fn compact_email_body_batches(
    state: &AppState,
    config: &RetentionConfig,
    actor: &RunActor,
    run_id: &str,
    now_ms: u64,
    cutoff_ms: u64,
    report: &mut CycleReport,
) {
    while report.summary.total() < config.max_rows_per_cycle {
        let remaining = config
            .max_rows_per_cycle
            .saturating_sub(report.summary.total());
        let limit = config.batch_size.min(remaining);
        let mut persistence = state.persistence.lock();
        let source_keys = match email_store::email_body_compaction_candidates(
            persistence.connection_ref(),
            &state.client_id,
            cutoff_ms,
            limit,
        ) {
            Ok(keys) => keys,
            Err(err) => {
                report.errors.push(format!("email_candidates:{err}"));
                break;
            }
        };
        if source_keys.is_empty() {
            break;
        }
        let identity = batch_identity("email_bodies", cutoff_ms, &source_keys);
        let result = email_store::compact_email_bodies(
            persistence.connection(),
            email_store::EmailBodyCompactionBatch {
                client_id: &state.client_id,
                actor_id: &actor.actor_id,
                actor_kind: actor.actor_kind,
                cutoff_ms,
                source_keys: &source_keys,
                mutation_entity_kind: store::RETENTION_ENTITY_KIND,
                mutation_change_kind: store::EMAIL_BODY_COMPACTION_CHANGE_KIND,
                entity_id: &identity.entity_id,
                idempotency_key: &identity.idempotency_key,
                correlation_id: Some(run_id),
                causation_id: None,
                now_ms,
            },
        );
        match result {
            Ok(MutationOutcome::Applied { .. } | MutationOutcome::ReplayedIdempotent { .. }) => {
                report.summary.email_bodies_compacted = report
                    .summary
                    .email_bodies_compacted
                    .saturating_add(source_keys.len());
            }
            Ok(MutationOutcome::RevisionConflict { .. }) => {
                report
                    .errors
                    .push("email_compaction:unexpected_revision_conflict".to_string());
                break;
            }
            Err(err) => {
                report.errors.push(format!("email_compaction:{err}"));
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compact_receipt_payload_batches(
    state: &AppState,
    config: &RetentionConfig,
    actor: &RunActor,
    run_id: &str,
    now_ms: u64,
    cutoff_ms: u64,
    max_rows: usize,
    report: &mut CycleReport,
) {
    while report.summary.receipt_payloads_compacted < max_rows {
        let remaining = max_rows.saturating_sub(report.summary.receipt_payloads_compacted);
        let limit = config.batch_size.min(remaining);
        let mut persistence = state.persistence.lock();
        let receipt_ids = match store_core::receipt_payload_compaction_candidates(
            persistence.connection_ref(),
            &state.client_id,
            cutoff_ms,
            RECEIPT_PAYLOAD_ENTITY_KINDS,
            RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS,
            limit,
        ) {
            Ok(ids) => ids,
            Err(err) => {
                report.errors.push(format!("receipt_candidates:{err}"));
                break;
            }
        };
        if receipt_ids.is_empty() {
            break;
        }
        let identity = batch_identity("receipt_payloads", cutoff_ms, &receipt_ids);
        let result = store_core::compact_receipt_payloads(
            persistence.connection(),
            store_core::ReceiptPayloadCompactionBatch {
                client_id: &state.client_id,
                actor_id: &actor.actor_id,
                actor_kind: actor.actor_kind,
                cutoff_ms,
                allowlisted_entity_kinds: RECEIPT_PAYLOAD_ENTITY_KINDS,
                restricted_change_kinds: RECEIPT_PAYLOAD_RESTRICTED_CHANGE_KINDS,
                receipt_ids: &receipt_ids,
                mutation_entity_kind: store::RETENTION_ENTITY_KIND,
                mutation_change_kind: store::RECEIPT_PAYLOAD_COMPACTION_CHANGE_KIND,
                entity_id: &identity.entity_id,
                idempotency_key: &identity.idempotency_key,
                correlation_id: Some(run_id),
                causation_id: None,
                now_ms,
            },
        );
        match result {
            Ok(MutationOutcome::Applied { .. } | MutationOutcome::ReplayedIdempotent { .. }) => {
                report.summary.receipt_payloads_compacted = report
                    .summary
                    .receipt_payloads_compacted
                    .saturating_add(receipt_ids.len());
            }
            Ok(MutationOutcome::RevisionConflict { .. }) => {
                report
                    .errors
                    .push("receipt_compaction:unexpected_revision_conflict".to_string());
                break;
            }
            Err(err) => {
                report.errors.push(format!("receipt_compaction:{err}"));
                break;
            }
        }
    }
}

fn run_storage_step(
    state: &AppState,
    step: &str,
    errors: &mut Vec<String>,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<(), StoreError>,
) {
    let persistence = state.persistence.lock();
    if let Err(err) = operation(persistence.connection_ref()) {
        tracing::warn!(step, error = %err, "data retention storage step failed");
        errors.push(format!("{step}:{err}"));
    }
}

fn setting_usize(
    conn: &rusqlite::Connection,
    client_id: &str,
    var: &env_registry::EnvVar,
    default: usize,
) -> Result<usize, StoreError> {
    crate::slices::admin_settings::service::usize_or(conn, client_id, var, default)
}

fn cutoff_ms(now_ms: u64, days: u64) -> u64 {
    now_ms.saturating_sub(days.saturating_mul(DAY_MS))
}

fn auto_vacuum_mode(raw: i64) -> SqliteAutoVacuumMode {
    match raw {
        0 => SqliteAutoVacuumMode::None,
        1 => SqliteAutoVacuumMode::Full,
        2 => SqliteAutoVacuumMode::Incremental,
        _ => SqliteAutoVacuumMode::Unknown,
    }
}

struct BatchIdentity {
    entity_id: String,
    idempotency_key: String,
}

fn batch_identity(operation: &str, cutoff_ms: u64, ordered_keys: &[String]) -> BatchIdentity {
    let first = ordered_keys.first().map(String::as_str).unwrap_or("");
    let last = ordered_keys.last().map(String::as_str).unwrap_or("");
    let mut hasher = Sha256::new();
    for part in [operation, &cutoff_ms.to_string(), first, last] {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    let digest = hasher.finalize();
    let suffix = format!("{:x}", digest)[..16].to_string();
    BatchIdentity {
        entity_id: format!("{operation}:{suffix}"),
        idempotency_key: format!("data_retention:{operation}:{cutoff_ms}:{suffix}"),
    }
}
