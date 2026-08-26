//! Approved-feed lead discovery poller. This is deliberately feed-only:
//! configured RSS/Atom/Reddit JSON sources are fetched through the shared
//! SSRF-guarded page reader, then staged for human review.

use std::sync::Arc;
use std::time::Duration;

use bos_contracts::lead_discovery::{LeadDiscoverySourceConfig, LeadDiscoverySourceKind};
use bos_contracts::receipt::ActorKindDto;
use bos_integrations::web_page_read::{
    HostResolver, ReqwestWebHttpClient, SystemHostResolver, WebCrawlConfig, WebHttp, WebPageReader,
};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::Value;

use super::{service, store};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::{MutationOutcome, StoreError};

pub const LEAD_DISCOVERY_AUTOSCRAPE_COOLDOWN_MS: u64 = 120_000;
const DEFAULT_INTERVAL_SECS: usize = 1800;
const DEFAULT_MAX_FINDINGS_PER_CYCLE: usize = 10;
const FETCH_BUDGET_PER_SOURCE: usize = 4;
const DISABLED_SETTINGS_REFRESH_SECS: u64 = 60;

pub struct LeadDiscoveryAutoscrapeConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_findings_per_cycle: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub sources_checked: usize,
    pub staged: usize,
    pub matched: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPost {
    pub guid: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<LeadDiscoveryAutoscrapeConfig, StoreError> {
    Ok(LeadDiscoveryAutoscrapeConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS,
                DEFAULT_INTERVAL_SECS,
            )?
            .max(300) as u64,
        ),
        max_findings_per_cycle: max_findings_from_settings(conn, client_id)?,
    })
}

pub fn max_findings_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<usize, StoreError> {
    Ok(crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE,
        DEFAULT_MAX_FINDINGS_PER_CYCLE,
    )?
    .clamp(1, 50))
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("lead discovery autoscrape not started because the slice is disabled");
        return;
    }
    std::thread::Builder::new()
        .name("lead-discovery-autoscrape-pump".to_string())
        .spawn(move || {
            tracing::info!("lead discovery autoscrape pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "lead discovery autoscrape config read failed");
                            LeadDiscoveryAutoscrapeConfig {
                                enabled: false,
                                interval: Duration::from_secs(DEFAULT_INTERVAL_SECS as u64),
                                max_findings_per_cycle: DEFAULT_MAX_FINDINGS_PER_CYCLE,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(&state, config.max_findings_per_cycle);
                    match summary {
                        Ok(summary) if summary.requests_used > 0 || summary.staged > 0 => {
                            tracing::info!(
                                requests_used = summary.requests_used,
                                sources_checked = summary.sources_checked,
                                matched = summary.matched,
                                staged = summary.staged,
                                skipped = summary.skipped,
                                "lead discovery autoscrape cycle complete"
                            );
                        }
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "lead discovery autoscrape failed"),
                    }
                }
                let sleep_for = if config.enabled {
                    config.interval
                } else {
                    Duration::from_secs(DISABLED_SETTINGS_REFRESH_SECS)
                };
                std::thread::sleep(sleep_for);
            }
        })
        .expect("spawn lead-discovery-autoscrape-pump thread");
}

pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::LeadDiscoveryAutoscrape)
        .lock();
    if status.in_flight {
        return Err("sync_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("sync_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

pub fn run_guarded_cycle(
    state: &AppState,
    max_findings_per_cycle: usize,
) -> Result<CycleSummary, String> {
    let http = Arc::new(ReqwestWebHttpClient::default());
    let resolver = Arc::new(SystemHostResolver);
    let reader = WebPageReader::new(http, resolver, WebCrawlConfig::default());
    let result = run_sync_cycle(state, &reader, max_findings_per_cycle, now_ms());
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::LeadDiscoveryAutoscrape)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + LEAD_DISCOVERY_AUTOSCRAPE_COOLDOWN_MS;
    match &result {
        Ok(summary) => {
            status.units_used = summary.requests_used;
            status.last_outcome = Some("ok".to_string());
        }
        Err(err) => status.last_outcome = Some(format!("error: {err}")),
    }
    result
}

pub fn run_sync_cycle<H: WebHttp, R: HostResolver>(
    state: &AppState,
    reader: &WebPageReader<H, R>,
    max_findings_per_cycle: usize,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let cycle_started_at = now;
    let sources: Vec<LeadDiscoverySourceConfig> = state
        .lead_discovery_overlay
        .sources
        .iter()
        .filter(|source| source.auto_poll && source.approved && source.enabled)
        .cloned()
        .collect();

    for source in sources {
        let already_created = {
            let persistence = state.persistence.lock();
            store::count_findings_created_since(
                persistence.connection_ref(),
                &state.client_id,
                cycle_started_at,
            )
            .map_err(|err| err.to_string())?
        };
        if already_created >= max_findings_per_cycle {
            break;
        }

        let Some(feed_url) = source
            .feed_url
            .as_deref()
            .or(source.url.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            summary.skipped += 1;
            continue;
        };
        summary.sources_checked += 1;

        let mut fetch_budget = FETCH_BUDGET_PER_SOURCE;
        let before = fetch_budget;
        let page = match reader.fetch_public_page(feed_url, &mut fetch_budget) {
            Ok(page) => page,
            Err(err) => {
                summary.requests_used += (before - fetch_budget) as u32;
                summary.skipped += 1;
                tracing::debug!(
                    source_id = %source.source_id,
                    url = %feed_url,
                    error = %err,
                    "lead discovery feed fetch skipped"
                );
                continue;
            }
        };
        summary.requests_used += (before - fetch_budget) as u32;
        let posts = match parse_feed(&source.kind, &page.html) {
            Ok(posts) => posts,
            Err(err) => {
                summary.skipped += 1;
                tracing::debug!(
                    source_id = %source.source_id,
                    url = %page.url,
                    error = %err,
                    "lead discovery feed parse skipped"
                );
                continue;
            }
        };

        for post in posts {
            let already_created = {
                let persistence = state.persistence.lock();
                store::count_findings_created_since(
                    persistence.connection_ref(),
                    &state.client_id,
                    cycle_started_at,
                )
                .map_err(|err| err.to_string())?
            };
            if already_created >= max_findings_per_cycle {
                break;
            }

            let match_text = format!("{}\n{}", post.title, post.body);
            let matched_terms = service::autoscrape_match_terms(
                &state.lead_discovery_overlay.criteria,
                &match_text,
            );
            if matched_terms.is_empty() {
                continue;
            }
            summary.matched += 1;
            let evidence_quote = if post.body.trim().is_empty() {
                post.title.as_str()
            } else {
                post.body.as_str()
            };
            let (mut finding, idempotency_key) = service::finding_from_autoscrape(
                &source,
                &state.lead_discovery_overlay.criteria,
                service::AutoscrapeFindingInput {
                    post_guid: &post.guid,
                    title: &post.title,
                    summary: if post.body.trim().is_empty() {
                        &post.title
                    } else {
                        &post.body
                    },
                    item_url: post.url.as_deref(),
                    evidence_quote,
                    captured_at_ms: None,
                },
                now,
            )
            .map_err(|err| err.to_string())?;
            finding.matched_terms = matched_terms;
            let outcome = {
                let mut persistence = state.persistence.lock();
                store::insert_finding(
                    persistence.connection(),
                    &state.client_id,
                    "system",
                    ActorKindDto::System,
                    &finding,
                    &idempotency_key,
                )
            };
            match outcome {
                Ok(MutationOutcome::Applied { .. }) => summary.staged += 1,
                Ok(MutationOutcome::ReplayedIdempotent { .. }) => {}
                Ok(MutationOutcome::RevisionConflict { .. }) => summary.skipped += 1,
                Err(err) => {
                    summary.skipped += 1;
                    tracing::debug!(
                        source_id = %source.source_id,
                        guid = %post.guid,
                        error = %err,
                        "lead discovery finding stage skipped"
                    );
                }
            }
        }
    }

    Ok(summary)
}

pub fn parse_feed(kind: &LeadDiscoverySourceKind, body: &str) -> Result<Vec<FeedPost>, String> {
    if matches!(kind, LeadDiscoverySourceKind::Reddit) || looks_like_json(body) {
        return parse_reddit_json(body).or_else(|json_err| {
            if looks_like_json(body) {
                Err(json_err)
            } else {
                parse_xml_feed(body)
            }
        });
    }
    parse_xml_feed(body)
}

fn parse_reddit_json(body: &str) -> Result<Vec<FeedPost>, String> {
    let value: Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    let children = value
        .pointer("/data/children")
        .and_then(Value::as_array)
        .ok_or_else(|| "reddit feed missing data.children".to_string())?;
    let mut posts = Vec::new();
    for child in children {
        let Some(data) = child.get("data") else {
            continue;
        };
        let title = data
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if title.is_empty() {
            continue;
        }
        let body = data
            .get("selftext")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let permalink = data
            .get("permalink")
            .and_then(Value::as_str)
            .and_then(normalize_reddit_permalink);
        let guid = data
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| data.get("id").and_then(Value::as_str))
            .or(permalink.as_deref())
            .unwrap_or(title)
            .to_string();
        posts.push(FeedPost {
            guid,
            title: title.to_string(),
            body,
            url: permalink,
        });
    }
    Ok(posts)
}

