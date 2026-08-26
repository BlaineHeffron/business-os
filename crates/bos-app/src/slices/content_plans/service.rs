//! Content plan domain logic: request normalization, source rendering for the
//! Produce spine, and deterministic advisory overlap checks.

use bos_contracts::content_drafts::ContentDraftStatus;
use bos_contracts::content_plans::{
    ContentCampaignPublicationStatus, ContentCampaignPublishRequest, ContentCollisionMatch,
    ContentCollisionSummary, ContentInventoryManualAddRequest, ContentInventorySourceKind,
    ContentInventoryStatus, ContentPlanItem, ContentPlanItemCreateRequest,
    ContentPlanItemUpdateRequest, ContentPlanStatus,
};
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::social_publishing::{SocialPostProposal, SocialProposalStatus};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use super::store::{self, CampaignPublicationApproval, CollisionCandidate, InventoryProjectionRow};
use crate::store_core::{MutationOutcome, StoreError};

pub const CATEGORY_ID: &str = "content_plan";
const TITLE_LIMIT: usize = 180;
const FIELD_LIMIT: usize = 500;
const NOTES_LIMIT: usize = 4_000;
pub const DRAFT_OVERLAP_BODY_MATCH_CHARS: usize = 400;
pub const DRAFT_OVERLAP_MAX_TERMS: usize = 24;

