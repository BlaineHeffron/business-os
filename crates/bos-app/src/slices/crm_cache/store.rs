//! CRM cache persistence: content-hash snapshot upserts plus sync cursors.
//! Raw SQL for this slice lives here and in the migration only.

use bos_contracts::receipt::ActorKindDto;
use bos_integrations::crm_read::{CrmContactRecord, CrmDealRecord};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationRequest, StoreError};

pub const CONTACT_ENTITY_KIND: &str = "crm_contact_snapshot";
pub const DEAL_ENTITY_KIND: &str = "crm_deal_snapshot";
pub const CURSOR_ENTITY_KIND: &str = "crm_cache_sync_cursor";
pub const SYNC_ACTOR: &str = "crm_cache_sync_pump";

pub const ENTITY_CONTACT: &str = "contact";
pub const ENTITY_DEAL: &str = "deal";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpsertSummary {
    pub written: usize,
    pub unchanged: usize,
    pub tombstoned: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CrmSyncCursor {
    pub next_after_cursor: Option<String>,
    pub high_water_modified_at: Option<String>,
    pub backfill_complete: bool,
    pub sync_started_at_ms: Option<u64>,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_advanced_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExistingSnapshot {
    content_hash: String,
    first_seen_at_ms: u64,
    last_seen_at_ms: u64,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactSnapshotRow {
    pub provider_contact_id: String,
    pub email: Option<String>,
    pub full_name: Option<String>,
    pub company: Option<String>,
    pub phone: Option<String>,
    pub lifecycle_stage: Option<String>,
    pub owner: Option<String>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DealSnapshotRow {
    pub provider_deal_id: String,
    pub deal_name: Option<String>,
    pub stage: Option<String>,
    pub amount_cents: Option<i64>,
    pub currency: Option<String>,
    pub pipeline: Option<String>,
    pub close_date: Option<String>,
    pub associated_contact_email: Option<String>,
    pub associated_company: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCounts {
    pub contacts: u64,
    pub deals: u64,
    pub last_synced_at_ms: Option<u64>,
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

fn contact_hash(record: &CrmContactRecord) -> String {
    snapshot_hash(&[
        record.email.as_deref().unwrap_or(""),
        record.name.as_deref().unwrap_or(""),
        record.company.as_deref().unwrap_or(""),
        record.phone.as_deref().unwrap_or(""),
        record.lifecycle_stage.as_deref().unwrap_or(""),
        record.owner.as_deref().unwrap_or(""),
        record.last_activity_at.as_deref().unwrap_or(""),
    ])
}

fn deal_hash(record: &CrmDealRecord) -> String {
    snapshot_hash(&[
        record.name.as_deref().unwrap_or(""),
        record.stage.as_deref().unwrap_or(""),
        &record
            .amount_cents
            .map(|v| v.to_string())
            .unwrap_or_default(),
        record.currency.as_deref().unwrap_or(""),
        record.pipeline.as_deref().unwrap_or(""),
        record.close_date.as_deref().unwrap_or(""),
        record.associated_contact_email.as_deref().unwrap_or(""),
        record.associated_contact_company.as_deref().unwrap_or(""),
    ])
}

fn existing_hash(
    conn: &Connection,
    table: &str,
    id_column: &str,
    client_id: &str,
    id: &str,
) -> Result<Option<ExistingSnapshot>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT content_hash, first_seen_at_ms, last_seen_at_ms, active FROM {table} \
                 WHERE client_id = ?1 AND {id_column} = ?2"
            ),
            params![client_id, id],
            |row| {
                Ok(ExistingSnapshot {
                    content_hash: row.get(0)?,
                    first_seen_at_ms: row.get::<_, i64>(1)? as u64,
                    last_seen_at_ms: row.get::<_, i64>(2)? as u64,
                    active: row.get::<_, bool>(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn maybe_mark_contact_seen(
    conn: &mut Connection,
    client_id: &str,
    provider_contact_id: &str,
    existing: &ExistingSnapshot,
    seen_at_ms: u64,
) -> Result<bool, StoreError> {
    if existing.active && existing.last_seen_at_ms >= seen_at_ms {
        return Ok(false);
    }
    let idempotency_key = format!("crm_cache_seen:contact:{provider_contact_id}:{seen_at_ms}");
    let after = serde_json::json!({
        "active": true,
        "last_seen_at_ms": seen_at_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_id = provider_contact_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CONTACT_ENTITY_KIND,
            entity_id: provider_contact_id,
            change_kind: "sync_seen",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: seen_at_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE crm_contact_snapshots SET active = 1, last_seen_at_ms = ?3 \
                 WHERE client_id = ?1 AND provider_contact_id = ?2",
                params![owned_client, owned_id, seen_at_ms as i64],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

fn maybe_mark_deal_seen(
    conn: &mut Connection,
    client_id: &str,
    provider_deal_id: &str,
    existing: &ExistingSnapshot,
    seen_at_ms: u64,
) -> Result<bool, StoreError> {
    if existing.active && existing.last_seen_at_ms >= seen_at_ms {
        return Ok(false);
    }
    let idempotency_key = format!("crm_cache_seen:deal:{provider_deal_id}:{seen_at_ms}");
    let after = serde_json::json!({
        "active": true,
        "last_seen_at_ms": seen_at_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_id = provider_deal_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DEAL_ENTITY_KIND,
            entity_id: provider_deal_id,
            change_kind: "sync_seen",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: seen_at_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE crm_deal_snapshots SET active = 1, last_seen_at_ms = ?3 \
                 WHERE client_id = ?1 AND provider_deal_id = ?2",
                params![owned_client, owned_id, seen_at_ms as i64],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

pub fn upsert_contact_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[CrmContactRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = contact_hash(record);
        let existing = existing_hash(
            conn,
            "crm_contact_snapshots",
            "provider_contact_id",
            client_id,
            &record.provider_contact_id,
        )?;
        if let Some(existing) = existing
            .as_ref()
            .filter(|current| current.content_hash == hash)
        {
            if maybe_mark_contact_seen(
                conn,
                client_id,
                &record.provider_contact_id,
                existing,
                now_ms,
            )? {
                summary.written += 1;
            }
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing
            .as_ref()
            .map(|state| state.first_seen_at_ms)
            .unwrap_or(now_ms);
        let previous_hash = existing
            .as_ref()
            .map(|state| state.content_hash.as_str())
            .unwrap_or("none");
        let idempotency_key = format!(
            "crm_cache_sync:contact:{}:{previous_hash}:{hash}",
            record.provider_contact_id
        );
        let after = serde_json::json!({
            "email": record.email,
            "full_name": record.name,
            "company": record.company,
            "lifecycle_stage": record.lifecycle_stage,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: CONTACT_ENTITY_KIND,
                entity_id: &record.provider_contact_id,
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
                    "INSERT INTO crm_contact_snapshots \
                     (client_id, provider_contact_id, email, full_name, company, phone, \
                      lifecycle_stage, owner, last_activity_at, active, content_hash, \
                      first_seen_at_ms, last_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?13) \
                     ON CONFLICT (client_id, provider_contact_id) DO UPDATE SET \
                       email = excluded.email, full_name = excluded.full_name, \
                       company = excluded.company, phone = excluded.phone, \
                       lifecycle_stage = excluded.lifecycle_stage, owner = excluded.owner, \
                       last_activity_at = excluded.last_activity_at, active = excluded.active, \
                       content_hash = excluded.content_hash, \
                       last_seen_at_ms = excluded.last_seen_at_ms, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.provider_contact_id,
                        owned.email,
                        owned.name,
                        owned.company,
                        owned.phone,
                        owned.lifecycle_stage,
                        owned.owner,
                        owned.last_activity_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
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

pub fn upsert_deal_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[CrmDealRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = deal_hash(record);
        let existing = existing_hash(
            conn,
            "crm_deal_snapshots",
            "provider_deal_id",
            client_id,
            &record.provider_deal_id,
        )?;
        if let Some(existing) = existing
            .as_ref()
            .filter(|current| current.content_hash == hash)
        {
            if maybe_mark_deal_seen(conn, client_id, &record.provider_deal_id, existing, now_ms)? {
                summary.written += 1;
            }
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing
            .as_ref()
            .map(|state| state.first_seen_at_ms)
            .unwrap_or(now_ms);
        let previous_hash = existing
            .as_ref()
            .map(|state| state.content_hash.as_str())
            .unwrap_or("none");
        let idempotency_key = format!(
            "crm_cache_sync:deal:{}:{previous_hash}:{hash}",
            record.provider_deal_id
        );
        let after = serde_json::json!({
            "deal_name": record.name,
            "stage": record.stage,
            "amount_cents": record.amount_cents,
            "currency": record.currency,
            "pipeline": record.pipeline,
            "close_date": record.close_date,
            "associated_contact_email": record.associated_contact_email,
            "associated_company": record.associated_contact_company,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: DEAL_ENTITY_KIND,
                entity_id: &record.provider_deal_id,
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
                    "INSERT INTO crm_deal_snapshots \
                     (client_id, provider_deal_id, deal_name, stage, amount_cents, currency, \
                      pipeline, close_date, associated_contact_email, associated_company, active, \
                      content_hash, first_seen_at_ms, last_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12, ?13, ?14) \
                     ON CONFLICT (client_id, provider_deal_id) DO UPDATE SET \
                       deal_name = excluded.deal_name, stage = excluded.stage, \
                       amount_cents = excluded.amount_cents, currency = excluded.currency, \
                       pipeline = excluded.pipeline, close_date = excluded.close_date, \
                       associated_contact_email = excluded.associated_contact_email, \
                       associated_company = excluded.associated_company, active = excluded.active, \
                       content_hash = excluded.content_hash, \
                       last_seen_at_ms = excluded.last_seen_at_ms, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.provider_deal_id,
                        owned.name,
                        owned.stage,
                        owned.amount_cents,
                        owned.currency,
                        owned.pipeline,
                        owned.close_date,
                        owned.associated_contact_email,
                        owned.associated_contact_company,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
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

pub fn get_cursor(
    conn: &Connection,
    client_id: &str,
    entity: &str,
) -> Result<CrmSyncCursor, StoreError> {
    let row = conn
        .query_row(
            "SELECT next_after_cursor, high_water_modified_at, backfill_complete, \
             sync_started_at_ms, rate_limited_until_ms, last_error, last_advanced_at_ms \
             FROM crm_cache_sync_cursors WHERE client_id = ?1 AND entity = ?2",
            params![client_id, entity],
            |row| {
                Ok(CrmSyncCursor {
                    next_after_cursor: row.get(0)?,
                    high_water_modified_at: row.get(1)?,
                    backfill_complete: row.get(2)?,
                    sync_started_at_ms: row.get::<_, Option<i64>>(3)?.map(|ms| ms as u64),
                    rate_limited_until_ms: row.get::<_, i64>(4)? as u64,
                    last_error: row.get(5)?,
                    last_advanced_at_ms: row.get::<_, Option<i64>>(6)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    entity: &str,
    cursor: &CrmSyncCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_cursor(conn, client_id, entity)?;
    if current == *cursor {
        return Ok(false);
    }
    let content = snapshot_hash(&[
        cursor.next_after_cursor.as_deref().unwrap_or(""),
        cursor.high_water_modified_at.as_deref().unwrap_or(""),
        &(cursor.backfill_complete as u8).to_string(),
        &cursor
            .sync_started_at_ms
            .map(|ms| ms.to_string())
            .unwrap_or_default(),
        &cursor.rate_limited_until_ms.to_string(),
        cursor.last_error.as_deref().unwrap_or(""),
        &cursor
            .last_advanced_at_ms
            .map(|ms| ms.to_string())
            .unwrap_or_default(),
    ]);
    let idempotency_key = format!("crm_cache_cursor:{entity}:{content}");
    let after = serde_json::json!({
        "entity": entity,
        "next_after_cursor": cursor.next_after_cursor,
        "backfill_complete": cursor.backfill_complete,
        "sync_started_at_ms": cursor.sync_started_at_ms,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_entity = entity.to_string();
    let owned = cursor.clone();
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
                "INSERT INTO crm_cache_sync_cursors \
                 (client_id, entity, next_after_cursor, high_water_modified_at, backfill_complete, \
                  sync_started_at_ms, rate_limited_until_ms, last_error, last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (client_id, entity) DO UPDATE SET \
                   next_after_cursor = excluded.next_after_cursor, \
                   high_water_modified_at = excluded.high_water_modified_at, \
                   backfill_complete = excluded.backfill_complete, \
                   sync_started_at_ms = excluded.sync_started_at_ms, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client,
                    owned_entity,
                    owned.next_after_cursor,
                    owned.high_water_modified_at,
                    owned.backfill_complete,
                    owned.sync_started_at_ms.map(|ms| ms as i64),
                    owned.rate_limited_until_ms as i64,
                    owned.last_error,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

pub fn contact_by_provider_id(
    conn: &Connection,
    client_id: &str,
    provider_contact_id: &str,
) -> Result<Option<ContactSnapshotRow>, StoreError> {
    conn.query_row(
        "SELECT provider_contact_id, email, full_name, company, phone, lifecycle_stage, owner, \
         last_activity_at FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND provider_contact_id = ?2 AND active = 1",
        params![client_id, provider_contact_id],
        contact_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn contact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactSnapshotRow> {
    Ok(ContactSnapshotRow {
        provider_contact_id: row.get(0)?,
        email: row.get(1)?,
        full_name: row.get(2)?,
        company: row.get(3)?,
        phone: row.get(4)?,
        lifecycle_stage: row.get(5)?,
        owner: row.get(6)?,
        last_activity_at: row.get(7)?,
    })
}

fn deal_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DealSnapshotRow> {
    Ok(DealSnapshotRow {
        provider_deal_id: row.get(0)?,
        deal_name: row.get(1)?,
        stage: row.get(2)?,
        amount_cents: row.get(3)?,
        currency: row.get(4)?,
        pipeline: row.get(5)?,
        close_date: row.get(6)?,
        associated_contact_email: row.get(7)?,
        associated_company: row.get(8)?,
    })
}

pub fn contacts_by_email(
    conn: &Connection,
    client_id: &str,
    email: &str,
) -> Result<Vec<ContactSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_contact_id, email, full_name, company, phone, lifecycle_stage, owner, \
         last_activity_at FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1 AND lower(email) = lower(?2) \
         ORDER BY full_name COLLATE NOCASE ASC, provider_contact_id ASC LIMIT 25",
    )?;
    let rows = stmt.query_map(params![client_id, email.trim()], contact_from_row)?;
    collect_rows(rows)
}

pub fn contact_by_company(
    conn: &Connection,
    client_id: &str,
    company: &str,
) -> Result<Vec<ContactSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_contact_id, email, full_name, company, phone, lifecycle_stage, owner, \
         last_activity_at FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1 AND company = ?2 COLLATE NOCASE \
         ORDER BY full_name COLLATE NOCASE ASC, email COLLATE NOCASE ASC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![client_id, company.trim()], contact_from_row)?;
    collect_rows(rows)
}

pub fn deals_by_contact(
    conn: &Connection,
    client_id: &str,
    contact_email: &str,
) -> Result<Vec<DealSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_deal_id, deal_name, stage, amount_cents, currency, pipeline, close_date, \
         associated_contact_email, associated_company FROM crm_deal_snapshots \
         WHERE client_id = ?1 AND active = 1 AND lower(associated_contact_email) = lower(?2) \
         ORDER BY close_date DESC, provider_deal_id DESC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![client_id, contact_email.trim()], deal_from_row)?;
    collect_rows(rows)
}

pub fn tombstone_stale_contact_snapshots(
    conn: &mut Connection,
    client_id: &str,
    sync_started_at_ms: u64,
    now_ms: u64,
) -> Result<usize, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_contact_id FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1 AND last_seen_at_ms < ?2 \
         ORDER BY provider_contact_id ASC",
    )?;
    let ids = stmt
        .query_map(params![client_id, sync_started_at_ms as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut tombstoned = 0;
    for provider_contact_id in ids {
        let idempotency_key =
            format!("crm_cache_tombstone:contact:{provider_contact_id}:{sync_started_at_ms}");
        let after = serde_json::json!({
            "active": false,
            "sync_started_at_ms": sync_started_at_ms,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned_id = provider_contact_id.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: CONTACT_ENTITY_KIND,
                entity_id: &provider_contact_id,
                change_kind: "sync_tombstone",
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
                    "UPDATE crm_contact_snapshots SET active = 0, last_written_at_ms = ?3 \
                     WHERE client_id = ?1 AND provider_contact_id = ?2",
                    params![owned_client, owned_id, now_ms as i64],
                )?;
                Ok(())
            },
        )?;
        tombstoned += 1;
    }
    Ok(tombstoned)
}

pub fn tombstone_stale_deal_snapshots(
    conn: &mut Connection,
    client_id: &str,
    sync_started_at_ms: u64,
    now_ms: u64,
) -> Result<usize, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_deal_id FROM crm_deal_snapshots \
         WHERE client_id = ?1 AND active = 1 AND last_seen_at_ms < ?2 \
         ORDER BY provider_deal_id ASC",
    )?;
    let ids = stmt
        .query_map(params![client_id, sync_started_at_ms as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut tombstoned = 0;
    for provider_deal_id in ids {
        let idempotency_key =
            format!("crm_cache_tombstone:deal:{provider_deal_id}:{sync_started_at_ms}");
        let after = serde_json::json!({
            "active": false,
            "sync_started_at_ms": sync_started_at_ms,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned_id = provider_deal_id.clone();
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: DEAL_ENTITY_KIND,
                entity_id: &provider_deal_id,
                change_kind: "sync_tombstone",
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
                    "UPDATE crm_deal_snapshots SET active = 0, last_written_at_ms = ?3 \
                     WHERE client_id = ?1 AND provider_deal_id = ?2",
                    params![owned_client, owned_id, now_ms as i64],
                )?;
                Ok(())
            },
        )?;
        tombstoned += 1;
    }
    Ok(tombstoned)
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, StoreError> {
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn snapshot_counts(conn: &Connection, client_id: &str) -> Result<SnapshotCounts, StoreError> {
    let contacts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM crm_contact_snapshots WHERE client_id = ?1 AND active = 1",
        params![client_id],
        |row| row.get(0),
    )?;
    let deals: i64 = conn.query_row(
        "SELECT COUNT(*) FROM crm_deal_snapshots WHERE client_id = ?1 AND active = 1",
        params![client_id],
        |row| row.get(0),
    )?;
    let contact_sync: Option<i64> = conn.query_row(
        "SELECT MAX(MAX(last_written_at_ms, last_seen_at_ms)) FROM crm_contact_snapshots \
         WHERE client_id = ?1 AND active = 1",
        params![client_id],
        |row| row.get(0),
    )?;
    let deal_sync: Option<i64> = conn.query_row(
        "SELECT MAX(MAX(last_written_at_ms, last_seen_at_ms)) FROM crm_deal_snapshots \
         WHERE client_id = ?1 AND active = 1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok(SnapshotCounts {
        contacts: contacts as u64,
        deals: deals as u64,
        last_synced_at_ms: contact_sync
            .into_iter()
            .chain(deal_sync)
            .max()
            .map(|ms| ms as u64),
    })
}
