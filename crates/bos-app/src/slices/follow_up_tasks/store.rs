//! Follow-up draft + local task persistence through store_core. Approval
//! writes the task row in the SAME mutation transaction that flips the draft.

use bos_contracts::follow_up_tasks::{
    FollowUpDraft, FollowUpDraftStatus, FollowUpDraftWithRevision, TaskRecord, TaskStatus,
    TaskWithRevision,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::http::OperatorScope;
use crate::slices::draft_store::{
    self, DraftStore, DraftTableSpec, ScopedDraftStore, ScopedStatusDraftStore,
};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "follow_up_task_draft";
pub const TASK_ENTITY_KIND: &str = "task";
const APPROVE_SQL: &str = "UPDATE follow_up_task_drafts SET status = 'approved', task_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str =
    "UPDATE follow_up_task_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: FollowUpDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: FollowUpDraftStore::NOT_FOUND,
    not_staged_code: "follow_up_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, \
     d.source_user_id, d.status, d.title, d.due_date, d.context, d.provenance_json, d.model, \
     d.confidence, d.task_id, d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<FollowUpDraftWithRevision> {
    Ok(FollowUpDraftWithRevision {
        draft: FollowUpDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            source_user_id: row.get("source_user_id")?,
            status: draft_status_from_str(&row.get::<_, String>("status")?),
            title: row.get("title")?,
            due_date: row.get("due_date")?,
            context: row.get("context")?,
            provenance: serde_json::from_str(&row.get::<_, String>("provenance_json")?)
                .unwrap_or_default(),
            model: row.get("model")?,
            confidence: row.get("confidence")?,
            task_id: row.get("task_id")?,
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

struct FollowUpDraftStore;

impl DraftStore for FollowUpDraftStore {
    type WithRevision = FollowUpDraftWithRevision;

    const TABLE: &'static str = "follow_up_task_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "follow_up_draft_not_found";

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision> {
        draft_from_row(row)
    }
}

impl ScopedDraftStore for FollowUpDraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str> {
        entry.draft.source_user_id.as_deref()
    }
}

impl ScopedStatusDraftStore for FollowUpDraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?))
    }
}

/// The one staged-or-approved draft for an item, if any.
pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<FollowUpDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<FollowUpDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<FollowUpDraftWithRevision>, StoreError> {
    draft_store::get_draft_scoped::<FollowUpDraftStore>(conn, client_id, draft_id, scope)
}

/// Drafts newest-first, optionally scoped to one work item.
pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<FollowUpDraftWithRevision>, StoreError> {
    draft_store::list_drafts_scoped::<FollowUpDraftStore>(conn, client_id, item_id, limit, scope)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<FollowUpDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<FollowUpDraftStore>(conn, client_id)
}

/// Stage a freshly produced draft. The unique active-draft index turns a
/// produce race into a domain error rather than a duplicate.
pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &FollowUpDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let row = draft.clone();
    let owned_client = client_id.to_string();
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
                "INSERT INTO follow_up_task_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, source_user_id, status, \
                  title, due_date, context, provenance_json, model, confidence, created_at_ms, \
                  updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.source_user_id,
                    row.title,
                    row.due_date,
                    row.context,
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
                    StoreError::Domain("follow_up_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::ScopedMutationContext as DraftActionContext;

/// Approve a staged draft: status flip + local task insert, one transaction.
/// This IS the write — follow-up tasks live here, no provider involved.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    task: &TaskRecord,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_draft_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "follow_up_draft_not_staged:{current}"
        )));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
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
            correlation_id: Some(&task.task_id),
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some(format!(
                "{{\"status\":\"approved\",\"task_id\":\"{}\"}}",
                task.task_id
            )),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE follow_up_task_drafts SET status = 'approved', task_id = ?3, \
                 updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2",
                params![owned_client, owned_draft, owned_task.task_id, now_ms as i64],
            )?;
            insert_task_within(tx, &owned_client, &owned_task, now_ms)?;
            Ok(())
        },
    )
}

