//! Grounded content drafting (the `content_draft` packet kind): operator
//! brief → BM25 top-k over the drive corpus → deterministic evidence
//! selection to a hard snippet budget → ONE drafting transform whose claims
//! must cite snippet ids → deterministic citation gate. Token economics by
//! construction: retrieval is free (local FTS5), the model sees at most
//! EVIDENCE_MAX_SNIPPETS × EVIDENCE_SNIPPET_CHARS of evidence, and there is
//! exactly one LLM call per draft. DRAFT-ONLY: approval has no provider
//! write; publish stays manual.
//!
//! Harvested from agent-monitor-rust: the snippet budget + heading-path
//! term scoring (build_evidence_snippets) as the secondary scorer/dedupe
//! over FTS candidates, and the claim-support triad + citation-coverage
//! gate (citation_coverage_for_claims) — unsupported/uncited claims block
//! approval-readiness.

use bos_contracts::content_drafts::{
    ContentCitationGate, ContentClaim, ContentClaimStatus, ContentDraft, ContentDraftStatus,
    ContentEvidenceSnippet,
};
use bos_contracts::drive_corpus::DriveSearchHit;
use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentEligibility, EnrichmentFieldProposal, EnrichmentPlan,
    EnrichmentRunStatus, EnrichmentSeedEvidence, EnrichmentTier, EnrichmentTierEvent,
};
use bos_contracts::work_queue::WorkItem;
use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRawOutputRetention, TypedLlmRedactionPolicy,
    TypedLlmResponseFormat, TypedLlmRetryPolicy, TypedLlmSafetyPolicy, TypedLlmSourceEntity,
    TypedLlmTaskCapabilities, TypedLlmTaskClass, TypedLlmTaskInput, TypedLlmTaskRequest,
    TypedLlmTaskSpec, TypedLlmTextBlock,
};
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::env_registry;
use crate::outbox::{retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob};
use crate::slices::enrichment::service as enrichment_engine;
use crate::store_core::StoreError;

pub const PACKET_KIND: &str = "content_draft";
pub const FILL_SCHEMA_REF: &str = "bos.content_drafts.grounded_draft.v1";
pub const FILL_PURPOSE: &str = "content_grounded_draft";
pub const CONTENT_WEB_FACTS_ACTOR: &str = "content_company_facts";
pub const PROVIDER_CONTENT_PUBLISH_ADAPTER: &str = "content_publish_adapter";
pub const CAPABILITY_PUBLISH_POST: &str = "publish_post";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContentPublishPayload {
    pub schema_version: u32,
    pub client_id: String,
    pub draft_id: String,
    pub title: String,
    pub slug: String,
    pub published_at: String,
    pub body_markdown: String,
    pub target_query: Option<String>,
    pub meta_description: String,
}

#[derive(Debug, serde::Deserialize)]
struct ContentPublishAdapterResponse {
    published_url: String,
}

pub fn publishing_available() -> bool {
    env_registry::string(&env_registry::BOS_CONTENT_PUBLISH_ADAPTER_URL).is_some()
        && env_registry::string(&env_registry::BOS_CONTENT_PUBLISH_ADAPTER_TOKEN).is_some()
}

pub fn publishing_live_enabled(conn: &Connection, client_id: &str) -> bool {
    crate::slices::admin_settings::service::flag(
        conn,
        client_id,
        &env_registry::BOS_CONTENT_PUBLISH_WRITE_ENABLED,
    )
    .unwrap_or(false)
}

pub fn build_publish_job(
    client_id: &str,
    draft: &ContentDraft,
    slug_raw: &str,
    published_at_raw: &str,
    idempotency_key: &str,
) -> Result<NewOutboxJob, StoreError> {
    if draft.status != ContentDraftStatus::Approved {
        return Err(StoreError::Domain(
            "content_publish_not_approved".to_string(),
        ));
    }
    if !draft.citation_gate.passed {
        return Err(StoreError::Domain(
            "content_citation_gate_failed".to_string(),
        ));
    }
    let slug = slug_raw.trim();
    if !valid_publish_slug(slug) {
        return Err(StoreError::Domain(
            "content_publish_slug_invalid".to_string(),
        ));
    }
    let published_at = published_at_raw.trim();
    if !valid_civil_date(published_at) {
        return Err(StoreError::Domain(
            "content_publish_date_invalid".to_string(),
        ));
    }
    let Some(meta_description) = draft
        .meta_description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(StoreError::Domain(
            "content_publish_meta_description_required".to_string(),
        ));
    };
    let payload = ContentPublishPayload {
        schema_version: 1,
        client_id: client_id.to_string(),
        draft_id: draft.draft_id.clone(),
        title: draft.title.clone(),
        slug: slug.to_string(),
        published_at: published_at.to_string(),
        body_markdown: draft.body_markdown.clone(),
        target_query: draft.target_query.clone(),
        meta_description: meta_description.to_string(),
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|err| StoreError::Domain(format!("serialize content publish payload: {err}")))?;
    let digest = Sha256::digest(idempotency_key.as_bytes());
    let suffix: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(NewOutboxJob {
        job_id: format!("content_publish_{}_{}", draft.draft_id, suffix),
        provider: PROVIDER_CONTENT_PUBLISH_ADAPTER.to_string(),
        capability: CAPABILITY_PUBLISH_POST.to_string(),
        payload_json,
        source_entity_kind: super::store::DRAFT_ENTITY_KIND.to_string(),
        source_entity_id: draft.draft_id.clone(),
        correlation_id: Some(draft.item_id.clone()),
        causation_id: None,
        idempotency_key: format!("outbox:content_publish:{idempotency_key}"),
    })
}

