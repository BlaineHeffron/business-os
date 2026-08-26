//! Calendar draft persistence through store_core. Approval enqueues the
//! provider-write outbox job inside the SAME mutation transaction.

use bos_contracts::calendar_drafts::{
    CalendarDraftStatus, CalendarDraftWithRevision, CalendarEventDraft,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, Row};

use crate::http::OperatorScope;
use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{
    self, DraftStore, DraftTableSpec, ScopedDraftStore, ScopedStatusDraftStore,
};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "calendar_event_draft";
const APPROVE_SQL: &str =
    "UPDATE calendar_event_drafts SET status = 'approved', outbox_job_id = ?3, \
     updated_at_ms = ?4 WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str =
    "UPDATE calendar_event_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: CalendarDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: CalendarDraftStore::NOT_FOUND,
    not_staged_code: "calendar_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.source_user_id, d.title, d.start_at, d.end_at, d.timezone, d.location, d.description, \
     d.calendar_id, d.attendees_json, d.send_invitations, d.provenance_json, d.model, d.confidence, d.outbox_job_id, \
     d.created_at_ms, d.updated_at_ms, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<CalendarDraftWithRevision> {
    Ok(CalendarDraftWithRevision {
        draft: CalendarEventDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            source_user_id: row.get("source_user_id")?,
            title: row.get("title")?,
            start_at: row.get("start_at")?,
            end_at: row.get("end_at")?,
            timezone: row.get("timezone")?,
            location: row.get("location")?,
            description: row.get("description")?,
            calendar_id: row.get("calendar_id")?,
            attendees: serde_json::from_str(&row.get::<_, String>("attendees_json")?).map_err(
                |err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                },
            )?,
            send_invitations: row.get::<_, i64>("send_invitations")? != 0,
            provenance: serde_json::from_str(&row.get::<_, String>("provenance_json")?)
                .unwrap_or_default(),
            model: row.get("model")?,
            confidence: row.get("confidence")?,
            outbox_job_id: row.get("outbox_job_id")?,
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
    mut entry: CalendarDraftWithRevision,
) -> Result<CalendarDraftWithRevision, StoreError> {
    if let Some(job_id) = entry.draft.outbox_job_id.as_deref() {
        entry.outbox_job = outbox::job_summary(conn, client_id, job_id)?;
    }
    Ok(entry)
}

struct CalendarDraftStore;

impl DraftStore for CalendarDraftStore {
    type WithRevision = CalendarDraftWithRevision;

    const TABLE: &'static str = "calendar_event_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "calendar_draft_not_found";

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

impl ScopedDraftStore for CalendarDraftStore {
    fn source_user_id(entry: &Self::WithRevision) -> Option<&str> {
        entry.draft.source_user_id.as_deref()
    }
}

impl ScopedStatusDraftStore for CalendarDraftStore {
    fn map_status(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>)> {
        Ok((row.get(0)?, row.get(1)?))
    }
}

/// The one staged-or-approved draft for an item, if any (rejected drafts are
/// history and do not block a re-produce).
pub fn active_draft_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<Option<CalendarDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<CalendarDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
    scope: &OperatorScope,
) -> Result<Option<CalendarDraftWithRevision>, StoreError> {
    draft_store::get_draft_scoped::<CalendarDraftStore>(conn, client_id, draft_id, scope)
}

/// Drafts newest-first, optionally scoped to one work item.
pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
    scope: &OperatorScope,
) -> Result<Vec<CalendarDraftWithRevision>, StoreError> {
    draft_store::list_drafts_scoped::<CalendarDraftStore>(conn, client_id, item_id, limit, scope)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<CalendarDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<CalendarDraftStore>(conn, client_id)
}

/// Stage a freshly produced draft. The unique active-draft index turns a
/// produce race into a domain error rather than a duplicate.
pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &CalendarEventDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let after = serde_json::to_string(draft)
        .map_err(|err| StoreError::Domain(format!("serialize draft: {err}")))?;
    let provenance_json = serde_json::to_string(&draft.provenance)
        .map_err(|err| StoreError::Domain(format!("serialize provenance: {err}")))?;
    let attendees_json = serde_json::to_string(&draft.attendees)
        .map_err(|err| StoreError::Domain(format!("serialize attendees: {err}")))?;
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
                "INSERT INTO calendar_event_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, source_user_id, \
                  status, title, start_at, end_at, timezone, location, description, calendar_id, \
                  attendees_json, send_invitations, provenance_json, model, confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                  ?14, ?15, ?16, ?17, ?18, ?19, ?19)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.source_user_id,
                    row.title,
                    row.start_at,
                    row.end_at,
                    row.timezone,
                    row.location,
                    row.description,
                    row.calendar_id,
                    attendees_json,
                    i64::from(row.send_invitations),
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
                    StoreError::Domain("calendar_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::ScopedMutationContext as DraftActionContext;

/// Approve a staged draft: status flip + outbox enqueue, one transaction.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    draft_store::approve(conn, ctx.into(), &DRAFT_TABLE, draft_id, Some(job))
}

