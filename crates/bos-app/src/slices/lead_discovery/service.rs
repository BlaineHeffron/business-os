//! Lead discovery domain logic. Source crawling/import clients are deliberately
//! outside this slice for now; this slice only accepts findings from explicitly
//! configured, approved sources and stages them for operator review.

use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::lead_discovery::{
    LeadDiscoveryCriteria, LeadDiscoverySourceConfig, LeadDiscoveryStatusResponse, LeadFinding,
    LeadFindingStageRequest, LeadFindingStatus,
};
use bos_contracts::source::{EvidenceRecord, EvidenceSourceRef, EvidenceUsagePolicy};
use sha2::{Digest, Sha256};

use crate::overlay::LeadDiscoveryOverlay;
use crate::store_core::StoreError;

pub const CATEGORY_ID: &str = "lead_discovery";
const AUTOSCRAPE_KEY_SLUG_MAX_CHARS: usize = 72;

pub fn status(overlay: &LeadDiscoveryOverlay) -> LeadDiscoveryStatusResponse {
    status_with_auto_poll_last_checked(overlay, None)
}

pub fn status_with_auto_poll_last_checked(
    overlay: &LeadDiscoveryOverlay,
    auto_poll_last_checked_at_ms: Option<u64>,
) -> LeadDiscoveryStatusResponse {
    let enabled_sources = overlay
        .sources
        .iter()
        .filter(|source| source.approved && source.enabled)
        .count();
    let pending_sources = overlay
        .sources
        .iter()
        .filter(|source| !source.approved || !source.enabled)
        .count();
    LeadDiscoveryStatusResponse {
        configured: enabled_sources > 0,
        enabled_sources,
        pending_sources,
        sources: overlay.sources.clone(),
        criteria: overlay.criteria.clone(),
        auto_poll_last_checked_at_ms,
    }
}

pub fn resolve_enabled_source<'a>(
    overlay: &'a LeadDiscoveryOverlay,
    source_id: &str,
) -> Result<&'a LeadDiscoverySourceConfig, StoreError> {
    let source = overlay
        .sources
        .iter()
        .find(|candidate| candidate.source_id == source_id)
        .ok_or_else(|| StoreError::Domain("lead_source_not_configured".to_string()))?;
    if !source.approved || !source.enabled {
        return Err(StoreError::Domain("lead_source_not_enabled".to_string()));
    }
    Ok(source)
}

