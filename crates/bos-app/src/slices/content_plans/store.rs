//! Content plan persistence through store_core. Queueing a plan item updates
//! the plan row and inserts the work item in one mutation; do not call
//! work_queue::store::insert_item here because it opens its own mutation.

use bos_contracts::content_drafts::ContentDraftStatus;
use bos_contracts::content_plans::{
    ContentCampaignLaunchMode, ContentCampaignPublication, ContentCampaignPublicationStatus,
    ContentCampaignPublicationWithRevision, ContentCollisionSummary, ContentInventoryItem,
    ContentInventoryItemWithRevision, ContentInventorySourceKind, ContentInventoryStatus,
    ContentPlanDraftState, ContentPlanItem, ContentPlanItemWithRevision, ContentPlanStatus,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::SocialProposalTarget;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::outbox::{self, NewOutboxJob};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const PLAN_ENTITY_KIND: &str = "content_plan_item";
pub const INVENTORY_ENTITY_KIND: &str = "content_inventory_item";
pub const CAMPAIGN_PUBLICATION_ENTITY_KIND: &str = "content_campaign_publication";
const INVENTORY_REFRESH_ENTITY_KIND: &str = "content_inventory_refresh";
const INVENTORY_REFRESH_ENTITY_ID: &str = "default";

const PLAN_COLUMNS: &str = "p.plan_item_id, p.status, p.topic, p.angle, p.format, \
     p.target_query, p.audience, p.notes, p.work_item_id, p.published_url, \
     p.collision_summary_json, p.created_at_ms, p.updated_at_ms, \
     COALESCE(er.revision, 0) AS revision, d.draft_id AS active_draft_id, \
     d.status AS active_draft_status";

#[derive(Debug, Clone, PartialEq)]
pub struct CollisionCandidate {
    pub inventory_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub work_item_id: Option<String>,
    pub title: String,
    pub target_query: Option<String>,
    pub canonical_key: String,
    pub search_text: String,
    pub bm25_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InventoryProjectionRow {
    pub inventory_id: String,
    pub source_kind: ContentInventorySourceKind,
    pub source_ref: String,
    pub status: ContentInventoryStatus,
    pub title: String,
    pub target_query: Option<String>,
    pub url: Option<String>,
    pub summary: Option<String>,
    pub canonical_key: String,
    pub metrics_json: String,
    pub last_seen_at_ms: Option<u64>,
}

pub fn inventory_id_for(client_id: &str, canonical_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(client_id.as_bytes());
    hasher.update([0]);
    hasher.update(canonical_key.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("inv_");
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn item_from_row(row: &Row<'_>) -> rusqlite::Result<ContentPlanItemWithRevision> {
    let status = status_from_str(&row.get::<_, String>("status")?);
    let summary = row
        .get::<_, Option<String>>("collision_summary_json")?
        .and_then(|raw| serde_json::from_str::<ContentCollisionSummary>(&raw).ok());
    let active_draft_id: Option<String> = row.get("active_draft_id")?;
    let active_draft_status: Option<String> = row.get("active_draft_status")?;
    Ok(ContentPlanItemWithRevision {
        item: ContentPlanItem {
            plan_item_id: row.get("plan_item_id")?,
            status,
            topic: row.get("topic")?,
            angle: row.get("angle")?,
            format: row.get("format")?,
            target_query: row.get("target_query")?,
            audience: row.get("audience")?,
            notes: row.get("notes")?,
            work_item_id: row.get("work_item_id")?,
            published_url: row.get("published_url")?,
            collision_summary: summary,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
        draft_state: draft_state_from_str(active_draft_status.as_deref()),
        active_draft_id,
    })
}

fn inventory_from_row(row: &Row<'_>) -> rusqlite::Result<ContentInventoryItemWithRevision> {
    Ok(ContentInventoryItemWithRevision {
        item: ContentInventoryItem {
            inventory_id: row.get("inventory_id")?,
            source_kind: inventory_source_from_str(&row.get::<_, String>("source_kind")?),
            source_ref: row.get("source_ref")?,
            status: inventory_status_from_str(&row.get::<_, String>("status")?),
            title: row.get("title")?,
            target_query: row.get("target_query")?,
            url: row.get("url")?,
            summary: row.get("summary")?,
            canonical_key: row.get("canonical_key")?,
            metrics_json: row.get("metrics_json")?,
            last_seen_at_ms: row
                .get::<_, Option<i64>>("last_seen_at_ms")?
                .map(|ms| ms as u64),
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

pub fn list_items(
    conn: &Connection,
    client_id: &str,
    status: Option<ContentPlanStatus>,
    limit: usize,
) -> Result<Vec<ContentPlanItemWithRevision>, StoreError> {
    let status = status.map(status_str);
    let mut stmt = conn.prepare(&format!(
        "SELECT {PLAN_COLUMNS} \
         FROM content_plan_items p \
         LEFT JOIN entity_revisions er \
           ON er.client_id = p.client_id AND er.entity_kind = ?2 AND er.entity_id = p.plan_item_id \
         LEFT JOIN content_drafts d \
           ON d.client_id = p.client_id AND d.item_id = p.work_item_id AND d.status != 'rejected' \
         WHERE p.client_id = ?1 AND (?3 IS NULL OR p.status = ?3) \
         ORDER BY p.updated_at_ms DESC, p.plan_item_id DESC LIMIT ?4",
    ))?;
    let rows = stmt.query_map(
        params![client_id, PLAN_ENTITY_KIND, status, limit as i64],
        item_from_row,
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn list_inventory(
    conn: &Connection,
    client_id: &str,
    status: Option<ContentInventoryStatus>,
    limit: usize,
) -> Result<Vec<ContentInventoryItemWithRevision>, StoreError> {
    let status = status.map(inventory_status_str);
    let mut stmt = conn.prepare(
        "SELECT i.inventory_id, i.source_kind, i.source_ref, i.status, i.title, i.target_query, \
                i.url, i.summary, i.canonical_key, i.metrics_json, i.last_seen_at_ms, \
                i.created_at_ms, i.updated_at_ms, COALESCE(er.revision, 0) AS revision \
         FROM content_inventory_items i \
         LEFT JOIN entity_revisions er \
           ON er.client_id = i.client_id AND er.entity_kind = ?2 AND er.entity_id = i.inventory_id \
         WHERE i.client_id = ?1 AND (?3 IS NULL OR i.status = ?3) \
         ORDER BY i.updated_at_ms DESC, i.inventory_id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![client_id, INVENTORY_ENTITY_KIND, status, limit as i64],
        inventory_from_row,
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn get_inventory(
    conn: &Connection,
    client_id: &str,
    inventory_id: &str,
) -> Result<Option<ContentInventoryItemWithRevision>, StoreError> {
    conn.query_row(
        "SELECT i.inventory_id, i.source_kind, i.source_ref, i.status, i.title, i.target_query, \
                i.url, i.summary, i.canonical_key, i.metrics_json, i.last_seen_at_ms, \
                i.created_at_ms, i.updated_at_ms, COALESCE(er.revision, 0) AS revision \
         FROM content_inventory_items i \
         LEFT JOIN entity_revisions er \
           ON er.client_id = i.client_id AND er.entity_kind = ?2 AND er.entity_id = i.inventory_id \
         WHERE i.client_id = ?1 AND i.inventory_id = ?3",
        params![client_id, INVENTORY_ENTITY_KIND, inventory_id],
        inventory_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_item(
    conn: &Connection,
    client_id: &str,
    plan_item_id: &str,
) -> Result<Option<ContentPlanItemWithRevision>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {PLAN_COLUMNS} \
             FROM content_plan_items p \
             LEFT JOIN entity_revisions er \
               ON er.client_id = p.client_id AND er.entity_kind = ?2 AND er.entity_id = p.plan_item_id \
             LEFT JOIN content_drafts d \
               ON d.client_id = p.client_id AND d.item_id = p.work_item_id AND d.status != 'rejected' \
             WHERE p.client_id = ?1 AND p.plan_item_id = ?3",
        ),
        params![client_id, PLAN_ENTITY_KIND, plan_item_id],
        item_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn insert_item(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    item: &ContentPlanItem,
    summary: &ContentCollisionSummary,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let mut receipt_item = item.clone();
    receipt_item.collision_summary = Some(summary.clone());
    let after = serde_json::to_string(&receipt_item)
        .map_err(|err| StoreError::Domain(format!("serialize content plan item: {err}")))?;
    let row = item.clone();
    let summary_json = serde_json::to_string(summary)
        .map_err(|err| StoreError::Domain(format!("serialize collision summary: {err}")))?;
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: PLAN_ENTITY_KIND,
            entity_id: &item.plan_item_id,
            change_kind: "create",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms: item.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO content_plan_items \
                 (client_id, plan_item_id, status, topic, angle, format, target_query, audience, \
                  notes, work_item_id, published_url, collision_summary_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11, ?11)",
                params![
                    owned_client,
                    row.plan_item_id,
                    status_str(row.status),
                    row.topic,
                    row.angle,
                    row.format,
                    row.target_query,
                    row.audience,
                    row.notes,
                    summary_json,
                    row.created_at_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn update_item(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    before: &ContentPlanItem,
    after: &ContentPlanItem,
    summary: &ContentCollisionSummary,
) -> Result<MutationOutcome, StoreError> {
    let before_json = serde_json::to_string(before)
        .map_err(|err| StoreError::Domain(format!("serialize before plan item: {err}")))?;
    let mut receipt_after = after.clone();
    receipt_after.collision_summary = Some(summary.clone());
    let after_json = serde_json::to_string(&receipt_after)
        .map_err(|err| StoreError::Domain(format!("serialize after plan item: {err}")))?;
    let row = after.clone();
    let summary_json = serde_json::to_string(summary)
        .map_err(|err| StoreError::Domain(format!("serialize collision summary: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PLAN_ENTITY_KIND,
            entity_id: &after.plan_item_id,
            change_kind: "update",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&after.plan_item_id),
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE content_plan_items \
                 SET topic = ?3, angle = ?4, format = ?5, target_query = ?6, audience = ?7, \
                     notes = ?8, collision_summary_json = ?9, updated_at_ms = ?10 \
                 WHERE client_id = ?1 AND plan_item_id = ?2 AND status = 'planned'",
                params![
                    owned_client,
                    row.plan_item_id,
                    row.topic,
                    row.angle,
                    row.format,
                    row.target_query,
                    row.audience,
                    row.notes,
                    summary_json,
                    ctx.now_ms as i64,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain("content_plan_not_planned".to_string()));
            }
            Ok(())
        },
    )
}

pub fn persist_check(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    item: &ContentPlanItem,
    summary: &ContentCollisionSummary,
) -> Result<MutationOutcome, StoreError> {
    let summary_json = serde_json::to_string(summary)
        .map_err(|err| StoreError::Domain(format!("serialize collision summary: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    let owned_item = item.plan_item_id.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PLAN_ENTITY_KIND,
            entity_id: &item.plan_item_id,
            change_kind: "check",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&item.plan_item_id),
            causation_id: None,
            before_json: None,
            after_json: Some(summary_json.clone()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE content_plan_items \
                 SET collision_summary_json = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND plan_item_id = ?2",
                params![owned_client, owned_item, summary_json, ctx.now_ms as i64],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain("content_plan_not_found".to_string()));
            }
            Ok(())
        },
    )
}

pub fn queue_item(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    item: &ContentPlanItem,
    summary: &ContentCollisionSummary,
    title: &str,
    work_summary: &str,
) -> Result<MutationOutcome, StoreError> {
    queue_item_with_acceptance(conn, ctx, item, summary, title, work_summary, false)
}

pub fn queue_item_for_generation(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    item: &ContentPlanItem,
    summary: &ContentCollisionSummary,
    title: &str,
    work_summary: &str,
) -> Result<MutationOutcome, StoreError> {
    queue_item_with_acceptance(conn, ctx, item, summary, title, work_summary, true)
}

fn queue_item_with_acceptance(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    item: &ContentPlanItem,
    summary: &ContentCollisionSummary,
    title: &str,
    work_summary: &str,
    accepted: bool,
) -> Result<MutationOutcome, StoreError> {
    if item.status != ContentPlanStatus::Planned {
        return Err(StoreError::Domain("content_plan_not_planned".to_string()));
    }
    let work_item_id = work_item_id(&item.plan_item_id);
    let before_json = serde_json::to_string(item)
        .map_err(|err| StoreError::Domain(format!("serialize before plan item: {err}")))?;
    let after_json = serde_json::json!({
        "status": "queued",
        "work_item_id": work_item_id,
        "collision_summary": summary,
    })
    .to_string();
    let summary_json = serde_json::to_string(summary)
        .map_err(|err| StoreError::Domain(format!("serialize collision summary: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    let owned_plan = item.plan_item_id.clone();
    let owned_title = title.to_string();
    let owned_summary = work_summary.to_string();
    let owned_accept_actor = accepted.then(|| ctx.actor_id.to_string());
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PLAN_ENTITY_KIND,
            entity_id: &item.plan_item_id,
            change_kind: "queue",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&item.plan_item_id),
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            emit_work_item_for_plan(
                tx,
                WorkItemInsert {
                    client_id: &owned_client,
                    plan_item_id: &owned_plan,
                    item_id: &work_item_id,
                    title: &owned_title,
                    summary: &owned_summary,
                    accept_actor: owned_accept_actor.as_deref(),
                    now_ms: ctx.now_ms,
                },
            )?;
            let changed = tx.execute(
                "UPDATE content_plan_items \
                 SET status = 'queued', work_item_id = ?3, collision_summary_json = ?4, updated_at_ms = ?5 \
                 WHERE client_id = ?1 AND plan_item_id = ?2 AND status = 'planned'",
                params![
                    owned_client,
                    owned_plan,
                    work_item_id,
                    summary_json,
                    ctx.now_ms as i64,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain("content_plan_not_planned".to_string()));
            }
            Ok(())
        },
    )
}

pub fn add_manual_inventory(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    row: &InventoryProjectionRow,
) -> Result<MutationOutcome, StoreError> {
    let after_json = serde_json::to_string(row)
        .map_err(|err| StoreError::Domain(format!("serialize inventory row: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    let owned = row.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: INVENTORY_ENTITY_KIND,
            entity_id: &row.inventory_id,
            change_kind: "manual_add",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&row.inventory_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            upsert_inventory_with_fts(tx, &owned_client, &owned, ctx.now_ms, false)?;
            Ok(())
        },
    )
}

pub fn archive_inventory(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    before: &ContentInventoryItem,
) -> Result<MutationOutcome, StoreError> {
    if before.status == ContentInventoryStatus::Archived {
        return Err(StoreError::Domain(
            "content_inventory_already_archived".to_string(),
        ));
    }
    let before_json = serde_json::to_string(before)
        .map_err(|err| StoreError::Domain(format!("serialize before inventory row: {err}")))?;
    let after_json = serde_json::json!({ "status": "archived" }).to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_inventory = before.inventory_id.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: INVENTORY_ENTITY_KIND,
            entity_id: &before.inventory_id,
            change_kind: "archive",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&before.inventory_id),
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE content_inventory_items \
                 SET status = 'archived', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND inventory_id = ?2 AND status != 'archived'",
                params![owned_client, owned_inventory, ctx.now_ms as i64],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain(
                    "content_inventory_not_found".to_string(),
                ));
            }
            tx.execute(
                "DELETE FROM content_inventory_fts WHERE client_id = ?1 AND inventory_id = ?2",
                params![owned_client, owned_inventory],
            )?;
            Ok(())
        },
    )
}

pub fn refresh_inventory(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    rows: &[InventoryProjectionRow],
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let after_json = serde_json::json!({ "refreshed_rows": rows.len() }).to_string();
    let owned_client = client_id.to_string();
    let owned_rows = rows.to_vec();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: INVENTORY_REFRESH_ENTITY_KIND,
            entity_id: INVENTORY_REFRESH_ENTITY_ID,
            change_kind: "refresh",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(INVENTORY_REFRESH_ENTITY_ID),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            for row in &owned_rows {
                upsert_inventory_with_fts(tx, &owned_client, row, now_ms, true)?;
            }
            Ok(())
        },
    )
}

pub fn mark_published(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    before: &ContentPlanItem,
    published_url: &str,
    inventory_row: &InventoryProjectionRow,
) -> Result<MutationOutcome, StoreError> {
    if !matches!(
        before.status,
        ContentPlanStatus::Planned | ContentPlanStatus::Queued
    ) {
        return Err(StoreError::Domain(
            "content_plan_not_publishable".to_string(),
        ));
    }
    let before_json = serde_json::to_string(before)
        .map_err(|err| StoreError::Domain(format!("serialize before plan item: {err}")))?;
    let after_json = serde_json::json!({
        "status": "published",
        "published_url": published_url,
        "inventory_id": inventory_row.inventory_id,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_plan = before.plan_item_id.clone();
    let owned_url = published_url.to_string();
    let owned_inventory = inventory_row.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PLAN_ENTITY_KIND,
            entity_id: &before.plan_item_id,
            change_kind: "mark_published",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&before.plan_item_id),
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE content_plan_items \
                 SET status = 'published', published_url = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND plan_item_id = ?2 AND status IN ('planned', 'queued')",
                params![owned_client, owned_plan, owned_url, ctx.now_ms as i64],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain(
                    "content_plan_not_publishable".to_string(),
                ));
            }
            upsert_inventory_with_fts(tx, &owned_client, &owned_inventory, ctx.now_ms, false)?;
            Ok(())
        },
    )
}

pub fn collision_candidates(
    conn: &Connection,
    client_id: &str,
    exclude_plan_item_id: Option<&str>,
    match_expr: Option<&str>,
    canonical_key: &str,
    target_query: Option<&str>,
) -> Result<Vec<CollisionCandidate>, StoreError> {
    let mut candidates = Vec::new();
    append_inventory_candidates(
        conn,
        client_id,
        &mut candidates,
        match_expr,
        canonical_key,
        target_query,
        40,
    )?;
    append_draft_signal_candidates(
        conn,
        client_id,
        &mut candidates,
        canonical_key,
        target_query,
    )?;
    let mut stmt = conn.prepare(
        "SELECT plan_item_id, topic, target_query, published_url, angle, format, audience, notes, \
                work_item_id \
         FROM content_plan_items \
         WHERE client_id = ?1 AND status != 'cancelled' \
           AND (?2 IS NULL OR plan_item_id != ?2) \
         ORDER BY updated_at_ms DESC, plan_item_id DESC LIMIT 100",
    )?;
    let rows = stmt.query_map(params![client_id, exclude_plan_item_id], |row| {
        let plan_item_id: String = row.get(0)?;
        let topic: String = row.get(1)?;
        let target_query: Option<String> = row.get(2)?;
        let published_url: Option<String> = row.get(3)?;
        let mut text = topic.clone();
        for value in [
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            target_query.clone(),
        ]
        .into_iter()
        .flatten()
        {
            text.push('\n');
            text.push_str(&value);
        }
        Ok(CollisionCandidate {
            inventory_id: format!("plan:{plan_item_id}"),
            source_kind: "plan_item".to_string(),
            source_ref: plan_item_id,
            work_item_id: row.get(8)?,
            title: topic.clone(),
            target_query,
            canonical_key: super::service::canonical_key(published_url.as_deref(), &topic),
            search_text: text,
            bm25_score: None,
        })
    })?;
    for row in rows {
        candidates.push(row?);
    }
    let mut stmt = conn.prepare(
        "SELECT draft_id, item_id, title, target_query, substr(body_markdown, 1, 2000) AS body_preview \
         FROM content_drafts \
         WHERE client_id = ?1 AND status IN ('staged', 'approved') \
         ORDER BY updated_at_ms DESC, draft_id DESC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        let draft_id: String = row.get(0)?;
        let item_id: String = row.get(1)?;
        let title: String = row.get(2)?;
        let target_query: Option<String> = row.get(3)?;
        let body: String = row.get(4)?;
        Ok(CollisionCandidate {
            inventory_id: format!("draft:{draft_id}"),
            source_kind: "content_draft".to_string(),
            source_ref: draft_id,
            work_item_id: Some(item_id),
            title: title.clone(),
            target_query,
            canonical_key: super::service::canonical_key(None, &title),
            search_text: format!("{title}\n{body}"),
            bm25_score: None,
        })
    })?;
    for row in rows {
        candidates.push(row?);
    }
    dedupe_candidates(&mut candidates);
    Ok(candidates)
}

struct WorkItemInsert<'a> {
    client_id: &'a str,
    plan_item_id: &'a str,
    item_id: &'a str,
    title: &'a str,
    summary: &'a str,
    accept_actor: Option<&'a str>,
    now_ms: u64,
}

fn emit_work_item_for_plan(
    tx: &rusqlite::Transaction<'_>,
    work_item: WorkItemInsert<'_>,
) -> Result<(), StoreError> {
    let packet_kinds_json = serde_json::to_string(&vec![
        crate::slices::content_drafts::service::PACKET_KIND.to_string(),
    ])
    .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let status = if work_item.accept_actor.is_some() {
        "accepted"
    } else {
        "open"
    };
    tx.execute(
        "INSERT INTO work_items \
         (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
          packet_kinds_json, status, accept_actor, ai_suggested, rationale, produce_guidance, \
          created_at_ms, updated_at_ms, source_user_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, '', '', ?11, ?11, NULL)",
        params![
            work_item.client_id,
            work_item.item_id,
            crate::slices::content_plans::SOURCE_KIND_CONTENT_PLAN_ITEM,
            work_item.plan_item_id,
            super::service::CATEGORY_ID,
            work_item.title,
            work_item.summary,
            packet_kinds_json,
            status,
            work_item.accept_actor,
            work_item.now_ms as i64,
        ],
    )?;
    store_core::initialize_revision_within(
        tx,
        work_item.client_id,
        crate::slices::work_queue::store::ITEM_ENTITY_KIND,
        work_item.item_id,
        1,
        work_item.now_ms,
    )?;
    Ok(())
}

fn upsert_inventory_with_fts(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    row: &InventoryProjectionRow,
    now_ms: u64,
    preserve_archived: bool,
) -> Result<(), StoreError> {
    let existing_archived = preserve_archived
        && tx
            .query_row(
                "SELECT status = 'archived' FROM content_inventory_items \
                 WHERE client_id = ?1 AND inventory_id = ?2",
                params![client_id, row.inventory_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
    let final_status = if existing_archived {
        ContentInventoryStatus::Archived
    } else {
        row.status
    };
    tx.execute(
        "INSERT INTO content_inventory_items \
         (client_id, inventory_id, source_kind, source_ref, status, title, target_query, url, \
          summary, canonical_key, metrics_json, last_seen_at_ms, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13) \
         ON CONFLICT (client_id, inventory_id) DO UPDATE SET \
           source_kind = excluded.source_kind, \
           source_ref = excluded.source_ref, \
           status = excluded.status, \
           title = excluded.title, \
           target_query = excluded.target_query, \
           url = excluded.url, \
           summary = excluded.summary, \
           canonical_key = excluded.canonical_key, \
           metrics_json = excluded.metrics_json, \
           last_seen_at_ms = excluded.last_seen_at_ms, \
           updated_at_ms = excluded.updated_at_ms",
        params![
            client_id,
            row.inventory_id,
            inventory_source_str(row.source_kind),
            row.source_ref,
            inventory_status_str(final_status),
            row.title,
            row.target_query,
            row.url,
            row.summary,
            row.canonical_key,
            row.metrics_json,
            row.last_seen_at_ms.map(|ms| ms as i64),
            now_ms as i64,
        ],
    )?;
    tx.execute(
        "DELETE FROM content_inventory_fts WHERE client_id = ?1 AND inventory_id = ?2",
        params![client_id, row.inventory_id],
    )?;
    if final_status != ContentInventoryStatus::Archived {
        tx.execute(
            "INSERT INTO content_inventory_fts \
             (client_id, inventory_id, title, target_query, url, summary) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                client_id,
                row.inventory_id,
                row.title,
                row.target_query,
                row.url,
                row.summary
            ],
        )?;
    }
    if store_core::current_revision(tx, client_id, INVENTORY_ENTITY_KIND, &row.inventory_id)?
        .is_none()
    {
        store_core::initialize_revision_within(
            tx,
            client_id,
            INVENTORY_ENTITY_KIND,
            &row.inventory_id,
            1,
            now_ms,
        )?;
    }
    Ok(())
}

fn append_inventory_candidates(
    conn: &Connection,
    client_id: &str,
    candidates: &mut Vec<CollisionCandidate>,
    match_expr: Option<&str>,
    canonical_key: &str,
    target_query: Option<&str>,
    limit: usize,
) -> Result<(), StoreError> {
    if let Some(query) = target_query.filter(|query| !query.trim().is_empty()) {
        let normalized_query = super::service::normalized_phrase(query);
        let mut stmt = conn.prepare(
            "SELECT inventory_id, source_kind, source_ref, title, target_query, canonical_key, \
                    title || char(10) || COALESCE(target_query, '') || char(10) || COALESCE(url, '') || \
                    char(10) || COALESCE(summary, '') AS search_text \
             FROM content_inventory_items \
             WHERE client_id = ?1 AND status != 'archived' AND target_query IS NOT NULL \
             ORDER BY updated_at_ms DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(params![client_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        for row in rows {
            let (inventory_id, source_kind, source_ref, title, row_query, row_key, search_text) =
                row?;
            if row_query
                .as_deref()
                .map(super::service::normalized_phrase)
                .is_some_and(|candidate_query| candidate_query == normalized_query)
            {
                candidates.push(CollisionCandidate {
                    inventory_id,
                    source_kind,
                    source_ref,
                    work_item_id: None,
                    title,
                    target_query: row_query,
                    canonical_key: row_key,
                    search_text,
                    bm25_score: None,
                });
            }
        }
    }

    if !canonical_key.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT inventory_id, source_kind, source_ref, title, target_query, canonical_key, \
                    title || char(10) || COALESCE(target_query, '') || char(10) || COALESCE(url, '') || \
                    char(10) || COALESCE(summary, '') AS search_text \
             FROM content_inventory_items \
             WHERE client_id = ?1 AND status != 'archived' AND canonical_key = ?2 \
             LIMIT 20",
        )?;
        let rows = stmt.query_map(params![client_id, canonical_key], |row| {
            Ok(CollisionCandidate {
                inventory_id: row.get(0)?,
                source_kind: row.get(1)?,
                source_ref: row.get(2)?,
                work_item_id: None,
                title: row.get(3)?,
                target_query: row.get(4)?,
                canonical_key: row.get(5)?,
                search_text: row.get(6)?,
                bm25_score: None,
            })
        })?;
        for row in rows {
            candidates.push(row?);
        }
    }

    let Some(match_expr) = match_expr else {
        dedupe_candidates(candidates);
        return Ok(());
    };
    let mut stmt = conn.prepare(
        "SELECT f.inventory_id, i.source_kind, i.source_ref, i.title, i.target_query, \
                i.canonical_key, \
                i.title || char(10) || COALESCE(i.target_query, '') || char(10) || COALESCE(i.url, '') || \
                char(10) || COALESCE(i.summary, '') AS search_text, \
                bm25(content_inventory_fts, 0.0, 0.0, 5.0, 5.0, 2.0, 1.0) AS score \
         FROM content_inventory_fts f \
         JOIN content_inventory_items i \
           ON i.client_id = f.client_id AND i.inventory_id = f.inventory_id \
         WHERE content_inventory_fts MATCH ?1 AND f.client_id = ?2 AND i.status != 'archived' \
         ORDER BY score ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![match_expr, client_id, limit as i64], |row| {
        Ok(CollisionCandidate {
            inventory_id: row.get(0)?,
            source_kind: row.get(1)?,
            source_ref: row.get(2)?,
            work_item_id: None,
            title: row.get(3)?,
            target_query: row.get(4)?,
            canonical_key: row.get(5)?,
            search_text: row.get(6)?,
            bm25_score: row.get(7)?,
        })
    })?;
    for row in rows {
        candidates.push(row?);
    }
    dedupe_candidates(candidates);
    Ok(())
}