fn valid_publish_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 120
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        })
}

fn valid_civil_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Ok(year) = value[0..4].parse::<u32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day >= 1 && day <= max_day
}

pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_CONTENT_PUBLISH_ADAPTER || job.capability != CAPABILITY_PUBLISH_POST
    {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let write_enabled = {
        let persistence = state.persistence.lock();
        publishing_live_enabled(persistence.connection_ref(), &state.client_id)
    };
    if !write_enabled {
        return AttemptOutcome::Delivered {
            result_json: serde_json::json!({"dry_run": true}).to_string(),
        };
    }
    let Some(url) = env_registry::string(&env_registry::BOS_CONTENT_PUBLISH_ADAPTER_URL) else {
        return AttemptOutcome::Retry {
            error: "content_publish_adapter_url_missing".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        };
    };
    let Some(token) = env_registry::string(&env_registry::BOS_CONTENT_PUBLISH_ADAPTER_TOKEN) else {
        return AttemptOutcome::Retry {
            error: "content_publish_adapter_token_missing".to_string(),
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        };
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("content_publish_http_client:{err}"),
                result_json: None,
            }
        }
    };
    let response = match client
        .post(format!("{}/publish", url.trim_end_matches('/')))
        .bearer_auth(token)
        .header("Idempotency-Key", &job.idempotency_key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(job.payload_json.clone())
        .send()
    {
        Ok(response) => response,
        Err(err) => {
            return AttemptOutcome::Retry {
                error: format!("content_publish_adapter_request:{err}"),
                retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
            }
        }
    };
    let status = response.status();
    if status.is_success() {
        return match response.json::<ContentPublishAdapterResponse>() {
            Ok(result) if !result.published_url.trim().is_empty() => AttemptOutcome::Delivered {
                result_json: serde_json::json!({
                    "dry_run": false,
                    "provider_object_id": result.published_url,
                })
                .to_string(),
            },
            Ok(_) => AttemptOutcome::Terminal {
                error: "content_publish_adapter_response_missing_url".to_string(),
                result_json: None,
            },
            Err(err) => AttemptOutcome::Terminal {
                error: format!("content_publish_adapter_response_invalid:{err}"),
                result_json: None,
            },
        };
    }
    let error = format!("content_publish_adapter_http_{}", status.as_u16());
    if status.as_u16() == 429 || status.is_server_error() {
        AttemptOutcome::Retry {
            error,
            retry_at_ms: now_ms + retry_backoff_ms(job.attempts),
        }
    } else {
        AttemptOutcome::Terminal {
            error,
            result_json: None,
        }
    }
}

/// Retrieval geometry: wide candidate pool, hard snippet budget.
pub const EVIDENCE_TOP_K: usize = 30;
pub const EVIDENCE_MAX_SNIPPETS: usize = 10;
pub const EVIDENCE_MAX_PER_DOC: usize = 3;
pub const EVIDENCE_SNIPPET_CHARS: usize = 900;
pub const WEB_EVIDENCE_MAX_SNIPPETS: usize = 3;
const WEB_FACT_TEXT_SCAN_CHARS: usize = 12_000;

/// The brief a draft grounds against: the work item title + the source body.
pub fn brief_text(item: &WorkItem, message: &InboundMessageRecord) -> String {
    let mut brief = item.title.trim().to_string();
    let body = crate::slices::email_triage::service::body_for_ai(message);
    let body = body.trim();
    if !body.is_empty() {
        if !brief.is_empty() {
            brief.push_str("\n\n");
        }
        brief.push_str(body);
    }
    brief
}

