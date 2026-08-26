//! Shared helpers for draft slices.

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::http::OperatorScope;
use crate::outbox::{self, NewOutboxJob};
use crate::slices::mutation_context::{MutationContext, ScopedMutationContext};
use crate::store_core::StoreError;
use crate::store_core::{self, MutationOutcome, MutationRequest};
use bos_contracts::receipt::ActorKindDto;

pub(crate) trait DraftStore {
    type WithRevision;

    const TABLE: &'static str;
    const COLUMNS: &'static str;
    const ENTITY_KIND: &'static str;
    const NOT_FOUND: &'static str;

    fn map_row(row: &Row<'_>) -> rusqlite::Result<Self::WithRevision>;

    fn attach(
        _conn: &Connection,
        _client_id: &str,
        entry: Self::WithRevision,
    ) -> Result<Self::WithRevision, StoreError> {
        Ok(entry)
    }
}

pub(crate) trait ScopedDraftStore: DraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str>;
}

pub(crate) trait ScopedStatusDraftStore: DraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)>;
}

pub(crate) fn active_draft_for_item<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<S::WithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} d \
         LEFT JOIN entity_revisions er \
           ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
         WHERE d.client_id = ?1 AND d.item_id = ?3 AND d.status != 'rejected' \
         ORDER BY d.created_at_ms DESC, d.draft_id DESC LIMIT 1",
        S::COLUMNS,
        S::TABLE
    ))?;
    let row = stmt
        .query_row(params![client_id, S::ENTITY_KIND, item_id], S::map_row)
        .optional()?;
    row.map(|entry| S::attach(conn, client_id, entry))
        .transpose()
}

pub(crate) fn get_draft_scoped<S: ScopedDraftStore>(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<S::WithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} d \
         LEFT JOIN entity_revisions er \
           ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
         WHERE d.client_id = ?1 AND d.draft_id = ?3",
        S::COLUMNS,
        S::TABLE
    ))?;
    let row = stmt
        .query_row(params![client_id, S::ENTITY_KIND, draft_id], S::map_row)
        .optional()?;
    row.filter(|entry| scope.matches_source_user(S::source_user_id(entry)))
        .map(|entry| S::attach(conn, client_id, entry))
        .transpose()
}

pub(crate) fn get_draft_unscoped<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<S::WithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} d \
         LEFT JOIN entity_revisions er \
           ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
         WHERE d.client_id = ?1 AND d.draft_id = ?3",
        S::COLUMNS,
        S::TABLE
    ))?;
    let row = stmt
        .query_row(params![client_id, S::ENTITY_KIND, draft_id], S::map_row)
        .optional()?;
    row.map(|entry| S::attach(conn, client_id, entry))
        .transpose()
}

pub(crate) fn list_drafts_scoped<S: ScopedDraftStore>(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<S::WithRevision>, StoreError> {
    let (scope_pred, scope_all, scope_user) = scope.sql_filter("d.source_user_id", 5, 6);
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} d \
         LEFT JOIN entity_revisions er \
           ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
         WHERE d.client_id = ?1 AND (?3 IS NULL OR d.item_id = ?3) \
           AND {scope_pred} \
         ORDER BY d.created_at_ms DESC, d.draft_id DESC LIMIT ?4",
        S::COLUMNS,
        S::TABLE
    ))?;
    let rows = stmt.query_map(
        params![
            client_id,
            S::ENTITY_KIND,
            item_id,
            limit as i64,
            scope_all,
            scope_user,
        ],
        S::map_row,
    )?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(S::attach(conn, client_id, row?)?);
    }
    Ok(drafts)
}

pub(crate) fn list_drafts_unscoped<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<S::WithRevision>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} d \
         LEFT JOIN entity_revisions er \
           ON er.client_id = d.client_id AND er.entity_kind = ?2 AND er.entity_id = d.draft_id \
         WHERE d.client_id = ?1 AND (?3 IS NULL OR d.item_id = ?3) \
         ORDER BY d.created_at_ms DESC, d.draft_id DESC LIMIT ?4",
        S::COLUMNS,
        S::TABLE
    ))?;
    let rows = stmt.query_map(
        params![client_id, S::ENTITY_KIND, item_id, limit as i64],
        S::map_row,
    )?;
    let mut drafts = Vec::new();
    for row in rows {
        drafts.push(S::attach(conn, client_id, row?)?);
    }
    Ok(drafts)
}

pub(crate) fn count_drafts_for_item<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {} WHERE client_id = ?1 AND item_id = ?2",
            S::TABLE
        ),
        params![client_id, item_id],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