fn append_draft_signal_candidates(
    conn: &Connection,
    client_id: &str,
    candidates: &mut Vec<CollisionCandidate>,
    canonical_key: &str,
    target_query: Option<&str>,
) -> Result<(), StoreError> {
    if target_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .is_none()
        && canonical_key.is_empty()
    {
        return Ok(());
    }
    let normalized_query = target_query
        .map(super::service::normalized_phrase)
        .unwrap_or_default();
    let mut stmt = conn.prepare(
        "SELECT draft_id, item_id, title, target_query, substr(body_markdown, 1, 2000) AS body_preview \
         FROM content_drafts \
         WHERE client_id = ?1 AND status IN ('staged', 'approved') \
           AND (?2 != '' OR ?3 != '')",
    )?;
    let rows = stmt.query_map(params![client_id, normalized_query, canonical_key], |row| {
        let draft_id: String = row.get(0)?;
        let item_id: String = row.get(1)?;
        let title: String = row.get(2)?;
        let row_query: Option<String> = row.get(3)?;
        let body: String = row.get(4)?;
        Ok((draft_id, item_id, title, row_query, body))
    })?;
    for row in rows {
        let (draft_id, item_id, title, row_query, body) = row?;
        let same_query = !normalized_query.is_empty()
            && row_query
                .as_deref()
                .map(super::service::normalized_phrase)
                .is_some_and(|candidate_query| candidate_query == normalized_query);
        let row_key = super::service::canonical_key(None, &title);
        let same_key = !canonical_key.is_empty() && row_key == canonical_key;
        if same_query || same_key {
            candidates.push(CollisionCandidate {
                inventory_id: format!("draft:{draft_id}"),
                source_kind: "content_draft".to_string(),
                source_ref: draft_id,
                work_item_id: Some(item_id),
                title: title.clone(),
                target_query: row_query,
                canonical_key: row_key,
                search_text: format!("{title}\n{body}"),
                bm25_score: None,
            });
        }
    }
    dedupe_candidates(candidates);
    Ok(())
}