fn parse_xml_feed(body: &str) -> Result<Vec<FeedPost>, String> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut posts = Vec::new();
    let mut current: Option<XmlPost> = None;
    let mut field: Option<XmlField> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "item" | "entry" => current = Some(XmlPost::default()),
                    "title" if current.is_some() => field = Some(XmlField::Title),
                    "description" | "summary" | "content" if current.is_some() => {
                        field = Some(XmlField::Body)
                    }
                    "guid" | "id" if current.is_some() => field = Some(XmlField::Guid),
                    "link" if current.is_some() => {
                        if let Some(href) = href_attr(&event) {
                            if let Some(post) = current.as_mut() {
                                if post.link.is_none() {
                                    post.link = Some(href);
                                }
                            }
                        }
                        field = Some(XmlField::Link);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "link" {
                    if let (Some(post), Some(href)) = (current.as_mut(), href_attr(&event)) {
                        if post.link.is_none() {
                            post.link = Some(href);
                        }
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let (Some(post), Some(field)) = (current.as_mut(), field) {
                    let value = text
                        .xml10_content()
                        .map(|cow| cow.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(text.as_ref()).into_owned());
                    post.push(field, &value);
                }
            }
            Ok(Event::CData(text)) => {
                if let (Some(post), Some(field)) = (current.as_mut(), field) {
                    let value = String::from_utf8_lossy(text.as_ref());
                    post.push(field, &value);
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let (Some(post), Some(field)) = (current.as_mut(), field) {
                    let name = reference
                        .decode()
                        .map(|cow| cow.into_owned())
                        .unwrap_or_else(|_| {
                            String::from_utf8_lossy(reference.as_ref()).into_owned()
                        });
                    if let Some(value) = xml_general_ref_value(&name) {
                        post.push(field, &value);
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "item" | "entry" => {
                        if let Some(post) = current.take().and_then(XmlPost::into_feed_post) {
                            posts.push(post);
                        }
                        field = None;
                    }
                    "title" | "description" | "summary" | "content" | "guid" | "id" | "link" => {
                        field = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(err.to_string()),
            _ => {}
        }
    }
    Ok(posts)
}

#[derive(Default)]
struct XmlPost {
    title: String,
    body: String,
    link: Option<String>,
    guid: Option<String>,
}

#[derive(Clone, Copy)]
enum XmlField {
    Title,
    Body,
    Link,
    Guid,
}

impl XmlPost {
    fn push(&mut self, field: XmlField, value: &str) {
        let target = match field {
            XmlField::Title => &mut self.title,
            XmlField::Body => &mut self.body,
            XmlField::Link => {
                if self.link.is_none() {
                    self.link = Some(value.trim().to_string());
                }
                return;
            }
            XmlField::Guid => {
                if self.guid.is_none() {
                    self.guid = Some(value.trim().to_string());
                }
                return;
            }
        };
        if !target.is_empty() {
            target.push(' ');
        }
        target.push_str(value.trim());
    }

    fn into_feed_post(self) -> Option<FeedPost> {
        let title = self.title.trim();
        if title.is_empty() {
            return None;
        }
        let body = strip_markup(self.body.trim());
        let guid = self
            .guid
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or(self.link.as_deref())
            .unwrap_or(title)
            .to_string();
        Some(FeedPost {
            guid,
            title: title.to_string(),
            body,
            url: self.link.filter(|value| !value.trim().is_empty()),
        })
    }
}

fn looks_like_json(body: &str) -> bool {
    body.trim_start().starts_with('{') || body.trim_start().starts_with('[')
}

fn normalize_reddit_permalink(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else if trimmed.starts_with('/') {
        Some(format!("https://www.reddit.com{trimmed}"))
    } else {
        Some(format!("https://www.reddit.com/{trimmed}"))
    }
}

fn local_name(name: &[u8]) -> String {
    let raw = String::from_utf8_lossy(name);
    raw.rsplit(':').next().unwrap_or(&raw).to_ascii_lowercase()
}

fn href_attr(event: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|attr| local_name(attr.key.as_ref()) == "href")
        .map(|attr| {
            String::from_utf8_lossy(attr.value.as_ref())
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

fn xml_general_ref_value(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        value if value.starts_with("#x") => u32::from_str_radix(&value[2..], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|ch| ch.to_string()),
        value if value.starts_with('#') => value[1..]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|ch| ch.to_string()),
        _ => None,
    }
}

fn strip_markup(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    collapse_ws(&out)
}

fn collapse_ws(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}
