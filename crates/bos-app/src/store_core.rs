//! The ONE persistence path for mutations. Slices never write raw CRUD.
//!
//! `mutate` runs, in a single transaction:
//!   idempotency replay check → revision check → domain write (closure) →
//!   revision bump → receipt insert.
//!
//! A slice therefore cannot mutate state without producing an audit record, and
//! failed mutations are recorded as receipts too (outcome = failed/conflict).

use bos_contracts::receipt::{ActorKindDto, ReceiptDto, ReceiptOutcomeDto};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use rusqlite::{params_from_iter, types::Value};

#[derive(Debug, Clone)]
pub struct MutationRequest<'a> {
    pub client_id: &'a str,
    pub entity_kind: &'a str,
    pub entity_id: &'a str,
    pub change_kind: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    /// `None` = create or unconditional write; `Some(n)` = optimistic concurrency.
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub correlation_id: Option<&'a str>,
    pub causation_id: Option<&'a str>,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOutcome {
    Applied {
        receipt_id: String,
        revision: u64,
    },
    ReplayedIdempotent {
        receipt_id: String,
        revision: Option<u64>,
    },
    RevisionConflict {
        receipt_id: String,
        current_revision: Option<u64>,
    },
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(String),
    Domain(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(msg) => write!(f, "store sqlite error: {msg}"),
            Self::Domain(msg) => write!(f, "store domain error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err.to_string())
    }
}

impl StoreError {
    /// True when SQLite asked the caller to retry because the file is busy.
    pub fn is_sqlite_busy(&self) -> bool {
        match self {
            Self::Sqlite(message) => sqlite_busy_message(message),
            Self::Domain(_) => false,
        }
    }
}

fn sqlite_busy_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("database is locked")
        || lower.contains("database is busy")
        || lower.contains("sqlite_busy")
        || lower.contains("sqlite_locked")
        || lower.contains("database schema is locked")
}

#[derive(Debug, Clone)]
pub struct ReceiptPayloadCompactionBatch<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub actor_kind: ActorKindDto,
    pub cutoff_ms: u64,
    pub allowlisted_entity_kinds: &'a [&'a str],
    /// Optional exact change-kind restriction by entity kind. Entity kinds not
    /// present here retain the entity-wide behavior above.
    pub restricted_change_kinds: &'a [(&'a str, &'a [&'a str])],
    pub receipt_ids: &'a [String],
    pub mutation_entity_kind: &'a str,
    pub mutation_change_kind: &'a str,
    pub entity_id: &'a str,
    pub idempotency_key: &'a str,
    pub correlation_id: Option<&'a str>,
    pub causation_id: Option<&'a str>,
    pub now_ms: u64,
}

/// Return the oldest applied receipt payloads eligible under an explicit
/// caller-owned allowlist. Receipt rows themselves are permanent.
pub fn receipt_payload_compaction_candidates(
    conn: &Connection,
    client_id: &str,
    cutoff_ms: u64,
    allowlisted_entity_kinds: &[&str],
    restricted_change_kinds: &[(&str, &[&str])],
    limit: usize,
) -> Result<Vec<String>, StoreError> {
    if allowlisted_entity_kinds.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; allowlisted_entity_kinds.len()].join(", ");
    let restriction_sql = change_kind_restriction_sql(restricted_change_kinds);
    let sql = format!(
        "SELECT receipt_id FROM receipts \
         WHERE client_id = ? AND outcome = 'applied' AND created_at_ms < ? \
           AND (before_json IS NOT NULL OR after_json IS NOT NULL) \
           AND entity_kind IN ({placeholders}) {restriction_sql} \
         ORDER BY created_at_ms ASC, receipt_id ASC LIMIT ?"
    );
    let mut values = Vec::with_capacity(allowlisted_entity_kinds.len() + 3);
    values.push(Value::Text(client_id.to_string()));
    values.push(Value::Integer(cutoff_ms as i64));
    values.extend(
        allowlisted_entity_kinds
            .iter()
            .map(|kind| Value::Text((*kind).to_string())),
    );
    push_change_kind_restriction_values(&mut values, restricted_change_kinds);
    values.push(Value::Integer(limit as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
        row.get::<_, String>(0)
    })?;
    let mut receipt_ids = Vec::new();
    for row in rows {
        receipt_ids.push(row?);
    }
    Ok(receipt_ids)
}