fn dedupe_candidates(candidates: &mut Vec<CollisionCandidate>) {
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.inventory_id.clone()));
}

#[derive(Debug, Clone)]
pub struct CampaignPublicationApproval {
    pub publication_id: String,
    pub plan_item_id: String,
    pub content_draft_id: String,
    pub content_draft_revision: u64,
    pub social_proposal_id: Option<String>,
    pub social_proposal_revision: Option<u64>,
    pub expected_canonical_url: String,
    pub launch_mode: ContentCampaignLaunchMode,
    pub selected_channel_ids: Vec<String>,
    pub approved_social_targets: Vec<SocialProposalTarget>,
    pub approved_by: String,
    pub approved_at_ms: u64,
    pub blog_job: NewOutboxJob,
}

#[derive(Debug)]
struct CampaignPublicationRow {
    publication_id: String,
    plan_item_id: String,
    content_draft_id: String,
    content_draft_revision: u64,
    social_proposal_id: Option<String>,
    social_proposal_revision: Option<u64>,
    expected_canonical_url: String,
    actual_canonical_url: Option<String>,
    launch_mode: ContentCampaignLaunchMode,
    selected_channel_ids_json: String,
    approved_social_targets_json: String,
    status: ContentCampaignPublicationStatus,
    review_reason: Option<String>,
    approved_by: String,
    approved_at_ms: u64,
    blog_outbox_job_id: String,
    social_outbox_job_ids_json: String,
    revision: u64,
}

