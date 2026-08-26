//! Social proposal persistence through store_core. Approval updates the domain
//! row and enqueues all channel jobs in the same transaction.

use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::{
    SocialPostProposal, SocialPostProposalWithRevision, SocialProposalStatus, SocialProposalTarget,
    SocialPublishedSource, SocialSourceGenerationStatus,
};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::outbox::{self, NewOutboxJob};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const PROPOSAL_ENTITY_KIND: &str = "social_post_proposal";
pub const TARGET_ENTITY_KIND: &str = "social_post_target";
pub const SOURCE_ENTITY_KIND: &str = "social_published_source";

fn source_status_from_str(raw: &str) -> SocialSourceGenerationStatus {
    match raw {
        "generating" => SocialSourceGenerationStatus::Generating,
        "proposal_staged" => SocialSourceGenerationStatus::ProposalStaged,
        "generation_failed" => SocialSourceGenerationStatus::GenerationFailed,
        _ => SocialSourceGenerationStatus::Ready,
    }
}

fn source_from_row(row: &Row<'_>) -> rusqlite::Result<SocialPublishedSource> {
    Ok(SocialPublishedSource {
        source_id: row.get("source_id")?,
        source_kind: row.get("source_kind")?,
        external_id: row.get("external_id")?,
        source_content_draft_id: row.get("source_content_draft_id")?,
        source_content_draft_revision: row
            .get::<_, Option<i64>>("source_content_draft_revision")?
            .map(|value| value as u64),
        title: row.get("title")?,
        canonical_url: row.get("canonical_url")?,
        excerpt: row.get("excerpt")?,
        published_at: row.get("published_at")?,
        generation_status: source_status_from_str(&row.get::<_, String>("generation_status")?),
        generation_run_id: row.get("generation_run_id")?,
        generation_error: row.get("generation_error")?,
        proposal_id: row.get("proposal_id")?,
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

const SOURCE_SELECT_COLUMNS: &str = "s.source_id, s.source_kind, s.external_id, \
    s.source_content_draft_id, s.source_content_draft_revision, s.title, s.canonical_url, s.excerpt, s.published_at, \
    s.generation_status, s.generation_run_id, s.generation_error, s.proposal_id, \
    COALESCE(er.revision, 0) AS revision";

pub fn list_sources(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<SocialPublishedSource>, StoreError> {
    let sql = format!(
        "SELECT {SOURCE_SELECT_COLUMNS} FROM social_published_sources s \
         LEFT JOIN entity_revisions er ON er.client_id = s.client_id \
           AND er.entity_kind = ?2 AND er.entity_id = s.source_id \
         WHERE s.client_id = ?1 ORDER BY s.updated_at_ms DESC, s.source_id DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![client_id, SOURCE_ENTITY_KIND, limit as i64],
        source_from_row,
    )?;
    let mut sources = Vec::new();
    for row in rows {
        sources.push(row?);
    }
    Ok(sources)
}

pub fn get_source(
    conn: &Connection,
    client_id: &str,
    source_id: &str,
) -> Result<Option<SocialPublishedSource>, StoreError> {
    let sql = format!(
        "SELECT {SOURCE_SELECT_COLUMNS} FROM social_published_sources s \
         LEFT JOIN entity_revisions er ON er.client_id = s.client_id \
           AND er.entity_kind = ?3 AND er.entity_id = s.source_id \
         WHERE s.client_id = ?1 AND s.source_id = ?2"
    );
    Ok(conn
        .query_row(
            &sql,
            params![client_id, source_id, SOURCE_ENTITY_KIND],
            source_from_row,
        )
        .optional()?)
}

pub fn ingest_source(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    actor_kind: ActorKindDto,
    source: &SocialPublishedSource,
) -> Result<MutationOutcome, StoreError> {
    let before = get_source(conn, ctx.client_id, &source.source_id)?
        .and_then(|current| serde_json::to_string(&current).ok());
    let after = serde_json::to_string(source)
        .map_err(|err| StoreError::Domain(format!("serialize social source: {err}")))?;
    let owned_client = ctx.client_id.to_string();
    let owned = source.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: SOURCE_ENTITY_KIND,
            entity_id: &source.source_id,
            change_kind: "ingest",
            actor_id: ctx.actor_id,
            actor_kind,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(&source.source_id),
            causation_id: source.source_content_draft_id.as_deref(),
            before_json: before,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO social_published_sources \
                 (client_id, source_id, source_kind, external_id, source_content_draft_id, source_content_draft_revision, \
                  canonical_url, title, excerpt, published_at, generation_status, \
                  created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'ready', ?11, ?11) \
                 ON CONFLICT(client_id, source_id) DO UPDATE SET \
                   title = excluded.title, excerpt = excluded.excerpt, \
                   published_at = excluded.published_at, updated_at_ms = excluded.updated_at_ms",
                params![
                    owned_client,
                    owned.source_id,
                    owned.source_kind,
                    owned.external_id,
                    owned.source_content_draft_id,
                    owned.source_content_draft_revision.map(|value| value as i64),
                    owned.canonical_url,
                    owned.title,
                    owned.excerpt,
                    owned.published_at,
                    ctx.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn begin_generation(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    actor_kind: ActorKindDto,
    source_id: &str,
    run_id: &str,
) -> Result<MutationOutcome, StoreError> {
    require_expected_revision(ctx.expected_revision)?;
    let owned_client = ctx.client_id.to_string();
    let owned_source = source_id.to_string();
    let owned_run = run_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: SOURCE_ENTITY_KIND,
            entity_id: source_id,
            change_kind: "generation_started",
            actor_id: ctx.actor_id,
            actor_kind,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(source_id),
            causation_id: None,
            before_json: None,
            after_json: Some(
                serde_json::json!({ "generation_status": "generating", "run_id": run_id })
                    .to_string(),
            ),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let status: String = tx.query_row(
                "SELECT generation_status FROM social_published_sources \
                 WHERE client_id = ?1 AND source_id = ?2",
                params![owned_client, owned_source],
                |row| row.get(0),
            )?;
            if !matches!(status.as_str(), "ready" | "generation_failed") {
                return Err(StoreError::Domain(match status.as_str() {
                    "generating" => "social_generation_already_running".to_string(),
                    "proposal_staged" => "social_source_already_has_proposal".to_string(),
                    _ => "social_source_generation_state_invalid".to_string(),
                }));
            }
            let changed = tx.execute(
                "UPDATE social_published_sources SET generation_status = 'generating', \
                 generation_run_id = ?3, generation_error = NULL, updated_at_ms = ?4 \
                 WHERE client_id = ?1 AND source_id = ?2",
                params![owned_client, owned_source, owned_run, ctx.now_ms as i64],
            )?;
            debug_assert_eq!(changed, 1);
            Ok(())
        },
    )
}

pub fn finish_generation(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    source_id: &str,
    run_id: &str,
    proposal_id: Option<&str>,
    error_code: Option<&str>,
) -> Result<MutationOutcome, StoreError> {
    require_expected_revision(ctx.expected_revision)?;
    let (status, change_kind) = if proposal_id.is_some() {
        ("proposal_staged", "generation_succeeded")
    } else {
        ("generation_failed", "generation_failed")
    };
    let owned_client = ctx.client_id.to_string();
    let owned_source = source_id.to_string();
    let owned_run = run_id.to_string();
    let owned_proposal = proposal_id.map(str::to_string);
    let owned_error = error_code.map(str::to_string);
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: SOURCE_ENTITY_KIND,
            entity_id: source_id,
            change_kind,
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::System,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(source_id),
            causation_id: Some(run_id),
            before_json: None,
            after_json: Some(
                serde_json::json!({
                    "generation_status": status,
                    "run_id": run_id,
                    "proposal_id": proposal_id,
                    "error": error_code,
                })
                .to_string(),
            ),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE social_published_sources SET generation_status = ?4, \
                 proposal_id = ?5, generation_error = ?6, updated_at_ms = ?7 \
                 WHERE client_id = ?1 AND source_id = ?2 AND generation_run_id = ?3",
                params![
                    owned_client,
                    owned_source,
                    owned_run,
                    status,
                    owned_proposal,
                    owned_error,
                    ctx.now_ms as i64,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain(
                    "social_generation_run_changed".to_string(),
                ));
            }
            Ok(())
        },
    )
}

fn status_str(status: SocialProposalStatus) -> &'static str {
    match status {
        SocialProposalStatus::Staged => "staged",
        SocialProposalStatus::Approved => "approved",
        SocialProposalStatus::Rejected => "rejected",
    }
}

