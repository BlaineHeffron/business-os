//! QBO persistence: one client-wide credential (tokens NEVER in receipts),
//! per-entity sync cursors, and snapshot caches with content-hash upserts.
//!
//! Receipt-quietness is load-bearing: the sync pump re-fetches boundary rows
//! every cycle (inclusive since-filter), so unchanged rows must be compared
//! BEFORE store_core::mutate and skipped entirely — a steady-state cycle
//! writes zero rows anywhere (no receipts, no timestamp churn).

use bos_contracts::receipt::ActorKindDto;
use bos_integrations::accounting_read::{
    BalanceSheetSummary, BillRecord, CustomerRecord, InvoiceRecord,
};
use bos_integrations::qbo_oauth::QboTokenGrant;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const CREDENTIAL_ENTITY_KIND: &str = "qbo_credential";
pub const CURSOR_ENTITY_KIND: &str = "accounting_sync_cursor";
pub const INVOICE_ENTITY_KIND: &str = "accounting_invoice_snapshot";
pub const BILL_ENTITY_KIND: &str = "accounting_bill_snapshot";
pub const CUSTOMER_ENTITY_KIND: &str = "accounting_customer_snapshot";
pub const BALANCE_SHEET_ENTITY_KIND: &str = "accounting_balance_sheet_snapshot";
pub const SYNC_ACTOR: &str = "accounting_sync_pump";

pub const ENTITY_INVOICE: &str = "invoice";
pub const ENTITY_BILL: &str = "bill";
pub const ENTITY_CUSTOMER: &str = "customer";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredQboCredential {
    pub realm_id: String,
    pub environment: String,
    pub refresh_token: String,
    pub refresh_token_expires_at_ms: u64,
    pub access_token: Option<String>,
    pub access_token_expires_at_ms: u64,
    pub connected_by_user_id: String,
    pub connected_at_ms: u64,
}

