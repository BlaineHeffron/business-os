//! Work item + policy persistence through store_core.

use bos_contracts::email_triage::{EmailTriageGmailCategory, FALLBACK_CATEGORY_ID};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::work_queue::{
    WorkItem, WorkItemAcceptActor, WorkItemAssignActionKind, WorkItemStatus, WorkItemWithRevision,
    WorkQueueAiGmailScope, WorkQueuePolicy,
};
use rusqlite::{params, Connection, OptionalExtension};

use crate::http::OperatorScope;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const ITEM_ENTITY_KIND: &str = "work_item";
pub const POLICY_ENTITY_KIND: &str = "work_queue_policy";
pub const AGENT_LAUNCH_ENTITY_KIND: &str = "work_item_agent_launch";
pub const PRODUCE_GUIDANCE_MAX_CHARS: usize = 2_000;

pub fn list_items(
    conn: &Connection,
    client_id: &str,
    status: Option<WorkItemStatus>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<WorkItemWithRevision>, StoreError> {
    let status_str = status.map(status_str);
    let (scope_pred, scope_all, scope_user) = visibility_sql_filter(scope, 5, 6);
    let mut stmt = conn.prepare(&format!(
        "SELECT w.item_id, w.source_kind, w.source_ref, w.category_id, w.title, w.summary, \
         w.packet_kinds_json, w.status, w.accept_actor, w.created_at_ms, w.updated_at_ms, \
         COALESCE(er.revision, 0) AS revision, w.ai_suggested, w.rationale, w.source_user_id, \
         w.produce_guidance, w.assignee_user_id \
         FROM work_items w \
         LEFT JOIN entity_revisions er \
           ON er.client_id = w.client_id AND er.entity_kind = ?2 AND er.entity_id = w.item_id \
         WHERE w.client_id = ?1 AND (?3 IS NULL OR w.status = ?3) \
           AND {scope_pred} \
         ORDER BY w.created_at_ms DESC, w.item_id DESC LIMIT ?4",
    ))?;
    let rows = stmt.query_map(
        params![
            client_id,
            ITEM_ENTITY_KIND,
            status_str,
            limit as i64,
            scope_all,
            scope_user
        ],
        |row| {
            Ok(WorkItemWithRevision {
                item: WorkItem {
                    item_id: row.get("item_id")?,
                    source_kind: row.get("source_kind")?,
                    source_ref: row.get("source_ref")?,
                    category_id: row.get("category_id")?,
                    title: row.get("title")?,
                    summary: row.get("summary")?,
                    packet_kinds: serde_json::from_str(&row.get::<_, String>("packet_kinds_json")?)
                        .unwrap_or_default(),
                    status: status_from_str(&row.get::<_, String>("status")?),
                    accept_actor: row
                        .get::<_, Option<String>>("accept_actor")?
                        .as_deref()
                        .and_then(accept_actor_from_str),
                    ai_suggested: row.get("ai_suggested")?,
                    rationale: row.get("rationale")?,
                    source_user_id: row.get("source_user_id")?,
                    produce_guidance: row.get("produce_guidance")?,
                    assignee_user_id: row.get("assignee_user_id")?,
                    visible_to_user_ids: Vec::new(),
                    created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
                    updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
                },
                revision: row.get::<_, i64>("revision")? as u64,
                // Decoration happens in the service feed (cross-slice signal).
                staged_draft_kinds: Vec::new(),
                pending_produce_kinds: Vec::new(),
                failure_notifications: Vec::new(),
                attention: None,
            })
        },
    )?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    drop(stmt);
    let visibility_by_item = visibility_by_item_ids(conn, client_id, &items)?;
    for entry in &mut items {
        entry.item.visible_to_user_ids = visibility_by_item
            .get(&entry.item.item_id)
            .cloned()
            .unwrap_or_else(|| {
                entry
                    .item
                    .source_user_id
                    .iter()
                    .filter(|user_id| !user_id.trim().is_empty())
                    .cloned()
                    .collect()
            });
    }
    Ok(items)
}

pub fn get_item_unscoped(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<WorkItemWithRevision>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT w.item_id, w.source_kind, w.source_ref, w.category_id, w.title, w.summary, \
         w.packet_kinds_json, w.status, w.accept_actor, w.created_at_ms, w.updated_at_ms, \
         COALESCE(er.revision, 0) AS revision, w.ai_suggested, w.rationale, w.source_user_id, \
         w.produce_guidance, w.assignee_user_id \
         FROM work_items w \
         LEFT JOIN entity_revisions er \
           ON er.client_id = w.client_id AND er.entity_kind = ?2 AND er.entity_id = w.item_id \
         WHERE w.client_id = ?1 AND w.item_id = ?3",
    )?;
    let row = stmt
        .query_row(params![client_id, ITEM_ENTITY_KIND, item_id], |row| {
            Ok(WorkItemWithRevision {
                item: WorkItem {
                    item_id: row.get("item_id")?,
                    source_kind: row.get("source_kind")?,
                    source_ref: row.get("source_ref")?,
                    category_id: row.get("category_id")?,
                    title: row.get("title")?,
                    summary: row.get("summary")?,
                    packet_kinds: serde_json::from_str(&row.get::<_, String>("packet_kinds_json")?)
                        .unwrap_or_default(),
                    status: status_from_str(&row.get::<_, String>("status")?),
                    accept_actor: row
                        .get::<_, Option<String>>("accept_actor")?
                        .as_deref()
                        .and_then(accept_actor_from_str),
                    ai_suggested: row.get("ai_suggested")?,
                    rationale: row.get("rationale")?,
                    source_user_id: row.get("source_user_id")?,
                    produce_guidance: row.get("produce_guidance")?,
                    assignee_user_id: row.get("assignee_user_id")?,
                    visible_to_user_ids: Vec::new(),
                    created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
                    updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
                },
                revision: row.get::<_, i64>("revision")? as u64,
                staged_draft_kinds: Vec::new(),
                pending_produce_kinds: Vec::new(),
                failure_notifications: Vec::new(),
                attention: None,
            })
        })
        .optional()?;
    row.map(|mut entry| {
        entry.item.visible_to_user_ids = item_visible_user_ids(conn, client_id, &entry.item)?;
        Ok(entry)
    })
    .transpose()
}