fn status_from_str(raw: &str) -> SocialProposalStatus {
    match raw {
        "approved" => SocialProposalStatus::Approved,
        "rejected" => SocialProposalStatus::Rejected,
        _ => SocialProposalStatus::Staged,
    }
}

fn proposal_from_row(row: &Row<'_>) -> rusqlite::Result<SocialPostProposalWithRevision> {
    let targets_json: String = row.get("targets_json")?;
    let targets = serde_json::from_str(&targets_json).unwrap_or_default();
    Ok(SocialPostProposalWithRevision {
        proposal: SocialPostProposal {
            proposal_id: row.get("proposal_id")?,
            source_id: row.get("source_id")?,
            source_content_draft_id: row.get("source_content_draft_id")?,
            source_content_draft_revision: row
                .get::<_, Option<i64>>("source_content_draft_revision")?
                .map(|value| value as u64),
            canonical_url: row.get("canonical_url")?,
            status: status_from_str(&row.get::<_, String>("status")?),
            targets,
            approved_by: row.get("approved_by")?,
            approved_revision: row
                .get::<_, Option<i64>>("approved_revision")?
                .map(|value| value as u64),
            created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
            updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        },
        revision: row.get::<_, i64>("revision")? as u64,
    })
}

fn attach_jobs(
    conn: &Connection,
    client_id: &str,
    mut entry: SocialPostProposalWithRevision,
) -> Result<SocialPostProposalWithRevision, StoreError> {
    for target in &mut entry.proposal.targets {
        target.outbox_job = match target.outbox_job_id.as_deref() {
            Some(job_id) => outbox::job_summary(conn, client_id, job_id)?,
            None => None,
        };
    }
    Ok(entry)
}