pub fn finding_from_stage(
    request: &LeadFindingStageRequest,
    source: &LeadDiscoverySourceConfig,
    now_ms: u64,
) -> Result<LeadFinding, StoreError> {
    let title = request.title.trim();
    let summary = request.summary.trim();
    let evidence = request.evidence_quote.trim();
    if title.is_empty() {
        return Err(StoreError::Domain("lead_finding_title_empty".to_string()));
    }
    if summary.is_empty() {
        return Err(StoreError::Domain("lead_finding_summary_empty".to_string()));
    }
    if evidence.is_empty() {
        return Err(StoreError::Domain(
            "lead_finding_evidence_required".to_string(),
        ));
    }
    let evidence_record = EvidenceRecord {
        evidence_id: format!("lead_evidence_{}", request.idempotency_key.trim()),
        source: EvidenceSourceRef {
            source_id: source.source_id.clone(),
            kind: source.kind.into(),
            display_name: source.display_name.clone(),
            url: source.url.clone(),
        },
        policy: EvidenceUsagePolicy::approved_source_import(),
        item_url: trimmed_optional(request.item_url.as_deref()),
        captured_at_ms: request.captured_at_ms.or(Some(now_ms)),
        evidence_quote: evidence.chars().take(1_000).collect(),
        content_hash: None,
    };
    evidence_record
        .validate_for_ai_consumption()
        .map_err(|code| StoreError::Domain(code.to_string()))?;
    Ok(LeadFinding {
        finding_id: format!("lead_{}", request.idempotency_key.trim()),
        source_id: source.source_id.clone(),
        status: LeadFindingStatus::Staged,
        title: title.chars().take(120).collect(),
        summary: summary.chars().take(1_500).collect(),
        contact_hint: trimmed_optional(request.contact_hint.as_deref()),
        company_hint: trimmed_optional(request.company_hint.as_deref()),
        matched_terms: clean_terms(&request.matched_terms),
        evidence: evidence_record,
        work_item_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

pub fn matched_terms_for_text(criteria: &LeadDiscoveryCriteria, text: &str) -> Vec<String> {
    let mut matches = matched_terms_in_bucket(&criteria.lead_markets, text);
    matches.extend(matched_terms_in_bucket(&criteria.intent_terms, text));
    clean_terms(&matches)
}

pub fn autoscrape_match_terms(criteria: &LeadDiscoveryCriteria, text: &str) -> Vec<String> {
    let market_matches = matched_terms_in_bucket(&criteria.lead_markets, text);
    let intent_matches = matched_terms_in_bucket(&criteria.intent_terms, text);
    let has_required_market = criteria
        .lead_markets
        .iter()
        .all(|term| term.trim().is_empty())
        || !market_matches.is_empty();
    let has_required_intent = criteria
        .intent_terms
        .iter()
        .all(|term| term.trim().is_empty())
        || !intent_matches.is_empty();
    if !has_required_market || !has_required_intent {
        return Vec::new();
    }
    let mut matches = market_matches;
    matches.extend(intent_matches);
    clean_terms(&matches)
}

fn matched_terms_in_bucket(terms: &[String], text: &str) -> Vec<String> {
    let haystack = text.to_ascii_lowercase();
    let mut matches = Vec::new();
    for term in terms {
        let trimmed = term.trim();
        if trimmed.is_empty() {
            continue;
        }
        if haystack.contains(&trimmed.to_ascii_lowercase())
            && !matches.iter().any(|existing| existing == trimmed)
        {
            matches.push(trimmed.to_string());
        }
    }
    matches
}

pub struct AutoscrapeFindingInput<'a> {
    pub post_guid: &'a str,
    pub title: &'a str,
    pub summary: &'a str,
    pub item_url: Option<&'a str>,
    pub evidence_quote: &'a str,
    pub captured_at_ms: Option<u64>,
}

pub fn finding_from_autoscrape(
    source: &LeadDiscoverySourceConfig,
    criteria: &LeadDiscoveryCriteria,
    input: AutoscrapeFindingInput<'_>,
    now_ms: u64,
) -> Result<(LeadFinding, String), StoreError> {
    let title = input.title.trim();
    let summary = input.summary.trim();
    let evidence = input.evidence_quote.trim();
    if title.is_empty() {
        return Err(StoreError::Domain("lead_finding_title_empty".to_string()));
    }
    if summary.is_empty() {
        return Err(StoreError::Domain("lead_finding_summary_empty".to_string()));
    }
    if evidence.is_empty() {
        return Err(StoreError::Domain(
            "lead_finding_evidence_required".to_string(),
        ));
    }
    let idempotency_key = format!(
        "autoscrape_{}_{}",
        stable_key_fragment(&source.source_id),
        stable_key_fragment(input.post_guid)
    );
    let evidence_record = EvidenceRecord {
        evidence_id: format!("lead_evidence_{idempotency_key}"),
        source: EvidenceSourceRef {
            source_id: source.source_id.clone(),
            kind: source.kind.into(),
            display_name: source.display_name.clone(),
            url: source.feed_url.clone().or_else(|| source.url.clone()),
        },
        policy: EvidenceUsagePolicy::approved_source_import(),
        item_url: trimmed_optional(input.item_url),
        captured_at_ms: input.captured_at_ms.or(Some(now_ms)),
        evidence_quote: evidence.chars().take(1_000).collect(),
        content_hash: None,
    };
    evidence_record
        .validate_for_ai_consumption()
        .map_err(|code| StoreError::Domain(code.to_string()))?;
    let match_text = format!("{title}\n{summary}\n{evidence}");
    Ok((
        LeadFinding {
            finding_id: format!("lead_{idempotency_key}"),
            source_id: source.source_id.clone(),
            status: LeadFindingStatus::Staged,
            title: title.chars().take(120).collect(),
            summary: summary.chars().take(1_500).collect(),
            contact_hint: None,
            company_hint: None,
            matched_terms: matched_terms_for_text(criteria, &match_text),
            evidence: evidence_record,
            work_item_id: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        },
        idempotency_key,
    ))
}

pub fn routing_packet_kinds(criteria: &LeadDiscoveryCriteria) -> Vec<String> {
    if criteria.routing_packet_kinds.is_empty() {
        return vec!["follow_up_task".to_string(), "crm_activity".to_string()];
    }
    criteria.routing_packet_kinds.clone()
}

pub fn source_view(finding: &LeadFinding) -> InboundMessageRecord {
    let mut body = finding.summary.clone();
    body.push_str("\n\nEvidence:\n");
    body.push_str(&finding.evidence.evidence_quote);
    if let Some(url) = finding.evidence.item_url.as_deref() {
        body.push_str("\n\nURL: ");
        body.push_str(url);
    }
    InboundMessageRecord {
        source_key: finding.finding_id.clone(),
        message_id: finding.finding_id.clone(),
        thread_id: None,
        internal_date_ms: finding.evidence.captured_at_ms.map(|ms| ms as i64),
        from_addr: Some(finding.evidence.source.display_name.clone()),
        to_addr: None,
        subject: Some(finding.title.clone()),
        body_excerpt: body.clone(),
        body_full: body,
        headers: Vec::new(),
        labels: vec!["lead_discovery".to_string()],
        resolved_category: CATEGORY_ID.to_string(),
        matched_rule_id: None,
        ingested_at_ms: finding.created_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: None,
    }
}

fn trimmed_optional(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(240).collect())
}

pub(crate) fn clean_terms(raw: &[String]) -> Vec<String> {
    let mut terms = Vec::new();
    for term in raw {
        let term = term.trim();
        if !term.is_empty() && !terms.iter().any(|existing| existing == term) {
            terms.push(term.chars().take(80).collect());
        }
    }
    terms
}

fn stable_key_fragment(raw: &str) -> String {
    let trimmed = raw.trim();
    let digest = Sha256::digest(trimmed.as_bytes());
    let hash = &format!("{:x}", digest)[..16];
    let mut out = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            out.push(ch);
        } else if ch.is_whitespace() || matches!(ch, '/' | ':' | '?' | '&' | '=' | '#') {
            out.push('_');
        }
        if out.len() >= AUTOSCRAPE_KEY_SLUG_MAX_CHARS {
            break;
        }
    }
    let out = out.trim_matches('_').to_string();
    if !out.is_empty() {
        return format!("{out}_{hash}");
    }
    hash.to_string()
}
