//! Content draft persistence through store_core. Approval and publishing are
//! separate operator decisions; publish atomically attaches an outbox job.

use bos_contracts::content_drafts::{
    ContentCitationGate, ContentDraft, ContentDraftStatus, ContentDraftWithRevision,
    ContentEvidenceSnippet,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::draft_store::{self, DraftStore, DraftTableSpec};
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const DRAFT_ENTITY_KIND: &str = "content_draft";
pub const WEB_FACTS_ENTITY_KIND: &str = "content_web_facts";
const APPROVE_SQL: &str = "UPDATE content_drafts SET status = 'approved', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const REJECT_SQL: &str = "UPDATE content_drafts SET status = 'rejected', updated_at_ms = ?3 \
     WHERE client_id = ?1 AND draft_id = ?2";
const DRAFT_TABLE: DraftTableSpec = DraftTableSpec {
    table: ContentDraftStore::TABLE,
    entity_kind: DRAFT_ENTITY_KIND,
    not_found_code: ContentDraftStore::NOT_FOUND,
    not_staged_code: "content_draft_not_staged",
    approve_sql: APPROVE_SQL,
    reject_sql: REJECT_SQL,
};

const DRAFT_COLUMNS: &str = "d.draft_id, d.item_id, d.source_kind, d.source_ref, d.status, \
     d.title, d.body_markdown, d.target_query, d.meta_description, d.claims_json, \
     d.evidence_json, d.gate_passed, d.gate_json, d.model, d.confidence, d.created_at_ms, \
     d.updated_at_ms, d.publish_outbox_job_id, COALESCE(er.revision, 0) AS revision";

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<ContentDraftWithRevision> {
    let gate_passed: bool = row.get("gate_passed")?;
    let gate: ContentCitationGate = serde_json::from_str(&row.get::<_, String>("gate_json")?)
        .unwrap_or(ContentCitationGate {
            passed: gate_passed,
            missing_citation_claim_ids: Vec::new(),
            unsupported_claim_ids: Vec::new(),
        });
    Ok(ContentDraftWithRevision {
        draft: ContentDraft {
            draft_id: row.get("draft_id")?,
            item_id: row.get("item_id")?,
            source_kind: row.get("source_kind")?,
            source_ref: row.get("source_ref")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            title: row.get("title")?,
            body_markdown: row.get("body_markdown")?,
            target_query: row.get("target_query")?,
            meta_description: row.get("meta_description")?,
            claims: serde_json::from_str(&row.get::<_, String>("claims_json")?).unwrap_or_default(),
            evidence: serde_json::from_str(&row.get::<_, String>("evidence_json")?)
                .unwrap_or_default(),
            citation_gate: gate,
            model: row.get("model")?,
            confidence: row.get("confidence")?,
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
    mut entry: ContentDraftWithRevision,
) -> Result<ContentDraftWithRevision, StoreError> {
    let job_id: Option<String> = conn
        .query_row(
            "SELECT publish_outbox_job_id FROM content_drafts \
             WHERE client_id = ?1 AND draft_id = ?2",
            params![client_id, entry.draft.draft_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(job_id) = job_id {
        entry.outbox_job = outbox::job_summary(conn, client_id, &job_id)?;
    }
    Ok(entry)
}

struct ContentDraftStore;

impl DraftStore for ContentDraftStore {
    type WithRevision = ContentDraftWithRevision;

    const TABLE: &'static str = "content_drafts";
    const COLUMNS: &'static str = DRAFT_COLUMNS;
    const ENTITY_KIND: &'static str = DRAFT_ENTITY_KIND;
    const NOT_FOUND: &'static str = "content_draft_not_found";

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
) -> Result<Option<ContentDraftWithRevision>, StoreError> {
    draft_store::active_draft_for_item::<ContentDraftStore>(conn, client_id, item_id)
}

pub fn get_draft(
    conn: &Connection,
    client_id: &str,
    draft_id: &str,
) -> Result<Option<ContentDraftWithRevision>, StoreError> {
    draft_store::get_draft_unscoped::<ContentDraftStore>(conn, client_id, draft_id)
}

pub fn list_drafts(
    conn: &Connection,
    client_id: &str,
    item_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ContentDraftWithRevision>, StoreError> {
    draft_store::list_drafts_unscoped::<ContentDraftStore>(conn, client_id, item_id, limit)
}

pub fn count_drafts_for_item(
    conn: &Connection,
    client_id: &str,
    item_id: &str,
) -> Result<u64, StoreError> {
    draft_store::count_drafts_for_item::<ContentDraftStore>(conn, client_id, item_id)
}

/// Item ids with a STAGED draft (operator decision pending). Feeds the
/// queue's "needs you" decoration via the produce spine.
pub fn staged_item_ids(conn: &Connection, client_id: &str) -> Result<Vec<String>, StoreError> {
    draft_store::staged_item_ids::<ContentDraftStore>(conn, client_id)
}

pub fn insert_draft(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    draft: &ContentDraft,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let claims_json = serde_json::to_string(&draft.claims)
        .map_err(|err| StoreError::Domain(format!("serialize claims: {err}")))?;
    let evidence_json = serde_json::to_string(&draft.evidence)
        .map_err(|err| StoreError::Domain(format!("serialize evidence: {err}")))?;
    let gate_json = serde_json::to_string(&draft.citation_gate)
        .map_err(|err| StoreError::Domain(format!("serialize gate: {err}")))?;
    // Receipt payload: the decision-relevant surface, not the whole body.
    let after = serde_json::json!({
        "title": draft.title,
        "claims": draft.claims.len(),
        "evidence": draft.evidence.len(),
        "gate_passed": draft.citation_gate.passed,
        "confidence": draft.confidence,
    })
    .to_string();
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
                "INSERT INTO content_drafts \
                 (client_id, draft_id, item_id, source_kind, source_ref, status, title, \
                  body_markdown, target_query, meta_description, claims_json, evidence_json, \
                  gate_passed, gate_json, model, confidence, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                         ?14, ?15, ?16, ?16)",
                params![
                    owned_client,
                    row.draft_id,
                    row.item_id,
                    row.source_kind,
                    row.source_ref,
                    row.title,
                    row.body_markdown,
                    row.target_query,
                    row.meta_description,
                    claims_json,
                    evidence_json,
                    row.citation_gate.passed,
                    gate_json,
                    row.model,
                    row.confidence,
                    row.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("content_draft_already_active".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub use crate::slices::mutation_context::MutationContext as DraftActionContext;

#[derive(Debug, Clone)]
pub struct ContentWebFactsRecord {
    pub target_id: String,
    pub item_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub run_id: String,
    pub snippets: Vec<ContentEvidenceSnippet>,
}

fn web_fact_from_row(row: &Row<'_>) -> rusqlite::Result<ContentEvidenceSnippet> {
    Ok(ContentEvidenceSnippet {
        snippet_id: row.get(0)?,
        file_id: row.get(1)?,
        doc_title: row.get(2)?,
        heading_path: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
        text: row.get(4)?,
        web_view_link: row.get(5)?,
    })
}

pub fn web_facts_by_run(
    conn: &Connection,
    client_id: &str,
    run_id: &str,
) -> Result<Vec<ContentEvidenceSnippet>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT snippet_id, file_id, doc_title, heading_path_json, text, web_view_link \
         FROM content_web_facts WHERE client_id = ?1 AND run_id = ?2 ORDER BY rank ASC",
    )?;
    let rows = stmt.query_map(params![client_id, run_id], web_fact_from_row)?;
    let mut snippets = Vec::new();
    for row in rows {
        snippets.push(row?);
    }
    Ok(snippets)
}

#[cfg(test)]
pub(crate) fn web_facts_for_target(
    conn: &Connection,
    client_id: &str,
    target_id: &str,
) -> Result<Vec<ContentEvidenceSnippet>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT snippet_id, file_id, doc_title, heading_path_json, text, web_view_link \
         FROM content_web_facts WHERE client_id = ?1 AND target_id = ?2 ORDER BY rank ASC",
    )?;
    let rows = stmt.query_map(params![client_id, target_id], web_fact_from_row)?;
    let mut snippets = Vec::new();
    for row in rows {
        snippets.push(row?);
    }
    Ok(snippets)
}

pub fn persist_web_facts(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    record: &ContentWebFactsRecord,
) -> Result<MutationOutcome, StoreError> {
    let heading_json: Vec<String> = record
        .snippets
        .iter()
        .map(|snippet| {
            serde_json::to_string(&snippet.heading_path)
                .map_err(|err| StoreError::Domain(format!("serialize heading path: {err}")))
        })
        .collect::<Result<_, _>>()?;
    let after = serde_json::json!({
        "target_id": record.target_id,
        "item_id": record.item_id,
        "run_id": record.run_id,
        "snippets": record.snippets.len(),
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_record = record.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: WEB_FACTS_ENTITY_KIND,
            entity_id: &record.target_id,
            change_kind: "enrich_evidence",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&record.item_id),
            causation_id: Some(&record.run_id),
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM content_web_facts WHERE client_id = ?1 AND run_id = ?2",
                params![owned_client, owned_record.run_id],
            )?;
            for (rank, snippet) in owned_record.snippets.iter().enumerate() {
                tx.execute(
                    "INSERT INTO content_web_facts \
                     (client_id, target_id, item_id, source_kind, source_ref, run_id, \
                      snippet_id, file_id, doc_title, heading_path_json, text, web_view_link, \
                      rank, created_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    params![
                        owned_client,
                        owned_record.target_id,
                        owned_record.item_id,
                        owned_record.source_kind,
                        owned_record.source_ref,
                        owned_record.run_id,
                        snippet.snippet_id,
                        snippet.file_id,
                        snippet.doc_title,
                        heading_json[rank],
                        snippet.text,
                        snippet.web_view_link,
                        rank as i64,
                        now_ms as i64,
                    ],
                )?;
            }
            Ok(())
        },
    )
}

/// Approve a staged draft. DRAFT-ONLY vertical: a status flip.
/// The citation gate is enforced here — uncited/unsupported claims block
/// approval-readiness by construction.
pub fn approve_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    let (status, gate_passed) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "content_draft_not_staged:{status}"
        )));
    }
    if !gate_passed {
        return Err(StoreError::Domain(
            "content_citation_gate_failed".to_string(),
        ));
    }
    draft_store::approve(conn, ctx.into(), &DRAFT_TABLE, draft_id, None)
}