pub fn item_from_create(
    client_id: &str,
    request: &ContentPlanItemCreateRequest,
    now_ms: u64,
) -> Result<ContentPlanItem, StoreError> {
    let topic = required(&request.topic, "content_plan_topic_required", TITLE_LIMIT)?;
    let plan_item_id = stable_plan_item_id(client_id, &request.idempotency_key);
    Ok(ContentPlanItem {
        plan_item_id,
        status: ContentPlanStatus::Planned,
        topic,
        angle: optional(&request.angle, FIELD_LIMIT),
        format: optional(&request.format, FIELD_LIMIT),
        target_query: optional(&request.target_query, FIELD_LIMIT),
        audience: optional(&request.audience, FIELD_LIMIT),
        notes: optional(&request.notes, NOTES_LIMIT),
        work_item_id: None,
        published_url: None,
        collision_summary: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

pub fn updated_item(
    current: &ContentPlanItem,
    request: &ContentPlanItemUpdateRequest,
    now_ms: u64,
) -> Result<ContentPlanItem, StoreError> {
    if current.status != ContentPlanStatus::Planned {
        return Err(StoreError::Domain("content_plan_not_planned".to_string()));
    }
    let topic = required(&request.topic, "content_plan_topic_required", TITLE_LIMIT)?;
    Ok(ContentPlanItem {
        topic,
        angle: optional(&request.angle, FIELD_LIMIT),
        format: optional(&request.format, FIELD_LIMIT),
        target_query: optional(&request.target_query, FIELD_LIMIT),
        audience: optional(&request.audience, FIELD_LIMIT),
        notes: optional(&request.notes, NOTES_LIMIT),
        updated_at_ms: now_ms,
        collision_summary: None,
        ..current.clone()
    })
}

pub fn source_view(item: &ContentPlanItem) -> InboundMessageRecord {
    let body = brief_body(item);
    InboundMessageRecord {
        source_key: item.plan_item_id.clone(),
        message_id: item.plan_item_id.clone(),
        thread_id: None,
        internal_date_ms: Some(item.created_at_ms as i64),
        from_addr: None,
        to_addr: None,
        subject: Some(item.topic.clone()),
        body_excerpt: body.clone(),
        body_full: body,
        headers: Vec::new(),
        labels: vec![CATEGORY_ID.to_string()],
        resolved_category: CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: item.created_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

pub fn work_item_title(item: &ContentPlanItem) -> String {
    item.topic.chars().take(100).collect()
}

pub fn work_item_summary(item: &ContentPlanItem) -> String {
    brief_body(item).chars().take(1_000).collect()
}

pub fn run_collision_check(
    item: &ContentPlanItem,
    candidates: &[CollisionCandidate],
    now_ms: u64,
) -> ContentCollisionSummary {
    run_collision_check_for_parts(
        item.target_query.as_deref(),
        &canonical_key(None, &item.topic),
        &format!(
            "{} {} {}",
            item.topic,
            item.target_query.as_deref().unwrap_or_default(),
            item.angle.as_deref().unwrap_or_default()
        ),
        candidates,
        now_ms,
    )
}

pub fn run_draft_collision_check(
    draft_id: &str,
    item_id: &str,
    title: &str,
    body_markdown: &str,
    target_query: Option<&str>,
    candidates: &[CollisionCandidate],
    now_ms: u64,
) -> ContentCollisionSummary {
    let filtered = candidates
        .iter()
        .filter(|candidate| !draft_overlap_excluded(candidate, draft_id, item_id))
        .cloned()
        .collect::<Vec<_>>();
    run_collision_check_for_parts(
        target_query,
        &canonical_key(None, title),
        &draft_overlap_match_text(title, body_markdown, target_query),
        &filtered,
        now_ms,
    )
}

fn run_collision_check_for_parts(
    target_query: Option<&str>,
    canonical_key: &str,
    term_text: &str,
    candidates: &[CollisionCandidate],
    now_ms: u64,
) -> ContentCollisionSummary {
    let item_query = normalized_phrase(target_query.unwrap_or(""));
    let terms = match_terms(term_text);
    let mut matches = Vec::new();
    for candidate in candidates {
        let mut reason = None;
        let mut score = 0.0;
        if !item_query.is_empty()
            && candidate
                .target_query
                .as_deref()
                .map(normalized_phrase)
                .is_some_and(|candidate_query| candidate_query == item_query)
        {
            reason = Some("exact_query");
            score = 100.0;
        } else if !canonical_key.is_empty() && candidate.canonical_key == canonical_key {
            reason = Some("same_slug");
            score = 90.0;
        } else if let Some(bm25_score) = candidate.bm25_score {
            reason = Some("similar");
            score = 50.0 + (-bm25_score);
        } else {
            let lexical = lexical_score(&terms, &candidate.search_text);
            if lexical >= 2 {
                reason = Some("similar");
                score = lexical as f64;
            }
        }
        if let Some(reason) = reason {
            matches.push(ContentCollisionMatch {
                inventory_id: candidate.inventory_id.clone(),
                source_kind: candidate.source_kind.clone(),
                source_ref: candidate.source_ref.clone(),
                title: candidate.title.clone(),
                reason: reason.to_string(),
                score,
            });
        }
    }
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.inventory_id.cmp(&b.inventory_id))
    });
    matches.truncate(10);
    ContentCollisionSummary {
        checked_at_ms: now_ms,
        matches,
    }
}

pub fn manual_inventory_row(
    client_id: &str,
    request: &ContentInventoryManualAddRequest,
    now_ms: u64,
) -> Result<InventoryProjectionRow, StoreError> {
    let title = required(
        &request.title,
        "content_inventory_title_required",
        TITLE_LIMIT,
    )?;
    let url = optional(&request.url, FIELD_LIMIT);
    let canonical_key = canonical_key(url.as_deref(), &title);
    if canonical_key.is_empty() {
        return Err(StoreError::Domain(
            "content_inventory_canonical_key_required".to_string(),
        ));
    }
    Ok(InventoryProjectionRow {
        inventory_id: store::inventory_id_for(client_id, &canonical_key),
        source_kind: ContentInventorySourceKind::Manual,
        source_ref: store::inventory_id_for(client_id, &canonical_key),
        status: ContentInventoryStatus::Published,
        title,
        target_query: optional(&request.target_query, FIELD_LIMIT),
        url,
        summary: optional(&request.summary, NOTES_LIMIT),
        canonical_key,
        metrics_json: "{}".to_string(),
        last_seen_at_ms: Some(now_ms),
    })
}

pub fn published_plan_inventory_row(
    client_id: &str,
    item: &ContentPlanItem,
    published_url: &str,
    now_ms: u64,
) -> Result<InventoryProjectionRow, StoreError> {
    let url = required(published_url, "published_url_required", FIELD_LIMIT)?;
    let canonical_key = canonical_key(Some(&url), &item.topic);
    Ok(InventoryProjectionRow {
        inventory_id: store::inventory_id_for(client_id, &canonical_key),
        source_kind: ContentInventorySourceKind::PlanItem,
        source_ref: item.plan_item_id.clone(),
        status: ContentInventoryStatus::Published,
        title: item.topic.clone(),
        target_query: item.target_query.clone(),
        url: Some(url),
        summary: item.angle.clone().or_else(|| item.notes.clone()),
        canonical_key,
        metrics_json: "{}".to_string(),
        last_seen_at_ms: Some(now_ms),
    })
}

pub fn projected_inventory_rows(
    conn: &Connection,
    client_id: &str,
    now_ms: u64,
) -> Result<Vec<InventoryProjectionRow>, StoreError> {
    let mut rows = Vec::new();
    append_search_console_rows(conn, client_id, now_ms, &mut rows)?;
    append_published_plan_rows(conn, client_id, now_ms, &mut rows)?;
    Ok(rows)
}

pub fn collision_match_expression(item: &ContentPlanItem) -> Option<String> {
    fts_match_expression(&format!(
        "{} {}",
        item.topic,
        item.target_query.as_deref().unwrap_or_default()
    ))
}

pub fn draft_collision_match_expression(
    title: &str,
    body_markdown: &str,
    target_query: Option<&str>,
) -> Option<String> {
    fts_match_expression_with_limit(
        &draft_overlap_match_text(title, body_markdown, target_query),
        DRAFT_OVERLAP_MAX_TERMS,
    )
}

/// Build a safe FTS5 MATCH expression from free text: alphanumeric terms
/// longer than 2 chars, deduped, each quoted, OR-joined.
pub fn fts_match_expression(query: &str) -> Option<String> {
    fts_match_expression_with_limit(query, usize::MAX)
}

fn fts_match_expression_with_limit(query: &str, max_terms: usize) -> Option<String> {
    let mut terms: Vec<String> = query
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 2)
        .map(str::to_ascii_lowercase)
        .collect();
    terms.sort_unstable();
    terms.dedup();
    terms.truncate(max_terms);
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

fn draft_overlap_match_text(
    title: &str,
    body_markdown: &str,
    target_query: Option<&str>,
) -> String {
    format!(
        "{} {} {}",
        title,
        target_query.unwrap_or_default(),
        body_markdown
            .chars()
            .take(DRAFT_OVERLAP_BODY_MATCH_CHARS)
            .collect::<String>()
    )
}

fn draft_overlap_excluded(candidate: &CollisionCandidate, draft_id: &str, item_id: &str) -> bool {
    let same_work_item = candidate
        .work_item_id
        .as_deref()
        .is_some_and(|candidate_item_id| candidate_item_id == item_id);
    let current_draft =
        candidate.source_kind == "content_draft" && candidate.source_ref == draft_id;
    let sibling_draft = candidate.source_kind == "content_draft" && same_work_item;
    let origin_plan = candidate.source_kind == "plan_item" && same_work_item;
    current_draft || sibling_draft || origin_plan
}

pub fn canonical_key(url: Option<&str>, title: &str) -> String {
    let source = url.and_then(path_slug).unwrap_or_else(|| title.to_string());
    slug(&source)
}

fn path_slug(url: &str) -> Option<String> {
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or("");
    let trimmed = path.trim_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn match_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 2)
        .map(str::to_ascii_lowercase)
        .collect();
    terms.sort_unstable();
    terms.dedup();
    terms
}

fn lexical_score(terms: &[String], text: &str) -> usize {
    if terms.is_empty() {
        return 0;
    }
    let haystack = text.to_ascii_lowercase();
    terms
        .iter()
        .map(|term| haystack.matches(term).count().min(3))
        .sum()
}

fn brief_body(item: &ContentPlanItem) -> String {
    let mut lines = vec![format!("Topic: {}", item.topic)];
    push_line(&mut lines, "Angle", item.angle.as_deref());
    push_line(&mut lines, "Format", item.format.as_deref());
    push_line(&mut lines, "Target query", item.target_query.as_deref());
    push_line(&mut lines, "Audience", item.audience.as_deref());
    push_line(&mut lines, "Notes", item.notes.as_deref());
    lines.join("\n")
}

fn push_line(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        lines.push(format!("{label}: {value}"));
    }
}