const SELECT_COLUMNS: &str =
    "p.proposal_id, p.source_id, p.source_content_draft_id, p.source_content_draft_revision, p.canonical_url, \
    p.status, p.targets_json, p.approved_by, p.approved_revision, p.created_at_ms, \
    p.updated_at_ms, COALESCE(er.revision, 0) AS revision";

pub fn list_proposals(
    conn: &Connection,
    client_id: &str,
    limit: usize,
) -> Result<Vec<SocialPostProposalWithRevision>, StoreError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM social_post_proposals p \
         LEFT JOIN entity_revisions er \
           ON er.client_id = p.client_id AND er.entity_kind = ?2 AND er.entity_id = p.proposal_id \
         WHERE p.client_id = ?1 ORDER BY p.created_at_ms DESC, p.proposal_id DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![client_id, PROPOSAL_ENTITY_KIND, limit as i64],
        proposal_from_row,
    )?;
    let mut proposals = Vec::new();
    for row in rows {
        proposals.push(attach_jobs(conn, client_id, row?)?);
    }
    Ok(proposals)
}

pub fn get_proposal(
    conn: &Connection,
    client_id: &str,
    proposal_id: &str,
) -> Result<Option<SocialPostProposalWithRevision>, StoreError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM social_post_proposals p \
         LEFT JOIN entity_revisions er \
           ON er.client_id = p.client_id AND er.entity_kind = ?3 AND er.entity_id = p.proposal_id \
         WHERE p.client_id = ?1 AND p.proposal_id = ?2"
    );
    let proposal = conn
        .query_row(
            &sql,
            params![client_id, proposal_id, PROPOSAL_ENTITY_KIND],
            proposal_from_row,
        )
        .optional()?;
    proposal
        .map(|entry| attach_jobs(conn, client_id, entry))
        .transpose()
}

