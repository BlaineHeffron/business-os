//! Claim persistence: damage-event snapshots (content-hash upserts,
//! receipt-quiet steady state), the claims pump cursor, and claim drafts.
//! Approval flips the draft, enqueues the Gmail-draft outbox job, and
//! inserts the claim-tracking follow-up task (via the follow_up_tasks
//! store's seam) — ONE receipted transaction.

use bos_contracts::claim_drafts::{
    ClaimDraft, ClaimDraftStatus, ClaimDraftWithRevision, ClaimEvidence, ClaimPacketGate,
    ClaimShipmentRefs,
};
use bos_contracts::follow_up_tasks::TaskRecord;
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::stockforge_read::SfDamageEventRecord;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{self, DraftStore, DraftTableSpec};
use crate::slices::shipment_refs::{claim_refs_from_sf, deserialize_refs, serialize_refs};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "claim_draft";
pub const DAMAGE_ENTITY_KIND: &str = "stockforge_damage_snapshot";
pub const CURSOR_ENTITY_KIND: &str = "claims_sync_cursor";
pub const SYNC_ACTOR: &str = "claims_sync_pump";
const APPROVE_SQL: &str = "UPDATE claim_drafts SET status = 'approved', outbox_job_id = ?3, \
     follow_up_task_id = ?4, updated_at_ms = ?5 \
     WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE claim_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: ClaimDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: ClaimDraftStore::NOT_FOUND,
    not_staged_code: "claim_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

// ---------------------------------------------------------------------------
// Damage snapshots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DamageSnapshot {
    pub damage_event_id: String,
    pub shipment_id: String,
    pub reported_at: Option<String>,
    pub reported_by: String,
    pub severity: String,
    pub damage_type: String,
    pub photos: Vec<String>,
    pub description: Option<String>,
    pub claim_status: String,
    pub claim_amount_cents: Option<i64>,
    pub shipment_number: Option<String>,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub shipment_refs: Option<ClaimShipmentRefs>,
    pub pack_photo_urls: Vec<String>,
    pub pack_photos_fetched: bool,
    pub first_seen_at_ms: u64,
}

const DAMAGE_COLUMNS: &str = "damage_event_id, shipment_id, reported_at, reported_by, severity, \
     damage_type, photos_json, description, claim_status, claim_amount_cents, shipment_number, \
     carrier, tracking_number, shipment_refs_json, pack_photos_json, pack_photos_fetched, \
     first_seen_at_ms";

fn damage_from_row(row: &Row<'_>) -> rusqlite::Result<DamageSnapshot> {
    Ok(DamageSnapshot {
        damage_event_id: row.get(0)?,
        shipment_id: row.get(1)?,
        reported_at: row.get(2)?,
        reported_by: row.get(3)?,
        severity: row.get(4)?,
        damage_type: row.get(5)?,
        photos: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
        description: row.get(7)?,
        claim_status: row.get(8)?,
        claim_amount_cents: row.get(9)?,
        shipment_number: row.get(10)?,
        carrier: row.get(11)?,
        tracking_number: row.get(12)?,
        shipment_refs: deserialize_refs(row.get::<_, Option<String>>(13)?.as_deref()),
        pack_photo_urls: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        pack_photos_fetched: row.get(15)?,
        first_seen_at_ms: row.get::<_, i64>(16)? as u64,
    })
}

pub fn get_damage_snapshot(
    conn: &Connection,
    client_id: &str,
    damage_event_id: &str,
) -> Result<Option<DamageSnapshot>, StoreError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {DAMAGE_COLUMNS} FROM stockforge_damage_snapshots \
                 WHERE client_id = ?1 AND damage_event_id = ?2"
            ),
            params![client_id, damage_event_id],
            damage_from_row,
        )
        .optional()?;
    Ok(row)
}