pub fn eligible_receipt_payload_count(
    conn: &Connection,
    client_id: &str,
    cutoff_ms: u64,
    allowlisted_entity_kinds: &[&str],
    restricted_change_kinds: &[(&str, &[&str])],
) -> Result<u64, StoreError> {
    if allowlisted_entity_kinds.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; allowlisted_entity_kinds.len()].join(", ");
    let restriction_sql = change_kind_restriction_sql(restricted_change_kinds);
    let sql = format!(
        "SELECT COUNT(*) FROM receipts \
         WHERE client_id = ? AND outcome = 'applied' AND created_at_ms < ? \
           AND (before_json IS NOT NULL OR after_json IS NOT NULL) \
           AND entity_kind IN ({placeholders}) {restriction_sql}"
    );
    let mut values = Vec::with_capacity(allowlisted_entity_kinds.len() + 2);
    values.push(Value::Text(client_id.to_string()));
    values.push(Value::Integer(cutoff_ms as i64));
    values.extend(
        allowlisted_entity_kinds
            .iter()
            .map(|kind| Value::Text((*kind).to_string())),
    );
    push_change_kind_restriction_values(&mut values, restricted_change_kinds);
    conn.query_row(&sql, params_from_iter(values.iter()), |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| count as u64)
    .map_err(Into::into)
}

/// Clear JSON payloads from one exact batch while preserving every receipt
/// identity, outcome, revision, correlation, and idempotency field.
pub fn compact_receipt_payloads(
    conn: &mut Connection,
    batch: ReceiptPayloadCompactionBatch<'_>,
) -> Result<MutationOutcome, StoreError> {
    if batch.receipt_ids.is_empty() || batch.allowlisted_entity_kinds.is_empty() {
        return Err(StoreError::Domain(
            "receipt_payload_compaction_batch_empty".to_string(),
        ));
    }
    let first_receipt_id = batch.receipt_ids.first().cloned().unwrap_or_default();
    let last_receipt_id = batch.receipt_ids.last().cloned().unwrap_or_default();
    let after_json = serde_json::json!({
        "operation": "receipt_payload_compaction",
        "cutoff_ms": batch.cutoff_ms,
        "rows_compacted": batch.receipt_ids.len(),
        "first_receipt_id": first_receipt_id,
        "last_receipt_id": last_receipt_id,
    })
    .to_string();
    let kind_placeholders = vec!["?"; batch.allowlisted_entity_kinds.len()].join(", ");
    let restriction_sql = change_kind_restriction_sql(batch.restricted_change_kinds);
    let id_placeholders = vec!["?"; batch.receipt_ids.len()].join(", ");
    let sql = format!(
        "UPDATE receipts SET before_json = NULL, after_json = NULL \
         WHERE client_id = ? AND outcome = 'applied' AND created_at_ms < ? \
           AND (before_json IS NOT NULL OR after_json IS NOT NULL) \
           AND entity_kind IN ({kind_placeholders}) {restriction_sql} \
           AND receipt_id IN ({id_placeholders})"
    );
    let expected_rows = batch.receipt_ids.len();
    let client_id = batch.client_id.to_string();
    let cutoff_ms = batch.cutoff_ms;
    let allowlisted_entity_kinds = batch
        .allowlisted_entity_kinds
        .iter()
        .map(|kind| (*kind).to_string())
        .collect::<Vec<_>>();
    let restricted_change_kinds = batch.restricted_change_kinds.to_vec();
    let receipt_ids = batch.receipt_ids.to_vec();
    mutate(
        conn,
        MutationRequest {
            client_id: batch.client_id,
            entity_kind: batch.mutation_entity_kind,
            entity_id: batch.entity_id,
            change_kind: batch.mutation_change_kind,
            actor_id: batch.actor_id,
            actor_kind: batch.actor_kind,
            expected_revision: None,
            idempotency_key: batch.idempotency_key,
            correlation_id: batch.correlation_id,
            causation_id: batch.causation_id,
            before_json: None,
            after_json: Some(after_json),
            now_ms: batch.now_ms,
        },
        move |tx| {
            let mut values =
                Vec::with_capacity(allowlisted_entity_kinds.len() + receipt_ids.len() + 2);
            values.push(Value::Text(client_id));
            values.push(Value::Integer(cutoff_ms as i64));
            values.extend(allowlisted_entity_kinds.into_iter().map(Value::Text));
            push_change_kind_restriction_values(&mut values, &restricted_change_kinds);
            values.extend(receipt_ids.into_iter().map(Value::Text));
            let changed = tx.execute(&sql, params_from_iter(values.iter()))?;
            if changed != expected_rows {
                return Err(StoreError::Domain(format!(
                    "receipt_payload_compaction_race:expected={expected_rows}:changed={changed}"
                )));
            }
            Ok(())
        },
    )
}