const CAMPAIGN_COLUMNS: &str = "c.publication_id, c.plan_item_id, c.content_draft_id, \
    c.content_draft_revision, c.social_proposal_id, c.social_proposal_revision, \
    c.expected_canonical_url, c.actual_canonical_url, c.launch_mode, \
    c.selected_channel_ids_json, c.approved_social_targets_json, c.status, \
    c.review_reason, c.approved_by, c.approved_at_ms, c.blog_outbox_job_id, \
    c.social_outbox_job_ids_json, COALESCE(er.revision, 0) AS revision";

fn campaign_row(row: &Row<'_>) -> rusqlite::Result<CampaignPublicationRow> {
    Ok(CampaignPublicationRow {
        publication_id: row.get("publication_id")?,
        plan_item_id: row.get("plan_item_id")?,
        content_draft_id: row.get("content_draft_id")?,
        content_draft_revision: row.get::<_, i64>("content_draft_revision")? as u64,
        social_proposal_id: row.get("social_proposal_id")?,
        social_proposal_revision: row
            .get::<_, Option<i64>>("social_proposal_revision")?
            .map(|value| value as u64),
        expected_canonical_url: row.get("expected_canonical_url")?,
        actual_canonical_url: row.get("actual_canonical_url")?,
        launch_mode: launch_mode_from_str(&row.get::<_, String>("launch_mode")?),
        selected_channel_ids_json: row.get("selected_channel_ids_json")?,
        approved_social_targets_json: row.get("approved_social_targets_json")?,
        status: campaign_status_from_str(&row.get::<_, String>("status")?),
        review_reason: row.get("review_reason")?,
        approved_by: row.get("approved_by")?,
        approved_at_ms: row.get::<_, i64>("approved_at_ms")? as u64,
        blog_outbox_job_id: row.get("blog_outbox_job_id")?,
        social_outbox_job_ids_json: row.get("social_outbox_job_ids_json")?,
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

fn hydrate_campaign_publication(
    conn: &Connection,
    client_id: &str,
    row: CampaignPublicationRow,
) -> Result<ContentCampaignPublicationWithRevision, StoreError> {
    let selected_channel_ids = serde_json::from_str(&row.selected_channel_ids_json)
        .map_err(|_| StoreError::Domain("content_campaign_snapshot_invalid".to_string()))?;
    let approved_social_targets = serde_json::from_str(&row.approved_social_targets_json)
        .map_err(|_| StoreError::Domain("content_campaign_snapshot_invalid".to_string()))?;
    let social_outbox_job_ids: Vec<String> = serde_json::from_str(&row.social_outbox_job_ids_json)
        .map_err(|_| StoreError::Domain("content_campaign_snapshot_invalid".to_string()))?;
    let blog_outbox_job = outbox::job_summary(conn, client_id, &row.blog_outbox_job_id)?
        .ok_or_else(|| StoreError::Domain("content_campaign_blog_job_missing".to_string()))?;
    let mut social_outbox_jobs = Vec::with_capacity(social_outbox_job_ids.len());
    for job_id in &social_outbox_job_ids {
        let job = outbox::job_summary(conn, client_id, job_id)?
            .ok_or_else(|| StoreError::Domain("content_campaign_social_job_missing".to_string()))?;
        social_outbox_jobs.push(job);
    }
    Ok(ContentCampaignPublicationWithRevision {
        publication: ContentCampaignPublication {
            publication_id: row.publication_id,
            plan_item_id: row.plan_item_id,
            content_draft_id: row.content_draft_id,
            content_draft_revision: row.content_draft_revision,
            social_proposal_id: row.social_proposal_id,
            social_proposal_revision: row.social_proposal_revision,
            expected_canonical_url: row.expected_canonical_url,
            actual_canonical_url: row.actual_canonical_url,
            launch_mode: row.launch_mode,
            selected_channel_ids,
            approved_social_targets,
            status: row.status,
            review_reason: row.review_reason,
            approved_by: row.approved_by,
            approved_at_ms: row.approved_at_ms,
            blog_outbox_job,
            social_outbox_jobs,
        },
        revision: row.revision,
    })
}

pub fn list_campaign_publications(
    conn: &Connection,
    client_id: &str,
    plan_item_id: &str,
    limit: usize,
) -> Result<Vec<ContentCampaignPublicationWithRevision>, StoreError> {
    let sql = format!(
        "SELECT {CAMPAIGN_COLUMNS} FROM content_campaign_publications c \
         LEFT JOIN entity_revisions er ON er.client_id = c.client_id \
           AND er.entity_kind = ?3 AND er.entity_id = c.publication_id \
         WHERE c.client_id = ?1 AND c.plan_item_id = ?2 \
         ORDER BY c.created_at_ms DESC, c.publication_id DESC LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            client_id,
            plan_item_id,
            CAMPAIGN_PUBLICATION_ENTITY_KIND,
            limit as i64
        ],
        campaign_row,
    )?;
    let mut result = Vec::new();
    for row in rows {
        result.push(hydrate_campaign_publication(conn, client_id, row?)?);
    }
    Ok(result)
}

pub fn campaign_publication_exists(
    conn: &Connection,
    client_id: &str,
    publication_id: &str,
) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM content_campaign_publications \
         WHERE client_id = ?1 AND publication_id = ?2)",
        params![client_id, publication_id],
        |row| row.get(0),
    )?)
}