pub fn get_item_scoped(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    scope: &OperatorScope,
) -> Result<Option<WorkItemWithRevision>, StoreError> {
    match get_item_unscoped(conn, client_id, item_id)? {
        Some(entry) if item_visible_to_scope(conn, client_id, &entry.item, scope)? => {
            Ok(Some(entry))
        }
        _ => Ok(None),
    }
}

pub fn item_exists_for_source(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
) -> Result<bool, StoreError> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM work_items \
         WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3",
        params![client_id, source_kind, source_ref],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

pub fn get_item_for_source(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
) -> Result<Option<WorkItemWithRevision>, StoreError> {
    let item_id = conn
        .query_row(
            "SELECT item_id FROM work_items \
             WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3",
            params![client_id, source_kind, source_ref],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match item_id {
        Some(item_id) => get_item_unscoped(conn, client_id, &item_id),
        None => Ok(None),
    }
}

pub fn get_item_for_source_user(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_ref: &str,
    source_user_id: Option<&str>,
) -> Result<Option<WorkItemWithRevision>, StoreError> {
    let item_id = conn
        .query_row(
            "SELECT item_id FROM work_items \
             WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3 \
               AND ((?4 IS NULL AND source_user_id IS NULL) OR source_user_id = ?4) \
             ORDER BY created_at_ms ASC, item_id ASC LIMIT 1",
            params![client_id, source_kind, source_ref, source_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match item_id {
        Some(item_id) => get_item_unscoped(conn, client_id, &item_id),
        None => Ok(None),
    }
}

pub fn status_counts_for_sources(
    conn: &Connection,
    client_id: &str,
    source_kind: &str,
    source_refs: &[String],
) -> Result<Vec<(String, u64)>, StoreError> {
    let mut counts = std::collections::BTreeMap::<String, u64>::new();
    let mut stmt = conn.prepare(
        "SELECT status FROM work_items \
         WHERE client_id = ?1 AND source_kind = ?2 AND source_ref = ?3",
    )?;
    for source_ref in source_refs {
        let status = stmt
            .query_row(params![client_id, source_kind, source_ref], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        if let Some(status) = status {
            *counts.entry(status).or_insert(0) += 1;
        }
    }
    Ok(counts.into_iter().collect())
}

/// Insert a new open work item (system actor — emitted by ingestion).
/// Idempotency key derives from the source, so re-emits replay quietly.
pub fn insert_item(
    conn: &mut Connection,
    client_id: &str,
    item: &WorkItem,
) -> Result<MutationOutcome, StoreError> {
    let idempotency_key = format!("emit:{}:{}", item.source_kind, item.source_ref);
    insert_item_with_actor(
        conn,
        client_id,
        item,
        "work_queue_emitter",
        ActorKindDto::System,
        &idempotency_key,
    )
}

pub fn insert_item_with_actor(
    conn: &mut Connection,
    client_id: &str,
    item: &WorkItem,
    actor_id: &str,
    actor_kind: ActorKindDto,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(item)
        .map_err(|err| StoreError::Domain(format!("serialize work item: {err}")))?;
    let packet_kinds_json = serde_json::to_string(&item.packet_kinds)
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let status = status_str(item.status);
    let row = item.clone();
    let visibility_user_ids = effective_visibility_user_ids(item);
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: &item.item_id,
            change_kind: "emit",
            actor_id,
            actor_kind,
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
                "INSERT INTO work_items \
                 (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
                  packet_kinds_json, status, accept_actor, ai_suggested, rationale, produce_guidance, \
                  created_at_ms, updated_at_ms, source_user_id, assignee_user_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15, ?16) \
                 ON CONFLICT (client_id, item_id) DO NOTHING",
                params![
                    owned_client,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.category_id,
                    row.title,
                    row.summary,
                    packet_kinds_json,
                    status,
                    row.accept_actor.map(accept_actor_str),
                    row.ai_suggested,
                    row.rationale,
                    row.produce_guidance,
                    row.created_at_ms as i64,
                    row.source_user_id,
                    row.assignee_user_id,
                ],
            )?;
            insert_visibility_rows_within(
                tx,
                &owned_client,
                &row.item_id,
                &visibility_user_ids,
                row.created_at_ms,
            )?;
            Ok(())
        },
    )
}