pub(crate) fn staged_item_ids<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT item_id FROM {} WHERE client_id = ?1 AND status = 'staged'",
        S::TABLE
    ))?;
    let rows = stmt.query_map(params![client_id], |row| row.get(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

pub(crate) fn require_status_scoped<S: ScopedStatusDraftStore>(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>), StoreError> {
    conn.query_row(
        &format!(
            "SELECT status, source_user_id FROM {} \
             WHERE client_id = ?1 AND draft_id = ?2",
            S::TABLE
        ),
        params![client_id, draft_id],
        S::map_status,
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain(S::NOT_FOUND.to_string()))
}

pub(crate) fn require_status_unscoped<S: DraftStore>(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<String, StoreError> {
    conn.query_row(
        &format!(
            "SELECT status FROM {} WHERE client_id = ?1 AND draft_id = ?2",
            S::TABLE
        ),
        params![client_id, draft_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain(S::NOT_FOUND.to_string()))
}

pub(crate) struct DraftTableSpec {
    pub table: &'static str,
    pub entity_kind: &'static str,
    pub not_found_code: &'static str,
    pub not_staged_code: &'static str,
    pub approve_sql: &'static str,
    pub reject_sql: &'static str,
}

pub(crate) struct DraftMutationContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub scope: Option<&'a OperatorScope>,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

impl<'a> From<MutationContext<'a>> for DraftMutationContext<'a> {
    fn from(ctx: MutationContext<'a>) -> Self {
        Self {
            client_id: ctx.client_id,
            actor_id: ctx.actor_id,
            scope: None,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            now_ms: ctx.now_ms,
        }
    }
}

impl<'a> From<ScopedMutationContext<'a>> for DraftMutationContext<'a> {
    fn from(ctx: ScopedMutationContext<'a>) -> Self {
        Self {
            client_id: ctx.client_id,
            actor_id: ctx.actor_id,
            scope: Some(ctx.scope),
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            now_ms: ctx.now_ms,
        }
    }
}

fn require_staged(
    conn: &Connection,
    ctx: &DraftMutationContext<'_>,
    spec: &DraftTableSpec,
    draft_id: &str,
) -> Result<(), StoreError> {
    let current = if let Some(scope) = ctx.scope {
        let (status, source_user_id): (String, Option<String>) = conn
            .query_row(
                &format!(
                    "SELECT status, source_user_id FROM {} \
                     WHERE client_id = ?1 AND draft_id = ?2",
                    spec.table
                ),
                params![ctx.client_id, draft_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Domain(spec.not_found_code.to_string()))?;
        scope.require_source_user(source_user_id.as_deref())?;
        status
    } else {
        conn.query_row(
            &format!(
                "SELECT status FROM {} WHERE client_id = ?1 AND draft_id = ?2",
                spec.table
            ),
            params![ctx.client_id, draft_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Domain(spec.not_found_code.to_string()))?
    };
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "{}:{current}",
            spec.not_staged_code
        )));
    }
    Ok(())
}

pub(crate) fn approve(
    conn: &mut Connection,
    ctx: DraftMutationContext<'_>,
    spec: &DraftTableSpec,
    draft_id: &str,
    job: Option<&NewOutboxJob>,
) -> Result<MutationOutcome, StoreError> {
    require_staged(conn, &ctx, spec, draft_id)?;
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let owned_job = job.cloned();
    let now_ms = ctx.now_ms;
    let correlation_id = job.map(|job| job.job_id.as_str());
    let after_json = job.map_or_else(
        || "{\"status\":\"approved\"}".to_string(),
        |job| {
            format!(
                "{{\"status\":\"approved\",\"outbox_job_id\":\"{}\"}}",
                job.job_id
            )
        },
    );
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: spec.entity_kind,
            entity_id: draft_id,
            change_kind: "approve",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id,
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some(after_json),
            now_ms,
        },
        move |tx| {
            if let Some(job) = owned_job {
                tx.execute(
                    spec.approve_sql,
                    params![owned_client, owned_draft, job.job_id, now_ms as i64],
                )?;
                outbox::enqueue_within(tx, &owned_client, &job, now_ms)?;
            } else {
                tx.execute(
                    spec.approve_sql,
                    params![owned_client, owned_draft, now_ms as i64],
                )?;
            }
            Ok(())
        },
    )
}

pub(crate) fn reject(
    conn: &mut Connection,
    ctx: DraftMutationContext<'_>,
    spec: &DraftTableSpec,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    require_staged(conn, &ctx, spec, draft_id)?;
    let owned_client = ctx.client_id.to_string();
    let owned_draft = draft_id.to_string();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: spec.entity_kind,
            entity_id: draft_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: Some("{\"status\":\"staged\"}".to_string()),
            after_json: Some("{\"status\":\"rejected\"}".to_string()),
            now_ms,
        },
        move |tx| {
            tx.execute(
                spec.reject_sql,
                params![owned_client, owned_draft, now_ms as i64],
            )?;
            Ok(())
        },
    )
}