/// Insert a task row inside an existing receipted transaction — the seam
/// for verticals whose approval spawns a tracking task (claim_drafts) as
/// well as this slice's own approve path. Tasks stay owned by this slice;
/// other slices never write the tasks table directly.
pub fn insert_task_within(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    task: &TaskRecord,
    now_ms: u64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO tasks \
         (client_id, task_id, title, due_date, context, source_kind, source_ref, \
          source_user_id, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?9)",
        params![
            client_id,
            task.task_id,
            task.title,
            task.due_date,
            task.context,
            task.source_kind,
            task.source_ref,
            task.source_user_id,
            now_ms as i64,
        ],
    )?;
    crate::store_core::initialize_revision_within(
        tx,
        client_id,
        TASK_ENTITY_KIND,
        &task.task_id,
        1,
        now_ms,
    )?;
    Ok(())
}

/// Reject a staged draft (frees the item for a re-produce).
pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

// --- tasks ---

/// Edit a STAGED draft's AI-filled fields ("AI-produced fields remain
/// editable until accepted"; full replacement, receipted). Approval builds
/// the task from the stored row, so edits flow into the created task.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    title_raw: &str,
    due_date_raw: Option<&str>,
    context_raw: &str,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_draft_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "follow_up_draft_not_staged:{current}"
        )));
    }
    let fields = normalize_editable_fields(title_raw, due_date_raw, context_raw, ctx.now_ms)?;
    let title = fields.title;
    let due_date = fields.due_date;
    let context = fields.context;
    let before: serde_json::Value = conn.query_row(
        "SELECT title, due_date, context FROM follow_up_task_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "title": row.get::<_, String>(0)?,
                "due_date": row.get::<_, Option<String>>(1)?,
                "context": row.get::<_, String>(2)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "title": title, "due_date": due_date, "context": context,
    });
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_due = due_date.clone();
    let now_ms = ctx.now_ms;
    let (owned_title, owned_context) = (title, context);
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
                "UPDATE follow_up_task_drafts SET title = ?3, due_date = ?4, context = ?5, \
                 updated_at_ms = ?6 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    owned_title,
                    owned_due,
                    owned_context,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUpEditableFields {
    pub title: String,
    pub due_date: Option<String>,
    pub context: String,
}

/// One validation chokepoint for manual staging and later operator edits.
pub fn normalize_editable_fields(
    title_raw: &str,
    due_date_raw: Option<&str>,
    context_raw: &str,
    now_ms: u64,
) -> Result<FollowUpEditableFields, StoreError> {
    let title: String = title_raw.trim().chars().take(200).collect();
    if title.is_empty() {
        return Err(StoreError::Domain(
            "follow_up_draft_title_required".to_string(),
        ));
    }
    let date_context = crate::slices::datetime_input::context_from_now_ms(now_ms);
    let due_date = due_date_raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|date| {
            crate::slices::datetime_input::normalize_civil_date(date, Some(&date_context))
                .map_err(|_| StoreError::Domain("follow_up_draft_due_date_invalid".to_string()))
        })
        .transpose()?;
    let context: String = context_raw.trim().chars().take(1_000).collect();
    Ok(FollowUpEditableFields {
        title,
        due_date,
        context,
    })
}