fn required(raw: &str, code: &str, limit: usize) -> Result<String, StoreError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(StoreError::Domain(code.to_string()));
    }
    Ok(value.chars().take(limit).collect())
}

fn optional(raw: &Option<String>, limit: usize) -> Option<String> {
    raw.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(limit).collect())
}

pub fn normalized_phrase(raw: &str) -> String {
    raw.split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn slug(raw: &str) -> String {
    normalized_phrase(raw).replace(' ', "-")
}

fn stable_plan_item_id(client_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(client_id.as_bytes());
    hasher.update([0]);
    hasher.update(idempotency_key.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("cpi_");
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn candidate_for_item(item: &ContentPlanItem) -> CollisionCandidate {
    CollisionCandidate {
        inventory_id: format!("plan:{}", item.plan_item_id),
        source_kind: "plan_item".to_string(),
        source_ref: item.plan_item_id.clone(),
        work_item_id: item.work_item_id.clone(),
        title: item.topic.clone(),
        target_query: item.target_query.clone(),
        canonical_key: canonical_key(item.published_url.as_deref(), &item.topic),
        search_text: brief_body(item),
        bm25_score: None,
    }
}

fn append_search_console_rows(
    conn: &Connection,
    client_id: &str,
    now_ms: u64,
    rows: &mut Vec<InventoryProjectionRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT dimension_value, COALESCE(SUM(clicks), 0), COALESCE(SUM(impressions), 0), \
                MAX(updated_at_ms) \
         FROM search_console_dimension_metrics \
         WHERE client_id = ?1 AND dimension_type = 'page' \
         GROUP BY dimension_value \
         ORDER BY SUM(clicks) DESC, SUM(impressions) DESC LIMIT 500",
    )?;
    let found = stmt.query_map(params![client_id], |row| {
        let url: String = row.get(0)?;
        let clicks: i64 = row.get(1)?;
        let impressions: i64 = row.get(2)?;
        let last_seen: i64 = row.get(3)?;
        Ok((url, clicks, impressions, last_seen))
    })?;
    for found in found {
        let (url, clicks, impressions, last_seen) = found?;
        let title = title_from_url(&url);
        let canonical_key = canonical_key(Some(&url), &title);
        if canonical_key.is_empty() {
            continue;
        }
        rows.push(InventoryProjectionRow {
            inventory_id: store::inventory_id_for(client_id, &canonical_key),
            source_kind: ContentInventorySourceKind::SearchConsolePage,
            source_ref: url.clone(),
            status: ContentInventoryStatus::Published,
            title,
            target_query: None,
            url: Some(url),
            summary: None,
            canonical_key,
            metrics_json: serde_json::json!({
                "clicks": clicks,
                "impressions": impressions
            })
            .to_string(),
            last_seen_at_ms: Some((last_seen as u64).max(now_ms)),
        });
    }
    Ok(())
}

fn append_published_plan_rows(
    conn: &Connection,
    client_id: &str,
    now_ms: u64,
    rows: &mut Vec<InventoryProjectionRow>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT plan_item_id, topic, target_query, published_url, angle, notes \
         FROM content_plan_items \
         WHERE client_id = ?1 AND status = 'published' AND published_url IS NOT NULL \
         ORDER BY updated_at_ms DESC LIMIT 500",
    )?;
    let found = stmt.query_map(params![client_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    for found in found {
        let (plan_item_id, topic, target_query, url, angle, notes) = found?;
        let canonical_key = canonical_key(Some(&url), &topic);
        if canonical_key.is_empty() {
            continue;
        }
        rows.push(InventoryProjectionRow {
            inventory_id: store::inventory_id_for(client_id, &canonical_key),
            source_kind: ContentInventorySourceKind::PlanItem,
            source_ref: plan_item_id,
            status: ContentInventoryStatus::Published,
            title: topic,
            target_query,
            url: Some(url),
            summary: angle.or(notes),
            canonical_key,
            metrics_json: "{}".to_string(),
            last_seen_at_ms: Some(now_ms),
        });
    }
    Ok(())
}

fn title_from_url(url: &str) -> String {
    path_slug(url)
        .unwrap_or_else(|| url.to_string())
        .split(['-', '_', '/'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn prepare_campaign_publication(
    conn: &Connection,
    client_id: &str,
    plan: &ContentPlanItem,
    request: &ContentCampaignPublishRequest,
    actor_id: &str,
    now_ms: u64,
) -> Result<CampaignPublicationApproval, StoreError> {
    if request.idempotency_key.trim().is_empty() {
        return Err(StoreError::Domain("idempotency_key_required".to_string()));
    }
    let publication_id = campaign_publication_id(client_id, plan, &request.idempotency_key);
    let is_replay = store::campaign_publication_exists(conn, client_id, &publication_id)?;
    if plan.status != ContentPlanStatus::Queued && !is_replay {
        return Err(StoreError::Domain(
            "content_campaign_plan_not_queued".to_string(),
        ));
    }
    let expected_url = crate::slices::social_publishing::service::normalize_canonical_url(
        &request.expected_canonical_url,
    )?;
    validate_slug_matches_expected_url(&request.slug, &expected_url)?;
    let draft = crate::slices::content_drafts::store::get_draft(
        conn,
        client_id,
        &request.content_draft_id,
    )?
    .ok_or_else(|| StoreError::Domain("content_draft_not_found".to_string()))?;
    if draft.revision != request.expected_content_draft_revision {
        return Err(StoreError::Domain(
            "content_campaign_article_revision_changed".to_string(),
        ));
    }
    if draft.draft.status != ContentDraftStatus::Approved || !draft.draft.citation_gate.passed {
        return Err(StoreError::Domain(
            "content_campaign_article_not_approved".to_string(),
        ));
    }
    if plan.work_item_id.as_deref() != Some(&draft.draft.item_id) {
        return Err(StoreError::Domain(
            "content_campaign_article_plan_mismatch".to_string(),
        ));
    }
    if store::content_draft_campaign_locked_except(
        conn,
        client_id,
        &draft.draft.draft_id,
        Some(&publication_id),
    )? {
        return Err(StoreError::Domain(
            "content_campaign_article_already_approved".to_string(),
        ));
    }

    let channels =
        crate::slices::social_publishing::service::configured_channels().unwrap_or_default();
    let mut selected = request
        .selected_channel_ids
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    selected.sort();
    selected.dedup();
    if selected.len() != request.selected_channel_ids.len() {
        return Err(StoreError::Domain(
            "content_campaign_destination_set_invalid".to_string(),
        ));
    }
    if selected
        .iter()
        .any(|id| !channels.iter().any(|channel| channel.channel_id == *id))
    {
        return Err(StoreError::Domain(
            "content_campaign_destination_set_invalid".to_string(),
        ));
    }

    let (social_proposal_id, social_proposal_revision, approved_social_targets) = if selected
        .is_empty()
    {
        if request.social_proposal_id.is_some()
            || request.expected_social_proposal_revision.is_some()
        {
            return Err(StoreError::Domain(
                "content_campaign_social_snapshot_invalid".to_string(),
            ));
        }
        (None, None, Vec::new())
    } else {
        let proposal_id = request
            .social_proposal_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                StoreError::Domain("content_campaign_social_snapshot_required".to_string())
            })?;
        let expected_revision = request.expected_social_proposal_revision.ok_or_else(|| {
            StoreError::Domain("content_campaign_social_snapshot_required".to_string())
        })?;
        let proposal =
            crate::slices::social_publishing::store::get_proposal(conn, client_id, proposal_id)?
                .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
        if proposal.revision != expected_revision
            || proposal.proposal.status != SocialProposalStatus::Staged
            || proposal.proposal.source_content_draft_id.as_deref() != Some(&draft.draft.draft_id)
            || proposal.proposal.source_content_draft_revision != Some(draft.revision)
            || proposal.proposal.canonical_url != expected_url
        {
            return Err(StoreError::Domain(
                "content_campaign_social_snapshot_changed".to_string(),
            ));
        }
        if store::social_proposal_campaign_locked_except(
            conn,
            client_id,
            proposal_id,
            Some(&publication_id),
        )? {
            return Err(StoreError::Domain(
                "content_campaign_social_snapshot_already_approved".to_string(),
            ));
        }
        let targets = proposal
            .proposal
            .targets
            .into_iter()
            .filter(|target| selected.binary_search(&target.channel_id).is_ok())
            .collect::<Vec<_>>();
        if targets.len() != selected.len() {
            return Err(StoreError::Domain(
                "content_campaign_destination_set_invalid".to_string(),
            ));
        }
        (
            Some(proposal_id.to_string()),
            Some(expected_revision),
            targets,
        )
    };

    let mut blog_job = crate::slices::content_drafts::service::build_publish_job(
        client_id,
        &draft.draft,
        &request.slug,
        &request.published_at,
        &format!("campaign:{publication_id}"),
    )?;
    blog_job.correlation_id = Some(plan.plan_item_id.clone());
    blog_job.causation_id = Some(publication_id.clone());

    Ok(CampaignPublicationApproval {
        publication_id,
        plan_item_id: plan.plan_item_id.clone(),
        content_draft_id: draft.draft.draft_id,
        content_draft_revision: draft.revision,
        social_proposal_id,
        social_proposal_revision,
        expected_canonical_url: expected_url,
        launch_mode: request.launch_mode,
        selected_channel_ids: selected,
        approved_social_targets,
        approved_by: actor_id.to_string(),
        approved_at_ms: now_ms,
        blog_job,
    })
}

pub fn reconcile_campaign_publications(
    conn: &mut Connection,
    client_id: &str,
    now_ms: u64,
) -> Result<usize, StoreError> {
    let pending = store::awaiting_campaign_publications(conn, client_id, 20)?;
    let mut settled = 0;
    for current in pending {
        if current.publication.status == ContentCampaignPublicationStatus::SocialEnqueued
            || (current.publication.status == ContentCampaignPublicationStatus::RequiresReview
                && current.publication.review_reason.as_deref() == Some("social_delivery_failed"))
        {
            let unknown = current
                .publication
                .social_outbox_jobs
                .iter()
                .any(|job| job.status == crate::outbox::STATUS_DELIVERY_OUTCOME_UNKNOWN);
            let failed = current
                .publication
                .social_outbox_jobs
                .iter()
                .any(|job| job.status == crate::outbox::STATUS_FAILED_TERMINAL);
            let completed = !current.publication.social_outbox_jobs.is_empty()
                && current
                    .publication
                    .social_outbox_jobs
                    .iter()
                    .all(|job| job.status == crate::outbox::STATUS_DELIVERED);
            if !unknown && !failed && !completed {
                continue;
            }
            if failed
                && current.publication.status == ContentCampaignPublicationStatus::RequiresReview
                && !unknown
            {
                continue;
            }
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: if failed || unknown {
                        ContentCampaignPublicationStatus::RequiresReview
                    } else {
                        ContentCampaignPublicationStatus::Completed
                    },
                    actual_canonical_url: current.publication.actual_canonical_url.as_deref(),
                    review_reason: if unknown {
                        Some("social_delivery_outcome_unknown")
                    } else {
                        failed.then_some("social_delivery_failed")
                    },
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        }
        let blog = &current.publication.blog_outbox_job;
        if blog.status == crate::outbox::STATUS_PENDING {
            continue;
        }
        if blog.status == crate::outbox::STATUS_FAILED_TERMINAL {
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: ContentCampaignPublicationStatus::RequiresReview,
                    actual_canonical_url: None,
                    review_reason: Some("blog_publish_failed"),
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        }
        if blog.status == crate::outbox::STATUS_DELIVERY_OUTCOME_UNKNOWN {
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: ContentCampaignPublicationStatus::RequiresReview,
                    actual_canonical_url: None,
                    review_reason: Some("blog_delivery_outcome_unknown"),
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        }
        if blog.dry_run == Some(true) {
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: ContentCampaignPublicationStatus::BlogDryRun,
                    actual_canonical_url: None,
                    review_reason: None,
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        }
        let Some(actual_raw) = blog.provider_object_id.as_deref() else {
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: ContentCampaignPublicationStatus::RequiresReview,
                    actual_canonical_url: None,
                    review_reason: Some("blog_canonical_url_missing"),
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        };
        let actual =
            match crate::slices::social_publishing::service::normalize_canonical_url(actual_raw) {
                Ok(url) => url,
                Err(_) => {
                    store::settle_campaign_publication(
                        conn,
                        client_id,
                        &current,
                        store::CampaignSettlement {
                            status: ContentCampaignPublicationStatus::RequiresReview,
                            actual_canonical_url: Some(actual_raw),
                            review_reason: Some("blog_canonical_url_invalid"),
                            social_jobs: &[],
                            now_ms,
                        },
                    )?;
                    settled += 1;
                    continue;
                }
            };
        if actual != current.publication.expected_canonical_url {
            store::settle_campaign_publication(
                conn,
                client_id,
                &current,
                store::CampaignSettlement {
                    status: ContentCampaignPublicationStatus::RequiresReview,
                    actual_canonical_url: Some(&actual),
                    review_reason: Some("blog_canonical_url_changed"),
                    social_jobs: &[],
                    now_ms,
                },
            )?;
            settled += 1;
            continue;
        }

        ensure_plan_published(conn, client_id, &current, &actual, now_ms)?;
        let jobs = campaign_social_jobs(client_id, &current)?;
        let status = if jobs.is_empty() {
            ContentCampaignPublicationStatus::Completed
        } else {
            ContentCampaignPublicationStatus::SocialEnqueued
        };
        store::settle_campaign_publication(
            conn,
            client_id,
            &current,
            store::CampaignSettlement {
                status,
                actual_canonical_url: Some(&actual),
                review_reason: None,
                social_jobs: &jobs,
                now_ms,
            },
        )?;
        settled += 1;
    }
    Ok(settled)
}

fn campaign_social_jobs(
    client_id: &str,
    current: &bos_contracts::content_plans::ContentCampaignPublicationWithRevision,
) -> Result<Vec<crate::outbox::NewOutboxJob>, StoreError> {
    if current.publication.approved_social_targets.is_empty() {
        return Ok(Vec::new());
    }
    let proposal_id = current
        .publication
        .social_proposal_id
        .clone()
        .ok_or_else(|| StoreError::Domain("content_campaign_social_snapshot_invalid".into()))?;
    let proposal_revision = current
        .publication
        .social_proposal_revision
        .ok_or_else(|| StoreError::Domain("content_campaign_social_snapshot_invalid".into()))?;
    let proposal = SocialPostProposal {
        proposal_id,
        source_id: None,
        source_content_draft_id: Some(current.publication.content_draft_id.clone()),
        source_content_draft_revision: Some(current.publication.content_draft_revision),
        canonical_url: current.publication.expected_canonical_url.clone(),
        status: SocialProposalStatus::Staged,
        targets: current.publication.approved_social_targets.clone(),
        approved_by: None,
        approved_revision: None,
        created_at_ms: current.publication.approved_at_ms,
        updated_at_ms: current.publication.approved_at_ms,
    };
    let mut jobs = crate::slices::social_publishing::service::build_channel_jobs(
        client_id,
        &proposal,
        &current.publication.approved_by,
        proposal_revision,
        current.publication.approved_at_ms,
    )?;
    for job in &mut jobs {
        job.source_entity_kind = "content_campaign_social_target".to_string();
        job.source_entity_id = format!(
            "{}:{}",
            current.publication.publication_id, job.source_entity_id
        );
        job.correlation_id = Some(current.publication.publication_id.clone());
        job.causation_id = Some(current.publication.blog_outbox_job.job_id.clone());
    }
    Ok(jobs)
}

fn ensure_plan_published(
    conn: &mut Connection,
    client_id: &str,
    current: &bos_contracts::content_plans::ContentCampaignPublicationWithRevision,
    actual_url: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    let plan = store::get_item(conn, client_id, &current.publication.plan_item_id)?
        .ok_or_else(|| StoreError::Domain("content_plan_not_found".to_string()))?;
    if plan.item.status == ContentPlanStatus::Published {
        return if plan.item.published_url.as_deref() == Some(actual_url) {
            Ok(())
        } else {
            Err(StoreError::Domain(
                "content_campaign_plan_url_mismatch".to_string(),
            ))
        };
    }
    let inventory = published_plan_inventory_row(client_id, &plan.item, actual_url, now_ms)?;
    let outcome = store::mark_published(
        conn,
        crate::slices::mutation_context::MutationContext {
            client_id,
            actor_id: &current.publication.approved_by,
            expected_revision: Some(plan.revision),
            idempotency_key: &format!(
                "campaign-plan-published:{}",
                current.publication.publication_id
            ),
            now_ms,
        },
        &plan.item,
        actual_url,
        &inventory,
    )?;
    match outcome {
        MutationOutcome::Applied { .. } | MutationOutcome::ReplayedIdempotent { .. } => Ok(()),
        MutationOutcome::RevisionConflict { .. } => Err(StoreError::Domain(
            "content_campaign_plan_revision_changed".to_string(),
        )),
    }
}

fn validate_slug_matches_expected_url(slug: &str, expected_url: &str) -> Result<(), StoreError> {
    let url = url::Url::parse(expected_url)
        .map_err(|_| StoreError::Domain("social_canonical_url_invalid".to_string()))?;
    let expected_slug = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
        .unwrap_or_default();
    if expected_slug != slug.trim() {
        return Err(StoreError::Domain(
            "content_campaign_preview_url_slug_mismatch".to_string(),
        ));
    }
    Ok(())
}

fn campaign_publication_id(
    client_id: &str,
    plan: &ContentPlanItem,
    idempotency_key: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(client_id.as_bytes());
    hasher.update([0]);
    hasher.update(plan.plan_item_id.as_bytes());
    hasher.update([0]);
    hasher.update(idempotency_key.as_bytes());
    format!("ccp_{}", hex_prefix(&hasher.finalize(), 16))
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    bytes
        .iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