pub fn reject_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
) -> Result<MutationOutcome, StoreError> {
    draft_store::reject(conn, ctx.into(), &DRAFT_TABLE, draft_id)
}

/// Request publication of an approved draft. The outbox job and pointer are
/// committed in the same receipted mutation.
pub fn publish_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    job: &NewOutboxJob,
) -> Result<MutationOutcome, StoreError> {
    let (status, gate_passed) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "approved" {
        return Err(StoreError::Domain(
            "content_publish_not_approved".to_string(),
        ));
    }
    if !gate_passed {
        return Err(StoreError::Domain(
            "content_citation_gate_failed".to_string(),
        ));
    }
    let after = serde_json::json!({
        "publish_outbox_job_id": job.job_id,
        "provider": job.provider,
        "capability": job.capability,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_draft_id = draft_id.to_string();
    let owned_job = job.clone();
    let now_ms = ctx.now_ms;
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: DRAFT_ENTITY_KIND,
            entity_id: draft_id,
            change_kind: "publish",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: job.correlation_id.as_deref(),
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            let existing_job_id: Option<String> = tx.query_row(
                "SELECT publish_outbox_job_id FROM content_drafts \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![owned_client, owned_draft_id],
                |row| row.get(0),
            )?;
            if let Some(existing_job_id) = existing_job_id {
                if let Some(summary) = outbox::job_summary(tx, &owned_client, &existing_job_id)? {
                    let live_delivered =
                        summary.status == outbox::STATUS_DELIVERED && summary.dry_run != Some(true);
                    if summary.status == outbox::STATUS_PENDING || live_delivered {
                        return Err(StoreError::Domain(
                            "content_publish_already_requested".to_string(),
                        ));
                    }
                }
            }
            outbox::enqueue_within(tx, &owned_client, &owned_job, now_ms)?;
            tx.execute(
                "UPDATE content_drafts SET publish_outbox_job_id = ?3, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft_id,
                    owned_job.job_id,
                    now_ms as i64
                ],
            )?;
            Ok(())
        },
    )
}