/// Deterministic evidence selection: FTS5 BM25 candidates re-scored by
/// brief-term overlap (text + heading/title hits weigh extra — agent_monitor's
/// heading-path prior), deduped (per-document cap + near-duplicate text from
/// overlapping chunks), trimmed to the snippet budget. Domain errors are the
/// operator's to act on: an unsearchable brief or an empty corpus.
pub fn select_evidence(
    conn: &Connection,
    client_id: &str,
    brief: &str,
) -> Result<Vec<ContentEvidenceSnippet>, StoreError> {
    let Some(match_expr) = crate::slices::drive_corpus::service::fts_match_expression(brief) else {
        return Err(StoreError::Domain("content_brief_unsearchable".to_string()));
    };
    let candidates = crate::slices::drive_corpus::store::search_chunks(
        conn,
        client_id,
        &match_expr,
        EVIDENCE_TOP_K,
    )?;
    if candidates.is_empty() {
        return Err(StoreError::Domain("content_no_evidence".to_string()));
    }
    let terms = normalized_terms(brief);
    // Stable secondary sort: term-overlap score desc, BM25 order as the tie
    // break (candidates arrive BM25-ascending = best first).
    let mut scored: Vec<(usize, usize, &DriveSearchHit)> = candidates
        .iter()
        .enumerate()
        .map(|(rank, hit)| (snippet_score(hit, &terms), rank, hit))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let mut selected: Vec<ContentEvidenceSnippet> = Vec::new();
    let mut per_doc: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (_score, _rank, hit) in scored {
        if selected.len() >= EVIDENCE_MAX_SNIPPETS {
            break;
        }
        let doc_uses = per_doc.entry(hit.file_id.as_str()).or_insert(0);
        if *doc_uses >= EVIDENCE_MAX_PER_DOC {
            continue;
        }
        // Overlapping chunks share text; drop a candidate contained in (or
        // containing) an already-selected snippet from the same document.
        let near_duplicate = selected.iter().any(|snippet| {
            snippet.file_id == hit.file_id
                && (snippet.text.contains(probe(&hit.text))
                    || hit.text.contains(probe(&snippet.text)))
        });
        if near_duplicate {
            continue;
        }
        *doc_uses += 1;
        selected.push(ContentEvidenceSnippet {
            snippet_id: hit.chunk_id.clone(),
            file_id: hit.file_id.clone(),
            doc_title: hit.doc_title.clone(),
            heading_path: hit.heading_path.clone(),
            text: trim_snippet(&hit.text),
            web_view_link: hit.web_view_link.clone(),
        });
    }
    Ok(selected)
}

/// A stable probe substring for containment-based dedupe (full-text compare
/// fails once trimming differs).
fn probe(text: &str) -> &str {
    let end = text
        .char_indices()
        .nth(160)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    text[..end].trim()
}

fn normalized_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 2)
        .map(str::to_ascii_lowercase)
        .collect();
    terms.sort_unstable();
    terms.dedup();
    terms
}

/// Term-overlap score with the heading-path prior: a term hit in the
/// document title or heading path counts double.
fn snippet_score(hit: &DriveSearchHit, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 1;
    }
    let body = hit.text.to_ascii_lowercase();
    let context = format!("{} {}", hit.doc_title, hit.heading_path.join(" ")).to_ascii_lowercase();
    terms
        .iter()
        .map(|term| {
            let mut score = 0usize;
            if body.contains(term.as_str()) {
                score += 1;
            }
            if context.contains(term.as_str()) {
                score += 2;
            }
            score
        })
        .sum()
}

fn trim_snippet(text: &str) -> String {
    let mut snippet: String = text.trim().chars().take(EVIDENCE_SNIPPET_CHARS).collect();
    if text.trim().chars().count() > EVIDENCE_SNIPPET_CHARS {
        snippet.push_str("...");
    }
    snippet
}

pub(crate) fn extract_web_fact_snippets(
    target_id: &str,
    domain: &str,
    pages: &[bos_integrations::web_page_read::FetchedPage],
    brief: &str,
) -> Vec<ContentEvidenceSnippet> {
    let terms = normalized_terms(&format!("{brief} {domain}"));
    let page_texts: Vec<(usize, &str, String)> = pages
        .iter()
        .enumerate()
        .map(|(page_rank, page)| {
            (
                page_rank,
                page.url.as_str(),
                bos_integrations::web_page_read::strip_to_text(
                    &page.html,
                    WEB_FACT_TEXT_SCAN_CHARS,
                ),
            )
        })
        .collect();
    extract_web_fact_snippets_from_page_texts(target_id, domain, &page_texts, &terms)
}

#[derive(Debug)]
struct WebFactCandidate {
    score: usize,
    page_rank: usize,
    span_rank: usize,
    snippet: ContentEvidenceSnippet,
}