pub fn ensure_visibility_user_ids(
    conn: &mut Connection,
    client_id: &str,
    item_id: &str,
    user_ids: &[String],
    actor_id: &str,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let mut additions = Vec::new();
    for user_id in user_ids {
        let user_id = user_id.trim();
        if user_id.is_empty()
            || additions
                .iter()
                .any(|existing: &String| existing == user_id)
            || visibility_row_exists(conn, client_id, item_id, user_id)?
        {
            continue;
        }
        additions.push(user_id.to_string());
    }
    if additions.is_empty() {
        return Ok(false);
    }
    let before = item_visible_user_ids_for_source(conn, client_id, item_id, None)?;
    let mut after = before.clone();
    for user_id in &additions {
        if !after.contains(user_id) {
            after.push(user_id.clone());
        }
    }
    after.sort_by_key(|value| value.to_ascii_lowercase());
    let before_json = serde_json::json!({ "visible_to_user_ids": before }).to_string();
    let after_json = serde_json::json!({ "visible_to_user_ids": after }).to_string();
    let owned_client = client_id.to_string();
    let owned_item = item_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "set_visibility",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| insert_visibility_rows_within(tx, &owned_client, &owned_item, &additions, now_ms),
    )?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAction {
    Accept,
    Dismiss,
    Reopen,
}

impl ItemAction {
    fn change_kind(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Dismiss => "dismiss",
            Self::Reopen => "reopen",
        }
    }

    fn target_status(self) -> &'static str {
        match self {
            Self::Accept => "accepted",
            Self::Dismiss => "dismissed",
            Self::Reopen => "open",
        }
    }

    fn target_accept_actor(self) -> Option<WorkItemAcceptActor> {
        match self {
            Self::Accept => Some(WorkItemAcceptActor::Operator),
            Self::Dismiss | Self::Reopen => None,
        }
    }
}

pub use crate::slices::mutation_context::ScopedMutationContext as ItemActionContext;

