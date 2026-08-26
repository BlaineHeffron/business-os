//! Inventory persistence: per-entity sync cursors and snapshot caches with
//! content-hash upserts (the qbo_views pattern). Receipt-quietness is
//! load-bearing: full-set syncs re-fetch everything each cycle, so unchanged
//! rows are compared BEFORE store_core::mutate and skipped — a steady-state
//! cycle writes zero rows anywhere.
//!
//! No credential storage: the Stockforge service-account login lives in env
//! and session tokens stay in memory (worker), so nothing secret touches
//! sqlite or receipts.

use bos_contracts::claim_drafts::ClaimShipmentRefs;
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::stockforge_read::{
    SfAlertRecord, SfMaterialRecord, SfOrderCardRecord, SfPurchaseOrderRecord,
    SfReorderSuggestionRecord,
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::slices::shipment_refs::{claim_refs_from_sf, deserialize_refs, serialize_refs};
use crate::store_core::{self, MutationRequest, StoreError};

pub const CURSOR_ENTITY_KIND: &str = "stockforge_sync_cursor";
pub const MATERIAL_ENTITY_KIND: &str = "stockforge_material_snapshot";
pub const ALERT_ENTITY_KIND: &str = "stockforge_alert_snapshot";
pub const REORDER_ENTITY_KIND: &str = "stockforge_reorder_snapshot";
pub const ORDER_ENTITY_KIND: &str = "stockforge_order_snapshot";
pub const PO_ENTITY_KIND: &str = "stockforge_po_snapshot";
pub const SYNC_ACTOR: &str = "stockforge_sync_pump";

pub const ENTITY_MATERIAL: &str = "material";
pub const ENTITY_ALERT: &str = "alert";
pub const ENTITY_REORDER: &str = "reorder";
pub const ENTITY_ORDER: &str = "order";
pub const ENTITY_PO: &str = "po";

/// All sync entities, in pump order.
pub const ALL_ENTITIES: &[&str] = &[
    ENTITY_MATERIAL,
    ENTITY_ALERT,
    ENTITY_REORDER,
    ENTITY_ORDER,
    ENTITY_PO,
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SfSyncCursor {
    /// Offset for the in-progress material walk (skip/take paging; the other
    /// entities are single-request full sets and keep this at 0).
    pub next_skip: u32,
    pub backfill_complete: bool,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub last_error_at_ms: Option<u64>,
    pub last_advanced_at_ms: Option<u64>,
}

impl SfSyncCursor {
    /// last_error_at_ms is deliberately excluded: a repeating identical error
    /// keeps the first occurrence ("failing since") and must not write a receipt.
    fn content_eq(&self, other: &Self) -> bool {
        self.next_skip == other.next_skip
            && self.backfill_complete == other.backfill_complete
            && self.rate_limited_until_ms == other.rate_limited_until_ms
            && self.last_error == other.last_error
            && self.last_error_class == other.last_error_class
    }
}

pub fn get_cursor(
    conn: &Connection,
    client_id: &str,
    entity: &str,
) -> Result<SfSyncCursor, StoreError> {
    let row = conn
        .query_row(
            "SELECT next_skip, backfill_complete, rate_limited_until_ms, last_error, \
             last_error_class, last_error_at_ms, last_advanced_at_ms \
             FROM stockforge_sync_cursors WHERE client_id = ?1 AND entity = ?2",
            params![client_id, entity],
            |row| {
                Ok(SfSyncCursor {
                    next_skip: row.get::<_, i64>(0)? as u32,
                    backfill_complete: row.get(1)?,
                    rate_limited_until_ms: row.get::<_, i64>(2)? as u64,
                    last_error: row.get(3)?,
                    last_error_class: row.get(4)?,
                    last_error_at_ms: row.get::<_, Option<i64>>(5)?.map(|ms| ms as u64),
                    last_advanced_at_ms: row.get::<_, Option<i64>>(6)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

/// Compare-before-write cursor persistence: returns false (and writes
/// nothing, not even a receipt) when the cursor is unchanged.
pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    entity: &str,
    cursor: &SfSyncCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_cursor(conn, client_id, entity)?;
    if current.content_eq(cursor) {
        return Ok(false);
    }
    let last_error_class = if cursor.last_error.is_some() {
        cursor.last_error_class.clone()
    } else {
        None
    };
    let last_error_at_ms = if cursor.last_error.is_some() {
        cursor.last_error_at_ms.or(Some(now_ms))
    } else {
        None
    };
    let last_advanced_at_ms = if cursor.last_error.is_none() {
        Some(now_ms)
    } else {
        current.last_advanced_at_ms
    };
    let content = snapshot_hash(&[
        &cursor.next_skip.to_string(),
        &(cursor.backfill_complete as u8).to_string(),
        &cursor.rate_limited_until_ms.to_string(),
        cursor.last_error.as_deref().unwrap_or(""),
        last_error_class.as_deref().unwrap_or(""),
    ]);
    let idempotency_key = format!("stockforge_cursor:{entity}:{content}");
    let after = serde_json::json!({
        "entity": entity,
        "next_skip": cursor.next_skip,
        "backfill_complete": cursor.backfill_complete,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
        "last_error_class": last_error_class,
        "last_error_at_ms": last_error_at_ms,
        "last_advanced_at_ms": last_advanced_at_ms,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_entity = entity.to_string();
    let mut owned_cursor = cursor.clone();
    owned_cursor.last_error_class = last_error_class;
    owned_cursor.last_error_at_ms = last_error_at_ms;
    owned_cursor.last_advanced_at_ms = last_advanced_at_ms;
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
                "INSERT INTO stockforge_sync_cursors \
                 (client_id, entity, next_skip, backfill_complete, rate_limited_until_ms, \
                  last_error, last_error_class, last_error_at_ms, last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (client_id, entity) DO UPDATE SET \
                   next_skip = excluded.next_skip, \
                   backfill_complete = excluded.backfill_complete, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_error_class = excluded.last_error_class, \
                   last_error_at_ms = excluded.last_error_at_ms, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client,
                    owned_entity,
                    owned_cursor.next_skip as i64,
                    owned_cursor.backfill_complete,
                    owned_cursor.rate_limited_until_ms as i64,
                    owned_cursor.last_error,
                    owned_cursor.last_error_class,
                    owned_cursor.last_error_at_ms.map(|ms| ms as i64),
                    owned_cursor.last_advanced_at_ms.map(|ms| ms as i64),
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

/// f64 → stable hash text (full precision via Debug formatting).
fn num(value: f64) -> String {
    format!("{value:?}")
}

fn opt_num(value: Option<f64>) -> String {
    value.map(num).unwrap_or_default()
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

/// Shared upsert shell: hash-compare first (skip quietly), then one receipted
/// mutation per changed/new row with a content-derived idempotency key.
#[allow(clippy::too_many_arguments)]
fn upsert_snapshot(
    conn: &mut Connection,
    client_id: &str,
    table: &'static str,
    id_column: &'static str,
    entity_kind: &'static str,
    entity_id: &str,
    hash: String,
    after: String,
    now_ms: u64,
    write: impl FnOnce(&rusqlite::Transaction<'_>, u64) -> rusqlite::Result<()>,
) -> Result<bool, StoreError> {
    let existing = existing_hash(conn, table, id_column, client_id, entity_id)?;
    if existing
        .as_ref()
        .is_some_and(|(current, _)| *current == hash)
    {
        return Ok(false);
    }
    let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
    let idempotency_key = format!("stockforge_sync:{entity_kind}:{entity_id}:{hash}");
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind,
            entity_id,
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
            write(tx, first_seen)?;
            Ok(())
        },
    )?;
    Ok(true)
}

pub fn upsert_material_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[SfMaterialRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = snapshot_hash(&[
            &record.name,
            record.sku.as_deref().unwrap_or(""),
            record.category.as_deref().unwrap_or(""),
            &num(record.current_quantity),
            &opt_num(record.reserved_qty),
            &opt_num(record.incoming_qty),
            record.unit.as_deref().unwrap_or(""),
            &opt_num(record.warning_threshold),
            &opt_num(record.critical_threshold),
            record.threshold_type.as_deref().unwrap_or(""),
            &record.unit_cost_cents.to_string(),
            &record.lead_time_days.unwrap_or(-1).to_string(),
            record.vendor_name.as_deref().unwrap_or(""),
            &(record.is_active as u8).to_string(),
            &record
                .is_purchasable
                .map(|value| value as u8)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            record.replenishment_policy.as_deref().unwrap_or(""),
            record.sale_depletion_policy.as_deref().unwrap_or(""),
        ]);
        let after = serde_json::json!({
            "name": record.name,
            "sku": record.sku,
            "quantity": record.current_quantity,
            "reserved_qty": record.reserved_qty,
            "incoming_qty": record.incoming_qty,
            "unit": record.unit,
            "is_active": record.is_active,
            "is_purchasable": record.is_purchasable,
            "replenishment_policy": record.replenishment_policy,
            "sale_depletion_policy": record.sale_depletion_policy,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let written = upsert_snapshot(
            conn,
            client_id,
            "stockforge_material_snapshots",
            "material_id",
            MATERIAL_ENTITY_KIND,
            &record.material_id,
            hash,
            after,
            now_ms,
            move |tx, first_seen| {
                tx.execute(
                    "INSERT INTO stockforge_material_snapshots \
                     (client_id, material_id, name, sku, category, quantity, reserved_qty, incoming_qty, unit, \
                      warning_threshold, critical_threshold, threshold_type, unit_cost_cents, \
                      lead_time_days, vendor_name, is_active, sf_updated_at, content_hash, \
                      first_seen_at_ms, last_written_at_ms, is_purchasable, \
                      replenishment_policy, sale_depletion_policy) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                             ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23) \
                     ON CONFLICT (client_id, material_id) DO UPDATE SET \
                       name = excluded.name, sku = excluded.sku, category = excluded.category, \
                       quantity = excluded.quantity, reserved_qty = excluded.reserved_qty, \
                       incoming_qty = excluded.incoming_qty, unit = excluded.unit, \
                       warning_threshold = excluded.warning_threshold, \
                       critical_threshold = excluded.critical_threshold, \
                       threshold_type = excluded.threshold_type, \
                       unit_cost_cents = excluded.unit_cost_cents, \
                       lead_time_days = excluded.lead_time_days, \
                       vendor_name = excluded.vendor_name, \
                       is_active = excluded.is_active, \
                       is_purchasable = excluded.is_purchasable, \
                       replenishment_policy = excluded.replenishment_policy, \
                       sale_depletion_policy = excluded.sale_depletion_policy, \
                       sf_updated_at = excluded.sf_updated_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.material_id,
                        owned.name,
                        owned.sku,
                        owned.category,
                        owned.current_quantity,
                        owned.reserved_qty,
                        owned.incoming_qty,
                        owned.unit,
                        owned.warning_threshold,
                        owned.critical_threshold,
                        owned.threshold_type,
                        owned.unit_cost_cents,
                        owned.lead_time_days,
                        owned.vendor_name,
                        owned.is_active,
                        owned.updated_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                        owned.is_purchasable,
                        owned.replenishment_policy,
                        owned.sale_depletion_policy,
                    ],
                )?;
                Ok(())
            },
        )?;
        if written {
            summary.written += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

pub fn upsert_alert_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[SfAlertRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = snapshot_hash(&[
            record.material_id.as_deref().unwrap_or(""),
            record.material_name.as_deref().unwrap_or(""),
            record.material_sku.as_deref().unwrap_or(""),
            &record.severity,
            &record.status,
            &opt_num(record.current_quantity),
            &opt_num(record.threshold_value),
            &opt_num(record.percentage_remaining),
            record.message.as_deref().unwrap_or(""),
        ]);
        let after = serde_json::json!({
            "material_name": record.material_name,
            "severity": record.severity,
            "status": record.status,
            "message": record.message,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let written = upsert_snapshot(
            conn,
            client_id,
            "stockforge_alert_snapshots",
            "alert_id",
            ALERT_ENTITY_KIND,
            &record.alert_id,
            hash,
            after,
            now_ms,
            move |tx, first_seen| {
                tx.execute(
                    "INSERT INTO stockforge_alert_snapshots \
                     (client_id, alert_id, material_id, material_name, material_sku, severity, \
                      status, quantity, threshold_value, percentage_remaining, message, \
                      sf_created_at, content_hash, first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                     ON CONFLICT (client_id, alert_id) DO UPDATE SET \
                       material_id = excluded.material_id, \
                       material_name = excluded.material_name, \
                       material_sku = excluded.material_sku, \
                       severity = excluded.severity, status = excluded.status, \
                       quantity = excluded.quantity, \
                       threshold_value = excluded.threshold_value, \
                       percentage_remaining = excluded.percentage_remaining, \
                       message = excluded.message, sf_created_at = excluded.sf_created_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.alert_id,
                        owned.material_id,
                        owned.material_name,
                        owned.material_sku,
                        owned.severity,
                        owned.status,
                        owned.current_quantity,
                        owned.threshold_value,
                        owned.percentage_remaining,
                        owned.message,
                        owned.created_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        if written {
            summary.written += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

pub fn upsert_reorder_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[SfReorderSuggestionRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let hash = snapshot_hash(&[
            record.material_id.as_deref().unwrap_or(""),
            record.material_name.as_deref().unwrap_or(""),
            record.vendor_name.as_deref().unwrap_or(""),
            &record.urgency,
            &record.status,
            &opt_num(record.current_quantity),
            &opt_num(record.suggested_quantity),
            &record.estimated_cost_cents.to_string(),
            &opt_num(record.days_until_stockout),
            record.reasoning.as_deref().unwrap_or(""),
        ]);
        let after = serde_json::json!({
            "material_name": record.material_name,
            "urgency": record.urgency,
            "status": record.status,
            "suggested_quantity": record.suggested_quantity,
            "estimated_cost_cents": record.estimated_cost_cents,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let written = upsert_snapshot(
            conn,
            client_id,
            "stockforge_reorder_snapshots",
            "suggestion_id",
            REORDER_ENTITY_KIND,
            &record.suggestion_id,
            hash,
            after,
            now_ms,
            move |tx, first_seen| {
                tx.execute(
                    "INSERT INTO stockforge_reorder_snapshots \
                     (client_id, suggestion_id, material_id, material_name, material_sku, \
                      vendor_name, urgency, status, current_quantity, suggested_quantity, unit, \
                      estimated_cost_cents, days_until_stockout, lead_time_days, reasoning, \
                      sf_created_at, content_hash, first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                             ?16, ?17, ?18, ?19) \
                     ON CONFLICT (client_id, suggestion_id) DO UPDATE SET \
                       material_id = excluded.material_id, \
                       material_name = excluded.material_name, \
                       material_sku = excluded.material_sku, \
                       vendor_name = excluded.vendor_name, urgency = excluded.urgency, \
                       status = excluded.status, \
                       current_quantity = excluded.current_quantity, \
                       suggested_quantity = excluded.suggested_quantity, \
                       unit = excluded.unit, \
                       estimated_cost_cents = excluded.estimated_cost_cents, \
                       days_until_stockout = excluded.days_until_stockout, \
                       lead_time_days = excluded.lead_time_days, \
                       reasoning = excluded.reasoning, sf_created_at = excluded.sf_created_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.suggestion_id,
                        owned.material_id,
                        owned.material_name,
                        owned.material_sku,
                        owned.vendor_name,
                        owned.urgency,
                        owned.status,
                        owned.current_quantity,
                        owned.suggested_quantity,
                        owned.unit,
                        owned.estimated_cost_cents,
                        owned.days_until_stockout,
                        owned.lead_time_days,
                        owned.reasoning,
                        owned.created_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        if written {
            summary.written += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

pub fn upsert_order_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[SfOrderCardRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let shipment_refs = claim_refs_from_sf(record.shipment_refs.as_ref());
        let shipment_refs_json = serialize_refs(shipment_refs.as_ref())?;
        let line_material_ids_json = serde_json::to_string(&record.line_material_ids)
            .map_err(|err| StoreError::Domain(format!("inventory_line_identity_json: {err}")))?;
        let hash = snapshot_hash(&[
            &record.order_number,
            record.external_order_id.as_deref().unwrap_or(""),
            record.platform.as_deref().unwrap_or(""),
            &record.board_status,
            record.raw_status.as_deref().unwrap_or(""),
            record.customer_name.as_deref().unwrap_or(""),
            record.customer_email.as_deref().unwrap_or(""),
            &record.total_amount_cents.to_string(),
            record.order_date.as_deref().unwrap_or(""),
            record.processed_at.as_deref().unwrap_or(""),
            &record.item_count.to_string(),
            &record.unit_count.to_string(),
            &record.mapped_line_count.to_string(),
            &line_material_ids_json,
            &(record.line_identity_complete as u8).to_string(),
            record.carrier.as_deref().unwrap_or(""),
            record.tracking_number.as_deref().unwrap_or(""),
            shipment_refs_json.as_deref().unwrap_or(""),
            record.shipment_id.as_deref().unwrap_or(""),
            record.ship_date.as_deref().unwrap_or(""),
            &record.photo_count.to_string(),
            record.pack_station_container_id.as_deref().unwrap_or(""),
            &format!(
                "{}{}{}{}{}{}{}",
                record.needs_mapping as u8,
                record.blocked as u8,
                record.deducted as u8,
                record.deduction_failed as u8,
                record.label_needed as u8,
                record.packed_missing_photo as u8,
                record.exception as u8,
            ),
            &record.depletion_total.to_string(),
            &record.depletion_applied.to_string(),
            &record.depletion_failed.to_string(),
            &record.depletion_reversed.to_string(),
            &record.blocked_reasons_json,
        ]);
        let after = serde_json::json!({
            "order_number": record.order_number,
            "board_status": record.board_status,
            "customer_name": record.customer_name,
            "total_amount_cents": record.total_amount_cents,
            "blocked": record.blocked,
            "needs_mapping": record.needs_mapping,
            "deduction_failed": record.deduction_failed,
            "line_material_ids": record.line_material_ids,
            "line_identity_complete": record.line_identity_complete,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let owned_refs_json = shipment_refs_json.clone();
        let owned_line_material_ids_json = line_material_ids_json.clone();
        let written = upsert_snapshot(
            conn,
            client_id,
            "stockforge_order_snapshots",
            "order_id",
            ORDER_ENTITY_KIND,
            &record.order_id,
            hash,
            after,
            now_ms,
            move |tx, first_seen| {
                tx.execute(
                    "INSERT INTO stockforge_order_snapshots \
                     (client_id, order_id, order_number, external_order_id, platform, \
                      board_status, raw_status, customer_name, customer_email, \
                      total_amount_cents, currency, order_date, processed_at, item_count, \
                      unit_count, mapped_line_count, line_material_ids_json, line_identity_complete, \
                      carrier, tracking_number, shipment_refs_json, shipment_id, \
                      ship_date, photo_count, pack_station_container_id, needs_mapping, blocked, \
                      deducted, deduction_failed, label_needed, packed_missing_photo, exception, \
                      depletion_total, depletion_applied, depletion_failed, depletion_reversed, \
                      blocked_reasons_json, content_hash, first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                             ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, \
                             ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40) \
                     ON CONFLICT (client_id, order_id) DO UPDATE SET \
                       order_number = excluded.order_number, \
                       external_order_id = excluded.external_order_id, \
                       platform = excluded.platform, \
                       board_status = excluded.board_status, raw_status = excluded.raw_status, \
                       customer_name = excluded.customer_name, \
                       customer_email = excluded.customer_email, \
                       total_amount_cents = excluded.total_amount_cents, \
                       currency = excluded.currency, order_date = excluded.order_date, \
                       processed_at = excluded.processed_at, \
                       item_count = excluded.item_count, unit_count = excluded.unit_count, \
                       mapped_line_count = excluded.mapped_line_count, \
                       line_material_ids_json = excluded.line_material_ids_json, \
                       line_identity_complete = excluded.line_identity_complete, \
                       carrier = excluded.carrier, \
                       tracking_number = excluded.tracking_number, \
                       shipment_refs_json = excluded.shipment_refs_json, \
                       shipment_id = excluded.shipment_id, ship_date = excluded.ship_date, \
                       photo_count = excluded.photo_count, \
                       pack_station_container_id = excluded.pack_station_container_id, \
                       needs_mapping = excluded.needs_mapping, blocked = excluded.blocked, \
                       deducted = excluded.deducted, \
                       deduction_failed = excluded.deduction_failed, \
                       label_needed = excluded.label_needed, \
                       packed_missing_photo = excluded.packed_missing_photo, \
                       exception = excluded.exception, \
                       depletion_total = excluded.depletion_total, \
                       depletion_applied = excluded.depletion_applied, \
                       depletion_failed = excluded.depletion_failed, \
                       depletion_reversed = excluded.depletion_reversed, \
                       blocked_reasons_json = excluded.blocked_reasons_json, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.order_id,
                        owned.order_number,
                        owned.external_order_id,
                        owned.platform,
                        owned.board_status,
                        owned.raw_status,
                        owned.customer_name,
                        owned.customer_email,
                        owned.total_amount_cents,
                        owned.currency,
                        owned.order_date,
                        owned.processed_at,
                        owned.item_count,
                        owned.unit_count,
                        owned.mapped_line_count,
                        owned_line_material_ids_json,
                        owned.line_identity_complete,
                        owned.carrier,
                        owned.tracking_number,
                        owned_refs_json,
                        owned.shipment_id,
                        owned.ship_date,
                        owned.photo_count,
                        owned.pack_station_container_id,
                        owned.needs_mapping,
                        owned.blocked,
                        owned.deducted,
                        owned.deduction_failed,
                        owned.label_needed,
                        owned.packed_missing_photo,
                        owned.exception,
                        owned.depletion_total,
                        owned.depletion_applied,
                        owned.depletion_failed,
                        owned.depletion_reversed,
                        owned.blocked_reasons_json,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        if written {
            summary.written += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

pub fn upsert_po_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[SfPurchaseOrderRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let line_material_ids_json = serde_json::to_string(&record.line_material_ids)
            .map_err(|err| StoreError::Domain(format!("inventory_line_identity_json: {err}")))?;
        let hash = snapshot_hash(&[
            record.vendor_name.as_deref().unwrap_or(""),
            &record.status,
            &record.total_estimated_cost_cents.to_string(),
            record.freight_mode.as_deref().unwrap_or(""),
            &record.line_count.to_string(),
            &line_material_ids_json,
            &(record.line_identity_complete as u8).to_string(),
            record.sent_at.as_deref().unwrap_or(""),
            record.received_at.as_deref().unwrap_or(""),
        ]);
        let after = serde_json::json!({
            "vendor_name": record.vendor_name,
            "status": record.status,
            "total_estimated_cost_cents": record.total_estimated_cost_cents,
            "line_material_ids": record.line_material_ids,
            "line_identity_complete": record.line_identity_complete,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let owned_line_material_ids_json = line_material_ids_json.clone();
        let written = upsert_snapshot(
            conn,
            client_id,
            "stockforge_po_snapshots",
            "po_id",
            PO_ENTITY_KIND,
            &record.po_id,
            hash,
            after,
            now_ms,
            move |tx, first_seen| {
                tx.execute(
                    "INSERT INTO stockforge_po_snapshots \
                     (client_id, po_id, vendor_name, status, total_estimated_cost_cents, \
                      freight_mode, line_count, line_material_ids_json, line_identity_complete, \
                      sf_created_at, sf_sent_at, sf_received_at, \
                      content_hash, first_seen_at_ms, last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                     ON CONFLICT (client_id, po_id) DO UPDATE SET \
                       vendor_name = excluded.vendor_name, status = excluded.status, \
                       total_estimated_cost_cents = excluded.total_estimated_cost_cents, \
                       freight_mode = excluded.freight_mode, \
                       line_count = excluded.line_count, \
                       line_material_ids_json = excluded.line_material_ids_json, \
                       line_identity_complete = excluded.line_identity_complete, \
                       sf_created_at = excluded.sf_created_at, \
                       sf_sent_at = excluded.sf_sent_at, \
                       sf_received_at = excluded.sf_received_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.po_id,
                        owned.vendor_name,
                        owned.status,
                        owned.total_estimated_cost_cents,
                        owned.freight_mode,
                        owned.line_count,
                        owned_line_material_ids_json,
                        owned.line_identity_complete,
                        owned.created_at,
                        owned.sent_at,
                        owned.received_at,
                        owned_hash,
                        first_seen as i64,
                        now_ms as i64,
                    ],
                )?;
                Ok(())
            },
        )?;
        if written {
            summary.written += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

/// Full-set prune: delete cached rows whose ids are NOT in the latest fetch.
/// Alerts/reorders/orders are full-snapshot syncs, so a missing id means
/// resolved / accepted / out of window. One receipted mutation per prune
/// (listing the removed ids); nothing to prune writes nothing.
pub fn prune_missing(
    conn: &mut Connection,
    client_id: &str,
    table: &'static str,
    id_column: &'static str,
    entity_kind: &'static str,
    keep_ids: &HashSet<String>,
    now_ms: u64,
) -> Result<usize, StoreError> {
    let existing: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT {id_column} FROM {table} WHERE client_id = ?1"
        ))?;
        let rows = stmt.query_map(params![client_id], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        ids
    };
    let mut removed: Vec<String> = existing
        .into_iter()
        .filter(|id| !keep_ids.contains(id))
        .collect();
    if removed.is_empty() {
        return Ok(0);
    }
    removed.sort();
    let hash = snapshot_hash(&removed.iter().map(String::as_str).collect::<Vec<_>>());
    let idempotency_key = format!("stockforge_prune:{entity_kind}:{hash}");
    let after = serde_json::json!({ "removed": removed.len(), "ids": removed }).to_string();
    let owned_client = client_id.to_string();
    let owned_removed = removed.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind,
            entity_id: "window",
            change_kind: "sync_prune",
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
            for id in &owned_removed {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE client_id = ?1 AND {id_column} = ?2"),
                    params![owned_client, id],
                )?;
            }
            Ok(())
        },
    )?;
    Ok(removed.len())
}

/// A cached material row as read back for the views.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialSnapshotRow {
    pub material_id: String,
    pub name: String,
    pub sku: Option<String>,
    pub category: Option<String>,
    pub quantity: f64,
    pub reserved_qty: Option<f64>,
    pub incoming_qty: Option<f64>,
    pub unit: Option<String>,
    pub warning_threshold: Option<f64>,
    pub critical_threshold: Option<f64>,
    pub threshold_type: Option<String>,
    pub unit_cost_cents: i64,
    pub lead_time_days: Option<i64>,
    pub vendor_name: Option<String>,
    pub is_active: bool,
    pub is_purchasable: Option<bool>,
    pub replenishment_policy: Option<String>,
    pub sale_depletion_policy: Option<String>,
}

pub fn list_materials(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<MaterialSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT material_id, name, sku, category, quantity, reserved_qty, incoming_qty, unit, warning_threshold, \
         critical_threshold, threshold_type, unit_cost_cents, lead_time_days, vendor_name, \
         is_active, is_purchasable, replenishment_policy, sale_depletion_policy \
         FROM stockforge_material_snapshots WHERE client_id = ?1 \
         ORDER BY name COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(MaterialSnapshotRow {
            material_id: row.get(0)?,
            name: row.get(1)?,
            sku: row.get(2)?,
            category: row.get(3)?,
            quantity: row.get(4)?,
            reserved_qty: row.get(5)?,
            incoming_qty: row.get(6)?,
            unit: row.get(7)?,
            warning_threshold: row.get(8)?,
            critical_threshold: row.get(9)?,
            threshold_type: row.get(10)?,
            unit_cost_cents: row.get(11)?,
            lead_time_days: row.get(12)?,
            vendor_name: row.get(13)?,
            is_active: row.get(14)?,
            is_purchasable: row.get(15)?,
            replenishment_policy: row.get(16)?,
            sale_depletion_policy: row.get(17)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertSnapshotRow {
    pub alert_id: String,
    pub material_id: Option<String>,
    pub material_name: Option<String>,
    pub material_sku: Option<String>,
    pub severity: String,
    pub quantity: Option<f64>,
    pub percentage_remaining: Option<f64>,
    pub message: Option<String>,
    pub created_at: Option<String>,
}

pub fn list_alerts(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<AlertSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT alert_id, material_id, material_name, material_sku, severity, quantity, \
         percentage_remaining, message, sf_created_at \
         FROM stockforge_alert_snapshots WHERE client_id = ?1 \
         ORDER BY CASE severity WHEN 'CRITICAL' THEN 0 ELSE 1 END, material_name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(AlertSnapshotRow {
            alert_id: row.get(0)?,
            material_id: row.get(1)?,
            material_name: row.get(2)?,
            material_sku: row.get(3)?,
            severity: row.get(4)?,
            quantity: row.get(5)?,
            percentage_remaining: row.get(6)?,
            message: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReorderSnapshotRow {
    pub suggestion_id: String,
    pub material_id: Option<String>,
    pub material_name: Option<String>,
    pub material_sku: Option<String>,
    pub vendor_name: Option<String>,
    pub urgency: String,
    pub status: String,
    pub days_until_stockout: Option<f64>,
    pub suggested_quantity: Option<f64>,
    pub unit: Option<String>,
    pub estimated_cost_cents: i64,
    pub lead_time_days: Option<i64>,
    pub reasoning: Option<String>,
}

pub fn list_reorder_suggestions(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<ReorderSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT suggestion_id, material_id, material_name, material_sku, vendor_name, urgency, status, \
         days_until_stockout, suggested_quantity, unit, estimated_cost_cents, lead_time_days, \
         reasoning \
         FROM stockforge_reorder_snapshots WHERE client_id = ?1 \
         ORDER BY CASE urgency \
            WHEN 'CRITICAL' THEN 0 WHEN 'HIGH' THEN 1 WHEN 'MEDIUM' THEN 2 ELSE 3 END, \
          days_until_stockout ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(ReorderSnapshotRow {
            suggestion_id: row.get(0)?,
            material_id: row.get(1)?,
            material_name: row.get(2)?,
            material_sku: row.get(3)?,
            vendor_name: row.get(4)?,
            urgency: row.get(5)?,
            status: row.get(6)?,
            days_until_stockout: row.get(7)?,
            suggested_quantity: row.get(8)?,
            unit: row.get(9)?,
            estimated_cost_cents: row.get(10)?,
            lead_time_days: row.get(11)?,
            reasoning: row.get(12)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderSnapshotRow {
    pub order_id: String,
    pub order_number: String,
    pub external_order_id: Option<String>,
    pub platform: Option<String>,
    pub board_status: String,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub total_amount_cents: i64,
    pub order_date: Option<String>,
    pub processed_at: Option<String>,
    pub item_count: i64,
    pub unit_count: i64,
    pub mapped_line_count: i64,
    pub line_material_ids: Vec<String>,
    pub line_identity_complete: bool,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub shipment_refs: Option<ClaimShipmentRefs>,
    pub shipment_id: Option<String>,
    pub ship_date: Option<String>,
    pub photo_count: i64,
    pub pack_station_container_id: Option<String>,
    pub needs_mapping: bool,
    pub blocked: bool,
    pub deducted: bool,
    pub deduction_failed: bool,
    pub exception: bool,
    pub depletion_total: i64,
    pub depletion_applied: i64,
    pub depletion_failed: i64,
    pub depletion_reversed: i64,
    pub blocked_reasons_json: String,
}

const ORDER_ROW_COLUMNS: &str = "order_id, order_number, external_order_id, platform, \
     board_status, customer_name, customer_email, total_amount_cents, order_date, processed_at, \
     item_count, unit_count, mapped_line_count, line_material_ids_json, line_identity_complete, \
     carrier, tracking_number, shipment_refs_json, shipment_id, \
     ship_date, photo_count, pack_station_container_id, needs_mapping, blocked, deducted, \
     deduction_failed, exception, depletion_total, depletion_applied, depletion_failed, \
     depletion_reversed, blocked_reasons_json";

fn order_row_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OrderSnapshotRow> {
    let line_material_ids_json: String = row.get(13)?;
    let line_material_ids = serde_json::from_str(&line_material_ids_json);
    let line_identity_complete = row.get::<_, bool>(14)? && line_material_ids.is_ok();
    Ok(OrderSnapshotRow {
        order_id: row.get(0)?,
        order_number: row.get(1)?,
        external_order_id: row.get(2)?,
        platform: row.get(3)?,
        board_status: row.get(4)?,
        customer_name: row.get(5)?,
        customer_email: row.get(6)?,
        total_amount_cents: row.get(7)?,
        order_date: row.get(8)?,
        processed_at: row.get(9)?,
        item_count: row.get(10)?,
        unit_count: row.get(11)?,
        mapped_line_count: row.get(12)?,
        line_material_ids: line_material_ids.unwrap_or_default(),
        line_identity_complete,
        carrier: row.get(15)?,
        tracking_number: row.get(16)?,
        shipment_refs: deserialize_refs(row.get::<_, Option<String>>(17)?.as_deref()),
        shipment_id: row.get(18)?,
        ship_date: row.get(19)?,
        photo_count: row.get(20)?,
        pack_station_container_id: row.get(21)?,
        needs_mapping: row.get(22)?,
        blocked: row.get(23)?,
        deducted: row.get(24)?,
        deduction_failed: row.get(25)?,
        exception: row.get(26)?,
        depletion_total: row.get(27)?,
        depletion_applied: row.get(28)?,
        depletion_failed: row.get(29)?,
        depletion_reversed: row.get(30)?,
        blocked_reasons_json: row.get(31)?,
    })
}

pub fn list_orders(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<OrderSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ORDER_ROW_COLUMNS} FROM stockforge_order_snapshots WHERE client_id = ?1 \
         ORDER BY order_date DESC, order_id DESC"
    ))?;
    let rows = stmt.query_map(params![client_id], order_row_from_row)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// The cached order card whose shipment links a Stockforge damage event —
/// the claim packet's order reference and packing-proof source.
pub fn get_order_by_shipment(
    conn: &Connection,
    client_id: &str,
    shipment_id: &str,
) -> Result<Option<OrderSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ORDER_ROW_COLUMNS} FROM stockforge_order_snapshots \
         WHERE client_id = ?1 AND shipment_id = ?2 LIMIT 1"
    ))?;
    let row = stmt
        .query_row(params![client_id, shipment_id], order_row_from_row)
        .optional()?;
    Ok(row)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoSnapshotRow {
    pub po_id: String,
    pub vendor_name: Option<String>,
    pub status: String,
    pub total_estimated_cost_cents: i64,
    pub freight_mode: Option<String>,
    pub line_count: i64,
    pub line_material_ids: Vec<String>,
    pub line_identity_complete: bool,
    pub created_at: Option<String>,
    pub sent_at: Option<String>,
}

pub fn list_purchase_orders(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<PoSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT po_id, vendor_name, status, total_estimated_cost_cents, freight_mode, \
         line_count, line_material_ids_json, line_identity_complete, sf_created_at, sf_sent_at \
         FROM stockforge_po_snapshots WHERE client_id = ?1 \
         ORDER BY sf_created_at DESC, po_id DESC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        let line_material_ids_json: String = row.get(6)?;
        let line_material_ids = serde_json::from_str(&line_material_ids_json);
        let line_identity_complete = row.get::<_, bool>(7)? && line_material_ids.is_ok();
        Ok(PoSnapshotRow {
            po_id: row.get(0)?,
            vendor_name: row.get(1)?,
            status: row.get(2)?,
            total_estimated_cost_cents: row.get(3)?,
            freight_mode: row.get(4)?,
            line_count: row.get(5)?,
            line_material_ids: line_material_ids.unwrap_or_default(),
            line_identity_complete,
            created_at: row.get(8)?,
            sent_at: row.get(9)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn snapshot_counts(conn: &Connection, client_id: &str) -> Result<(u64, u64), StoreError> {
    let materials: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stockforge_material_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    let orders: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stockforge_order_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok((materials as u64, orders as u64))
}

/// Order-control numbers for the owner digest: orders placed in the civil
/// date window (inclusive; `order_date` date prefix) plus the CURRENT
/// exception backlog flags across the cached board.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderControlCounts {
    pub orders_in_window: u64,
    pub exceptions: u64,
    pub deduction_failed: u64,
    pub needs_mapping: u64,
    pub packed_missing_photo: u64,
    pub blocked: u64,
}

pub fn order_control_counts(
    conn: &Connection,
    client_id: &str,
    start_date: &str,
    end_date: &str,
) -> Result<OrderControlCounts, StoreError> {
    conn.query_row(
        "SELECT \
           COALESCE(SUM(CASE WHEN substr(order_date, 1, 10) >= ?2 \
                              AND substr(order_date, 1, 10) <= ?3 THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(exception), 0), \
           COALESCE(SUM(deduction_failed), 0), \
           COALESCE(SUM(needs_mapping), 0), \
           COALESCE(SUM(packed_missing_photo), 0), \
           COALESCE(SUM(blocked), 0) \
         FROM stockforge_order_snapshots WHERE client_id = ?1",
        params![client_id, start_date, end_date],
        |row| {
            Ok(OrderControlCounts {
                orders_in_window: row.get::<_, i64>(0)? as u64,
                exceptions: row.get::<_, i64>(1)? as u64,
                deduction_failed: row.get::<_, i64>(2)? as u64,
                needs_mapping: row.get::<_, i64>(3)? as u64,
                packed_missing_photo: row.get::<_, i64>(4)? as u64,
                blocked: row.get::<_, i64>(5)? as u64,
            })
        },
    )
    .map_err(Into::into)
}