fn extract_web_fact_snippets_from_page_texts(
    target_id: &str,
    domain: &str,
    page_texts: &[(usize, &str, String)],
    terms: &[String],
) -> Vec<ContentEvidenceSnippet> {
    let mut candidates = Vec::new();
    for (page_rank, url, page_text) in page_texts {
        for (span_rank, span) in web_fact_spans(page_text).into_iter().enumerate() {
            let score = web_fact_score(&span, terms);
            let stable = stable_web_snippet_id(target_id, url, &span);
            candidates.push(WebFactCandidate {
                score,
                page_rank: *page_rank,
                span_rank,
                snippet: ContentEvidenceSnippet {
                    snippet_id: stable,
                    file_id: format!("web:{domain}"),
                    doc_title: format!("Web facts: {domain}"),
                    heading_path: vec!["Web facts".to_string(), (*url).to_string()],
                    text: trim_snippet(&span),
                    web_view_link: Some((*url).to_string()),
                },
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.page_rank.cmp(&b.page_rank))
            .then(a.span_rank.cmp(&b.span_rank))
            .then(a.snippet.snippet_id.cmp(&b.snippet.snippet_id))
    });

    let mut selected: Vec<ContentEvidenceSnippet> = Vec::new();
    for candidate in candidates {
        if selected.len() >= WEB_EVIDENCE_MAX_SNIPPETS {
            break;
        }
        if candidate.snippet.text.chars().count() < 80 {
            continue;
        }
        let duplicate = selected.iter().any(|snippet| {
            snippet.web_view_link == candidate.snippet.web_view_link
                && (snippet.text.contains(probe(&candidate.snippet.text))
                    || candidate.snippet.text.contains(probe(&snippet.text)))
        });
        if !duplicate {
            selected.push(candidate.snippet);
        }
    }
    selected
}

#[cfg(test)]
pub(crate) fn extract_web_fact_snippets_from_legacy_page_texts(
    target_id: &str,
    domain: &str,
    page_texts: &[(usize, &str, String)],
    brief: &str,
) -> Vec<ContentEvidenceSnippet> {
    let terms = normalized_terms(&format!("{brief} {domain}"));
    extract_web_fact_snippets_from_page_texts(target_id, domain, page_texts, &terms)
}

fn web_fact_spans(page_text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut current = String::new();
    for line in page_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if current.len() + line.len() + 1 > EVIDENCE_SNIPPET_CHARS {
            if !current.trim().is_empty() {
                spans.push(current.trim().to_string());
            }
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        spans.push(current.trim().to_string());
    }
    spans
}

fn web_fact_score(span: &str, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 1;
    }
    let haystack = span.to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count()
}

fn stable_web_snippet_id(target_id: &str, url: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(target_id.as_bytes());
    hasher.update(b":");
    hasher.update(url.as_bytes());
    hasher.update(b":");
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    format!("web:{target_id}:{}", hex_prefix(&digest, 8))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .take(len)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn merge_evidence_with_web(
    mut local: Vec<ContentEvidenceSnippet>,
    web: Vec<ContentEvidenceSnippet>,
) -> Vec<ContentEvidenceSnippet> {
    if local.len() >= EVIDENCE_MAX_SNIPPETS {
        local.truncate(EVIDENCE_MAX_SNIPPETS);
        return local;
    }
    let remaining = EVIDENCE_MAX_SNIPPETS - local.len();
    let web_take = remaining.min(WEB_EVIDENCE_MAX_SNIPPETS);
    for snippet in web {
        if local.len() >= EVIDENCE_MAX_SNIPPETS {
            break;
        }
        if local
            .iter()
            .any(|existing| existing.snippet_id == snippet.snippet_id)
        {
            continue;
        }
        local.push(snippet);
        if local
            .iter()
            .filter(|snippet| snippet.file_id.starts_with("web:"))
            .count()
            >= web_take
        {
            break;
        }
    }
    local
}

fn content_web_facts_enabled(state: &crate::http::AppState) -> bool {
    let persistence = state.persistence.lock();
    let content_enabled = crate::slices::admin_settings::service::flag(
        persistence.connection_ref(),
        &state.client_id,
        &env_registry::BOS_CONTENT_WEB_FACTS_ENABLED,
    )
    .unwrap_or(false);
    let web_enabled = crate::slices::admin_settings::service::flag(
        persistence.connection_ref(),
        &state.client_id,
        &env_registry::BOS_WEB_ENRICHMENT_ENABLED,
    )
    .unwrap_or(false);
    content_enabled && web_enabled
}

fn live_content_crawl(
    domain: &str,
) -> Result<
    Vec<bos_integrations::web_page_read::FetchedPage>,
    bos_integrations::web_page_read::WebFetchError,
> {
    crate::slices::enrichment::web_tier::live_guarded_crawl(domain)
}

pub(crate) fn enrich_context_with_web_facts(
    state: &crate::http::AppState,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: serde_json::Value,
    attempt: u64,
    enabled: bool,
    crawl: &dyn Fn(
        &str,
    ) -> Result<
        Vec<bos_integrations::web_page_read::FetchedPage>,
        bos_integrations::web_page_read::WebFetchError,
    >,
) -> serde_json::Value {
    let original = context.clone();
    match enrich_context_with_web_facts_inner(
        state, item, message, context, attempt, enabled, crawl,
    ) {
        Ok(context) => context,
        Err(err) => {
            tracing::info!(item_id = %item.item_id, error = %err, "content web facts enrichment skipped");
            original
        }
    }
}

fn enrich_context_with_web_facts_inner(
    state: &crate::http::AppState,
    item: &WorkItem,
    message: &InboundMessageRecord,
    context: serde_json::Value,
    attempt: u64,
    enabled: bool,
    crawl: &dyn Fn(
        &str,
    ) -> Result<
        Vec<bos_integrations::web_page_read::FetchedPage>,
        bos_integrations::web_page_read::WebFetchError,
    >,
) -> Result<serde_json::Value, StoreError> {
    if !enabled {
        return Ok(context);
    }
    let local_evidence: Vec<ContentEvidenceSnippet> = context
        .get("evidence")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let brief = brief_text(item, message);
    let target_id = format!("cnt_{}_{}", item.item_id, attempt);
    let subject = ContentCompanyFactsSubject {
        target_id: target_id.clone(),
        item: item.clone(),
        brief: brief.clone(),
        local_evidence_count: local_evidence.len(),
        crawl,
    };
    let outcome = enrichment_engine::run(
        state,
        enrichment_engine::EnrichmentRunContext {
            slice_id: "content_drafts",
            actor_id: CONTENT_WEB_FACTS_ACTOR,
            item,
        },
        &subject,
    );
    let web_facts = {
        let persistence = state.persistence.lock();
        super::store::web_facts_by_run(
            persistence.connection_ref(),
            &state.client_id,
            &outcome.run_id,
        )?
    };
    if web_facts.is_empty() {
        return Ok(context);
    }
    let evidence = merge_evidence_with_web(local_evidence, web_facts);
    let mut context = context;
    let evidence_json = serde_json::to_value(evidence)
        .map_err(|err| StoreError::Domain(format!("serialize evidence: {err}")))?;
    if let Some(object) = context.as_object_mut() {
        object.insert("evidence".to_string(), evidence_json);
        Ok(context)
    } else {
        Ok(serde_json::json!({ "evidence": evidence_json }))
    }
}

struct ContentCompanyFactsSubject<'a> {
    target_id: String,
    item: WorkItem,
    brief: String,
    local_evidence_count: usize,
    crawl: &'a dyn Fn(
        &str,
    ) -> Result<
        Vec<bos_integrations::web_page_read::FetchedPage>,
        bos_integrations::web_page_read::WebFetchError,
    >,
}