pub fn apply_item_action(
    conn: &mut Connection,
    ctx: ItemActionContext<'_>,
    item_id: &str,
    action: ItemAction,
) -> Result<MutationOutcome, StoreError> {
    let current: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT status, source_user_id, accept_actor FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![ctx.client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((current, source_user_id, before_accept_actor)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    require_item_visible(
        conn,
        ctx.client_id,
        item_id,
        source_user_id.as_deref(),
        ctx.scope,
    )?;
    let owned_item = item_id.to_string();
    let owned_client = ctx.client_id.to_string();
    let next_accept_actor = action.target_accept_actor().map(accept_actor_str);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: action.change_kind(),
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "status": current,
                    "accept_actor": before_accept_actor,
                })
                .to_string(),
            ),
            after_json: Some(
                serde_json::json!({
                    "status": action.target_status(),
                    "accept_actor": next_accept_actor,
                })
                .to_string(),
            ),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items SET status = ?3, accept_actor = ?4, updated_at_ms = ?5 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![
                    owned_client,
                    owned_item,
                    action.target_status(),
                    next_accept_actor,
                    ctx.now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

pub fn system_accept_item(
    conn: &mut Connection,
    client_id: &str,
    item_id: &str,
    actor_id: &str,
    expected_revision: Option<u64>,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let current: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status, accept_actor FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((current, before_accept_actor)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    let owned_item = item_id.to_string();
    let owned_client = client_id.to_string();
    let next_accept_actor = Some(accept_actor_str(WorkItemAcceptActor::System));
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "system_accept",
            actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(
                serde_json::json!({
                    "status": current,
                    "accept_actor": before_accept_actor,
                })
                .to_string(),
            ),
            after_json: Some(
                serde_json::json!({
                    "status": "accepted",
                    "accept_actor": next_accept_actor,
                })
                .to_string(),
            ),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items SET status = 'accepted', accept_actor = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![owned_client, owned_item, next_accept_actor, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

/// Replace an item's suggested packet kinds (operator tuning what gets
/// produced). Kinds must exist in the catalog; dismissed items refuse —
/// reopen first if it deserves work after all.
pub fn update_packet_kinds(
    conn: &mut Connection,
    ctx: ItemActionContext<'_>,
    item_id: &str,
    packet_kinds: &[String],
) -> Result<MutationOutcome, StoreError> {
    for kind in packet_kinds {
        if !super::packet_kind_exists(kind) {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_unknown:{kind}"
            )));
        }
    }
    let mut deduped: Vec<String> = Vec::new();
    for kind in packet_kinds {
        if !deduped.contains(kind) {
            deduped.push(kind.clone());
        }
    }
    let current: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT status, packet_kinds_json, source_user_id FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![ctx.client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((status, before_kinds_json, source_user_id)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    require_item_visible(
        conn,
        ctx.client_id,
        item_id,
        source_user_id.as_deref(),
        ctx.scope,
    )?;
    if status == "dismissed" {
        return Err(StoreError::Domain(
            "work_item_kinds_not_editable".to_string(),
        ));
    }
    let packet_kinds_json = serde_json::to_string(&deduped)
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let owned_item = item_id.to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_kinds_json = packet_kinds_json.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "set_kinds",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(format!("{{\"packet_kinds\":{before_kinds_json}}}")),
            after_json: Some(format!("{{\"packet_kinds\":{packet_kinds_json}}}")),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items SET packet_kinds_json = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![
                    owned_client,
                    owned_item,
                    owned_kinds_json,
                    ctx.now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

/// Append AI-suggested packet kinds to an existing item. Existing kinds win;
/// dismissed items stay closed, and a suggestion that adds nothing is receipt-quiet.
pub fn append_ai_packet_kinds(
    conn: &mut Connection,
    client_id: &str,
    item_id: &str,
    packet_kinds: &[String],
    rationale: &str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    for kind in packet_kinds {
        if !super::packet_kind_exists(kind) {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_unknown:{kind}"
            )));
        }
    }
    let current: Option<(String, String, bool, String)> = conn
        .query_row(
            "SELECT status, packet_kinds_json, ai_suggested, rationale FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((status, before_kinds_json, before_ai_suggested, before_rationale)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    if status == "dismissed" {
        return Ok(false);
    }
    let mut merged: Vec<String> = serde_json::from_str(&before_kinds_json).unwrap_or_default();
    let before_len = merged.len();
    for kind in packet_kinds {
        if !merged.contains(kind) {
            merged.push(kind.clone());
        }
    }
    if merged.len() == before_len {
        return Ok(false);
    }
    let packet_kinds_json = serde_json::to_string(&merged)
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let idempotency_key = format!("ai_suggest_kinds:{item_id}:{}", merged.join(","));
    let before_json = serde_json::json!({
        "packet_kinds": serde_json::from_str::<serde_json::Value>(&before_kinds_json)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
        "ai_suggested": before_ai_suggested,
        "rationale": before_rationale,
    })
    .to_string();
    let after_json = serde_json::json!({
        "packet_kinds": merged,
        "ai_suggested": true,
        "rationale": rationale,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_item = item_id.to_string();
    let owned_kinds_json = packet_kinds_json;
    let owned_rationale = rationale.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "ai_add_kinds",
            actor_id: "ai_triage_pass",
            actor_kind: ActorKindDto::Agent,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: Some(item_id),
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items \
                 SET packet_kinds_json = ?3, ai_suggested = 1, rationale = ?4, updated_at_ms = ?5 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![
                    owned_client,
                    owned_item,
                    owned_kinds_json,
                    owned_rationale,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(true)
}

/// Replace the operator-authored context that every produce-stage LLM call for
/// this item receives. Dismissed items refuse edits because they are no longer
/// on the production path.
pub fn update_produce_guidance(
    conn: &mut Connection,
    ctx: ItemActionContext<'_>,
    item_id: &str,
    produce_guidance: &str,
) -> Result<MutationOutcome, StoreError> {
    let produce_guidance = produce_guidance.trim();
    if produce_guidance.chars().count() > PRODUCE_GUIDANCE_MAX_CHARS {
        return Err(StoreError::Domain(
            "work_item_guidance_too_long".to_string(),
        ));
    }
    let current: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT status, produce_guidance, source_user_id FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![ctx.client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((status, before_guidance, source_user_id)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    require_item_visible(
        conn,
        ctx.client_id,
        item_id,
        source_user_id.as_deref(),
        ctx.scope,
    )?;
    if status == "dismissed" {
        return Err(StoreError::Domain(
            "work_item_guidance_not_editable".to_string(),
        ));
    }
    let before_json = serde_json::json!({ "produce_guidance": before_guidance }).to_string();
    let after_json = serde_json::json!({ "produce_guidance": produce_guidance }).to_string();
    let owned_item = item_id.to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_guidance = produce_guidance.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "set_guidance",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items SET produce_guidance = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![owned_client, owned_item, owned_guidance, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn update_assignment(
    conn: &mut Connection,
    ctx: ItemActionContext<'_>,
    item_id: &str,
    action: WorkItemAssignActionKind,
    requested_assignee_user_id: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    let current: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT source_user_id, assignee_user_id FROM work_items \
             WHERE client_id = ?1 AND item_id = ?2",
            params![ctx.client_id, item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((source_user_id, before_assignee)) = current else {
        return Err(StoreError::Domain("work_item_not_found".to_string()));
    };
    require_item_visible(
        conn,
        ctx.client_id,
        item_id,
        source_user_id.as_deref(),
        ctx.scope,
    )?;
    let visible_user_ids =
        item_visible_user_ids_for_source(conn, ctx.client_id, item_id, source_user_id.as_deref())?;
    let next_assignee = match action {
        WorkItemAssignActionKind::AssignToMe => match ctx.scope {
            OperatorScope::User(user_id) => Some(user_id.clone()),
            OperatorScope::All => {
                return Err(StoreError::Domain(
                    "work_queue_assignment_named_user_required".to_string(),
                ));
            }
        },
        WorkItemAssignActionKind::AssignToUser => Some(
            requested_assignee_user_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| StoreError::Domain("work_queue_assignee_required".to_string()))?
                .to_string(),
        ),
        WorkItemAssignActionKind::Unassign => {
            if let OperatorScope::User(user_id) = ctx.scope {
                if before_assignee.as_deref() != Some(user_id.as_str()) {
                    return Err(StoreError::Domain(
                        "work_queue_unassign_forbidden".to_string(),
                    ));
                }
            }
            None
        }
    };
    if let Some(assignee) = next_assignee.as_deref() {
        require_active_operator_user(conn, ctx.client_id, assignee)?;
        let assign_to_user_target_allowed = action != WorkItemAssignActionKind::AssignToUser
            || visible_user_ids.iter().any(|id| id == assignee);
        if !assign_to_user_target_allowed {
            return Err(StoreError::Domain(
                "work_queue_assignee_not_visible".to_string(),
            ));
        }
    }
    let before_json = serde_json::json!({ "assignee_user_id": before_assignee }).to_string();
    let after_json = serde_json::json!({ "assignee_user_id": next_assignee }).to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_item = item_id.to_string();
    let owned_assignee = next_assignee.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: ITEM_ENTITY_KIND,
            entity_id: item_id,
            change_kind: "assign",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(before_json),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE work_items SET assignee_user_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND item_id = ?2",
                params![owned_client, owned_item, owned_assignee, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub struct AgentLaunchRequestContext<'a> {
    pub client_id: &'a str,
    pub item_id: &'a str,
    pub actor_id: &'a str,
    pub operator_context: &'a str,
    pub job: &'a crate::outbox::NewOutboxJob,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn record_agent_launch_request(
    conn: &mut Connection,
    ctx: AgentLaunchRequestContext<'_>,
) -> Result<MutationOutcome, StoreError> {
    let after_json = serde_json::json!({
        "item_id": ctx.item_id,
        "context_chars": ctx.operator_context.chars().count(),
        "outbox_job_id": ctx.job.job_id,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_job = ctx.job.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: AGENT_LAUNCH_ENTITY_KIND,
            entity_id: ctx.item_id,
            change_kind: "launch_agent",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(ctx.item_id),
            causation_id: None,
            before_json: None,
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| crate::outbox::enqueue_within(tx, &owned_client, &owned_job, ctx.now_ms),
    )
}

pub fn list_policies(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<WorkQueuePolicy>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT category_id, create_work_item, packet_kinds_json, auto_produce, \
            ai_suggestible_packet_kinds_json, ai_suggestible_gmail_scope, \
            ai_suggestible_gmail_categories_json \
         FROM work_queue_policies \
         WHERE client_id = ?1 ORDER BY category_id ASC",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        Ok(policy_from_parts(
            row.get(0)?,
            row.get(1)?,
            row.get::<_, String>(2)?,
            row.get(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut policies = Vec::new();
    for row in rows {
        policies.push(row?);
    }
    Ok(policies)
}

pub fn policy_for_category(
    conn: &Connection,
    client_id: &str,
    category_id: &str,
) -> Result<Option<WorkQueuePolicy>, StoreError> {
    let row = conn
        .query_row(
            "SELECT create_work_item, packet_kinds_json, auto_produce, \
                ai_suggestible_packet_kinds_json, ai_suggestible_gmail_scope, \
                ai_suggestible_gmail_categories_json \
             FROM work_queue_policies \
             WHERE client_id = ?1 AND category_id = ?2",
            params![client_id, category_id],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(
        |(
            create_work_item,
            packet_kinds_json,
            auto_produce,
            ai_suggestible_packet_kinds_json,
            ai_suggestible_gmail_scope,
            ai_suggestible_gmail_categories_json,
        )| {
            policy_from_parts(
                category_id.to_string(),
                create_work_item,
                packet_kinds_json,
                auto_produce,
                ai_suggestible_packet_kinds_json,
                ai_suggestible_gmail_scope,
                ai_suggestible_gmail_categories_json,
            )
        },
    ))
}

fn policy_from_parts(
    category_id: String,
    create_work_item: bool,
    packet_kinds_json: String,
    auto_produce: bool,
    ai_suggestible_packet_kinds_json: String,
    ai_suggestible_gmail_scope: String,
    ai_suggestible_gmail_categories_json: String,
) -> WorkQueuePolicy {
    let scope = match ai_suggestible_gmail_scope.as_str() {
        "all" => WorkQueueAiGmailScope::All,
        "selected" => WorkQueueAiGmailScope::Selected,
        _ => WorkQueueAiGmailScope::Default,
    };
    WorkQueuePolicy {
        category_id,
        create_work_item,
        packet_kinds: serde_json::from_str(&packet_kinds_json).unwrap_or_default(),
        auto_produce,
        ai_suggestible_packet_kinds: serde_json::from_str(&ai_suggestible_packet_kinds_json)
            .unwrap_or_default(),
        ai_suggestible_gmail_scope: scope,
        ai_suggestible_gmail_categories: serde_json::from_str(
            &ai_suggestible_gmail_categories_json,
        )
        .unwrap_or_default(),
    }
}

pub(crate) fn sanitize_policy(policy: &WorkQueuePolicy) -> Result<WorkQueuePolicy, StoreError> {
    if policy.category_id.trim().is_empty() {
        return Err(StoreError::Domain(
            "work_queue_category_required".to_string(),
        ));
    }
    for kind in &policy.packet_kinds {
        if !super::packet_kind_exists(kind) {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_unknown:{kind}"
            )));
        }
    }
    let ai_suggest_all = bos_contracts::work_queue::AI_SUGGEST_ALL_SENTINEL;
    if policy
        .ai_suggestible_packet_kinds
        .iter()
        .any(|kind| kind == ai_suggest_all)
        && policy.ai_suggestible_packet_kinds.len() != 1
    {
        return Err(StoreError::Domain(
            "work_queue_ai_suggest_all_exclusive".to_string(),
        ));
    }
    for kind in &policy.ai_suggestible_packet_kinds {
        // The all-or-nothing sentinel ("any enabled kind") is not a catalog id.
        if kind == ai_suggest_all {
            continue;
        }
        if !super::packet_kind_exists(kind) {
            return Err(StoreError::Domain(format!(
                "work_queue_packet_kind_unknown:{kind}"
            )));
        }
    }
    let mut sanitized = policy.clone();
    if !sanitized.create_work_item {
        sanitized.packet_kinds.clear();
        sanitized.ai_suggestible_packet_kinds.clear();
        sanitized.ai_suggestible_gmail_categories.clear();
        sanitized.ai_suggestible_gmail_scope = WorkQueueAiGmailScope::Default;
        sanitized.auto_produce = false;
    }
    if sanitized.ai_suggestible_packet_kinds.is_empty() {
        sanitized.ai_suggestible_gmail_categories.clear();
        sanitized.ai_suggestible_gmail_scope = WorkQueueAiGmailScope::Default;
    } else if sanitized.category_id == FALLBACK_CATEGORY_ID {
        match sanitized.ai_suggestible_gmail_scope {
            WorkQueueAiGmailScope::Default => {
                sanitized.ai_suggestible_gmail_categories = vec![
                    EmailTriageGmailCategory::Primary,
                    EmailTriageGmailCategory::Updates,
                ];
            }
            WorkQueueAiGmailScope::All => {
                sanitized.ai_suggestible_gmail_categories.clear();
            }
            WorkQueueAiGmailScope::Selected => {
                if sanitized.ai_suggestible_gmail_categories.is_empty() {
                    return Err(StoreError::Domain(
                        "work_queue_ai_gmail_scope_selected_empty".to_string(),
                    ));
                }
            }
        }
    }
    if sanitized.category_id != FALLBACK_CATEGORY_ID
        && (sanitized.ai_suggestible_gmail_scope != WorkQueueAiGmailScope::Default
            || !sanitized.ai_suggestible_gmail_categories.is_empty())
    {
        return Err(StoreError::Domain(
            "work_queue_ai_gmail_scope_fallback_only".to_string(),
        ));
    }
    Ok(sanitized)
}

pub(crate) fn write_policy_tx(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    policy: &WorkQueuePolicy,
    now_ms: u64,
) -> Result<(), StoreError> {
    let packet_kinds_json = serde_json::to_string(&policy.packet_kinds)
        .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let ai_suggestible_packet_kinds_json =
        serde_json::to_string(&policy.ai_suggestible_packet_kinds)
            .map_err(|err| StoreError::Domain(format!("serialize packet kinds: {err}")))?;
    let ai_suggestible_gmail_scope = match policy.ai_suggestible_gmail_scope {
        WorkQueueAiGmailScope::Default => "default",
        WorkQueueAiGmailScope::All => "all",
        WorkQueueAiGmailScope::Selected => "selected",
    };
    let ai_suggestible_gmail_categories_json =
        serde_json::to_string(&policy.ai_suggestible_gmail_categories)
            .map_err(|err| StoreError::Domain(format!("serialize gmail categories: {err}")))?;
    tx.execute(
        "INSERT INTO work_queue_policies \
         (client_id, category_id, create_work_item, packet_kinds_json, auto_produce, \
          ai_suggestible_packet_kinds_json, ai_suggestible_gmail_scope, \
          ai_suggestible_gmail_categories_json, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT (client_id, category_id) DO UPDATE SET \
           create_work_item = excluded.create_work_item, \
           packet_kinds_json = excluded.packet_kinds_json, \
           auto_produce = excluded.auto_produce, \
           ai_suggestible_packet_kinds_json = excluded.ai_suggestible_packet_kinds_json, \
           ai_suggestible_gmail_scope = excluded.ai_suggestible_gmail_scope, \
           ai_suggestible_gmail_categories_json = excluded.ai_suggestible_gmail_categories_json, \
           updated_at_ms = excluded.updated_at_ms",
        params![
            client_id,
            policy.category_id,
            policy.create_work_item,
            packet_kinds_json,
            policy.auto_produce,
            ai_suggestible_packet_kinds_json,
            ai_suggestible_gmail_scope,
            ai_suggestible_gmail_categories_json,
            now_ms as i64
        ],
    )?;
    Ok(())
}

pub fn upsert_policy(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    policy: &WorkQueuePolicy,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let sanitized = sanitize_policy(policy)?;
    let after = serde_json::to_string(&sanitized)
        .map_err(|err| StoreError::Domain(format!("serialize policy: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_category_id = sanitized.category_id.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: POLICY_ENTITY_KIND,
            entity_id: &owned_category_id,
            change_kind: "upsert",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: None,
            idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| write_policy_tx(tx, &owned_client, &sanitized, now_ms),
    )
}

fn status_str(status: WorkItemStatus) -> &'static str {
    match status {
        WorkItemStatus::Open => "open",
        WorkItemStatus::Accepted => "accepted",
        WorkItemStatus::Dismissed => "dismissed",
    }
}

fn visibility_sql_filter(
    scope: &OperatorScope,
    all_idx: usize,
    user_idx: usize,
) -> (String, i64, String) {
    let (scope_all, scope_user) = scope.sql_params();
    (
        format!(
            "(?{all_idx} = 1 OR w.source_user_id = ?{user_idx} OR EXISTS (\
             SELECT 1 FROM work_item_visibility v \
             WHERE v.client_id = w.client_id AND v.item_id = w.item_id \
               AND v.user_id = ?{user_idx}))"
        ),
        scope_all,
        scope_user,
    )
}

fn item_visible_to_scope(
    conn: &Connection,
    client_id: &str,
    item: &WorkItem,
    scope: &OperatorScope,
) -> Result<bool, StoreError> {
    match scope {
        OperatorScope::All => Ok(true),
        OperatorScope::User(user_id) => {
            if item.source_user_id.as_deref() == Some(user_id.as_str()) {
                return Ok(true);
            }
            visibility_row_exists(conn, client_id, &item.item_id, user_id)
        }
    }
}

fn require_item_visible(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    source_user_id: Option<&str>,
    scope: &OperatorScope,
) -> Result<(), StoreError> {
    let visible = match scope {
        OperatorScope::All => true,
        OperatorScope::User(user_id) => {
            source_user_id == Some(user_id.as_str())
                || visibility_row_exists(conn, client_id, item_id, user_id)?
        }
    };
    if visible {
        Ok(())
    } else {
        Err(StoreError::Domain("scope_forbidden".to_string()))
    }
}

fn visibility_row_exists(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    user_id: &str,
) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM work_item_visibility \
         WHERE client_id = ?1 AND item_id = ?2 AND user_id = ?3",
        params![client_id, item_id, user_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn item_visible_user_ids(
    conn: &Connection,
    client_id: &str,
    item: &WorkItem,
) -> Result<Vec<String>, StoreError> {
    item_visible_user_ids_for_source(
        conn,
        client_id,
        &item.item_id,
        item.source_user_id.as_deref(),
    )
}

fn item_visible_user_ids_for_source(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
    source_user_id: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT user_id FROM work_item_visibility \
         WHERE client_id = ?1 AND item_id = ?2 ORDER BY user_id COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map(params![client_id, item_id], |row| row.get::<_, String>(0))?;
    let mut user_ids = Vec::new();
    for row in rows {
        let user_id = row?;
        if !user_ids.contains(&user_id) {
            user_ids.push(user_id);
        }
    }
    if user_ids.is_empty() {
        if let Some(source_user_id) = source_user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            user_ids.push(source_user_id.to_string());
        }
    }
    Ok(user_ids)
}

fn visibility_by_item_ids(
    conn: &Connection,
    client_id: &str,
    items: &[WorkItemWithRevision],
) -> Result<std::collections::HashMap<String, Vec<String>>, StoreError> {
    if items.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = vec!["?"; items.len()].join(", ");
    let sql = format!(
        "SELECT item_id, user_id FROM work_item_visibility \
         WHERE client_id = ? AND item_id IN ({placeholders}) \
         ORDER BY item_id ASC, user_id COLLATE NOCASE ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(items.len() + 1);
    binds.push(&client_id);
    for entry in items {
        binds.push(&entry.item.item_id);
    }
    let rows = stmt.query_map(binds.as_slice(), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut by_item = std::collections::HashMap::<String, Vec<String>>::new();
    for row in rows {
        let (item_id, user_id) = row?;
        let entry = by_item.entry(item_id).or_default();
        if !entry.contains(&user_id) {
            entry.push(user_id);
        }
    }
    Ok(by_item)
}

fn effective_visibility_user_ids(item: &WorkItem) -> Vec<String> {
    let mut user_ids = Vec::new();
    for user_id in &item.visible_to_user_ids {
        let trimmed = user_id.trim();
        if !trimmed.is_empty() && !user_ids.iter().any(|existing| existing == trimmed) {
            user_ids.push(trimmed.to_string());
        }
    }
    if user_ids.is_empty() {
        if let Some(source_user_id) = item
            .source_user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            user_ids.push(source_user_id.to_string());
        }
    }
    user_ids
}

fn insert_visibility_rows_within(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    item_id: &str,
    user_ids: &[String],
    now_ms: u64,
) -> Result<(), StoreError> {
    for user_id in user_ids {
        tx.execute(
            "INSERT OR IGNORE INTO work_item_visibility \
             (client_id, item_id, user_id, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![client_id, item_id, user_id, now_ms as i64],
        )?;
    }
    Ok(())
}

fn require_active_operator_user(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<(), StoreError> {
    let active: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM operator_users \
             WHERE client_id = ?1 AND user_id = ?2 AND active = 1 AND archived_at_ms IS NULL",
            params![client_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    if active.is_some() {
        Ok(())
    } else {
        Err(StoreError::Domain(
            "work_queue_assignee_not_active".to_string(),
        ))
    }
}

pub fn accept_actor_str(actor: WorkItemAcceptActor) -> &'static str {
    match actor {
        WorkItemAcceptActor::Operator => "operator",
        WorkItemAcceptActor::System => "system",
    }
}

fn accept_actor_from_str(raw: &str) -> Option<WorkItemAcceptActor> {
    match raw {
        "operator" => Some(WorkItemAcceptActor::Operator),
        "system" => Some(WorkItemAcceptActor::System),
        _ => None,
    }
}

fn status_from_str(raw: &str) -> WorkItemStatus {
    match raw {
        "accepted" => WorkItemStatus::Accepted,
        "dismissed" => WorkItemStatus::Dismissed,
        _ => WorkItemStatus::Open,
    }
}
