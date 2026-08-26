//! Shopify sales cache persistence. Provider reads happen in the worker;
//! this module owns local snapshots and every mutation goes through
//! store_core.

use bos_contracts::receipt::ActorKindDto;
use bos_integrations::shopify_sales_read::{ShopifyCustomerRecord, ShopifyOrderRecord};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::http::OperatorScope;
use crate::store_core::{self, MutationRequest, StoreError};

pub const ORDER_ENTITY_KIND: &str = "shopify_order_snapshot";
pub const CUSTOMER_ENTITY_KIND: &str = "shopify_customer_snapshot";
pub const SYNC_STATE_ENTITY_KIND: &str = "shopify_sales_sync_state";
pub const SYNC_ACTOR: &str = "shopify_sales_sync_pump";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShopifySalesSyncState {
    pub shop_domain_fingerprint: Option<String>,
    pub backfill_complete: bool,
    pub order_backfill_complete: bool,
    pub customer_backfill_complete: bool,
    pub order_backfill_cursor: Option<String>,
    pub customer_backfill_cursor: Option<String>,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_advanced_at_ms: Option<u64>,
    pub last_order_count: u64,
    pub last_customer_count: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpsertSummary {
    pub written: usize,
    pub unchanged: usize,
}

pub fn get_sync_state(
    conn: &Connection,
    client_id: &str,
) -> Result<ShopifySalesSyncState, StoreError> {
    let row = conn
        .query_row(
            "SELECT shop_domain_fingerprint, backfill_complete, order_backfill_complete, \
             customer_backfill_complete, order_backfill_cursor, customer_backfill_cursor, \
             rate_limited_until_ms, last_error, last_advanced_at_ms, last_order_count, \
             last_customer_count \
             FROM shopify_sales_sync_state WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(ShopifySalesSyncState {
                    shop_domain_fingerprint: row.get(0)?,
                    backfill_complete: row.get(1)?,
                    order_backfill_complete: row.get(2)?,
                    customer_backfill_complete: row.get(3)?,
                    order_backfill_cursor: row.get(4)?,
                    customer_backfill_cursor: row.get(5)?,
                    rate_limited_until_ms: row.get::<_, i64>(6)? as u64,
                    last_error: row.get(7)?,
                    last_advanced_at_ms: row.get::<_, Option<i64>>(8)?.map(|ms| ms as u64),
                    last_order_count: row.get::<_, i64>(9)? as u64,
                    last_customer_count: row.get::<_, i64>(10)? as u64,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

pub fn reset_if_shop_changed(
    conn: &mut Connection,
    client_id: &str,
    shop_domain_fingerprint: &str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_sync_state(conn, client_id)?;
    if current.shop_domain_fingerprint.as_deref() == Some(shop_domain_fingerprint) {
        return Ok(false);
    }
    let after = serde_json::json!({
        "shop_domain_fingerprint": shop_domain_fingerprint,
        "previous_shop_domain_fingerprint": current.shop_domain_fingerprint,
    })
    .to_string();
    let idempotency_key = format!("shopify_sales_shop_reset:{shop_domain_fingerprint}:{now_ms}");
    let owned_client = client_id.to_string();
    let owned_fingerprint = shop_domain_fingerprint.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: SYNC_STATE_ENTITY_KIND,
            entity_id: "shopify_sales",
            change_kind: "shop_reset",
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
                "DELETE FROM shopify_order_snapshots WHERE client_id = ?1",
                params![owned_client],
            )?;
            tx.execute(
                "DELETE FROM shopify_customer_snapshots WHERE client_id = ?1",
                params![owned_client],
            )?;
            tx.execute(
                "INSERT INTO shopify_sales_sync_state \
                 (client_id, shop_domain_fingerprint, backfill_complete, order_backfill_complete, \
                  customer_backfill_complete, order_backfill_cursor, customer_backfill_cursor, \
                  rate_limited_until_ms, last_error, \
                  last_advanced_at_ms, last_order_count, last_customer_count) \
                 VALUES (?1, ?2, 0, 0, 0, NULL, NULL, 0, NULL, NULL, 0, 0) \
                 ON CONFLICT (client_id) DO UPDATE SET \
                   shop_domain_fingerprint = excluded.shop_domain_fingerprint, \
                   backfill_complete = 0, order_backfill_complete = 0, \
                   customer_backfill_complete = 0, order_backfill_cursor = NULL, \
                   customer_backfill_cursor = NULL, rate_limited_until_ms = 0, last_error = NULL, \
                   last_advanced_at_ms = NULL, last_order_count = 0, last_customer_count = 0",
                params![owned_client, owned_fingerprint],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

pub fn put_sync_state(
    conn: &mut Connection,
    client_id: &str,
    state: &ShopifySalesSyncState,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_sync_state(conn, client_id)?;
    if current == *state {
        return Ok(false);
    }
    let hash = snapshot_hash(&[
        state.shop_domain_fingerprint.as_deref().unwrap_or(""),
        &(state.backfill_complete as u8).to_string(),
        &(state.order_backfill_complete as u8).to_string(),
        &(state.customer_backfill_complete as u8).to_string(),
        &state.rate_limited_until_ms.to_string(),
        state.last_error.as_deref().unwrap_or(""),
        state.order_backfill_cursor.as_deref().unwrap_or(""),
        state.customer_backfill_cursor.as_deref().unwrap_or(""),
        &state.last_advanced_at_ms.unwrap_or(0).to_string(),
        &state.last_order_count.to_string(),
        &state.last_customer_count.to_string(),
    ]);
    let idempotency_key = format!("shopify_sales_sync_state:{hash}");
    let after = serde_json::json!({
        "backfill_complete": state.backfill_complete,
        "order_backfill_complete": state.order_backfill_complete,
        "customer_backfill_complete": state.customer_backfill_complete,
        "order_backfill_cursor": state.order_backfill_cursor,
        "customer_backfill_cursor": state.customer_backfill_cursor,
        "rate_limited_until_ms": state.rate_limited_until_ms,
        "last_error": state.last_error,
        "last_advanced_at_ms": state.last_advanced_at_ms,
        "last_order_count": state.last_order_count,
        "last_customer_count": state.last_customer_count,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned = state.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: SYNC_STATE_ENTITY_KIND,
            entity_id: "shopify_sales",
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
                "INSERT INTO shopify_sales_sync_state \
                 (client_id, shop_domain_fingerprint, backfill_complete, order_backfill_complete, \
                  customer_backfill_complete, order_backfill_cursor, customer_backfill_cursor, \
                  rate_limited_until_ms, last_error, \
                  last_advanced_at_ms, last_order_count, last_customer_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT (client_id) DO UPDATE SET \
                   shop_domain_fingerprint = excluded.shop_domain_fingerprint, \
                   backfill_complete = excluded.backfill_complete, \
                   order_backfill_complete = excluded.order_backfill_complete, \
                   customer_backfill_complete = excluded.customer_backfill_complete, \
                   order_backfill_cursor = excluded.order_backfill_cursor, \
                   customer_backfill_cursor = excluded.customer_backfill_cursor, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms, \
                   last_order_count = excluded.last_order_count, \
                   last_customer_count = excluded.last_customer_count",
                params![
                    owned_client,
                    owned.shop_domain_fingerprint,
                    owned.backfill_complete,
                    owned.order_backfill_complete,
                    owned.customer_backfill_complete,
                    owned.order_backfill_cursor,
                    owned.customer_backfill_cursor,
                    owned.rate_limited_until_ms as i64,
                    owned.last_error,
                    owned.last_advanced_at_ms.map(|ms| ms as i64),
                    owned.last_order_count as i64,
                    owned.last_customer_count as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
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

pub fn upsert_order_snapshots(
    conn: &mut Connection,
    client_id: &str,
    records: &[ShopifyOrderRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let line_items_json = serde_json::to_string(&record.line_items)
            .map_err(|err| StoreError::Domain(err.to_string()))?;
        let line_items_summary = line_items_summary(record);
        let hash = snapshot_hash(&[
            &record.order_number,
            record.customer_email.as_deref().unwrap_or(""),
            record.customer_name.as_deref().unwrap_or(""),
            &record.total.cents.to_string(),
            record.total.currency.as_deref().unwrap_or(""),
            record.financial_status.as_deref().unwrap_or(""),
            record.fulfillment_status.as_deref().unwrap_or(""),
            record.tracking_number.as_deref().unwrap_or(""),
            record.tracking_carrier.as_deref().unwrap_or(""),
            record.tracking_url.as_deref().unwrap_or(""),
            &line_items_summary,
            &line_items_json,
            record.created_at.as_deref().unwrap_or(""),
            record.updated_at.as_deref().unwrap_or(""),
        ]);
        let existing = existing_hash(
            conn,
            "shopify_order_snapshots",
            "provider_order_id",
            client_id,
            &record.order_id,
        )?;
        if existing
            .as_ref()
            .is_some_and(|(current, _)| *current == hash)
        {
            summary.unchanged += 1;
            continue;
        }
        let first_seen = existing.map(|(_, seen)| seen).unwrap_or(now_ms);
        let idempotency_key = format!("shopify_sync:order:{}:{hash}", record.order_id);
        let after = serde_json::json!({
            "order_number": record.order_number,
            "customer_email": record.customer_email,
            "customer_name": record.customer_name,
            "total_cents": record.total.cents,
            "financial_status": record.financial_status,
            "fulfillment_status": record.fulfillment_status,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let owned_summary = line_items_summary;
        let owned_json = line_items_json;
        store_core::mutate(
            conn,
            MutationRequest {
                client_id,
                entity_kind: ORDER_ENTITY_KIND,
                entity_id: &record.order_id,
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
                    "INSERT INTO shopify_order_snapshots \
                     (client_id, provider_order_id, order_number, customer_email, customer_name, \
                      total_cents, currency, financial_status, fulfillment_status, tracking_number, \
                      tracking_carrier, tracking_url, line_items_summary, line_items_json, \
                      order_created_at, provider_updated_at, content_hash, first_seen_at_ms, \
                      last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                             ?16, ?17, ?18, ?19) \
                     ON CONFLICT (client_id, provider_order_id) DO UPDATE SET \
                       order_number = excluded.order_number, \
                       customer_email = excluded.customer_email, customer_name = excluded.customer_name, \
                       total_cents = excluded.total_cents, currency = excluded.currency, \
                       financial_status = excluded.financial_status, \
                       fulfillment_status = excluded.fulfillment_status, \
                       tracking_number = excluded.tracking_number, \
                       tracking_carrier = excluded.tracking_carrier, tracking_url = excluded.tracking_url, \
                       line_items_summary = excluded.line_items_summary, \
                       line_items_json = excluded.line_items_json, \
                       order_created_at = excluded.order_created_at, \
                       provider_updated_at = excluded.provider_updated_at, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.order_id,
                        owned.order_number,
                        owned.customer_email,
                        owned.customer_name,
                        owned.total.cents,
                        owned.total.currency,
                        owned.financial_status,
                        owned.fulfillment_status,
                        owned.tracking_number,
                        owned.tracking_carrier,
                        owned.tracking_url,
                        owned_summary,
                        owned_json,
                        owned.created_at,
                        owned.updated_at,
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
    records: &[ShopifyCustomerRecord],
    now_ms: u64,
) -> Result<UpsertSummary, StoreError> {
    let mut summary = UpsertSummary::default();
    for record in records {
        let tags = record.tags.join(", ");
        let hash = snapshot_hash(&[
            record.email.as_deref().unwrap_or(""),
            record.name.as_deref().unwrap_or(""),
            record.phone.as_deref().unwrap_or(""),
            &record.total_spent.cents.to_string(),
            record.total_spent.currency.as_deref().unwrap_or(""),
            &record.orders_count.to_string(),
            &tags,
            record.tier.as_deref().unwrap_or(""),
        ]);
        let existing = existing_hash(
            conn,
            "shopify_customer_snapshots",
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
        let idempotency_key = format!("shopify_sync:customer:{}:{hash}", record.customer_id);
        let after = serde_json::json!({
            "email": record.email,
            "name": record.name,
            "total_spent_cents": record.total_spent.cents,
            "orders_count": record.orders_count,
            "tier": record.tier,
        })
        .to_string();
        let owned_client = client_id.to_string();
        let owned = record.clone();
        let owned_hash = hash.clone();
        let owned_tags = tags;
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
                    "INSERT INTO shopify_customer_snapshots \
                     (client_id, provider_customer_id, email, name, phone, total_spent_cents, \
                      currency, orders_count, tags, tier, content_hash, first_seen_at_ms, \
                      last_written_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                     ON CONFLICT (client_id, provider_customer_id) DO UPDATE SET \
                       email = excluded.email, name = excluded.name, phone = excluded.phone, \
                       total_spent_cents = excluded.total_spent_cents, \
                       currency = excluded.currency, orders_count = excluded.orders_count, \
                       tags = excluded.tags, tier = excluded.tier, \
                       content_hash = excluded.content_hash, \
                       last_written_at_ms = excluded.last_written_at_ms",
                    params![
                        owned_client,
                        owned.customer_id,
                        owned.email,
                        owned.name,
                        owned.phone,
                        owned.total_spent.cents,
                        owned.total_spent.currency,
                        owned.orders_count,
                        owned_tags,
                        owned.tier,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyOrderSnapshotRow {
    pub order_id: String,
    pub order_number: String,
    pub customer_email: Option<String>,
    pub customer_name: Option<String>,
    pub total_cents: Option<i64>,
    pub currency: Option<String>,
    pub financial_status: Option<String>,
    pub fulfillment_status: Option<String>,
    pub tracking_number: Option<String>,
    pub tracking_carrier: Option<String>,
    pub tracking_url: Option<String>,
    pub line_items_summary: String,
    pub line_items_json: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyCustomerSnapshotRow {
    pub customer_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub total_spent_cents: Option<i64>,
    pub currency: Option<String>,
    pub orders_count: i64,
    pub tags: String,
    pub tier: Option<String>,
}

pub fn list_recent_orders(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    financial_visible: bool,
    limit: usize,
) -> Result<Vec<ShopifyOrderSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_order_id, order_number, customer_email, customer_name, total_cents, \
         currency, financial_status, fulfillment_status, tracking_number, tracking_carrier, \
         tracking_url, line_items_summary, line_items_json, order_created_at \
         FROM shopify_order_snapshots WHERE client_id = ?1 \
         ORDER BY order_created_at DESC, provider_order_id DESC LIMIT ?2",
    )?;
    order_rows(
        &mut stmt,
        params![client_id, limit as i64],
        financial_visible,
    )
}

pub fn orders_by_customer(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    financial_visible: bool,
    email: &str,
    limit: usize,
) -> Result<Vec<ShopifyOrderSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_order_id, order_number, customer_email, customer_name, total_cents, \
         currency, financial_status, fulfillment_status, tracking_number, tracking_carrier, \
         tracking_url, line_items_summary, line_items_json, order_created_at \
         FROM shopify_order_snapshots \
         WHERE client_id = ?1 AND lower(customer_email) = lower(?2) \
         ORDER BY order_created_at DESC, provider_order_id DESC LIMIT ?3",
    )?;
    order_rows(
        &mut stmt,
        params![client_id, email.trim(), limit as i64],
        financial_visible,
    )
}

fn order_rows<P: rusqlite::Params>(
    stmt: &mut rusqlite::Statement<'_>,
    params: P,
    financial_visible: bool,
) -> Result<Vec<ShopifyOrderSnapshotRow>, StoreError> {
    let rows = stmt.query_map(params, |row| {
        let total_cents: i64 = row.get(4)?;
        Ok(ShopifyOrderSnapshotRow {
            order_id: row.get(0)?,
            order_number: row.get(1)?,
            customer_email: row.get(2)?,
            customer_name: row.get(3)?,
            total_cents: financial_visible.then_some(total_cents),
            currency: row.get(5)?,
            financial_status: row.get(6)?,
            fulfillment_status: row.get(7)?,
            tracking_number: row.get(8)?,
            tracking_carrier: row.get(9)?,
            tracking_url: row.get(10)?,
            line_items_summary: row.get(11)?,
            line_items_json: row.get(12)?,
            created_at: row.get(13)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn customers_by_email(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    financial_visible: bool,
    email: &str,
    limit: usize,
) -> Result<Vec<ShopifyCustomerSnapshotRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT provider_customer_id, email, name, phone, total_spent_cents, currency, \
         orders_count, tags, tier \
         FROM shopify_customer_snapshots \
         WHERE client_id = ?1 AND lower(email) = lower(?2) \
         ORDER BY name COLLATE NOCASE ASC, provider_customer_id ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![client_id, email.trim(), limit as i64], |row| {
        let total_spent_cents: i64 = row.get(4)?;
        Ok(ShopifyCustomerSnapshotRow {
            customer_id: row.get(0)?,
            email: row.get(1)?,
            name: row.get(2)?,
            phone: row.get(3)?,
            total_spent_cents: financial_visible.then_some(total_spent_cents),
            currency: row.get(5)?,
            orders_count: row.get(6)?,
            tags: row.get(7)?,
            tier: row.get(8)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn snapshot_counts(conn: &Connection, client_id: &str) -> Result<(u64, u64), StoreError> {
    let orders: i64 = conn.query_row(
        "SELECT COUNT(*) FROM shopify_order_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    let customers: i64 = conn.query_row(
        "SELECT COUNT(*) FROM shopify_customer_snapshots WHERE client_id = ?1",
        params![client_id],
        |row| row.get(0),
    )?;
    Ok((orders as u64, customers as u64))
}

fn line_items_summary(record: &ShopifyOrderRecord) -> String {
    record
        .line_items
        .iter()
        .take(20)
        .map(|item| format!("{}x {}", item.quantity, item.title))
        .collect::<Vec<_>>()
        .join(", ")
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