pub fn awaiting_campaign_publications(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<ContentCampaignPublicationWithRevision>, StoreError> {
    let sql = format!(
        "SELECT {CAMPAIGN_COLUMNS} FROM content_campaign_publications c \
         LEFT JOIN entity_revisions er ON er.client_id = c.client_id \
           AND er.entity_kind = ?2 AND er.entity_id = c.publication_id \
         WHERE c.client_id = ?1 AND ( \
           (c.status = 'awaiting_blog' AND EXISTS ( \
             SELECT 1 FROM outbox_jobs blog_job \
             WHERE blog_job.client_id = c.client_id \
               AND blog_job.job_id = c.blog_outbox_job_id \
               AND blog_job.status <> 'pending' \
           )) \
           OR (c.status = 'social_enqueued' AND ( \
             EXISTS ( \
               SELECT 1 FROM json_each(c.social_outbox_job_ids_json) ids \
               JOIN outbox_jobs social_job \
                 ON social_job.client_id = c.client_id AND social_job.job_id = ids.value \
               WHERE social_job.status IN ('failed_terminal', 'delivery_outcome_unknown') \
             ) \
             OR (json_array_length(c.social_outbox_job_ids_json) > 0 AND NOT EXISTS ( \
               SELECT 1 FROM json_each(c.social_outbox_job_ids_json) ids \
               LEFT JOIN outbox_jobs social_job \
                 ON social_job.client_id = c.client_id AND social_job.job_id = ids.value \
               WHERE social_job.status IS NULL OR social_job.status <> 'delivered' \
             )) \
           )) \
           OR (c.status = 'requires_review' AND c.review_reason = 'social_delivery_failed' \
             AND ( \
               EXISTS ( \
                 SELECT 1 FROM json_each(c.social_outbox_job_ids_json) ids \
                 JOIN outbox_jobs social_job \
                   ON social_job.client_id = c.client_id AND social_job.job_id = ids.value \
                 WHERE social_job.status = 'delivery_outcome_unknown' \
               ) \
               OR (json_array_length(c.social_outbox_job_ids_json) > 0 AND NOT EXISTS ( \
                 SELECT 1 FROM json_each(c.social_outbox_job_ids_json) ids \
                 LEFT JOIN outbox_jobs social_job \
                   ON social_job.client_id = c.client_id AND social_job.job_id = ids.value \
                 WHERE social_job.status IS NULL OR social_job.status <> 'delivered' \
               )) \
             )) \
           OR (c.status = 'requires_review' AND c.review_reason = 'blog_publish_failed' \
             AND EXISTS ( \
               SELECT 1 FROM outbox_jobs blog_job \
               WHERE blog_job.client_id = c.client_id \
                 AND blog_job.job_id = c.blog_outbox_job_id \
                 AND blog_job.status IN ('delivered', 'delivery_outcome_unknown') \
             ))) \
         ORDER BY c.created_at_ms, c.publication_id LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![client_id, CAMPAIGN_PUBLICATION_ENTITY_KIND, limit as i64],
        campaign_row,
    )?;
    let mut result = Vec::new();
    for row in rows {
        result.push(hydrate_campaign_publication(conn, client_id, row?)?);
    }
    Ok(result)
}

#[cfg(test)]
pub fn seed_terminal_social_campaign_for_test(
    conn: &Connection,
    client_id: &str,
    publication_id: &str,
    created_at_ms: u64,
) -> Result<(), StoreError> {
    let blog_job_id = format!("{publication_id}-blog");
    let social_job_id = format!("{publication_id}-social");
    for (job_id, status, result_json) in [
        (
            blog_job_id.as_str(),
            "delivered",
            Some("{\"dry_run\":false,\"provider_object_id\":\"https://example.com/terminal\"}"),
        ),
        (social_job_id.as_str(), "failed_terminal", None),
    ] {
        conn.execute(
            "INSERT INTO outbox_jobs \
             (client_id, job_id, provider, capability, payload_json, status, attempts, \
              next_attempt_at_ms, last_error, result_json, source_entity_kind, source_entity_id, \
              idempotency_key, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, 'buffer', 'create_post', '{}', ?3, 1, 0, \
                     'terminal', ?4, 'test', ?2, ?2, ?5, ?5)",
            params![client_id, job_id, status, result_json, created_at_ms as i64],
        )?;
    }
    conn.execute(
        "INSERT INTO content_campaign_publications \
         (client_id, publication_id, plan_item_id, content_draft_id, content_draft_revision, \
          expected_canonical_url, actual_canonical_url, launch_mode, selected_channel_ids_json, \
          approved_social_targets_json, status, review_reason, approved_by, approved_at_ms, \
          blog_outbox_job_id, social_outbox_job_ids_json, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, 1, 'https://example.com/terminal', \
                 'https://example.com/terminal', 'publish_now', '[]', '[]', \
                 'requires_review', 'social_delivery_failed', 'operator', ?5, ?6, ?7, ?5, ?5)",
        params![
            client_id,
            publication_id,
            format!("{publication_id}-plan"),
            format!("{publication_id}-draft"),
            created_at_ms as i64,
            blog_job_id,
            serde_json::to_string(&vec![social_job_id])
                .map_err(|err| StoreError::Domain(format!("serialize test job ids: {err}")))?,
        ],
    )?;
    Ok(())
}

pub fn social_proposal_campaign_locked(
    conn: &Connection,
    client_id: &str,
    proposal_id: &str,
) -> Result<bool, StoreError> {
    social_proposal_campaign_locked_except(conn, client_id, proposal_id, None)
}

pub fn social_proposal_campaign_locked_except(
    conn: &Connection,
    client_id: &str,
    proposal_id: &str,
    excluded_publication_id: Option<&str>,
) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM content_campaign_publications \
         WHERE client_id = ?1 AND social_proposal_id = ?2 \
           AND (?3 IS NULL OR publication_id <> ?3) \
           AND status IN ('awaiting_blog', 'social_enqueued', 'completed', 'requires_review'))",
        params![client_id, proposal_id, excluded_publication_id],
        |row| row.get(0),
    )?)
}

