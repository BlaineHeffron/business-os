//! Shared web-tier helpers for enrichment subjects.
//!
//! This module is intentionally outside `service.rs`: it may depend on
//! `bos_integrations`, while the engine core remains provider-agnostic.

use bos_contracts::enrichment::{
    EnrichmentConfidence, EnrichmentFieldProposal, EnrichmentRunStatus, EnrichmentTier,
    EnrichmentTierEvent,
};
use bos_integrations::web_page_read::{
    FetchedPage, ReqwestWebHttpClient, SystemHostResolver, WebCrawlConfig, WebFetchError,
    WebPageReader,
};
use bos_integrations::web_search_enrichment::{
    ReqwestWebSearchApi, SearchEvidence, WebSearchCollector, WebSearchConfig,
};
use std::ops::ControlFlow;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct AcceptedValue<'a> {
    pub field_id: &'a str,
    pub value: &'a str,
    pub quote: &'a str,
    pub provenance_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DomainSeedInvalid;

pub(crate) fn normalize_domain_seed(
    domain_seed: Option<&str>,
) -> Result<Option<String>, DomainSeedInvalid> {
    let Some(raw) = domain_seed.map(str::trim).filter(|seed| !seed.is_empty()) else {
        return Ok(None);
    };
    bos_integrations::web_page_read::find_domain(raw)
        .ok_or(DomainSeedInvalid)
        .map(Some)
}

pub(crate) fn live_guarded_crawl(domain: &str) -> Result<Vec<FetchedPage>, WebFetchError> {
    let reader = WebPageReader::new(
        Arc::new(ReqwestWebHttpClient::default()),
        Arc::new(SystemHostResolver),
        WebCrawlConfig::default(),
    );
    reader.crawl(domain)
}

pub(crate) fn crawl_outcome(
    state: &crate::http::AppState,
    run: super::service::EnrichmentRunHandle<'_>,
    domain: &str,
    result: Result<Vec<FetchedPage>, WebFetchError>,
) -> ControlFlow<super::service::EnrichmentOutcome, Vec<FetchedPage>> {
    match result {
        Ok(pages) if !pages.is_empty() => {
            let crawl_events = page_fetch_events(&pages);
            run.append(state, "tier3-crawl", &crawl_events, &[], 0);
            ControlFlow::Continue(pages)
        }
        Ok(_) => {
            let events = vec![super::service::skip_event(
                EnrichmentTier::WebSearch,
                "page_fetch",
                "crawl_returned_no_pages",
            )];
            run.append(state, "tier3-empty-crawl", &events, &[], 0);
            ControlFlow::Break(run.transition(
                state,
                EnrichmentRunStatus::Partial,
                "crawl_returned_no_pages",
            ))
        }
        Err(err) => {
            tracing::info!(domain = %domain, error = %err, "web enrichment crawl yielded nothing");
            let events = vec![super::service::skip_event(
                EnrichmentTier::WebSearch,
                "failure",
                &format!("crawl_failed:{err}"),
            )];
            run.append(state, "tier3-crawl-failed", &events, &[], 0);
            ControlFlow::Break(run.transition(state, EnrichmentRunStatus::Failed, "crawl_failed"))
        }
    }
}

pub(crate) fn collect_search_evidence(
    slice_id: &str,
    reason: &str,
    queries: &[String],
    config: WebSearchConfig,
) -> SearchEvidence {
    let collector = WebSearchCollector::new(
        Arc::new(ReqwestWebSearchApi),
        Arc::new(ReqwestWebHttpClient::default()),
        Arc::new(SystemHostResolver),
        config,
    );
    collector.collect(slice_id, reason, queries)
}

pub(crate) fn page_fetch_events(pages: &[FetchedPage]) -> Vec<EnrichmentTierEvent> {
    pages
        .iter()
        .map(|page| EnrichmentTierEvent {
            event_type: "page_fetch".to_string(),
            tier: EnrichmentTier::WebSearch,
            field_id: None,
            status: Some("fetched".to_string()),
            reason: None,
            source_id: Some(format!("page:{}", page.url)),
            url: Some(page.url.clone()),
            final_url: Some(page.url.clone()),
            query: None,
            rank: None,
            title: None,
            snippet: None,
            proposed_value: None,
            confidence: None,
            quote: None,
            latency_ms: None,
            bytes: Some(page.html.len() as u64),
            cost_micros: None,
            ..Default::default()
        })
        .collect()
}

pub(crate) fn search_evidence_events(evidence: &SearchEvidence) -> Vec<EnrichmentTierEvent> {
    let mut events = Vec::new();
    for query in &evidence.queries {
        events.push(EnrichmentTierEvent {
            event_type: "search_query".to_string(),
            tier: EnrichmentTier::WebSearch,
            field_id: None,
            status: Some("completed".to_string()),
            reason: Some(evidence.reason.clone()),
            source_id: None,
            url: None,
            final_url: None,
            query: Some(query.clone()),
            rank: None,
            title: None,
            snippet: None,
            proposed_value: None,
            confidence: None,
            quote: None,
            latency_ms: None,
            bytes: None,
            cost_micros: None,
            ..Default::default()
        });
    }
    for (idx, result) in evidence.results.iter().enumerate() {
        events.push(EnrichmentTierEvent {
            event_type: "search_result".to_string(),
            tier: EnrichmentTier::WebSearch,
            field_id: None,
            status: Some("considered".to_string()),
            reason: Some(evidence.reason.clone()),
            source_id: Some(format!("search:{}", result.url)),
            url: Some(result.url.clone()),
            final_url: None,
            query: Some(result.query.clone()),
            rank: Some((idx + 1) as u32),
            title: Some(result.title.clone()),
            snippet: Some(result.snippet.clone()),
            proposed_value: None,
            confidence: None,
            quote: None,
            latency_ms: None,
            bytes: None,
            cost_micros: None,
            ..Default::default()
        });
    }
    for failure in &evidence.failures {
        events.push(super::service::skip_event(
            EnrichmentTier::WebSearch,
            "failure",
            failure.as_str(),
        ));
    }
    events
}

pub(crate) fn accepted_value_diagnostics<'a, I>(
    fields: I,
    tier: EnrichmentTier,
    reason: &str,
) -> (Vec<EnrichmentTierEvent>, Vec<EnrichmentFieldProposal>)
where
    I: IntoIterator<Item = AcceptedValue<'a>>,
{
    let mut events = Vec::new();
    let mut proposals = Vec::new();
    for field in fields {
        events.push(super::service::field_event(
            tier,
            field.field_id,
            field.value,
            "accepted",
            reason,
            Some(field.quote),
        ));
        proposals.push(EnrichmentFieldProposal {
            field_id: field.field_id.to_string(),
            proposed_value: field.value.to_string(),
            source_tier: tier,
            confidence: EnrichmentConfidence::Medium,
            provenance_refs: field.provenance_refs,
            accepted: true,
            reason: reason.to_string(),
        });
    }
    (events, proposals)
}