fn change_kind_restriction_sql(restrictions: &[(&str, &[&str])]) -> String {
    restrictions
        .iter()
        .map(|(_, changes)| {
            let placeholders = vec!["?"; changes.len()].join(", ");
            format!("AND (entity_kind <> ? OR change_kind IN ({placeholders}))")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_change_kind_restriction_values(values: &mut Vec<Value>, restrictions: &[(&str, &[&str])]) {
    for (entity_kind, changes) in restrictions {
        values.push(Value::Text((*entity_kind).to_string()));
        values.extend(
            changes
                .iter()
                .map(|change| Value::Text((*change).to_string())),
        );
    }
}

/// Execute a mutation through the receipt spine.
///
/// `domain_write` receives the open transaction and performs the slice's own
/// table writes. If it errors, the transaction rolls back and a failure receipt
/// is written in a fresh transaction so the failure remains auditable.
pub fn mutate<F>(
    conn: &mut Connection,
    request: MutationRequest<'_>,
    domain_write: F,
) -> Result<MutationOutcome, StoreError>
where
    F: FnOnce(&Transaction<'_>) -> Result<(), StoreError>,
{
    mutate_with_receipt_id(conn, request, |tx, _receipt_id| domain_write(tx))
}

/// Execute a mutation and make the applied receipt id available to the
/// domain-write closure. This is intentionally narrow: workflow trace rows need
/// to materialize their step receipt id in the same transaction as the receipt.
pub fn mutate_with_receipt_id<F>(
    conn: &mut Connection,
    request: MutationRequest<'_>,
    domain_write: F,
) -> Result<MutationOutcome, StoreError>
where
    F: FnOnce(&Transaction<'_>, &str) -> Result<(), StoreError>,
{
    mutate_with_receipt_id_inner(conn, request, domain_write)
}

/// Record an auditable failed operation that did not reach a domain write.
///
/// Use this for asynchronous/background stages where validation or orchestration
/// fails after the initiating HTTP request has already returned. It keeps those
/// failures visible through the normal receipt/debug spine without pretending a
/// domain row was mutated.
pub fn record_failed_receipt(
    conn: &mut Connection,
    request: MutationRequest<'_>,
    error_class: &str,
) -> Result<String, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_revision = read_revision(
        &tx,
        request.client_id,
        request.entity_kind,
        request.entity_id,
    )?;
    let receipt_id = insert_receipt(
        &tx,
        &request,
        ReceiptOutcomeDto::Failed,
        Some(error_class),
        current_revision,
        current_revision,
    )?;
    tx.commit()?;
    Ok(receipt_id)
}

fn mutate_with_receipt_id_inner<F>(
    conn: &mut Connection,
    request: MutationRequest<'_>,
    domain_write: F,
) -> Result<MutationOutcome, StoreError>
where
    F: FnOnce(&Transaction<'_>, &str) -> Result<(), StoreError>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    if let Some(receipt) = find_applied_receipt(&tx, request.client_id, request.idempotency_key)? {
        let revision = receipt.1;
        let replay_receipt_id = insert_receipt(
            &tx,
            &request,
            ReceiptOutcomeDto::ReplayedIdempotent,
            None,
            revision,
            revision,
        )?;
        tx.commit()?;
        return Ok(MutationOutcome::ReplayedIdempotent {
            receipt_id: replay_receipt_id,
            revision,
        });
    }

    let current_revision = read_revision(
        &tx,
        request.client_id,
        request.entity_kind,
        request.entity_id,
    )?;

    if let Some(expected) = request.expected_revision {
        if current_revision != Some(expected) {
            let receipt_id = insert_receipt(
                &tx,
                &request,
                ReceiptOutcomeDto::RevisionConflict,
                Some("revision_conflict"),
                current_revision,
                current_revision,
            )?;
            tx.commit()?;
            return Ok(MutationOutcome::RevisionConflict {
                receipt_id,
                current_revision,
            });
        }
    }

    let applied_receipt_id = next_receipt_id(&tx)?;
    match domain_write(&tx, &applied_receipt_id) {
        Ok(()) => {
            let next_revision = current_revision.unwrap_or(0) + 1;
            write_revision(
                &tx,
                request.client_id,
                request.entity_kind,
                request.entity_id,
                next_revision,
                request.now_ms,
            )?;
            let receipt_id = insert_receipt(
                &tx,
                &request,
                ReceiptOutcomeDto::Applied,
                None,
                current_revision,
                Some(next_revision),
            )?;
            debug_assert_eq!(receipt_id, applied_receipt_id);
            tx.commit()?;
            Ok(MutationOutcome::Applied {
                receipt_id,
                revision: next_revision,
            })
        }
        Err(err) => {
            drop(tx);
            let failure_tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            insert_receipt(
                &failure_tx,
                &request,
                ReceiptOutcomeDto::Failed,
                Some(&error_class(&err)),
                current_revision,
                current_revision,
            )?;
            failure_tx.commit()?;
            Err(err)
        }
    }
}

/// List receipts for an entity, newest first.
pub fn receipts_for_entity(
    conn: &Connection,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
    limit: usize,
) -> Result<Vec<ReceiptDto>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT receipt_id, client_id, entity_kind, entity_id, change_kind, actor_id, \
         actor_kind, outcome, error_class, revision_before, revision_after, \
         idempotency_key, correlation_id, causation_id, created_at_ms \
         FROM receipts \
         WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
         ORDER BY created_at_ms DESC, receipt_id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![client_id, entity_kind, entity_id, limit as i64],
        receipt_from_row,
    )?;
    let mut receipts = Vec::new();
    for row in rows {
        receipts.push(row?);
    }
    Ok(receipts)
}

/// List receipts caused by one of the supplied correlation ids, newest first.
pub fn receipts_by_correlation(
    conn: &Connection,
    client_id: &str,
    correlation_ids: &[String],
    limit: usize,
) -> Result<Vec<ReceiptDto>, StoreError> {
    if correlation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; correlation_ids.len()].join(", ");
    let sql = format!(
        "SELECT receipt_id, client_id, entity_kind, entity_id, change_kind, actor_id, \
         actor_kind, outcome, error_class, revision_before, revision_after, \
         idempotency_key, correlation_id, causation_id, created_at_ms \
         FROM receipts \
         WHERE client_id = ? AND correlation_id IN ({placeholders}) \
         ORDER BY created_at_ms DESC, receipt_id DESC LIMIT ?"
    );
    let limit_i64 = limit as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(correlation_ids.len() + 2);
    params.push(&client_id);
    for id in correlation_ids {
        params.push(id);
    }
    params.push(&limit_i64);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), receipt_from_row)?;
    let mut receipts = Vec::new();
    for row in rows {
        receipts.push(row?);
    }
    Ok(receipts)
}