pub fn content_draft_campaign_locked(
    conn: &Connection,
    client_id: &str,
    content_draft_id: &str,
) -> Result<bool, StoreError> {
    content_draft_campaign_locked_except(conn, client_id, content_draft_id, None)
}

pub fn content_draft_campaign_locked_except(
    conn: &Connection,
    client_id: &str,
    content_draft_id: &str,
    excluded_publication_id: Option<&str>,
) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM content_campaign_publications \
         WHERE client_id = ?1 AND content_draft_id = ?2 \
           AND (?3 IS NULL OR publication_id <> ?3) \
           AND status IN ('awaiting_blog', 'social_enqueued', 'completed', 'requires_review'))",
        params![client_id, content_draft_id, excluded_publication_id],
        |row| row.get(0),
    )?)
}

pub fn insert_campaign_publication(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    approval: &CampaignPublicationApproval,
) -> Result<MutationOutcome, StoreError> {
    let selected_json = serde_json::to_string(&approval.selected_channel_ids)
        .map_err(|err| StoreError::Domain(format!("serialize selected channels: {err}")))?;
    let targets_json = serde_json::to_string(&approval.approved_social_targets)
        .map_err(|err| StoreError::Domain(format!("serialize approved social targets: {err}")))?;
    let after = serde_json::json!({
        "plan_item_id": approval.plan_item_id,
        "content_draft_id": approval.content_draft_id,
        "content_draft_revision": approval.content_draft_revision,
        "social_proposal_id": approval.social_proposal_id,
        "social_proposal_revision": approval.social_proposal_revision,
        "expected_canonical_url": approval.expected_canonical_url,
        "selected_channel_ids": approval.selected_channel_ids,
        "launch_mode": launch_mode_str(approval.launch_mode),
        "status": "awaiting_blog",
        "blog_outbox_job_id": approval.blog_job.job_id,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let row = approval.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: CAMPAIGN_PUBLICATION_ENTITY_KIND,
            entity_id: &approval.publication_id,
            change_kind: "approve_and_publish_blog",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&approval.plan_item_id),
            causation_id: Some(&approval.content_draft_id),
            before_json: None,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO content_campaign_publications \
                 (client_id, publication_id, plan_item_id, content_draft_id, \
                  content_draft_revision, social_proposal_id, social_proposal_revision, \
                  expected_canonical_url, launch_mode, selected_channel_ids_json, \
                  approved_social_targets_json, status, approved_by, approved_at_ms, \
                  blog_outbox_job_id, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, \
                         'awaiting_blog', ?12, ?13, ?14, ?13, ?13)",
                params![
                    owned_client,
                    row.publication_id,
                    row.plan_item_id,
                    row.content_draft_id,
                    row.content_draft_revision as i64,
                    row.social_proposal_id,
                    row.social_proposal_revision.map(|value| value as i64),
                    row.expected_canonical_url,
                    launch_mode_str(row.launch_mode),
                    selected_json,
                    targets_json,
                    row.approved_by,
                    row.approved_at_ms as i64,
                    row.blog_job.job_id,
                ],
            )?;
            outbox::enqueue_within(tx, &owned_client, &row.blog_job, row.approved_at_ms)?;
            Ok(())
        },
    )
}