impl enrichment_engine::EnrichmentSubject for ContentCompanyFactsSubject<'_> {
    fn draft_id(&self) -> &str {
        &self.target_id
    }

    fn item_id(&self) -> &str {
        &self.item.item_id
    }

    fn plan(&self) -> EnrichmentPlan {
        EnrichmentPlan {
            subject: "content_company_facts".to_string(),
            fields: vec![enrichment_engine::field_spec(
                "company_facts",
                "description",
                EnrichmentEligibility::MissingOnly,
                EnrichmentConfidence::Medium,
            )],
            seed_evidence: vec![EnrichmentSeedEvidence {
                source_id: format!("{}:{}", self.item.source_kind, self.item.source_ref),
                label: "Brief".to_string(),
                quote: Some(self.brief.chars().take(500).collect()),
            }],
            enabled_tiers: vec![EnrichmentTier::Local, EnrichmentTier::WebSearch],
            stop_policy: vec![
                "web_facts_persisted".to_string(),
                "no_literal_domain_for_tier3".to_string(),
                "tier_budget_exhausted".to_string(),
            ],
        }
    }

    fn tier1_events(&self) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
        let events = vec![enrichment_engine::source_evidence_event(
            &format!("{}:{}", self.item.source_kind, self.item.source_ref),
            &format!("local_corpus_snippets:{}", self.local_evidence_count),
        )];
        (events, Vec::new())
    }

    fn literal_domain(&self) -> Option<String> {
        bos_integrations::web_page_read::find_domain(&self.brief)
    }

    fn run_web_search_tier(
        &self,
        state: &crate::http::AppState,
        _ctx: enrichment_engine::EnrichmentRunContext<'_>,
        run: enrichment_engine::EnrichmentRunHandle<'_>,
        domain: &str,
    ) -> enrichment_engine::EnrichmentOutcome {
        let existing = {
            let persistence = state.persistence.lock();
            super::store::web_facts_by_run(
                persistence.connection_ref(),
                &state.client_id,
                run.run_id(),
            )
            .unwrap_or_default()
        };
        if !existing.is_empty() {
            let (events, proposals) = web_fact_diagnostics(&existing, "cache");
            run.append(state, "tier3-cache", &events, &proposals, 0);
            return run.transition(
                state,
                EnrichmentRunStatus::Completed,
                "persisted_web_facts_reused",
            );
        }

        let pages = match crate::slices::enrichment::web_tier::crawl_outcome(
            state,
            run,
            domain,
            (self.crawl)(domain),
        ) {
            std::ops::ControlFlow::Continue(pages) => pages,
            std::ops::ControlFlow::Break(outcome) => return outcome,
        };

        let snippets = extract_web_fact_snippets(&self.target_id, domain, &pages, &self.brief);
        let (events, proposals) = web_fact_diagnostics(&snippets, "deterministic");
        run.append(state, "tier3-deterministic", &events, &proposals, 0);
        if snippets.is_empty() {
            return run.transition(state, EnrichmentRunStatus::Partial, "no_usable_web_facts");
        }

        let mut persistence = state.persistence.lock();
        let record = super::store::ContentWebFactsRecord {
            target_id: self.target_id.clone(),
            item_id: self.item.item_id.clone(),
            source_kind: self.item.source_kind.clone(),
            source_ref: self.item.source_ref.clone(),
            run_id: run.run_id().to_string(),
            snippets,
        };
        let idempotency_key = format!("contentwebfacts:{}:{}", self.target_id, run.run_id());
        let ctx = super::store::DraftActionContext {
            client_id: &state.client_id,
            actor_id: CONTENT_WEB_FACTS_ACTOR,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            now_ms: crate::http::now_ms(),
        };
        if let Err(err) = super::store::persist_web_facts(persistence.connection(), ctx, &record) {
            tracing::warn!(item_id = %self.item.item_id, error = %err, "content web facts apply failed");
            drop(persistence);
            let events = vec![enrichment_engine::skip_event(
                EnrichmentTier::WebSearch,
                "failure",
                &format!("apply_failed:{err}"),
            )];
            run.append(state, "tier3-apply-failed", &events, &[], 0);
            return run.transition(state, EnrichmentRunStatus::Failed, "apply_failed");
        }
        drop(persistence);
        run.transition(state, EnrichmentRunStatus::Completed, "web_facts_persisted")
    }
}