/// Actor id from the newest applied receipt for an entity, if any.
pub fn latest_applied_receipt_actor(
    conn: &Connection,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT actor_id FROM receipts \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
               AND outcome = 'applied' \
             ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
            params![client_id, entity_kind, entity_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn receipt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReceiptDto> {
    Ok(ReceiptDto {
        receipt_id: row.get(0)?,
        client_id: row.get(1)?,
        entity_kind: row.get(2)?,
        entity_id: row.get(3)?,
        change_kind: row.get(4)?,
        actor_id: row.get(5)?,
        actor_kind: actor_kind_from_str(&row.get::<_, String>(6)?),
        outcome: outcome_from_str(&row.get::<_, String>(7)?),
        error_class: row.get(8)?,
        revision_before: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        revision_after: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        idempotency_key: row.get(11)?,
        correlation_id: row.get(12)?,
        causation_id: row.get(13)?,
        created_at_ms: row.get::<_, i64>(14)? as u64,
    })
}

fn find_applied_receipt(
    tx: &Transaction<'_>,
    client_id: &str,
    idempotency_key: &str,
) -> Result<Option<(String, Option<u64>)>, StoreError> {
    let row = tx
        .query_row(
            "SELECT receipt_id, revision_after FROM receipts \
             WHERE client_id = ?1 AND idempotency_key = ?2 AND outcome = 'applied'",
            params![client_id, idempotency_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                ))
            },
        )
        .optional()?;
    Ok(row)
}