pub fn stage_proposal(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    proposal: &SocialPostProposal,
    idempotency_key: &str,
) -> Result<MutationOutcome, StoreError> {
    let targets_json = serialize_targets(&proposal.targets)?;
    let after = proposal_snapshot_json(proposal, None)?;
    let owned_client = client_id.to_string();
    let row = proposal.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: PROPOSAL_ENTITY_KIND,
            entity_id: &proposal.proposal_id,
            change_kind: "stage",
            actor_id,
            actor_kind,
            expected_revision: None,
            idempotency_key,
            correlation_id: Some(&proposal.proposal_id),
            causation_id: proposal.source_content_draft_id.as_deref(),
            before_json: None,
            after_json: Some(after),
            now_ms: proposal.created_at_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO social_post_proposals \
                 (client_id, proposal_id, source_id, source_content_draft_id, source_content_draft_revision, canonical_url, status, \
                  targets_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'staged', ?7, ?8, ?8)",
                params![
                    owned_client,
                    row.proposal_id,
                    row.source_id,
                    row.source_content_draft_id,
                    row.source_content_draft_revision.map(|value| value as i64),
                    row.canonical_url,
                    targets_json,
                    row.created_at_ms as i64,
                ],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StoreError::Domain("social_proposal_already_exists".to_string())
                }
                other => other.into(),
            })?;
            Ok(())
        },
    )
}

pub fn update_proposal(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    proposal_id: &str,
    canonical_url: &str,
    targets: &[SocialProposalTarget],
) -> Result<MutationOutcome, StoreError> {
    require_expected_revision(ctx.expected_revision)?;
    let current = get_proposal(conn, ctx.client_id, proposal_id)?
        .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
    if Some(current.revision) == ctx.expected_revision
        && current.proposal.status != SocialProposalStatus::Staged
    {
        return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
    }
    let targets_json = serialize_targets(targets)?;
    let before = proposal_snapshot_json(&current.proposal, Some(current.revision))?;
    let mut next = current.proposal.clone();
    next.canonical_url = canonical_url.to_string();
    next.targets = targets.to_vec();
    next.updated_at_ms = ctx.now_ms;
    let after = proposal_snapshot_json(&next, ctx.expected_revision)?;
    let owned_client = ctx.client_id.to_string();
    let owned_proposal = proposal_id.to_string();
    let owned_url = canonical_url.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PROPOSAL_ENTITY_KIND,
            entity_id: proposal_id,
            change_kind: "edit",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(proposal_id),
            causation_id: current.proposal.source_content_draft_id.as_deref(),
            before_json: Some(before),
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let status: String = tx.query_row(
                "SELECT status FROM social_post_proposals WHERE client_id = ?1 AND proposal_id = ?2",
                params![owned_client, owned_proposal],
                |row| row.get(0),
            )?;
            if status != "staged" {
                return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
            }
            tx.execute(
                "UPDATE social_post_proposals SET canonical_url = ?3, targets_json = ?4, \
                 updated_at_ms = ?5 WHERE client_id = ?1 AND proposal_id = ?2",
                params![
                    owned_client,
                    owned_proposal,
                    owned_url,
                    targets_json,
                    ctx.now_ms as i64,
                ],
            )?;
            Ok(())
        },
    )
}