fn web_fact_diagnostics(
    snippets: &[ContentEvidenceSnippet],
    reason: &str,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>) {
    let values =
        snippets.iter().map(
            |snippet| crate::slices::enrichment::web_tier::AcceptedValue {
                field_id: "company_facts",
                value: &snippet.text,
                quote: &snippet.text,
                provenance_refs: vec![snippet.snippet_id.clone()],
            },
        );
    crate::slices::enrichment::web_tier::accepted_value_diagnostics(
        values,
        EnrichmentTier::WebSearch,
        reason,
    )
}

/// Render the evidence pack the model sees — one deterministic block, each
/// snippet addressable by its id.
pub fn render_evidence_block(evidence: &[ContentEvidenceSnippet]) -> String {
    let mut out = String::new();
    for snippet in evidence {
        out.push_str(&format!(
            "[{}] {} — {}\n{}\n---\n",
            snippet.snippet_id,
            snippet.doc_title,
            if snippet.heading_path.is_empty() {
                "(document root)".to_string()
            } else {
                snippet.heading_path.join(" > ")
            },
            snippet.text,
        ));
    }
    out
}

pub fn build_grounded_draft_request(
    client_id: &str,
    item: &WorkItem,
    message: &InboundMessageRecord,
    evidence: &[ContentEvidenceSnippet],
    background: Option<TypedLlmTextBlock>,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("content_draft_{}_{attempt}", item.item_id);
    let mut request = TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: item.item_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "work_item".to_string(),
            entity_id: item.item_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "content_grounded_draft".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: FILL_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 32 * 1024,
            max_tokens: 0, // filled from runtime config
            timeout_ms: 0, // filled from runtime config
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Write a grounded content draft (e.g. a blog post or web page section) from the BRIEF, using ONLY the EVIDENCE snippets as factual sources. Respond with a single JSON object with EXACTLY these fields: title (the piece's title), body_markdown (the full draft in markdown; every factual statement must come from the evidence — do not invent facts, statistics, prices, or capabilities), target_query (the primary search query this piece targets, or null), meta_description (140-160 char search snippet, or null), claims (array of {text, snippet_ids}: every distinct factual claim made in the body, each citing the snippet id(s) — the [bracketed] ids in EVIDENCE — that support it verbatim-or-near-verbatim; a claim with no supporting snippet must NOT appear in the body at all), confidence (\"high\" | \"medium\" | \"low\"). Claims are MANDATORY: a draft asserting facts with an empty claims array is invalid.",
                "current_category": item.category_id,
            }),
            text_blocks: vec![
                TypedLlmTextBlock {
                    block_id: "brief".to_string(),
                    text: brief_text(item, message),
                },
                TypedLlmTextBlock {
                    block_id: "evidence".to_string(),
                    text: render_evidence_block(evidence),
                },
            ],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::Harness, // realigned by the router
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 300_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention: TypedLlmRawOutputRetention::None,
        },
    };
    // Optional company-background grounding (tone/context only).
    if let Some(block) = background {
        request.input.text_blocks.push(block);
    }
    request
}

/// A validated grounded-draft fill (claims not yet gated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundedDraftFill {
    pub title: String,
    pub body_markdown: String,
    pub target_query: Option<String>,
    pub meta_description: Option<String>,
    /// (claim text, cited snippet ids)
    pub claims: Vec<(String, Vec<String>)>,
    pub confidence: String,
}