/// Read the applied revision for one client-wide idempotency key. Useful when
/// an expensive transform must detect an HTTP replay before spending again;
/// the owning mutation still goes through [`mutate`].
pub fn applied_revision_for_idempotency(
    conn: &Connection,
    client_id: &str,
    idempotency_key: &str,
) -> Result<Option<u64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT revision_after FROM receipts \
             WHERE client_id = ?1 AND idempotency_key = ?2 AND outcome = 'applied' \
             ORDER BY created_at_ms DESC, receipt_id DESC LIMIT 1",
            params![client_id, idempotency_key],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .map(|revision| revision as u64))
}

fn read_revision(
    tx: &Transaction<'_>,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
) -> Result<Option<u64>, StoreError> {
    let revision = tx
        .query_row(
            "SELECT revision FROM entity_revisions \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3",
            params![client_id, entity_kind, entity_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(revision.map(|v| v as u64))
}

/// Read the current optimistic-concurrency revision for an entity (the value a
/// caller hands back as `expected_revision`). `None` = no row yet. Spine read
/// used by settings/singleton stores.
pub fn current_revision(
    conn: &Connection,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
) -> Result<Option<u64>, StoreError> {
    conn.query_row(
        "SELECT revision FROM entity_revisions \
         WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3",
        params![client_id, entity_kind, entity_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|value| value.map(|revision| revision as u64))
    .map_err(Into::into)
}

fn write_revision(
    tx: &Transaction<'_>,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
    revision: u64,
    now_ms: u64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO entity_revisions (client_id, entity_kind, entity_id, revision, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (client_id, entity_kind, entity_id) \
         DO UPDATE SET revision = excluded.revision, updated_at_ms = excluded.updated_at_ms",
        params![client_id, entity_kind, entity_id, revision as i64, now_ms as i64],
    )?;
    Ok(())
}

/// Initialize a newly-created entity's revision inside an existing receipted
/// transaction. This is intentionally insert-only: callers use it when another
/// entity's mutation creates a child row, and an existing revision means the
/// create path is not actually creating a fresh entity.
pub fn initialize_revision_within(
    tx: &Transaction<'_>,
    client_id: &str,
    entity_kind: &str,
    entity_id: &str,
    revision: u64,
    now_ms: u64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO entity_revisions (client_id, entity_kind, entity_id, revision, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            client_id,
            entity_kind,
            entity_id,
            revision as i64,
            now_ms as i64
        ],
    )?;
    Ok(())
}

fn insert_receipt(
    tx: &Transaction<'_>,
    request: &MutationRequest<'_>,
    outcome: ReceiptOutcomeDto,
    error_class: Option<&str>,
    revision_before: Option<u64>,
    revision_after: Option<u64>,
) -> Result<String, StoreError> {
    let receipt_id = next_receipt_id(tx)?;
    tx.execute(
        "INSERT INTO receipts (receipt_id, client_id, entity_kind, entity_id, change_kind, \
         actor_id, actor_kind, outcome, error_class, before_json, after_json, \
         revision_before, revision_after, idempotency_key, correlation_id, causation_id, \
         created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            receipt_id,
            request.client_id,
            request.entity_kind,
            request.entity_id,
            request.change_kind,
            request.actor_id,
            actor_kind_str(request.actor_kind),
            outcome_str(outcome),
            error_class,
            request.before_json,
            request.after_json,
            revision_before.map(|v| v as i64),
            revision_after.map(|v| v as i64),
            request.idempotency_key,
            request.correlation_id,
            request.causation_id,
            request.now_ms as i64,
        ],
    )?;
    Ok(receipt_id)
}

fn next_receipt_id(tx: &Transaction<'_>) -> Result<String, StoreError> {
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))?;
    Ok(format!("rcpt_{:012}", count + 1))
}