/// Reject a staged draft (frees the item for a re-produce).
pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// The operator-editable field set ("AI-produced fields remain editable
/// until accepted"). Provenance stays untouched — it documents what the
/// MODEL extracted; the edit receipt records the operator's change.
pub struct CalendarDraftEdit {
    pub title: String,
    pub start_at: String,
    pub end_at: String,
    pub timezone: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// None = write to the server default calendar (BOS_GOOGLE_CALENDAR_ID).
    pub calendar_id: Option<String>,
    pub attendees: Vec<String>,
    pub send_invitations: bool,
}

/// Edit a STAGED draft's AI-filled fields (full replacement, receipted).
/// Approval builds the provider payload from the stored row, so edits flow
/// into the write.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    edit: &CalendarDraftEdit,
) -> Result<MutationOutcome, StoreError> {
    let (current, source_user_id) = require_status(conn, ctx.client_id, draft_id)?;
    ctx.scope.require_source_user(source_user_id.as_deref())?;
    if current != "staged" {
        return Err(StoreError::Domain(format!(
            "calendar_draft_not_staged:{current}"
        )));
    }
    let title: String = edit.title.trim().chars().take(200).collect();
    if title.is_empty() {
        return Err(StoreError::Domain(
            "calendar_draft_title_required".to_string(),
        ));
    }
    let start_at = crate::slices::datetime_input::normalize_rfc3339_datetime(&edit.start_at)
        .map_err(|_| StoreError::Domain("calendar_draft_start_invalid".to_string()))?;
    let end_at = crate::slices::datetime_input::normalize_rfc3339_datetime(&edit.end_at)
        .map_err(|_| StoreError::Domain("calendar_draft_end_invalid".to_string()))?;
    let opt = |raw: &Option<String>| {
        raw.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(1_000).collect::<String>())
    };
    let (timezone, location, description, calendar_id) = (
        opt(&edit.timezone),
        opt(&edit.location),
        opt(&edit.description),
        opt(&edit.calendar_id),
    );
    let attendees =
        bos_integrations::google_calendar::normalize_calendar_attendees(&edit.attendees)
            .map_err(|code| StoreError::Domain(code.to_string()))?;
    if edit.send_invitations && attendees.is_empty() {
        return Err(StoreError::Domain(
            "google_calendar_invitation_attendees_required".to_string(),
        ));
    }
    let attendees_json = serde_json::to_string(&attendees)
        .map_err(|err| StoreError::Domain(format!("serialize attendees: {err}")))?;
    let before: serde_json::Value = conn.query_row(
        "SELECT title, start_at, end_at, timezone, location, description, calendar_id, \
                attendees_json, send_invitations \
         FROM calendar_event_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "title": row.get::<_, String>(0)?,
                "start_at": row.get::<_, String>(1)?,
                "end_at": row.get::<_, String>(2)?,
                "timezone": row.get::<_, Option<String>>(3)?,
                "location": row.get::<_, Option<String>>(4)?,
                "description": row.get::<_, Option<String>>(5)?,
                "calendar_id": row.get::<_, Option<String>>(6)?,
                "attendees": serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(7)?)
                    .unwrap_or_else(|_| serde_json::json!([])),
                "send_invitations": row.get::<_, i64>(8)? != 0,
            }))
        },
    )?;
    let after = serde_json::json!({
        "title": title, "start_at": start_at, "end_at": end_at,
        "timezone": timezone, "location": location, "description": description,
        "calendar_id": calendar_id,
        "attendees": attendees,
        "send_invitations": edit.send_invitations,
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
                "UPDATE calendar_event_drafts SET title = ?3, start_at = ?4, end_at = ?5, \
                 timezone = ?6, location = ?7, description = ?8, calendar_id = ?9, \
                 attendees_json = ?10, send_invitations = ?11, updated_at_ms = ?12 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    title,
                    start_at,
                    end_at,
                    timezone,
                    location,
                    description,
                    calendar_id,
                    attendees_json,
                    i64::from(edit.send_invitations),
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

fn require_status(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<(String, Option<String>), StoreError> {
    draft_store::require_status_scoped::<CalendarDraftStore>(conn, client_id, draft_id)
}

fn status_from_str(raw: &str) -> CalendarDraftStatus {
    match raw {
        "approved" => CalendarDraftStatus::Approved,
        "rejected" => CalendarDraftStatus::Rejected,
        _ => CalendarDraftStatus::Staged,
    }
}