/// Edit a STAGED draft's text fields (full replacement, receipted). Claims,
/// evidence, and the gate verdict are immutable — they are the audit trail
/// of what the model grounded.
pub fn update_draft(
    conn: &mut Connection,
    ctx: DraftActionContext<'_>,
    draft_id: &str,
    title_raw: &str,
    body_markdown_raw: &str,
    target_query_raw: Option<&str>,
    meta_description_raw: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    let (status, _) = require_draft(conn, ctx.client_id, draft_id)?;
    if status != "staged" {
        return Err(StoreError::Domain(format!(
            "content_draft_not_staged:{status}"
        )));
    }
    let title: String = title_raw.trim().chars().take(200).collect();
    if title.is_empty() {
        return Err(StoreError::Domain(
            "content_draft_title_required".to_string(),
        ));
    }
    let body_markdown = body_markdown_raw.trim().to_string();
    if body_markdown.is_empty() {
        return Err(StoreError::Domain(
            "content_draft_body_required".to_string(),
        ));
    }
    if body_markdown.len() > 60_000 {
        return Err(StoreError::Domain(
            "content_draft_body_too_long".to_string(),
        ));
    }
    let target_query = target_query_raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| raw.chars().take(200).collect::<String>());
    let meta_description = meta_description_raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| raw.chars().take(300).collect::<String>());
    let before: serde_json::Value = conn.query_row(
        "SELECT title, target_query, meta_description FROM content_drafts \
         WHERE client_id = ?1 AND draft_id = ?2",
        params![ctx.client_id, draft_id],
        |row| {
            Ok(serde_json::json!({
                "title": row.get::<_, String>(0)?,
                "target_query": row.get::<_, Option<String>>(1)?,
                "meta_description": row.get::<_, Option<String>>(2)?,
            }))
        },
    )?;
    let after = serde_json::json!({
        "title": title,
        "target_query": target_query,
        "meta_description": meta_description,
        "body_markdown_chars": body_markdown.len(),
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
                "UPDATE content_drafts SET title = ?3, body_markdown = ?4, target_query = ?5, \
                 meta_description = ?6, updated_at_ms = ?7 \
                 WHERE client_id = ?1 AND draft_id = ?2",
                params![
                    owned_client,
                    owned_draft,
                    title,
                    body_markdown,
                    target_query,
                    meta_description,
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
) -> Result<(String, bool), StoreError> {
    conn.query_row(
        "SELECT status, gate_passed FROM content_drafts WHERE client_id = ?1 AND draft_id = ?2",
        params![client_id, draft_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| StoreError::Domain("content_draft_not_found".to_string()))
}

fn status_from_str(raw: &str) -> ContentDraftStatus {
    match raw {
        "approved" => ContentDraftStatus::Approved,
        "rejected" => ContentDraftStatus::Rejected,
        _ => ContentDraftStatus::Staged,
    }
}