pub fn approve_proposal(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    proposal_id: &str,
    jobs: &[NewOutboxJob],
) -> Result<MutationOutcome, StoreError> {
    let approved_revision = require_expected_revision(ctx.expected_revision)?;
    let current = get_proposal(conn, ctx.client_id, proposal_id)?
        .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
    if Some(current.revision) == ctx.expected_revision
        && current.proposal.status != SocialProposalStatus::Staged
    {
        return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
    }
    if current.proposal.targets.len() != jobs.len() || jobs.is_empty() {
        return Err(StoreError::Domain(
            "social_channel_job_set_invalid".to_string(),
        ));
    }
    let mut approved_targets = current.proposal.targets.clone();
    for (target, job) in approved_targets.iter_mut().zip(jobs) {
        if job.source_entity_kind != TARGET_ENTITY_KIND || job.source_entity_id != target.target_id
        {
            return Err(StoreError::Domain(
                "social_channel_job_target_mismatch".to_string(),
            ));
        }
        target.outbox_job_id = Some(job.job_id.clone());
        target.outbox_job = None;
    }
    let targets_json = serialize_targets(&approved_targets)?;
    let after = serde_json::json!({
        "status": "approved",
        "approved_revision": approved_revision,
        "approved_by": ctx.actor_id,
        "canonical_url": current.proposal.canonical_url,
        "targets": approved_targets,
    })
    .to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_proposal = proposal_id.to_string();
    let owned_actor = ctx.actor_id.to_string();
    let owned_jobs = jobs.to_vec();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PROPOSAL_ENTITY_KIND,
            entity_id: proposal_id,
            change_kind: "approve",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(proposal_id),
            causation_id: current.proposal.source_content_draft_id.as_deref(),
            before_json: None,
            after_json: Some(after),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let status: String = tx.query_row(
                "SELECT status FROM social_post_proposals WHERE client_id = ?1 AND proposal_id = ?2",
                params![owned_client, owned_proposal],
                |row| row.get(0),
            )?;
            if status != "staged" {
                return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
            }
            tx.execute(
                "UPDATE social_post_proposals SET status = 'approved', targets_json = ?3, \
                 approved_by = ?4, approved_revision = ?5, updated_at_ms = ?6 \
                 WHERE client_id = ?1 AND proposal_id = ?2",
                params![
                    owned_client,
                    owned_proposal,
                    targets_json,
                    owned_actor,
                    approved_revision as i64,
                    ctx.now_ms as i64,
                ],
            )?;
            for job in &owned_jobs {
                outbox::enqueue_within(tx, &owned_client, job, ctx.now_ms)?;
            }
            Ok(())
        },
    )
}

pub fn reject_proposal(
    conn: &mut Connection,
    ctx: MutationContext<'_>,
    proposal_id: &str,
) -> Result<MutationOutcome, StoreError> {
    require_expected_revision(ctx.expected_revision)?;
    let current = get_proposal(conn, ctx.client_id, proposal_id)?
        .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
    if Some(current.revision) == ctx.expected_revision
        && current.proposal.status != SocialProposalStatus::Staged
    {
        return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
    }
    let owned_client = ctx.client_id.to_string();
    let owned_proposal = proposal_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PROPOSAL_ENTITY_KIND,
            entity_id: proposal_id,
            change_kind: "reject",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: Some(proposal_id),
            causation_id: current.proposal.source_content_draft_id.as_deref(),
            before_json: None,
            after_json: Some(serde_json::json!({ "status": "rejected" }).to_string()),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            let changed = tx.execute(
                "UPDATE social_post_proposals SET status = 'rejected', updated_at_ms = ?3 \
                 WHERE client_id = ?1 AND proposal_id = ?2 AND status = 'staged'",
                params![owned_client, owned_proposal, ctx.now_ms as i64],
            )?;
            if changed != 1 {
                return Err(StoreError::Domain("social_proposal_not_staged".to_string()));
            }
            Ok(())
        },
    )
}

fn serialize_targets(targets: &[SocialProposalTarget]) -> Result<String, StoreError> {
    serde_json::to_string(targets)
        .map_err(|err| StoreError::Domain(format!("serialize social targets: {err}")))
}

fn proposal_snapshot_json(
    proposal: &SocialPostProposal,
    revision: Option<u64>,
) -> Result<String, StoreError> {
    serde_json::to_string(&serde_json::json!({
        "proposal_id": proposal.proposal_id,
        "source_id": proposal.source_id,
        "source_content_draft_id": proposal.source_content_draft_id,
        "source_content_draft_revision": proposal.source_content_draft_revision,
        "canonical_url": proposal.canonical_url,
        "status": status_str(proposal.status),
        "revision": revision,
        "targets": proposal.targets,
    }))
    .map_err(|err| StoreError::Domain(format!("serialize social proposal snapshot: {err}")))
}

fn require_expected_revision(expected_revision: Option<u64>) -> Result<u64, StoreError> {
    expected_revision.ok_or_else(|| StoreError::Domain("expected_revision_required".to_string()))
}