pub struct CampaignSettlement<'a> {
    pub status: ContentCampaignPublicationStatus,
    pub actual_canonical_url: Option<&'a str>,
    pub review_reason: Option<&'a str>,
    pub social_jobs: &'a [NewOutboxJob],
    pub now_ms: u64,
}

pub fn settle_campaign_publication(
    conn: &mut Connection,
    client_id: &str,
    current: &ContentCampaignPublicationWithRevision,
    settlement: CampaignSettlement<'_>,
) -> Result<MutationOutcome, StoreError> {
    let publication_id = &current.publication.publication_id;
    let status_raw = campaign_status_str(settlement.status);
    let job_ids = if settlement.social_jobs.is_empty() {
        current
            .publication
            .social_outbox_jobs
            .iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>()
    } else {
        settlement
            .social_jobs
            .iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>()
    };
    let job_ids_json = serde_json::to_string(&job_ids)
        .map_err(|err| StoreError::Domain(format!("serialize social job ids: {err}")))?;
    let after = serde_json::json!({
        "status": status_raw,
        "actual_canonical_url": settlement.actual_canonical_url,
        "review_reason": settlement.review_reason,
        "social_outbox_job_ids": job_ids,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_publication = publication_id.clone();
    let owned_url = settlement.actual_canonical_url.map(str::to_string);
    let owned_reason = settlement.review_reason.map(str::to_string);
    let owned_jobs = settlement.social_jobs.to_vec();
    let owned_now_ms = settlement.now_ms;
    let prior_status = campaign_status_str(current.publication.status);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CAMPAIGN_PUBLICATION_ENTITY_KIND,
            entity_id: publication_id,
            change_kind: "settle_campaign_publication",
            actor_id: "content_campaign_coordinator",
            actor_kind: ActorKindDto::System,
            expected_revision: Some(current.revision),
            idempotency_key: &format!(
                "campaign-settle:{publication_id}:{status_raw}:{}",
                current.revision
            ),
            correlation_id: Some(&current.publication.plan_item_id),
            causation_id: Some(&current.publication.blog_outbox_job.job_id),
            before_json: None,
            after_json: Some(after),
            now_ms: settlement.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE content_campaign_publications SET status = ?3, \
                 actual_canonical_url = ?4, review_reason = ?5, \
                 social_outbox_job_ids_json = ?6, updated_at_ms = ?7 \
                 WHERE client_id = ?1 AND publication_id = ?2 AND status = ?8",
                params![
                    owned_client,
                    owned_publication,
                    status_raw,
                    owned_url,
                    owned_reason,
                    job_ids_json,
                    owned_now_ms as i64,
                    prior_status,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain(
                    "content_campaign_publication_state_changed".to_string(),
                ));
            }
            for job in &owned_jobs {
                outbox::enqueue_within(tx, &owned_client, job, owned_now_ms)?;
            }
            Ok(())
        },
    )
}