pub fn list_tasks(
    conn: &Connection,
    client_id: &str,
    status: Option<TaskStatus>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<TaskWithRevision>, StoreError> {
    let status_str = status.map(task_status_str);
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("t.source_user_id", 4, 5);
    // Open tasks: due-date ascending (undated last), then newest. Done: newest.
    let mut stmt = conn.prepare(&format!(
        "SELECT t.task_id, t.title, t.due_date, t.context, t.source_kind, t.source_ref, \
         t.source_user_id, f.item_id AS source_item_id, t.status, t.created_at_ms, t.updated_at_ms, \
         COALESCE(er.revision, 0) \
         FROM tasks t \
         LEFT JOIN entity_revisions er \
           ON er.client_id = t.client_id AND er.entity_kind = ?2 AND er.entity_id = t.task_id \
         LEFT JOIN follow_up_task_drafts f \
           ON f.client_id = t.client_id AND f.task_id = t.task_id AND f.status = 'approved' \
         WHERE t.client_id = ?1 AND (?3 IS NULL OR t.status = ?3) \
           AND {scope_pred} \
         ORDER BY t.status ASC, (t.due_date IS NULL) ASC, t.due_date ASC, \
                  t.created_at_ms DESC LIMIT ?6",
    ))?;
    let rows = stmt.query_map(
        params![
            client_id,
            TASK_ENTITY_KIND,
            status_str,
            scope_all,
            scope_user,
            limit as i64,
        ],
        |row| {
            Ok(TaskWithRevision {
                task: TaskRecord {
                    task_id: row.get(0)?,
                    title: row.get(1)?,
                    due_date: row.get(2)?,
                    context: row.get(3)?,
                    source_kind: row.get(4)?,
                    source_ref: row.get(5)?,
                    source_user_id: row.get(6)?,
                    source_item_id: row.get(7)?,
                    status: task_status_from_str(&row.get::<_, String>(8)?),
                    created_at_ms: row.get::<_, i64>(9)? as u64,
                    updated_at_ms: row.get::<_, i64>(10)? as u64,
                },
                revision: row.get::<_, i64>(11)? as u64,
                escalation: None,
                follow_up: None,
            })
        },
    )?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Complete,
    Reopen,
}

impl TaskAction {
    fn change_kind(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Reopen => "reopen",
        }
    }

    fn target_status(self) -> &'static str {
        match self {
            Self::Complete => "done",
            Self::Reopen => "open",
        }
    }
}

pub fn apply_task_action(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    task_id: &str,
    action: TaskAction,
) -> Result<MutationOutcome, StoreError> {
    let current: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status, source_user_id FROM tasks WHERE client_id = ?1 AND task_id = ?2",
            params![ctx.client_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((current, source_user_id)) = current else {
        return Err(StoreError::Domain("task_not_found".to_string()));
    };
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    let owned_client = ctx.client_id.to_string();
    let owned_task = task_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: TASK_ENTITY_KIND,
            entity_id: task_id,
            change_kind: action.change_kind(),
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some(format!("{{\"status\":\"{current}\"}}")),
            after_json: Some(format!("{{\"status\":\"{}\"}}", action.target_status())),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "UPDATE tasks SET status = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND task_id = ?2",
                params![
                    owned_client,
                    owned_task,
                    action.target_status(),
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

fn require_draft_status(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>), StoreError> {
    draft_store::require_status_scoped::<FollowUpDraftStore>(conn, client_id, draft_id)
}

fn draft_status_from_str(raw: &str) -> FollowUpDraftStatus {
    match raw {
        "approved" => FollowUpDraftStatus::Approved,
        "rejected" => FollowUpDraftStatus::Rejected,
        _ => FollowUpDraftStatus::Staged,
    }
}

fn task_status_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Open => "open",
        TaskStatus::Done => "done",
    }
}

fn task_status_from_str(raw: &str) -> TaskStatus {
    match raw {
        "done" => TaskStatus::Done,
        _ => TaskStatus::Open,
    }
}

/// Tasks flipped to done within an epoch-ms window (start inclusive, end
/// exclusive; the done flip is the task's last update). The owner digest's
/// follow-up completion read.
pub fn count_done_between(
    conn: &Connection,
    client_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks \
         WHERE client_id = ?1 AND status = 'done' \
           AND updated_at_ms >= ?2 AND updated_at_ms < ?3",
        params![client_id, start_ms as i64, end_ms as i64],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}