fn error_class(err: &StoreError) -> String {
    match err {
        StoreError::Sqlite(_) => "sqlite".to_string(),
        StoreError::Domain(_) => "domain".to_string(),
    }
}

fn actor_kind_str(kind: ActorKindDto) -> &'static str {
    match kind {
        ActorKindDto::Operator => "operator",
        ActorKindDto::System => "system",
        ActorKindDto::Agent => "agent",
    }
}

fn actor_kind_from_str(raw: &str) -> ActorKindDto {
    match raw {
        "operator" => ActorKindDto::Operator,
        "agent" => ActorKindDto::Agent,
        _ => ActorKindDto::System,
    }
}

fn outcome_str(outcome: ReceiptOutcomeDto) -> &'static str {
    match outcome {
        ReceiptOutcomeDto::Applied => "applied",
        ReceiptOutcomeDto::ReplayedIdempotent => "replayed_idempotent",
        ReceiptOutcomeDto::RevisionConflict => "revision_conflict",
        ReceiptOutcomeDto::Failed => "failed",
    }
}

fn outcome_from_str(raw: &str) -> ReceiptOutcomeDto {
    match raw {
        "applied" => ReceiptOutcomeDto::Applied,
        "replayed_idempotent" => ReceiptOutcomeDto::ReplayedIdempotent,
        "revision_conflict" => ReceiptOutcomeDto::RevisionConflict,
        _ => ReceiptOutcomeDto::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{Persistence, PersistencePool};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    fn request<'a>(
        idempotency_key: &'a str,
        expected_revision: Option<u64>,
    ) -> MutationRequest<'a> {
        MutationRequest {
            client_id: "test-client",
            entity_kind: "widget",
            entity_id: "w1",
            change_kind: "upsert",
            actor_id: "op_test",
            actor_kind: ActorKindDto::Operator,
            expected_revision,
            idempotency_key,
            correlation_id: Some("corr_test"),
            causation_id: None,
            before_json: None,
            after_json: Some("{\"v\":1}".to_string()),
            now_ms: 1_000,
        }
    }

    fn temp_state_dir(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bos-store-core-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn applied_mutation_bumps_revision_and_writes_receipt() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let outcome = mutate(persistence.connection(), request("idem_1", None), |tx| {
            tx.execute(
                "CREATE TABLE IF NOT EXISTS widgets (id TEXT PRIMARY KEY)",
                [],
            )?;
            tx.execute("INSERT INTO widgets (id) VALUES ('w1')", [])?;
            Ok(())
        })
        .expect("mutation");
        match outcome {
            MutationOutcome::Applied { revision, .. } => assert_eq!(revision, 1),
            other => panic!("expected Applied, got {other:?}"),
        }
        let receipts =
            receipts_for_entity(persistence.connection(), "test-client", "widget", "w1", 10)
                .expect("receipts");
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].outcome, ReceiptOutcomeDto::Applied);
    }

    #[test]
    fn idempotency_key_replays_without_reapplying() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        conn.execute("CREATE TABLE widgets (id TEXT PRIMARY KEY)", [])
            .expect("table");
        mutate(conn, request("idem_1", None), |tx| {
            tx.execute("INSERT INTO widgets (id) VALUES ('w1')", [])?;
            Ok(())
        })
        .expect("first");
        assert_eq!(
            applied_revision_for_idempotency(conn, "test-client", "idem_1")
                .expect("applied lookup"),
            Some(1)
        );
        assert_eq!(
            applied_revision_for_idempotency(conn, "test-client", "missing")
                .expect("missing lookup"),
            None
        );
        let outcome = mutate(conn, request("idem_1", None), |tx| {
            tx.execute("INSERT INTO widgets (id) VALUES ('w1')", [])?;
            Ok(())
        })
        .expect("replay");
        assert!(matches!(
            outcome,
            MutationOutcome::ReplayedIdempotent { .. }
        ));
        let count: i64 = persistence
            .connection()
            .query_row("SELECT COUNT(*) FROM widgets", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1, "domain write must not re-apply on replay");
    }

    #[test]
    fn revision_conflict_is_reported_and_audited() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        mutate(conn, request("idem_1", None), |_| Ok(())).expect("create");
        let outcome = mutate(conn, request("idem_2", Some(99)), |_| Ok(()))
            .expect("conflict path returns Ok");
        assert!(matches!(
            outcome,
            MutationOutcome::RevisionConflict {
                current_revision: Some(1),
                ..
            }
        ));
        let receipts =
            receipts_for_entity(persistence.connection(), "test-client", "widget", "w1", 10)
                .expect("receipts");
        assert_eq!(receipts[0].outcome, ReceiptOutcomeDto::RevisionConflict);
    }

    #[test]
    fn failed_domain_write_rolls_back_and_writes_failure_receipt() {
        let mut persistence = Persistence::open_in_memory().expect("db");
        let conn = persistence.connection();
        conn.execute("CREATE TABLE widgets (id TEXT PRIMARY KEY)", [])
            .expect("table");
        let result = mutate(conn, request("idem_1", None), |tx| {
            tx.execute("INSERT INTO widgets (id) VALUES ('w1')", [])?;
            Err(StoreError::Domain("validation failed".to_string()))
        });
        assert!(result.is_err());
        let conn = persistence.connection();
        let widget_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM widgets", [], |row| row.get(0))
            .expect("count");
        assert_eq!(widget_count, 0, "domain write must roll back");
        let receipts =
            receipts_for_entity(conn, "test-client", "widget", "w1", 10).expect("receipts");
        assert_eq!(receipts.len(), 1, "failure must still be audited");
        assert_eq!(receipts[0].outcome, ReceiptOutcomeDto::Failed);
        assert_eq!(receipts[0].error_class.as_deref(), Some("domain"));
    }

    #[test]
    fn pooled_wal_reads_continue_while_mutation_write_is_open() {
        let state_dir = temp_state_dir("wal-read");
        let pool = PersistencePool::open_at(&state_dir).expect("db");
        {
            let mut conn = pool.lock();
            conn.connection()
                .execute("CREATE TABLE wal_widgets (id TEXT PRIMARY KEY)", [])
                .expect("table");
        }

        let (write_started_tx, write_started_rx) = mpsc::channel();
        let writer_pool = pool.clone();
        let writer = std::thread::spawn(move || {
            let mut conn = writer_pool.lock();
            mutate_with_receipt_id(conn.connection(), request("idem_wal", None), |tx, _| {
                tx.execute("INSERT INTO wal_widgets (id) VALUES ('w1')", [])?;
                write_started_tx.send(()).expect("signal write started");
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .expect("write mutation");
        });

        write_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer reached open transaction");
        let reader_pool = pool.clone();
        let (read_tx, read_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let conn = reader_pool.lock();
            let count: i64 = conn
                .connection_ref()
                .query_row("SELECT COUNT(*) FROM wal_widgets", [], |row| row.get(0))
                .expect("read count");
            read_tx.send(count).expect("send count");
        });
        let count = read_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("read should not block behind open writer");
        assert_eq!(count, 0, "reader sees last committed snapshot");
        writer.join().expect("writer thread");
        std::fs::remove_dir_all(state_dir).ok();
    }

    #[test]
    fn concurrent_same_idempotency_key_replays_and_writes_once() {
        let state_dir = temp_state_dir("idem");
        let pool = PersistencePool::open_at(&state_dir).expect("db");
        {
            let mut conn = pool.lock();
            conn.connection()
                .execute("CREATE TABLE idem_widgets (id TEXT PRIMARY KEY)", [])
                .expect("table");
        }
        let barrier = Arc::new(Barrier::new(2));
        let domain_writes = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();

        for thread_id in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let domain_writes = domain_writes.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut conn = pool.lock();
                let outcome = mutate(conn.connection(), request("idem_concurrent", None), |tx| {
                    domain_writes.fetch_add(1, Ordering::SeqCst);
                    tx.execute(
                        "INSERT INTO idem_widgets (id) VALUES (?1)",
                        [format!("w{thread_id}")],
                    )?;
                    std::thread::sleep(Duration::from_millis(100));
                    Ok(())
                });
                tx.send(outcome).expect("send outcome");
            });
        }
        drop(tx);

        let mut outcomes = rx
            .iter()
            .take(2)
            .collect::<Result<Vec<_>, _>>()
            .expect("mutations should both return");
        outcomes.sort_by_key(|outcome| match outcome {
            MutationOutcome::Applied { .. } => 0,
            MutationOutcome::ReplayedIdempotent { .. } => 1,
            MutationOutcome::RevisionConflict { .. } => 2,
        });
        assert!(matches!(outcomes[0], MutationOutcome::Applied { .. }));
        assert!(matches!(
            outcomes[1],
            MutationOutcome::ReplayedIdempotent { .. }
        ));
        assert_eq!(domain_writes.load(Ordering::SeqCst), 1);

        let conn = pool.lock();
        let count: i64 = conn
            .connection_ref()
            .query_row("SELECT COUNT(*) FROM idem_widgets", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1, "domain write must execute once");
        std::fs::remove_dir_all(state_dir).ok();
    }

    #[test]
    fn concurrent_failed_mutations_are_each_audited() {
        let state_dir = temp_state_dir("failures");
        let pool = PersistencePool::open_at(&state_dir).expect("db");
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel();

        for thread_id in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut conn = pool.lock();
                let result = mutate(
                    conn.connection(),
                    MutationRequest {
                        entity_id: if thread_id == 0 { "w1" } else { "w2" },
                        idempotency_key: if thread_id == 0 {
                            "idem_fail_1"
                        } else {
                            "idem_fail_2"
                        },
                        ..request("unused", None)
                    },
                    |_| Err(StoreError::Domain("validation failed".to_string())),
                );
                tx.send(result).expect("send result");
            });
        }
        drop(tx);

        let results = rx.iter().take(2).collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(Result::is_err));

        let conn = pool.lock();
        let failed_count: i64 = conn
            .connection_ref()
            .query_row(
                "SELECT COUNT(*) FROM receipts WHERE outcome = 'failed'",
                [],
                |row| row.get(0),
            )
            .expect("failed receipt count");
        assert_eq!(failed_count, 2, "each failed mutation remains auditable");
        std::fs::remove_dir_all(state_dir).ok();
    }
}