fn launch_mode_str(mode: ContentCampaignLaunchMode) -> &'static str {
    match mode {
        ContentCampaignLaunchMode::PublishNow => "publish_now",
        ContentCampaignLaunchMode::Schedule => "schedule",
    }
}

fn launch_mode_from_str(raw: &str) -> ContentCampaignLaunchMode {
    if raw == "schedule" {
        ContentCampaignLaunchMode::Schedule
    } else {
        ContentCampaignLaunchMode::PublishNow
    }
}

fn campaign_status_str(status: ContentCampaignPublicationStatus) -> &'static str {
    match status {
        ContentCampaignPublicationStatus::AwaitingBlog => "awaiting_blog",
        ContentCampaignPublicationStatus::BlogDryRun => "blog_dry_run",
        ContentCampaignPublicationStatus::SocialEnqueued => "social_enqueued",
        ContentCampaignPublicationStatus::Completed => "completed",
        ContentCampaignPublicationStatus::RequiresReview => "requires_review",
    }
}

fn campaign_status_from_str(raw: &str) -> ContentCampaignPublicationStatus {
    match raw {
        "blog_dry_run" => ContentCampaignPublicationStatus::BlogDryRun,
        "social_enqueued" => ContentCampaignPublicationStatus::SocialEnqueued,
        "completed" => ContentCampaignPublicationStatus::Completed,
        "requires_review" => ContentCampaignPublicationStatus::RequiresReview,
        _ => ContentCampaignPublicationStatus::AwaitingBlog,
    }
}

pub fn work_item_id(plan_item_id: &str) -> String {
    format!(
        "wi_{}_{}",
        crate::slices::content_plans::SOURCE_KIND_CONTENT_PLAN_ITEM,
        plan_item_id
    )
}

fn inventory_source_str(source: ContentInventorySourceKind) -> &'static str {
    match source {
        ContentInventorySourceKind::PlanItem => "plan_item",
        ContentInventorySourceKind::SearchConsolePage => "search_console_page",
        ContentInventorySourceKind::Manual => "manual",
    }
}

fn inventory_source_from_str(raw: &str) -> ContentInventorySourceKind {
    match raw {
        "plan_item" => ContentInventorySourceKind::PlanItem,
        "search_console_page" => ContentInventorySourceKind::SearchConsolePage,
        _ => ContentInventorySourceKind::Manual,
    }
}

fn inventory_status_str(status: ContentInventoryStatus) -> &'static str {
    match status {
        ContentInventoryStatus::Pipeline => "pipeline",
        ContentInventoryStatus::Published => "published",
        ContentInventoryStatus::Archived => "archived",
    }
}

fn inventory_status_from_str(raw: &str) -> ContentInventoryStatus {
    match raw {
        "published" => ContentInventoryStatus::Published,
        "archived" => ContentInventoryStatus::Archived,
        _ => ContentInventoryStatus::Pipeline,
    }
}

fn status_str(status: ContentPlanStatus) -> &'static str {
    match status {
        ContentPlanStatus::Planned => "planned",
        ContentPlanStatus::Queued => "queued",
        ContentPlanStatus::Published => "published",
        ContentPlanStatus::Cancelled => "cancelled",
    }
}

fn status_from_str(raw: &str) -> ContentPlanStatus {
    match raw {
        "queued" => ContentPlanStatus::Queued,
        "published" => ContentPlanStatus::Published,
        "cancelled" => ContentPlanStatus::Cancelled,
        _ => ContentPlanStatus::Planned,
    }
}

fn draft_state_from_str(raw: Option<&str>) -> ContentPlanDraftState {
    match raw {
        Some("staged") => ContentPlanDraftState::Staged,
        Some("approved") => ContentPlanDraftState::Approved,
        _ => ContentPlanDraftState::None,
    }
}

#[allow(dead_code)]
fn draft_status_str(status: ContentDraftStatus) -> &'static str {
    match status {
        ContentDraftStatus::Staged => "staged",
        ContentDraftStatus::Approved => "approved",
        ContentDraftStatus::Rejected => "rejected",
    }
}