/// OPEN damage snapshots whose pack photos have not been fetched yet —
/// the pump's photo-fetch queue.
pub fn damage_snapshots_needing_photos(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<DamageSnapshot>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DAMAGE_COLUMNS} FROM stockforge_damage_snapshots \
         WHERE client_id = ?1 AND claim_status = 'OPEN' AND pack_photos_fetched = 0 \
         ORDER BY first_seen_at_ms ASC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![client_id, limit as i64], damage_from_row)?;
    let mut snapshots = Vec::new();
    for row in rows {
        snapshots.push(row?);
    }
    Ok(snapshots)
}

/// Content-hash upsert of one fetched damage event. Unchanged rows are
/// skipped BEFORE store_core::mutate (zero receipts). Returns true when a
/// write happened (new or changed event — the pump emits the work item).
pub fn upsert_damage_snapshot(
    conn: &mut Connection,
    client_id: &str,
    record: &SfDamageEventRecord,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let photos_json = serde_json::to_string(&record.photos)
        .map_err(|err| StoreError::Domain(format!("serialize photos: {err}")))?;
    let shipment_refs = claim_refs_from_sf(record.shipment_refs.as_ref());
    let shipment_refs_json = serialize_refs(shipment_refs.as_ref())?;
    let hash = snapshot_hash(&[
        &record.shipment_id,
        record.reported_at.as_deref().unwrap_or(""),
        &record.reported_by,
        &record.severity,
        &record.damage_type,
        &photos_json,
        record.description.as_deref().unwrap_or(""),
        &record.claim_status,
        &record
            .claim_amount_cents
            .map(|cents| cents.to_string())
            .unwrap_or_default(),
        record.shipment_number.as_deref().unwrap_or(""),
        record.carrier.as_deref().unwrap_or(""),
        record.tracking_number.as_deref().unwrap_or(""),
        shipment_refs_json.as_deref().unwrap_or(""),
    ]);
    let existing: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM stockforge_damage_snapshots \
             WHERE client_id = ?1 AND damage_event_id = ?2",
            params![client_id, record.damage_event_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.as_deref() == Some(hash.as_str()) {
        return Ok(false);
    }
    let after = serde_json::json!({
        "shipment_id": record.shipment_id,
        "severity": record.severity,
        "damage_type": record.damage_type,
        "claim_status": record.claim_status,
        "photos": record.photos.len(),
        "tracking_number": record.tracking_number,
        "shipment_refs": shipment_refs,
    })
    .to_string();
    let idempotency_key = format!("claims_sync:damage:{}:{hash}", record.damage_event_id);
    let owned_client = client_id.to_string();
    let owned = record.clone();
    let owned_hash = hash.clone();
    let owned_refs_json = shipment_refs_json.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DAMAGE_ENTITY_KIND,
            entity_id: &record.damage_event_id,
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
                "INSERT INTO stockforge_damage_snapshots \
                 (client_id, damage_event_id, shipment_id, reported_at, reported_by, severity, \
                  damage_type, photos_json, description, claim_status, claim_amount_cents, \
                  shipment_number, carrier, tracking_number, shipment_refs_json, content_hash, first_seen_at_ms, \
                  last_written_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                         ?16, ?17, ?17) \
                 ON CONFLICT (client_id, damage_event_id) DO UPDATE SET \
                   shipment_id = excluded.shipment_id, \
                   reported_at = excluded.reported_at, \
                   reported_by = excluded.reported_by, \
                   severity = excluded.severity, \
                   damage_type = excluded.damage_type, \
                   photos_json = excluded.photos_json, \
                   description = excluded.description, \
                   claim_status = excluded.claim_status, \
                   claim_amount_cents = excluded.claim_amount_cents, \
                   shipment_number = excluded.shipment_number, \
                   carrier = excluded.carrier, \
                   tracking_number = excluded.tracking_number, \
                   shipment_refs_json = excluded.shipment_refs_json, \
                   content_hash = excluded.content_hash, \
                   last_written_at_ms = excluded.last_written_at_ms",
                params![
                    owned_client,
                    owned.damage_event_id,
                    owned.shipment_id,
                    owned.reported_at,
                    owned.reported_by,
                    owned.severity,
                    owned.damage_type,
                    photos_json,
                    owned.description,
                    owned.claim_status,
                    owned.claim_amount_cents,
                    owned.shipment_number,
                    owned.carrier,
                    owned.tracking_number,
                    owned_refs_json,
                    owned_hash,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// Record the pack-station photos fetched for a damage event's shipment
/// (or that there is nothing to fetch — `urls` empty marks done either way).
pub fn set_pack_photos(
    conn: &mut Connection,
    client_id: &str,
    damage_event_id: &str,
    urls: &[String],
    now_ms: u64,
) -> Result<(), StoreError> {
    let urls_json = serde_json::to_string(urls)
        .map_err(|err| StoreError::Domain(format!("serialize pack photos: {err}")))?;
    let idempotency_key = format!(
        "claims_sync:pack_photos:{damage_event_id}:{}",
        snapshot_hash(&[&urls_json])
    );
    let owned_client = client_id.to_string();
    let owned_id = damage_event_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DAMAGE_ENTITY_KIND,
            entity_id: damage_event_id,
            change_kind: "pack_photos",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(serde_json::json!({ "pack_photos": urls.len() }).to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE stockforge_damage_snapshots SET pack_photos_json = ?3, \
                 pack_photos_fetched = 1, last_written_at_ms = ?4 \
                 WHERE client_id = ?1 AND damage_event_id = ?2",
                params![owned_client, owned_id, urls_json, now_ms as i64],
            )?;
            Ok(())
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pump cursor (standdown + error surface)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimsSyncCursor {
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_advanced_at_ms: Option<u64>,
}

pub fn get_cursor(conn: &Connection, client_id: &str) -> Result<ClaimsSyncCursor, StoreError> {
    let row = conn
        .query_row(
            "SELECT rate_limited_until_ms, last_error, last_advanced_at_ms \
             FROM claims_sync_cursors WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok(ClaimsSyncCursor {
                    rate_limited_until_ms: row.get::<_, i64>(0)? as u64,
                    last_error: row.get(1)?,
                    last_advanced_at_ms: row.get::<_, Option<i64>>(2)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

/// Compare-before-write (no receipt when unchanged).
pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    cursor: &ClaimsSyncCursor,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let current = get_cursor(conn, client_id)?;
    if current == *cursor {
        return Ok(false);
    }
    let idempotency_key = format!(
        "claims_cursor:{}",
        snapshot_hash(&[
            &cursor.rate_limited_until_ms.to_string(),
            cursor.last_error.as_deref().unwrap_or(""),
        ])
    );
    let owned_client = client_id.to_string();
    let owned = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CURSOR_ENTITY_KIND,
            entity_id: "claims",
            change_kind: "advance",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({
                    "rate_limited_until_ms": cursor.rate_limited_until_ms,
                    "last_error": cursor.last_error,
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO claims_sync_cursors \
                 (client_id, rate_limited_until_ms, last_error, last_advanced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (client_id) DO UPDATE SET \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_advanced_at_ms = excluded.last_advanced_at_ms",
                params![
                    owned_client,
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

// ---------------------------------------------------------------------------
// Claim drafts
// ---------------------------------------------------------------------------

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.tracking_number, d.carrier, d.shipment_number, d.shipment_context_source, \
     d.shipment_refs_json, d.order_number, d.order_platform, d.external_order_id, d.customer_name, \
     d.order_total_cents, d.ship_date, d.damage_type, d.damage_severity, d.damage_reported_at, \
     d.claim_amount_cents, d.damage_narrative, d.item_description, d.evidence_json, \
     d.packet_ready, d.packet_json, d.provenance_json, d.model, d.confidence, d.outbox_job_id, \
     d.follow_up_task_id, d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<ClaimDraftWithRevision> {
    let packet_ready: bool = row.get("packet_ready")?;
    Ok(ClaimDraftWithRevision {
        draft: ClaimDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            tracking_number: row.get("tracking_number")?,
            carrier: row.get("carrier")?,
            shipment_number: row.get("shipment_number")?,
            shipment_context_source: row.get("shipment_context_source")?,
            shipment_refs: deserialize_refs(
                row.get::<_, Option<String>>("shipment_refs_json")?
                    .as_deref(),
            ),
            order_number: row.get("order_number")?,
            order_platform: row.get("order_platform")?,
            external_order_id: row.get("external_order_id")?,
            customer_name: row.get("customer_name")?,
            order_total_cents: row.get("order_total_cents")?,
            ship_date: row.get("ship_date")?,
            damage_type: row.get("damage_type")?,
            damage_severity: row.get("damage_severity")?,
            damage_reported_at: row.get("damage_reported_at")?,
            claim_amount_cents: row.get("claim_amount_cents")?,
            damage_narrative: row.get("damage_narrative")?,
            item_description: row.get("item_description")?,
            evidence: serde_json::from_str::<ClaimEvidence>(
                &row.get::<_, String>("evidence_json")?,
            )
            .unwrap_or_default(),
            packet: serde_json::from_str::<ClaimPacketGate>(&row.get::<_, String>("packet_json")?)
                .unwrap_or(ClaimPacketGate {
                    ready: packet_ready,
                    missing_roles: Vec::new(),
                }),
            provenance: serde_json::from_str(&row.get::<_, String>("provenance_json")?)
                .unwrap_or_default(),
            model: row.get("model")?,
            confidence: row.get("confidence")?,
            outbox_job_id: row.get("outbox_job_id")?,
            follow_up_task_id: row.get("follow_up_task_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
        outbox_job: None,
    })
}

fn attach_job_summary(
    conn: &Connection,
    client_id: &str,
    mut entry: ClaimDraftWithRevision,
) -> Result<ClaimDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct ClaimDraftStore;

impl DraftStore for ClaimDraftStore {
    type WithRevision = ClaimDraftWithRevision;

    const TABLE: &'static str = "claim_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "claim_draft_not_found";

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision> {
        draft_from_row(row)
    }

    fn attach(
        conn: &Connection,
        client_id: &str,
        entry: Self::WithRevision,
    ) -> Result<Self::WithRevision, StoreError> {
        attach_job_summary(conn, client_id, entry)
    }
}

pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<ClaimDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<ClaimDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<ClaimDraftWithRevision>, StoreError> {
    draft_store::get_draft_unscoped::<ClaimDraftStore>(conn, client_id, draft_id)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ClaimDraftWithRevision>, StoreError> {
    draft_store::list_drafts_unscoped::<ClaimDraftStore>(conn, client_id, item_id, limit)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<ClaimDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft — the queue's "needs you" decoration.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<ClaimDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &ClaimDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let evidence_json = serde_json::to_string(&draft.evidence)
        .map_err(|err| StoreError::Domain(format!("serialize evidence: {err}")))?;
    let packet_json = serde_json::to_string(&draft.packet)
        .map_err(|err| StoreError::Domain(format!("serialize packet gate: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let shipment_refs_json = serialize_refs(draft.shipment_refs.as_ref())?;
    let after = serde_json::json!({
        "tracking_number": draft.tracking_number,
        "carrier": draft.carrier,
        "shipment_context_source": draft.shipment_context_source,
        "shipment_refs": draft.shipment_refs,
        "order_number": draft.order_number,
        "order_platform": draft.order_platform,
        "external_order_id": draft.external_order_id,
        "claim_amount_cents": draft.claim_amount_cents,
        "packet_ready": draft.packet.ready,
        "missing_roles": draft.packet.missing_roles,
        "confidence": draft.confidence,
    })
    .to_string();
    let row = draft.clone();
    let owned_client = client_id.to_string();
    let owned_refs_json = shipment_refs_json.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: &draft.draft_id,
            change_kind: "stage",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&draft.item_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: draft.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO claim_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, status, \
                  tracking_number, carrier, shipment_number, shipment_context_source, \
                  shipment_refs_json, order_number, order_platform, external_order_id, customer_name, \
                  order_total_cents, ship_date, damage_type, damage_severity, \
                  damage_reported_at, claim_amount_cents, damage_narrative, item_description, \
                  evidence_json, packet_ready, packet_json, provenance_json, model, confidence, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                         ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?29)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.tracking_number,
                    row.carrier,
                    row.shipment_number,
                    row.shipment_context_source,
                    owned_refs_json,
                    row.order_number,
                    row.order_platform,
                    row.external_order_id,
                    row.customer_name,
                    row.order_total_cents,
                    row.ship_date,
                    row.damage_type,
                    row.damage_severity,
                    row.damage_reported_at,
                    row.claim_amount_cents,
                    row.damage_narrative,
                    row.item_description,
                    evidence_json,
                    row.packet.ready,
                    packet_json,
                    provenance_json,
                    row.model,
                    row.confidence,
                    row.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("claim_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

/// Approve a staged claim: status flip + Gmail-draft outbox enqueue + the
/// claim-tracking follow-up task, ONE transaction. The packet-completeness
/// gate and the grounded-amount rule are enforced here — an incomplete or
/// amountless packet cannot be approved.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
    task: &TaskRecord,
) -> Result<MutationOutcome, StoreError> {
    let (status, packet_ready, amount) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "claim_draft_not_staged:{status}"
        )));
    }
    if !packet_ready {
        return Err(StoreError::Domain("claim_packet_incomplete".to_string()));
    }
    if amount <= 0 {
        return Err(StoreError::Domain(
            "claim_draft_amount_required".to_string(),
        ));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_job = job.clone();
    let owned_task = task.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "approve",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&job.job_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some(format!(
                "{{\"status\":\"approved\",\"outbox_job_id\":\"{}\",\"follow_up_task_id\":\"{}\"}}",
                job.job_id, task.task_id
            )),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE claim_drafts SET status = 'approved', outbox_job_id = ?3, \
                 follow_up_task_id = ?4, updated_at_ms = ?5 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    owned_job.job_id,
                    owned_task.task_id,
                    now_ms as i64
                ],
            )?;
            outbox::enqueue_within(tx, &owned_client, &owned_job, now_ms)?;
            crate::slices::follow_up_tasks::store::insert_task_within(
                tx,
                &owned_client,
                &owned_task,
                now_ms,
            )?;
            Ok(())
        },
    )
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// Edit a STAGED draft's judgment fields (narrative, item description,
/// claim amount). Shipment/order/evidence fields are immutable — they are
/// cached provider truth. The human IS the grounding for an edited amount.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    damage_narrative_raw: &str,
    item_description_raw: &str,
    claim_amount_cents: i64,
) -> Result<MutationOutcome, StoreError> {
    let (status, _, _) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "claim_draft_not_staged:{status}"
        )));
    }
    let damage_narrative: String = damage_narrative_raw.trim().chars().take(2_000).collect();
    if damage_narrative.is_empty() {
        return Err(StoreError::Domain(
            "claim_draft_narrative_required".to_string(),
        ));
    }
    if claim_amount_cents <= 0 {
        return Err(StoreError::Domain("claim_draft_amount_invalid".to_string()));
    }
    let item_description: String = item_description_raw.trim().chars().take(500).collect();
    let before: serde_json::Value = conn.query_row(
        "SELECT damage_narrative, item_description, claim_amount_cents FROM claim_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "damage_narrative": row.get::<_, String>(0)?,
                "item_description": row.get::<_, String>(1)?,
                "claim_amount_cents": row.get::<_, i64>(2)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "damage_narrative": damage_narrative,
        "item_description": item_description,
        "claim_amount_cents": claim_amount_cents,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "edit",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before.to_string()),
            after_json: Some(after.to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE claim_drafts SET damage_narrative = ?3, item_description = ?4, \
                 claim_amount_cents = ?5, updated_at_ms = ?6 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    damage_narrative,
                    item_description,
                    claim_amount_cents,
                    now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

fn require_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, bool, i64), StoreError> {
    conn.query_row(
        "SELECT status, packet_ready, claim_amount_cents FROM claim_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain("claim_draft_not_found".to_string()))
}

fn status_from_str(raw: &str) -> ClaimDraftStatus {
    match raw {
        "approved" => ClaimDraftStatus::Approved,
        "rejected" => ClaimDraftStatus::Rejected,
        _ => ClaimDraftStatus::Staged,
    }
}

fn snapshot_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DamageStatusMetrics {
    pub damage_event_ids: Vec<String>,
    pub damage_events_in_period: u64,
    pub damage_open: u64,
    pub damage_resolved: u64,
    pub damage_by_severity: Vec<(String, u64)>,
    pub damage_by_status: Vec<(String, u64)>,
    pub damage_by_type: Vec<(String, u64)>,
}

fn damage_window_sql() -> &'static str {
    "COALESCE(substr(d.reported_at, 1, 10), date(d.first_seen_at_ms / 1000, 'unixepoch'))"
}

fn normalize_damage_status(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn normalize_damage_type(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "unspecified".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Damage events in a civil-date window, plus their source status and current
/// BusinessOS queue lifecycle. This is read-only reporting; carrier submission
/// state stays out of BusinessOS until a separate gated integration exists.
pub fn damage_status_metrics_between(
    conn: &Connection,
    client_id: &str,
    start_date: &str,
    end_date: &str,
) -> Result<DamageStatusMetrics, StoreError> {
    let mut metrics = DamageStatusMetrics::default();
    let window = damage_window_sql();
    let mut stmt = conn.prepare(&format!(
        "SELECT d.damage_event_id, d.severity, d.damage_type, d.claim_status \
         FROM stockforge_damage_snapshots d \
         WHERE d.client_id = ?1 AND {window} BETWEEN ?2 AND ?3"
    ))?;
    let rows = stmt.query_map(params![client_id, start_date, end_date], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut by_severity = std::collections::BTreeMap::<String, u64>::new();
    let mut by_status = std::collections::BTreeMap::<String, u64>::new();
    let mut by_type = std::collections::BTreeMap::<String, u64>::new();
    for row in rows {
        let (damage_event_id, severity, damage_type, claim_status) = row?;
        metrics.damage_event_ids.push(damage_event_id);
        metrics.damage_events_in_period += 1;
        let normalized_status = normalize_damage_status(&claim_status);
        if normalized_status == "open" {
            metrics.damage_open += 1;
        } else if normalized_status != "unknown" {
            metrics.damage_resolved += 1;
        }
        *by_severity.entry(severity).or_insert(0) += 1;
        *by_status.entry(normalized_status).or_insert(0) += 1;
        *by_type
            .entry(normalize_damage_type(&damage_type))
            .or_insert(0) += 1;
    }
    metrics.damage_by_severity = by_severity.into_iter().collect();
    metrics.damage_by_status = by_status.into_iter().collect();
    metrics.damage_by_type = by_type.into_iter().collect();
    Ok(metrics)
}

/// Claim drafts staged / approved within an epoch-ms window (start
/// inclusive, end exclusive). Approved uses the approval flip's update time.
pub fn claim_draft_counts_between(
    conn: &Connection,
    client_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<(u64, u64), StoreError> {
    conn.query_row(
        "SELECT \
           COALESCE(SUM(CASE WHEN created_at_ms >= ?2 AND created_at_ms < ?3 \
                             THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN status = 'approved' \
                              AND updated_at_ms >= ?2 AND updated_at_ms < ?3 \
                             THEN 1 ELSE 0 END), 0) \
         FROM claim_drafts WHERE client_id = ?1",
        params![client_id, start_ms as i64, end_ms as i64],
        |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
    )
    .map_err(Into::into)
}

/// Current statuses for claim drafts created during the reporting window.
pub fn claim_draft_status_counts_between(
    conn: &Connection,
    client_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<Vec<(String, u64)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) FROM claim_drafts \
         WHERE client_id = ?1 AND created_at_ms >= ?2 AND created_at_ms < ?3 \
         GROUP BY status ORDER BY status ASC",
    )?;
    let rows = stmt.query_map(params![client_id, start_ms as i64, end_ms as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    })?;
    let mut counts = Vec::new();
    for row in rows {
        counts.push(row?);
    }
    Ok(counts)
}