pub fn get_credential(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<StoredQboCredential>, StoreError> {
    let row = conn
        .query_row(
            "SELECT realm_id, environment, refresh_token, refresh_token_expires_at_ms, \
             access_token, access_token_expires_at_ms, connected_by_user_id, connected_at_ms \
             FROM qbo_credentials WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(StoredQboCredential {
                    realm_id: row.get(0)?,
                    environment: row.get(1)?,
                    refresh_token: row.get(2)?,
                    refresh_token_expires_at_ms: row.get::<_, i64>(3)? as u64,
                    access_token: row.get(4)?,
                    access_token_expires_at_ms: row.get::<_, i64>(5)? as u64,
                    connected_by_user_id: row.get(6)?,
                    connected_at_ms: row.get::<_, i64>(7)? as u64,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Store the credential from a fresh connect. Reconnecting to a DIFFERENT
/// realm wipes the snapshot caches + cursors in the same transaction — they
/// belong to another company's books.
pub fn store_credential(
    conn: &mut Connection,
    client_id: &str,
    realm_id: &str,
    environment: &str,
    grant: &QboTokenGrant,
    connected_by: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let previous_realm = get_credential(conn, client_id)?.map(|cred| cred.realm_id);
    let realm_changed = previous_realm
        .as_deref()
        .is_some_and(|previous| previous != realm_id);
    // Receipt payload: connection metadata only. Tokens never enter receipts.
    let after = serde_json::json!({
        "refresh_token": "[redacted]",
        "realm_id": realm_id,
        "environment": environment,
        "connected_by": connected_by,
        "previous_realm_wiped": realm_changed,
    })
    .to_string();
    let idempotency_key = format!("qbo_connect:{realm_id}:{now_ms}");
    let owned = (
        client_id.to_string(),
        realm_id.to_string(),
        environment.to_string(),
        grant.clone(),
        connected_by.to_string(),
    );
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CREDENTIAL_ENTITY_KIND,
            entity_id: "qbo",
            change_kind: "connect",
            actor_id: connected_by,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            let (client, realm, environment, grant, connected_by) = owned;
            if realm_changed {
                tx.execute(
                    "DELETE FROM accounting_invoice_snapshots WHERE client_id = ?1",
                    params![client],
                )?;
                tx.execute(
                    "DELETE FROM accounting_bill_snapshots WHERE client_id = ?1",
                    params![client],
                )?;
                tx.execute(
                    "DELETE FROM accounting_customer_snapshots WHERE client_id = ?1",
                    params![client],
                )?;
                tx.execute(
                    "DELETE FROM accounting_balance_sheet_snapshots WHERE client_id = ?1",
                    params![client],
                )?;
                tx.execute(
                    "DELETE FROM accounting_sync_cursors WHERE client_id = ?1",
                    params![client],
                )?;
            }
            tx.execute(
                "INSERT INTO qbo_credentials \
                 (client_id, realm_id, environment, refresh_token, refresh_token_expires_at_ms, \
                  access_token, access_token_expires_at_ms, connected_by_user_id, connected_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (client_id) DO UPDATE SET \
                   realm_id = excluded.realm_id, \
                   environment = excluded.environment, \
                   refresh_token = excluded.refresh_token, \
                   refresh_token_expires_at_ms = excluded.refresh_token_expires_at_ms, \
                   access_token = excluded.access_token, \
                   access_token_expires_at_ms = excluded.access_token_expires_at_ms, \
                   connected_by_user_id = excluded.connected_by_user_id, \
                   connected_at_ms = excluded.connected_at_ms",
                params![
                    client,
                    realm,
                    environment,
                    grant.refresh_token,
                    grant.refresh_token_expires_at_ms as i64,
                    grant.access_token,
                    grant.access_token_expires_at_ms as i64,
                    connected_by,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

/// Persist a rotated grant. MUST be called immediately after every refresh —
/// the old refresh token dies ~24h after rotation.
pub fn update_tokens_after_refresh(
    conn: &mut Connection,
    client_id: &str,
    grant: &QboTokenGrant,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::json!({
        "refresh_token": "[redacted]",
        "access_token": "[redacted]",
        "refresh_token_expires_at_ms": grant.refresh_token_expires_at_ms,
    })
    .to_string();
    let idempotency_key = format!("qbo_token_refresh:{now_ms}");
    let owned_client = client_id.to_string();
    let owned_grant = grant.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CREDENTIAL_ENTITY_KIND,
            entity_id: "qbo",
            change_kind: "token_refresh",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE qbo_credentials SET \
                   refresh_token = ?2, refresh_token_expires_at_ms = ?3, \
                   access_token = ?4, access_token_expires_at_ms = ?5, \
                   last_refreshed_at_ms = ?6 \
                 WHERE client_id = ?1",
                params![
                    owned_client,
                    owned_grant.refresh_token,
                    owned_grant.refresh_token_expires_at_ms as i64,
                    owned_grant.access_token,
                    owned_grant.access_token_expires_at_ms as i64,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

/// Delete the credential; with `purge`, also drop every cached snapshot and
/// cursor row in the SAME receipted transaction (the operator is walking
/// away from these books — e.g. a sandbox test company).
pub fn delete_credential(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    purge: bool,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let idempotency_key = format!("qbo_disconnect:{now_ms}");
    let after = serde_json::json!({ "purged": purge }).to_string();
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CREDENTIAL_ENTITY_KIND,
            entity_id: "qbo",
            change_kind: "disconnect",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM qbo_credentials WHERE client_id = ?1",
                params![owned_client],
            )?;
            if purge {
                for table in [
                    "accounting_invoice_snapshots",
                    "accounting_bill_snapshots",
                    "accounting_customer_snapshots",
                    "accounting_pnl_snapshots",
                    "accounting_balance_sheet_snapshots",
                    "accounting_sync_cursors",
                ] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE client_id = ?1"),
                        params![owned_client],
                    )?;
                }
            }
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QboSyncCursor {
    pub high_water_updated_at: Option<String>,
    /// Since-filter pinned for the IN-PROGRESS walk (changing it mid-walk
    /// would shift STARTPOSITION semantics). None during initial backfill.
    pub walk_since: Option<String>,
    pub walk_max_updated_at: Option<String>,
    pub next_start_position: u32,
    pub backfill_complete: bool,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_advanced_at_ms: Option<u64>,
}

impl QboSyncCursor {
    pub fn initial() -> Self {
        Self {
            next_start_position: 1,
            ..Self::default()
        }
    }
}

pub fn get_cursor(
    conn: &Connection,
    client_id: &str,
    entity: &str,
) -> Result<QboSyncCursor, StoreError> {
    let row = conn
        .query_row(
            "SELECT high_water_updated_at, walk_since, walk_max_updated_at, \
             next_start_position, backfill_complete, rate_limited_until_ms, last_error, \
             last_advanced_at_ms \
             FROM accounting_sync_cursors WHERE client_id = ?1 AND entity = ?2",
            params![client_id, entity],
            |row| {
                Ok(QboSyncCursor {
                    high_water_updated_at: row.get(0)?,
                    walk_since: row.get(1)?,
                    walk_max_updated_at: row.get(2)?,
                    next_start_position: row.get::<_, i64>(3)? as u32,
                    backfill_complete: row.get(4)?,
                    rate_limited_until_ms: row.get::<_, i64>(5)? as u64,
                    last_error: row.get(6)?,
                    last_advanced_at_ms: row.get::<_, Option<i64>>(7)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_else(QboSyncCursor::initial))
}

/// Compare-before-write cursor persistence: returns false (and writes
/// nothing, not even a receipt) when the cursor is unchanged.
pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    entity: &str,
    cursor: &QboSyncCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_cursor(conn, client_id, entity)?;
    if current == *cursor {
        return Ok(false);
    }
    let content = snapshot_hash(&[
        cursor.high_water_updated_at.as_deref().unwrap_or(""),
        cursor.walk_since.as_deref().unwrap_or(""),
        cursor.walk_max_updated_at.as_deref().unwrap_or(""),
        &cursor.next_start_position.to_string(),
        &(cursor.backfill_complete as u8).to_string(),
        &cursor.rate_limited_until_ms.to_string(),
        cursor.last_error.as_deref().unwrap_or(""),
    ]);
    let idempotency_key = format!("accounting_cursor:{entity}:{content}");
    let after = serde_json::json!({
        "entity": entity,
        "high_water_updated_at": cursor.high_water_updated_at,
        "next_start_position": cursor.next_start_position,
        "backfill_complete": cursor.backfill_complete,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_entity = entity.to_string();
    let owned_cursor = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CURSOR_ENTITY_KIND,
            entity_id: entity,
            change_kind: "advance",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO accounting_sync_cursors \
                 (client_id, entity, high_water_updated_at, walk_since, walk_max_updated_at, \
                  next_start_position, backfill_complete, rate_limited_until_ms, last_error, \
                  last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT (client_id, entity) DO UPDATE SET \
                   high_water_updated_at = excluded.high_water_updated_at, \
                   walk_since = excluded.walk_since, \
                   walk_max_updated_at = excluded.walk_max_updated_at, \
                   next_start_position = excluded.next_start_position, \
                   backfill_complete = excluded.backfill_complete, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client,
                    owned_entity,
                    owned_cursor.high_water_updated_at,
                    owned_cursor.walk_since,
                    owned_cursor.walk_max_updated_at,
                    owned_cursor.next_start_position as i64,
                    owned_cursor.backfill_complete,
                    owned_cursor.rate_limited_until_ms as i64,
                    owned_cursor.last_error,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpsertSummary {
    pub written: usize,
    pub unchanged: usize,
}

fn snapshot_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn invoice_hash(record: &InvoiceRecord) -> String {
    snapshot_hash(&[
        record.doc_number.as_deref().unwrap_or(""),
        record.customer_id.as_deref().unwrap_or(""),
        record.customer_name.as_deref().unwrap_or(""),
        record.txn_date.as_deref().unwrap_or(""),
        record.due_date.as_deref().unwrap_or(""),
        &record.total_amt_cents.to_string(),
        &record.balance_cents.to_string(),
        &(record.voided as u8).to_string(),
    ])
}

fn bill_hash(record: &BillRecord) -> String {
    snapshot_hash(&[
        record.vendor_id.as_deref().unwrap_or(""),
        record.vendor_name.as_deref().unwrap_or(""),
        record.txn_date.as_deref().unwrap_or(""),
        record.due_date.as_deref().unwrap_or(""),
        &record.total_amt_cents.to_string(),
        &record.balance_cents.to_string(),
        &(record.voided as u8).to_string(),
    ])
}

fn customer_hash(record: &CustomerRecord) -> String {
    snapshot_hash(&[
        &record.display_name,
        record.company_name.as_deref().unwrap_or(""),
        record.email.as_deref().unwrap_or(""),
        record.phone.as_deref().unwrap_or(""),
        &(record.active as u8).to_string(),
        record.tier_raw.as_deref().unwrap_or(""),
        record.tier_source.as_str(),
    ])
}

fn existing_hash(
    conn: &Connection,
    table: &str,
    id_column: &str,
    client_id: &str,
    id: &str,
) -> Result<Option<(String, u64)>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT content_hash, first_seen_at_ms FROM {table} \
                 WHERE client_id = ?1 AND {id_column} = ?2"
            ),
            params![client_id, id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
        )
        .optional()?;
    Ok(row)
}

/// Content-hash upsert: unchanged rows are skipped BEFORE mutate (zero
/// receipts); changed/new rows write one receipted mutation each with a
/// content-derived idempotency key (crash replays stay quiet).
pub fn upsert_invoice_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[InvoiceRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = invoice_hash(record);
        let existing = existing_hash(
            conn,
            "accounting_invoice_snapshots",
            "provider_invoice_id",
            client_id,
            &record.invoice_id,
        )?;
        if existing
            .as_ref()
            .is_some_and(|(current, _)| *current == hash)
        {
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
        let idempotency_key = format!("accounting_sync:invoice:{}:{hash}", record.invoice_id);
        let after = serde_json::json!({
            "doc_number": record.doc_number,
            "customer_name": record.customer_name,
            "txn_date": record.txn_date,
            "due_date": record.due_date,
            "total_amt_cents": record.total_amt_cents,
            "balance_cents": record.balance_cents,
            "voided": record.voided,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned_record = record.clone();
        let owned_hash = hash.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: INVOICE_ENTITY_KIND,
                entity_id: &record.invoice_id,
                change_kind: "sync_upsert",
                actor_id: SYNC_ACTOR,
                actor_kind: ActorKindDto::System,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: Some(after),
                now_ms,
            },
            move |tx| {
                tx.execute(
                    "INSERT INTO accounting_invoice_snapshots \
                     (client_id, provider_invoice_id, doc_number, customer_id, customer_name, \
                      txn_date, due_date, total_amt_cents, balance_cents, voided, \
                      provider_updated_at, content_hash, first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                     ON CONFLICT (client_id, provider_invoice_id) DO UPDATE SET \
                       doc_number = excluded.doc_number, \
                       customer_id = excluded.customer_id, \
                       customer_name = excluded.customer_name, \
                       txn_date = excluded.txn_date, \
                       due_date = excluded.due_date, \
                       total_amt_cents = excluded.total_amt_cents, \
                       balance_cents = excluded.balance_cents, \
                       voided = excluded.voided, \
                       provider_updated_at = excluded.provider_updated_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned_record.invoice_id,
                        owned_record.doc_number,
                        owned_record.customer_id,
                        owned_record.customer_name,
                        owned_record.txn_date,
                        owned_record.due_date,
                        owned_record.total_amt_cents,
                        owned_record.balance_cents,
                        owned_record.voided,
                        owned_record.updated_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        summary.written += 1;
    }
    Ok(summary)
}

pub fn upsert_bill_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[BillRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = bill_hash(record);
        let existing = existing_hash(
            conn,
            "accounting_bill_snapshots",
            "provider_bill_id",
            client_id,
            &record.bill_id,
        )?;
        if existing
            .as_ref()
            .is_some_and(|(current, _)| *current == hash)
        {
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
        let idempotency_key = format!("accounting_sync:bill:{}:{hash}", record.bill_id);
        let after = serde_json::json!({
            "vendor_name": record.vendor_name,
            "txn_date": record.txn_date,
            "due_date": record.due_date,
            "total_amt_cents": record.total_amt_cents,
            "balance_cents": record.balance_cents,
            "voided": record.voided,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned_record = record.clone();
        let owned_hash = hash.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: BILL_ENTITY_KIND,
                entity_id: &record.bill_id,
                change_kind: "sync_upsert",
                actor_id: SYNC_ACTOR,
                actor_kind: ActorKindDto::System,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: Some(after),
                now_ms,
            },
            move |tx| {
                tx.execute(
                    "INSERT INTO accounting_bill_snapshots \
                     (client_id, provider_bill_id, vendor_id, vendor_name, txn_date, due_date, \
                      total_amt_cents, balance_cents, voided, provider_updated_at, content_hash, \
                      first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                     ON CONFLICT (client_id, provider_bill_id) DO UPDATE SET \
                       vendor_id = excluded.vendor_id, \
                       vendor_name = excluded.vendor_name, \
                       txn_date = excluded.txn_date, \
                       due_date = excluded.due_date, \
                       total_amt_cents = excluded.total_amt_cents, \
                       balance_cents = excluded.balance_cents, \
                       voided = excluded.voided, \
                       provider_updated_at = excluded.provider_updated_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned_record.bill_id,
                        owned_record.vendor_id,
                        owned_record.vendor_name,
                        owned_record.txn_date,
                        owned_record.due_date,
                        owned_record.total_amt_cents,
                        owned_record.balance_cents,
                        owned_record.voided,
                        owned_record.updated_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        summary.written += 1;
    }
    Ok(summary)
}

pub fn upsert_customer_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[CustomerRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = customer_hash(record);
        let existing = existing_hash(
            conn,
            "accounting_customer_snapshots",
            "provider_customer_id",
            client_id,
            &record.customer_id,
        )?;
        if existing
            .as_ref()
            .is_some_and(|(current, _)| *current == hash)
        {
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
        let idempotency_key = format!("accounting_sync:customer:{}:{hash}", record.customer_id);
        let after = serde_json::json!({
            "display_name": record.display_name,
            "company_name": record.company_name,
            "email": record.email,
            "active": record.active,
            "tier": record.tier_raw,
            "tier_source": record.tier_source.as_str(),
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned_record = record.clone();
        let owned_hash = hash.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: CUSTOMER_ENTITY_KIND,
                entity_id: &record.customer_id,
                change_kind: "sync_upsert",
                actor_id: SYNC_ACTOR,
                actor_kind: ActorKindDto::System,
                expected_revision: None,
                idempotency_key: &idempotency_key,
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: Some(after),
                now_ms,
            },
            move |tx| {
                tx.execute(
                    "INSERT INTO accounting_customer_snapshots \
                     (client_id, provider_customer_id, display_name, company_name, email, phone, \
                      active, tier, tier_source, provider_updated_at, content_hash, \
                      first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                     ON CONFLICT (client_id, provider_customer_id) DO UPDATE SET \
                       display_name = excluded.display_name, \
                       company_name = excluded.company_name, \
                       email = excluded.email, \
                       phone = excluded.phone, \
                       active = excluded.active, \
                       tier = excluded.tier, \
                       tier_source = excluded.tier_source, \
                       provider_updated_at = excluded.provider_updated_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned_record.customer_id,
                        owned_record.display_name,
                        owned_record.company_name,
                        owned_record.email,
                        owned_record.phone,
                        owned_record.active,
                        owned_record.tier_raw,
                        owned_record.tier_source.as_str(),
                        owned_record.updated_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        summary.written += 1;
    }
    Ok(summary)
}

/// A cached invoice row as read back for the views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceSnapshotRow {
    pub invoice_id: String,
    pub doc_number: Option<String>,
    pub customer_name: Option<String>,
    pub txn_date: Option<String>,
    pub due_date: Option<String>,
    pub total_amt_cents: i64,
    pub balance_cents: i64,
    pub voided: bool,
}

/// A cached bill row as read back for A/P views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillSnapshotRow {
    pub bill_id: String,
    pub vendor_name: Option<String>,
    pub txn_date: Option<String>,
    pub due_date: Option<String>,
    pub total_amt_cents: i64,
    pub balance_cents: i64,
    pub voided: bool,
}

pub fn list_bills(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<BillSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_bill_id, vendor_name, txn_date, due_date, \
         total_amt_cents, balance_cents, voided \
         FROM accounting_bill_snapshots WHERE client_id = ?1 \
         ORDER BY txn_date DESC, provider_bill_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        Ok(BillSnapshotRow {
            bill_id: row.get(0)?,
            vendor_name: row.get(1)?,
            txn_date: row.get(2)?,
            due_date: row.get(3)?,
            total_amt_cents: row.get(4)?,
            balance_cents: row.get(5)?,
            voided: row.get(6)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn list_invoices(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<InvoiceSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_invoice_id, doc_number, customer_name, txn_date, due_date, \
         total_amt_cents, balance_cents, voided \
         FROM accounting_invoice_snapshots WHERE client_id = ?1 \
         ORDER BY txn_date DESC, provider_invoice_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![client_id, limit as i64], |row| {
        Ok(InvoiceSnapshotRow {
            invoice_id: row.get(0)?,
            doc_number: row.get(1)?,
            customer_name: row.get(2)?,
            txn_date: row.get(3)?,
            due_date: row.get(4)?,
            total_amt_cents: row.get(5)?,
            balance_cents: row.get(6)?,
            voided: row.get(7)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// An open-invoice match for the QBO payment arm's amount-must-match
/// validation: non-voided invoices whose outstanding balance equals the
/// payment amount exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceBalanceMatch {
    pub invoice_id: String,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub doc_number: Option<String>,
    pub customer_active: Option<bool>,
}

pub fn open_invoices_by_balance(
    conn: &Connection,
    client_id: &str,
    balance_cents: i64,
) -> Result<Vec<InvoiceBalanceMatch>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT i.provider_invoice_id, i.customer_id, i.customer_name, i.doc_number, c.active \
         FROM accounting_invoice_snapshots i \
         LEFT JOIN accounting_customer_snapshots c \
           ON c.client_id = i.client_id AND c.provider_customer_id = i.customer_id \
         WHERE i.client_id = ?1 AND i.voided = 0 AND i.balance_cents = ?2 \
         ORDER BY i.provider_invoice_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id, balance_cents], |row| {
        Ok(InvoiceBalanceMatch {
            invoice_id: row.get(0)?,
            customer_id: row.get(1)?,
            customer_name: row.get(2)?,
            doc_number: row.get(3)?,
            customer_active: row.get(4)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerSnapshotRow {
    pub customer_id: String,
    pub display_name: String,
    pub company_name: Option<String>,
    pub email: Option<String>,
    pub tier: Option<String>,
    pub active: bool,
}

pub fn list_customers(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<CustomerSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_customer_id, display_name, company_name, email, tier, active \
         FROM accounting_customer_snapshots WHERE client_id = ?1 \
         ORDER BY active DESC, display_name COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(CustomerSnapshotRow {
            customer_id: row.get(0)?,
            display_name: row.get(1)?,
            company_name: row.get(2)?,
            email: row.get(3)?,
            tier: row.get(4)?,
            active: row.get(5)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn snapshot_counts(conn: &Connection, client_id: &str) -> Result<(u64, u64), StoreError> {
    let invoices: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounting_invoice_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    let customers: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounting_customer_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok((invoices as u64, customers as u64))
}

pub fn bill_snapshot_count(conn: &Connection, client_id: &str) -> Result<u64, StoreError> {
    let bills: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounting_bill_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(bills as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceSheetSnapshotRow {
    pub as_of_date: String,
    pub cash_on_hand_cents: i64,
}

pub fn get_latest_balance_sheet_snapshot(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<BalanceSheetSnapshotRow>, StoreError> {
    let row = conn
        .query_row(
            "SELECT as_of_date, cash_on_hand_cents \
             FROM accounting_balance_sheet_snapshots WHERE client_id = ?1 \
             ORDER BY as_of_date DESC LIMIT 1",
            params![client_id],
            |row| {
                Ok(BalanceSheetSnapshotRow {
                    as_of_date: row.get(0)?,
                    cash_on_hand_cents: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub fn upsert_balance_sheet_snapshot(
    conn: &mut Connection,
    client_id: &str,
    as_of_date: &str,
    summary: BalanceSheetSummary,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let hash = snapshot_hash(&[as_of_date, &summary.cash_on_hand_cents.to_string()]);
    let existing = conn
        .query_row(
            "SELECT content_hash, first_seen_at_ms FROM accounting_balance_sheet_snapshots \
             WHERE client_id = ?1 AND as_of_date = ?2",
            params![client_id, as_of_date],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(current, _)| *current == hash)
    {
        return Ok(false);
    }
    let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
    let entity_id = format!("balance_sheet:{as_of_date}");
    let idempotency_key = format!("accounting_sync:{entity_id}:{hash}");
    let after = serde_json::json!({
        "as_of_date": as_of_date,
        "cash_on_hand_cents": summary.cash_on_hand_cents,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_date = as_of_date.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: BALANCE_SHEET_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "sync_upsert",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO accounting_balance_sheet_snapshots \
                 (client_id, as_of_date, cash_on_hand_cents, content_hash, \
                  first_seen_at_ms, last_written_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (client_id, as_of_date) DO UPDATE SET \
                   cash_on_hand_cents = excluded.cash_on_hand_cents, \
                   content_hash = excluded.content_hash, \
                   last_written_at_ms = excluded.last_written_at_ms",
                params![
                    owned_client,
                    owned_date,
                    summary.cash_on_hand_cents,
                    hash,
                    first_seen as i64,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

pub const PNL_ENTITY_KIND: &str = "accounting_pnl_snapshot";

/// One cached P&L period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnlSnapshotRow {
    pub period_kind: String,
    pub period_start: String,
    pub period_end: String,
    pub total_income_cents: i64,
    pub total_cogs_cents: i64,
    pub gross_profit_cents: i64,
    pub is_complete: bool,
}

pub fn get_pnl_snapshot(
    conn: &Connection,
    client_id: &str,
    period_kind: &str,
    period_start: &str,
) -> Result<Option<PnlSnapshotRow>, StoreError> {
    let row = conn
        .query_row(
            "SELECT period_kind, period_start, period_end, total_income_cents, \
             total_cogs_cents, gross_profit_cents, is_complete \
             FROM accounting_pnl_snapshots \
             WHERE client_id = ?1 AND period_kind = ?2 AND period_start = ?3",
            params![client_id, period_kind, period_start],
            pnl_from_row,
        )
        .optional()?;
    Ok(row)
}

pub fn list_pnl_snapshots(
    conn: &Connection,
    client_id: &str,
    period_kind: &str,
) -> Result<Vec<PnlSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT period_kind, period_start, period_end, total_income_cents, \
         total_cogs_cents, gross_profit_cents, is_complete \
         FROM accounting_pnl_snapshots WHERE client_id = ?1 AND period_kind = ?2 \
         ORDER BY period_start ASC",
    )?;
    let rows = stmt.query_map(params![client_id, period_kind], pnl_from_row)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

fn pnl_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PnlSnapshotRow> {
    Ok(PnlSnapshotRow {
        period_kind: row.get(0)?,
        period_start: row.get(1)?,
        period_end: row.get(2)?,
        total_income_cents: row.get(3)?,
        total_cogs_cents: row.get(4)?,
        gross_profit_cents: row.get(5)?,
        is_complete: row.get(6)?,
    })
}

/// Content-hash upsert for one P&L period: unchanged totals write nothing
/// (no receipt). Returns true when a write happened.
pub fn upsert_pnl_snapshot(
    conn: &mut Connection,
    client_id: &str,
    snapshot: &PnlSnapshotRow,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let hash = snapshot_hash(&[
        &snapshot.period_end,
        &snapshot.total_income_cents.to_string(),
        &snapshot.total_cogs_cents.to_string(),
        &snapshot.gross_profit_cents.to_string(),
        &(snapshot.is_complete as u8).to_string(),
    ]);
    let entity_id = format!("{}:{}", snapshot.period_kind, snapshot.period_start);
    let existing = conn
        .query_row(
            "SELECT content_hash, first_seen_at_ms FROM accounting_pnl_snapshots \
             WHERE client_id = ?1 AND period_kind = ?2 AND period_start = ?3",
            params![client_id, snapshot.period_kind, snapshot.period_start],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(current, _)| *current == hash)
    {
        return Ok(false);
    }
    let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
    let idempotency_key = format!("accounting_sync:pnl:{entity_id}:{hash}");
    let after = serde_json::json!({
        "period_kind": snapshot.period_kind,
        "period_start": snapshot.period_start,
        "period_end": snapshot.period_end,
        "total_income_cents": snapshot.total_income_cents,
        "total_cogs_cents": snapshot.total_cogs_cents,
        "gross_profit_cents": snapshot.gross_profit_cents,
        "is_complete": snapshot.is_complete,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned = snapshot.clone();
    let owned_hash = hash.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: PNL_ENTITY_KIND,
            entity_id: &entity_id,
            change_kind: "sync_upsert",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO accounting_pnl_snapshots \
                 (client_id, period_kind, period_start, period_end, total_income_cents, \
                  total_cogs_cents, gross_profit_cents, is_complete, content_hash, \
                  first_seen_at_ms, last_written_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (client_id, period_kind, period_start) DO UPDATE SET \
                   period_end = excluded.period_end, \
                   total_income_cents = excluded.total_income_cents, \
                   total_cogs_cents = excluded.total_cogs_cents, \
                   gross_profit_cents = excluded.gross_profit_cents, \
                   is_complete = excluded.is_complete, \
                   content_hash = excluded.content_hash, \
                   last_written_at_ms = excluded.last_written_at_ms",
                params![
                    owned_client,
                    owned.period_kind,
                    owned.period_start,
                    owned.period_end,
                    owned.total_income_cents,
                    owned.total_cogs_cents,
                    owned.gross_profit_cents,
                    owned.is_complete,
                    owned_hash,
                    first_seen as i64,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// Period starts of COMPLETE cached periods (the sync skips re-fetching them).
pub fn complete_pnl_period_starts(
    conn: &Connection,
    client_id: &str,
) -> Result<std::collections::HashSet<(String, String)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT period_kind, period_start FROM accounting_pnl_snapshots \
         WHERE client_id = ?1 AND is_complete = 1",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut found = std::collections::HashSet::new();
    for row in rows {
        found.insert(row?);
    }
    Ok(found)
}