pub fn parse_grounded_draft_response(
    response: &serde_json::Value,
) -> Result<GroundedDraftFill, String> {
    let title = string_field(response, "title").ok_or("title missing or empty")?;
    let body_markdown =
        string_field(response, "body_markdown").ok_or("body_markdown missing or empty")?;
    if body_markdown.len() > 60_000 {
        return Err("body_markdown implausibly long".to_string());
    }
    let confidence = string_field(response, "confidence")
        .filter(|raw| matches!(raw.as_str(), "high" | "medium" | "low"))
        .ok_or("confidence missing or invalid")?;
    let claims_raw = response
        .get("claims")
        .and_then(serde_json::Value::as_array)
        .ok_or("claims missing")?;
    let claims: Vec<(String, Vec<String>)> = claims_raw
        .iter()
        .filter_map(|entry| {
            let text: String = entry
                .get("text")?
                .as_str()?
                .trim()
                .chars()
                .take(500)
                .collect();
            if text.is_empty() {
                return None;
            }
            let snippet_ids = entry
                .get("snippet_ids")
                .and_then(serde_json::Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some((text, snippet_ids))
        })
        .collect();
    // A factual draft with zero claims dodges the grounding contract.
    if claims.is_empty() {
        return Err("claims empty — the grounding contract requires cited claims".to_string());
    }
    if claims.len() > 100 {
        return Err("claims implausibly many".to_string());
    }
    Ok(GroundedDraftFill {
        title: title.chars().take(200).collect(),
        body_markdown,
        target_query: string_field(response, "target_query").map(|q| q.chars().take(200).collect()),
        meta_description: string_field(response, "meta_description")
            .map(|d| d.chars().take(300).collect()),
        claims,
        confidence,
    })
}

/// Does the snippet text plausibly support the claim? Harvested rule of
/// thumb from agent_monitor's claim matcher: every brief-sized claim term present,
/// or at least 3 of them. Deterministic and conservative — the operator
/// reads the draft either way; this gates obviously-unsupported citations.
pub fn claim_supported_by_snippet(claim: &str, snippet_text: &str) -> bool {
    let terms = normalized_terms(claim);
    if terms.is_empty() {
        return false;
    }
    let haystack = snippet_text.to_ascii_lowercase();
    let matching = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    matching == terms.len() || matching >= 3
}

/// The deterministic citation gate (agent_monitor's citation_coverage_for_claims
/// shape): every claim must cite at least one known snippet that supports
/// it. Unknown snippet ids are ignored; a claim left with none is
/// MissingCitation; cited-but-unsupporting is Unsupported. The gate passes
/// only when every claim is Supported — approval requires it.
pub fn evaluate_citation_gate(
    claims: &[(String, Vec<String>)],
    evidence: &[ContentEvidenceSnippet],
) -> (Vec<ContentClaim>, ContentCitationGate) {
    let checked: Vec<ContentClaim> = claims
        .iter()
        .enumerate()
        .map(|(index, (text, cited_ids))| {
            let claim_id = format!("claim-{}", index + 1);
            let known: Vec<&ContentEvidenceSnippet> = cited_ids
                .iter()
                .filter_map(|id| evidence.iter().find(|snippet| &snippet.snippet_id == id))
                .collect();
            let (status, notes) = if known.is_empty() {
                (
                    ContentClaimStatus::MissingCitation,
                    Some("No citation evidence was provided for this claim.".to_string()),
                )
            } else if known
                .iter()
                .any(|snippet| claim_supported_by_snippet(text, &snippet.text))
            {
                (ContentClaimStatus::Supported, None)
            } else {
                (
                    ContentClaimStatus::Unsupported,
                    Some("No cited snippet supports this claim.".to_string()),
                )
            };
            ContentClaim {
                claim_id,
                text: text.clone(),
                snippet_ids: known
                    .iter()
                    .map(|snippet| snippet.snippet_id.clone())
                    .collect(),
                status,
                notes,
            }
        })
        .collect();
    let missing_citation_claim_ids: Vec<String> = checked
        .iter()
        .filter(|claim| claim.status == ContentClaimStatus::MissingCitation)
        .map(|claim| claim.claim_id.clone())
        .collect();
    let unsupported_claim_ids: Vec<String> = checked
        .iter()
        .filter(|claim| claim.status == ContentClaimStatus::Unsupported)
        .map(|claim| claim.claim_id.clone())
        .collect();
    let gate = ContentCitationGate {
        passed: missing_citation_claim_ids.is_empty() && unsupported_claim_ids.is_empty(),
        missing_citation_claim_ids,
        unsupported_claim_ids,
    };
    (checked, gate)
}

#[allow(clippy::too_many_arguments)]
pub fn draft_from_fill(
    item: &WorkItem,
    fill: &GroundedDraftFill,
    evidence: Vec<ContentEvidenceSnippet>,
    claims: Vec<ContentClaim>,
    gate: ContentCitationGate,
    attempt: u64,
    model: &str,
    now_ms: u64,
) -> ContentDraft {
    ContentDraft {
        draft_id: format!("cnt_{}_{attempt}", item.item_id),
        item_id: item.item_id.clone(),
        source_kind: item.source_kind.clone(),
        source_ref: item.source_ref.clone(),
        status: ContentDraftStatus::Staged,
        title: fill.title.clone(),
        body_markdown: fill.body_markdown.clone(),
        target_query: fill.target_query.clone(),
        meta_description: fill.meta_description.clone(),
        claims,
        evidence,
        citation_gate: gate,
        model: model.to_string(),
        confidence: fill.confidence.clone(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// The content kind's plug into the shared produce flow.
pub struct Produce;

impl crate::produce::ProduceFlavor for Produce {
    type Response = bos_contracts::content_drafts::ContentDraftProduceResponse;

    fn packet_kind(&self) -> &'static str {
        PACKET_KIND
    }

    fn purpose(&self) -> &'static str {
        FILL_PURPOSE
    }

    fn slice(&self) -> &'static str {
        "content_drafts"
    }

    fn already_active_code(&self) -> &'static str {
        "content_draft_already_active"
    }

    fn active_draft(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<Option<Self::Response>, StoreError> {
        Ok(
            super::store::active_draft_for_item(conn, client_id, item_id)?
                .map(|draft| bos_contracts::content_drafts::ContentDraftProduceResponse { draft }),
        )
    }

    fn draft_attempts(
        &self,
        conn: &Connection,
        client_id: &str,
        item_id: &str,
    ) -> Result<u64, StoreError> {
        super::store::count_drafts_for_item(conn, client_id, item_id)
    }

    /// Deterministic retrieval under the lock: the evidence pack the model
    /// will see, persisted into the request AND the stage-time gate.
    fn prepare_context(
        &self,
        conn: &Connection,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        _scope: &crate::http::OperatorScope,
        _actor_id: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let evidence = select_evidence(conn, client_id, &brief_text(item, message))?;
        let background = crate::produce::background_text_block(conn, client_id)?;
        let evidence = serde_json::to_value(evidence)
            .map_err(|err| StoreError::Domain(format!("serialize evidence: {err}")))?;
        Ok(serde_json::json!({ "evidence": evidence, "background": background }))
    }

    fn enrich_context_unlocked(&self, ctx: crate::produce::EnrichContext<'_>) -> serde_json::Value {
        let crate::produce::EnrichContext {
            state,
            item,
            message,
            context,
            attempt,
            ..
        } = ctx;
        enrich_context_with_web_facts(
            state,
            item,
            message,
            context,
            attempt,
            content_web_facts_enabled(state),
            &live_content_crawl,
        )
    }

    fn build_request(
        &self,
        client_id: &str,
        item: &WorkItem,
        message: &InboundMessageRecord,
        context: &serde_json::Value,
        attempt: u64,
    ) -> bos_integrations::llm_typed_tasks::TypedLlmTaskRequest {
        let evidence: Vec<ContentEvidenceSnippet> = context
            .get("evidence")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let background = context
            .get("background")
            .and_then(|v| serde_json::from_value::<TypedLlmTextBlock>(v.clone()).ok());
        build_grounded_draft_request(client_id, item, message, &evidence, background, attempt)
    }

    fn stage(&self, ctx: crate::produce::StageContext<'_>) -> Result<(), StoreError> {
        let crate::produce::StageContext {
            conn,
            client_id,
            actor_id,
            item,
            message: _message,
            response,
            context,
            model,
            attempt,
            idempotency_key,
            now_ms,
        } = ctx;
        let fill = match parse_grounded_draft_response(response) {
            Ok(fill) => fill,
            Err(parse_err) => {
                tracing::warn!(item_id = %item.item_id, error = %parse_err, "grounded draft unparseable");
                return Err(StoreError::Domain(
                    "content_fill_invalid_response".to_string(),
                ));
            }
        };
        let evidence: Vec<ContentEvidenceSnippet> = context
            .get("evidence")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|err| StoreError::Domain(format!("deserialize evidence: {err}")))?
            .unwrap_or_default();
        let (claims, gate) = evaluate_citation_gate(&fill.claims, &evidence);
        if !gate.passed {
            tracing::info!(
                item_id = %item.item_id,
                missing = gate.missing_citation_claim_ids.len(),
                unsupported = gate.unsupported_claim_ids.len(),
                "content draft staged with a FAILED citation gate (approval blocked)"
            );
        }
        let draft = draft_from_fill(item, &fill, evidence, claims, gate, attempt, model, now_ms);
        super::store::insert_draft(conn, client_id, actor_id, &draft, idempotency_key)?;
        Ok(())
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "null")
        .map(str::to_string)
}
