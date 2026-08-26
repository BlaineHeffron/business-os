//! Read-only website enrichment crawler + deterministic extractor (Increment
//! E of the CRM-records design). The operator's note literally named a domain;
//! we fetch a handful of that site's own pages and pull the structured company
//! facts a CRM record-create draft can be prefilled with.
//!
//! HOUSE PATTERN: the network is an injected trait ([`WebHttp`]) and so is DNS
//! resolution ([`HostResolver`]) — tests script both, so the whole crawl is
//! exercised with ZERO network and ZERO real DNS. No `std::env::var` (this
//! crate forbids it); the caller supplies config.
//!
//! GUARDS (every fetch, including each redirect hop):
//! - scheme forced to https (http is upgraded; anything else refused),
//! - the host is resolved and EVERY resolved IP must be publicly routable —
//!   loopback/private/link-local/CGNAT/reserved are refused (SSRF floor),
//! - response body capped at [`MAX_PAGE_BYTES`] (512 KiB),
//! - at most [`MAX_REQUESTS`] requests total across the whole crawl,
//! - only same-registrable-domain URLs are followed (the site's own pages),
//! - no cookies, no JS, bounded connect+read timeouts (live client).
//!
//! Extraction is fully deterministic (zero LLM): schema.org JSON-LD
//! Organization/LocalBusiness, OpenGraph meta, mailto:/tel: links, and phone
//! numbers validated by libphonenumber (the `phonenumber` crate) rather than a
//! hand-rolled regex. Postal addresses are intentionally NOT regex-parsed — when
//! JSON-LD doesn't carry one, the grounded LLM gap-filler extracts it from page
//! text (there is no lightweight, reliable US address parser worth embedding).
//! Every extracted value carries `page:<url>` provenance. The gap-filler that
//! runs AFTER this pass lives in the consuming slice (bos-app), not here.

use std::collections::BTreeSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Arc;

use percent_encoding::percent_decode_str;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use url::Url;

/// Hard cap on total HTTP requests across one crawl (homepage + sitemap +
/// candidate pages all count).
pub const MAX_REQUESTS: usize = 4;
/// Most candidate (non-homepage) pages fetched.
pub const MAX_CANDIDATE_PAGES: usize = 3;
/// Per-page body cap; larger responses are truncated at this boundary.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Most redirect hops followed for a single logical fetch.
pub const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebFetchError {
    /// A guard refused the URL (bad scheme, non-public IP, off-domain redirect,
    /// resolution failure). The reason is for logs, never the operator.
    Blocked { reason: String },
    /// Transport failure (connect/read/timeout). Best-effort enrichment treats
    /// this as "no data", never a hard error to the caller.
    Transport { message: String },
}

impl std::fmt::Display for WebFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked { reason } => write!(f, "blocked: {reason}"),
            Self::Transport { message } => write!(f, "transport: {message}"),
        }
    }
}

/// One HTTP response with redirects DISABLED at the transport — the crawler
/// follows them manually so each hop is re-guarded.
pub struct WebHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    /// `Location` header value on a 3xx (raw, possibly relative).
    pub location: Option<String>,
    /// Decoded body, UTF-8 lossy, already capped at [`MAX_PAGE_BYTES`].
    pub body: String,
}

pub const NORMALIZER_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPageText {
    /// Legacy-compatible visible text projection: whitespace-collapsed,
    /// newline-free, no NFKC/casefold/punctuation folding.
    pub flat_text: String,
    /// Layout-preserving display text for future evidence coordinate space.
    /// Byte offsets must index this display text, not canonicalized text.
    pub layout_text: Arc<str>,
    pub normalizer_version: u16,
}

/// Network seam. The live impl disables auto-redirects; tests script responses.
pub trait WebHttp: Send + Sync {
    fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError>;
}

/// DNS seam — split out so the public-IP guard is testable without real DNS.
pub trait HostResolver: Send + Sync {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, WebFetchError>;
}

/// Live blocking HTTP client: redirects OFF, bounded timeouts, body capped on
/// read so a hostile server can't stream us out of memory.
pub struct ReqwestWebHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestWebHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("BusinessOS-Enrichment/1.0 (+read-only)")
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl WebHttp for ReqwestWebHttpClient {
    fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError> {
        use std::io::Read;
        let response = self
            .client
            .get(url)
            .header("Accept", "text/html,application/xhtml+xml,application/xml")
            .send()
            .map_err(|err| WebFetchError::Transport {
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        // Read at most MAX_PAGE_BYTES + 1 so truncation is detectable, but only
        // ever keep the cap.
        let mut buf = Vec::new();
        let mut limited = response.take((MAX_PAGE_BYTES as u64) + 1);
        limited
            .read_to_end(&mut buf)
            .map_err(|err| WebFetchError::Transport {
                message: err.to_string(),
            })?;
        buf.truncate(MAX_PAGE_BYTES);
        Ok(WebHttpResponse {
            status,
            content_type,
            location,
            body: String::from_utf8_lossy(&buf).into_owned(),
        })
    }
}

/// Live DNS resolver via the stdlib. Port 443 is arbitrary — we only need the
/// A/AAAA records, not a connection.
#[derive(Default)]
pub struct SystemHostResolver;

impl HostResolver for SystemHostResolver {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, WebFetchError> {
        (host, 443u16)
            .to_socket_addrs()
            .map_err(|err| WebFetchError::Blocked {
                reason: format!("dns resolve failed for {host}: {err}"),
            })
            .map(|addrs| addrs.map(|addr| addr.ip()).collect())
    }
}

/// Is this address publicly routable? Loopback, private, link-local, CGNAT,
/// and assorted reserved ranges are NOT — refuse them so a crafted DNS record
/// (or a redirect) can't point us at an internal service.
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
            {
                return false;
            }
            let [a, b, c, _] = v4.octets();
            // 100.64.0.0/10 CGNAT.
            if a == 100 && (64..=127).contains(&b) {
                return false;
            }
            // IANA special-use ranges not covered by the std helpers above.
            if (a == 192 && ((b == 0 && c == 0) || b == 88)) || (a == 198 && (b == 18 || b == 19)) {
                return false;
            }
            // 224.0.0.0/4 multicast; 240.0.0.0/4 reserved.
            if a >= 224 {
                return false;
            }
            true
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return false;
            }
            // Map v4-in-v6 back to the v4 rules.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(&IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            // fc00::/7 unique-local, fe80::/10 link-local.
            if (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80 {
                return false;
            }
            // 2001:db8::/32 documentation.
            if seg0 == 0x2001 && v6.segments()[1] == 0x0db8 {
                return false;
            }
            true
        }
    }
}

/// Crawl tuning. Defaults match the design's hard caps.
#[derive(Debug, Clone)]
pub struct WebCrawlConfig {
    pub max_requests: usize,
    pub max_candidate_pages: usize,
    /// Cap on the stripped-text length kept per page for the LLM gap-filler.
    pub max_text_chars: usize,
}

impl Default for WebCrawlConfig {
    fn default() -> Self {
        Self {
            max_requests: MAX_REQUESTS,
            max_candidate_pages: MAX_CANDIDATE_PAGES,
            max_text_chars: 8_000,
        }
    }
}

/// One successfully fetched HTML page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPage {
    pub url: String,
    pub html: String,
}

pub struct WebPageReader<H: WebHttp, R: HostResolver> {
    http: Arc<H>,
    resolver: Arc<R>,
    config: WebCrawlConfig,
}

impl<H: WebHttp, R: HostResolver> WebPageReader<H, R> {
    pub fn new(http: Arc<H>, resolver: Arc<R>, config: WebCrawlConfig) -> Self {
        Self {
            http,
            resolver,
            config,
        }
    }

    /// Crawl the operator-named domain: homepage + a few scored candidate pages
    /// (contact/about/team), consulting sitemap.xml when present. Returns the
    /// fetched pages (homepage first). Best-effort: a guard refusal or empty
    /// homepage yields whatever was gathered (possibly empty), never panics.
    pub fn crawl(&self, seed_domain: &str) -> Result<Vec<FetchedPage>, WebFetchError> {
        let seed = normalize_seed_url(seed_domain)?;
        let seed_host = seed
            .host_str()
            .ok_or_else(|| WebFetchError::Blocked {
                reason: "seed url has no host".to_string(),
            })?
            .to_string();
        let mut budget = self.config.max_requests;

        let homepage = self.fetch_guarded(seed.as_str(), &seed_host, &mut budget)?;
        let mut pages = vec![homepage.clone()];

        // Discover candidate URLs from the homepage links, augmented by the
        // sitemap when one is present and the budget allows.
        let mut candidates = score_candidate_links(&homepage.html, &seed, &seed_host);
        if budget > 0 {
            if let Ok(sitemap_url) = seed.join("/sitemap.xml") {
                if let Ok(sitemap) =
                    self.fetch_guarded(sitemap_url.as_str(), &seed_host, &mut budget)
                {
                    merge_sitemap_candidates(&sitemap.html, &seed, &seed_host, &mut candidates);
                }
            }
        }

        let mut fetched_urls: BTreeSet<String> = pages.iter().map(|p| p.url.clone()).collect();
        let mut fetched_candidates = 0usize;
        for candidate in candidates {
            if budget == 0 || fetched_candidates >= self.config.max_candidate_pages {
                break;
            }
            if fetched_urls.contains(&candidate) {
                continue;
            }
            match self.fetch_guarded(&candidate, &seed_host, &mut budget) {
                Ok(page) => {
                    fetched_urls.insert(page.url.clone());
                    pages.push(page);
                    fetched_candidates += 1;
                }
                Err(err) => {
                    tracing::debug!(url = %candidate, error = %err, "enrichment candidate fetch skipped");
                }
            }
        }
        Ok(pages)
    }

    /// Fetch a single already-selected URL with the same public-network guards
    /// used by the site crawler. Redirects may move within the result URL's
    /// registrable domain only. The supplied budget is decremented once per
    /// network request, including redirects.
    pub fn fetch_public_page(
        &self,
        url: &str,
        budget: &mut usize,
    ) -> Result<FetchedPage, WebFetchError> {
        let parsed = Url::parse(url).map_err(|err| WebFetchError::Blocked {
            reason: format!("unparseable url {url}: {err}"),
        })?;
        let seed_host = parsed
            .host_str()
            .ok_or_else(|| WebFetchError::Blocked {
                reason: "url has no host".to_string(),
            })?
            .to_string();
        self.fetch_guarded(url, &seed_host, budget)
    }

    /// Fetch one URL, following redirects MANUALLY (each hop re-guarded), until
    /// a 2xx HTML body or a refusal. Decrements the shared request budget once
    /// per network call. Non-HTML 2xx bodies are returned as-is (the sitemap is
    /// XML); callers decide what to parse.
    fn fetch_guarded(
        &self,
        url: &str,
        seed_host: &str,
        budget: &mut usize,
    ) -> Result<FetchedPage, WebFetchError> {
        let mut current = Url::parse(url).map_err(|err| WebFetchError::Blocked {
            reason: format!("unparseable url {url}: {err}"),
        })?;
        for _hop in 0..=MAX_REDIRECTS {
            if *budget == 0 {
                return Err(WebFetchError::Blocked {
                    reason: "request budget exhausted".to_string(),
                });
            }
            let guarded = self.guard_url(&current, seed_host)?;
            *budget -= 1;
            let response = self.http.get(guarded.as_str())?;
            match response.status {
                200..=299 => {
                    return Ok(FetchedPage {
                        url: guarded.to_string(),
                        html: response.body,
                    });
                }
                300..=399 => {
                    let location = response.location.ok_or_else(|| WebFetchError::Transport {
                        message: format!("redirect with no Location from {guarded}"),
                    })?;
                    current = guarded
                        .join(&location)
                        .map_err(|err| WebFetchError::Blocked {
                            reason: format!("bad redirect target {location}: {err}"),
                        })?;
                }
                other => {
                    return Err(WebFetchError::Transport {
                        message: format!("status {other} from {guarded}"),
                    });
                }
            }
        }
        Err(WebFetchError::Blocked {
            reason: "too many redirects".to_string(),
        })
    }

    /// Apply every guard to a single URL and return the https-normalized form
    /// to actually fetch.
    fn guard_url(&self, url: &Url, seed_host: &str) -> Result<Url, WebFetchError> {
        let mut url = url.clone();
        // Force https (upgrade http; refuse anything else — no file:, ftp:, …).
        match url.scheme() {
            "https" => {}
            "http" => {
                url.set_scheme("https")
                    .map_err(|_| WebFetchError::Blocked {
                        reason: "could not upgrade scheme to https".to_string(),
                    })?;
            }
            other => {
                return Err(WebFetchError::Blocked {
                    reason: format!("unsupported scheme {other}"),
                });
            }
        }
        let host = url.host_str().ok_or_else(|| WebFetchError::Blocked {
            reason: "url has no host".to_string(),
        })?;
        if let Some(port) = url.port() {
            if port != 443 {
                return Err(WebFetchError::Blocked {
                    reason: format!("unsupported port {port}"),
                });
            }
        }
        if !same_registrable_domain(seed_host, host) {
            return Err(WebFetchError::Blocked {
                reason: format!("off-domain host {host} (seed {seed_host})"),
            });
        }
        let ips = self.resolver.resolve(host)?;
        if ips.is_empty() {
            return Err(WebFetchError::Blocked {
                reason: format!("no addresses for {host}"),
            });
        }
        if let Some(bad) = ips.iter().find(|ip| !is_public_ip(ip)) {
            return Err(WebFetchError::Blocked {
                reason: format!("non-public address {bad} for {host}"),
            });
        }
        Ok(url)
    }
}

/// Parse the operator's domain string into a fetchable https URL. Accepts bare
/// domains ("example.test"), scheme-prefixed, or with a path; always lands
/// on https.
pub fn normalize_seed_url(raw: &str) -> Result<Url, WebFetchError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WebFetchError::Blocked {
            reason: "empty domain".to_string(),
        });
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&candidate).map_err(|err| WebFetchError::Blocked {
        reason: format!("unparseable domain {trimmed}: {err}"),
    })?;
    if url.scheme() == "http" {
        let _ = url.set_scheme("https");
    }
    if url.host_str().is_none() {
        return Err(WebFetchError::Blocked {
            reason: format!("domain {trimmed} has no host"),
        });
    }
    Ok(url)
}

/// Find the first website domain LITERALLY present in free text (an operator's
/// note), returning it lower-cased without scheme/path/`www.`. Enrichment fires
/// ONLY on this — a domain is never guessed. A token immediately preceded by
/// `@` is skipped so an email address's domain (contact data, not "the site the
/// operator mentioned") doesn't trigger a crawl. Bare common public mailbox
/// hosts are ignored too.
pub fn find_domain(text: &str) -> Option<String> {
    let re = domain_regex();
    for caps in re.captures_iter(text) {
        let whole = caps.get(0)?;
        // Skip an email's domain part (preceded by '@').
        let start = whole.start();
        if start > 0 && text.as_bytes()[start - 1] == b'@' {
            continue;
        }
        let host = caps
            .name("host")
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim_end_matches('.')
            .trim_start_matches("www.")
            .to_lowercase();
        if host.is_empty() || !host.contains('.') {
            continue;
        }
        if COMMON_MAILBOX_HOSTS.contains(&host.as_str()) {
            continue;
        }
        // Require a plausible alphabetic TLD (≥2 letters) so "12.5" / version
        // strings don't read as domains.
        let tld = host.rsplit('.').next().unwrap_or("");
        if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        return Some(host);
    }
    None
}

const COMMON_MAILBOX_HOSTS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "outlook.com",
    "hotmail.com",
    "icloud.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
];

/// Second-level public-suffix labels we treat as part of the suffix when
/// computing the registrable domain (no full PSL dependency — this covers the
/// common ccTLD-with-second-level forms; everything else uses last-two-labels).
const MULTIPART_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "gov.uk", "ac.uk", "co.nz", "co.za", "com.au", "net.au", "org.au", "co.jp",
    "com.br", "co.in", "com.mx",
];

/// The registrable domain (eTLD+1) of a host, lower-cased. Approximation: the
/// last two labels, or three when the last two are a known multi-part suffix.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim_end_matches('.').to_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    let last_two = labels[labels.len() - 2..].join(".");
    if MULTIPART_SUFFIXES.contains(&last_two.as_str()) && labels.len() >= 3 {
        labels[labels.len() - 3..].join(".")
    } else {
        last_two
    }
}

/// Same registrable domain (so we only ever follow the site's own pages).
pub fn same_registrable_domain(seed_host: &str, candidate_host: &str) -> bool {
    let seed = registrable_domain(seed_host);
    !seed.is_empty() && seed == registrable_domain(candidate_host)
}

/// Parse a candidate URL with the same URL parser used by guarded fetches and
/// return its canonical representation when it is allowed by the research URL
/// policy: either surfaced exactly by search, or on the seed registrable domain.
pub fn canonical_research_fetch_url(
    candidate_url: &str,
    seed_host: &str,
    surfaced_urls: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<String> {
    let parsed = Url::parse(candidate_url).ok()?;
    let host = parsed.host_str()?;
    let surfaced = surfaced_urls
        .into_iter()
        .any(|surfaced| surfaced.as_ref() == candidate_url);
    if surfaced || same_registrable_domain(seed_host, host) {
        Some(parsed.to_string())
    } else {
        None
    }
}

/// General URL/text terms for pages likely to carry organization facts. These
/// are matched as exact tokens from parsed URL segments and anchor text, never
/// substrings (`contactless` is not `contact`).
const ORG_INFO_TERMS: &[(&str, i32)] = &[
    ("contact", 100),
    ("about", 90),
    ("team", 80),
    ("leadership", 80),
    ("people", 75),
    ("staff", 75),
    ("company", 70),
    ("organization", 70),
    ("mission", 65),
    ("story", 65),
    ("locations", 55),
    ("location", 55),
];

/// Broad page classes that can mention organization terms while primarily
/// serving another workflow. They stay eligible when they are the only signal,
/// but lose to direct company/contact pages under the fixed crawl budget.
const NON_FACT_PAGE_CLASS_TERMS: &[(&str, i32)] = &[
    ("login", -80),
    ("account", -80),
    ("cart", -80),
    ("checkout", -80),
    ("book", -70),
    ("booking", -70),
    ("reservation", -70),
    ("reserve", -70),
    ("shop", -65),
    ("store", -65),
    ("catalog", -65),
    ("inventory", -65),
    ("listing", -65),
    ("listings", -65),
    ("rental", -65),
    ("rentals", -65),
    ("products", -55),
    ("product", -55),
    ("items", -55),
    ("blog", -45),
    ("blogs", -45),
    ("news", -45),
    ("article", -45),
    ("articles", -45),
    ("post", -45),
    ("posts", -45),
    ("privacy", -80),
    ("terms", -80),
    ("legal", -80),
    ("policy", -70),
    ("policies", -70),
    ("cancellation", -70),
];

/// Extract and score same-domain candidate links from homepage HTML, returning
/// absolute URLs ordered best-first. Fragment-only, mailto:, tel:, and
/// off-domain links are dropped.
fn score_candidate_links(html: &str, seed: &Url, seed_host: &str) -> Vec<String> {
    let document = Html::parse_document(html);
    let anchor_selector = Selector::parse("a[href]").expect("static selector");
    let mut scored: Vec<(i32, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for anchor in document.select(&anchor_selector) {
        let raw = anchor.value().attr("href").unwrap_or("").trim();
        if raw.is_empty()
            || raw.starts_with('#')
            || raw.starts_with("mailto:")
            || raw.starts_with("tel:")
            || raw.starts_with("javascript:")
        {
            continue;
        }
        let Ok(joined) = seed.join(raw) else {
            continue;
        };
        if !matches!(joined.scheme(), "http" | "https") {
            continue;
        }
        let Some(host) = joined.host_str() else {
            continue;
        };
        if !same_registrable_domain(seed_host, host) {
            continue;
        }
        let mut normalized = joined.clone();
        normalized.set_fragment(None);
        let key = normalized.to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        let link_text = anchor.text().collect::<Vec<_>>().join(" ");
        if let Some(score) = candidate_score(&normalized, Some(&link_text)) {
            scored.push((score, key));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, url)| url).collect()
}

fn candidate_score(url: &Url, link_text: Option<&str>) -> Option<i32> {
    let tokens = candidate_tokens(url, link_text);
    let mut best: Option<i32> = None;
    for (keyword, score) in ORG_INFO_TERMS {
        if tokens.contains(*keyword) {
            best = Some(best.map_or(*score, |current| current.max(*score)));
        }
    }
    let mut score = best?;
    for (keyword, penalty) in NON_FACT_PAGE_CLASS_TERMS {
        if tokens.contains(*keyword) {
            score += *penalty;
        }
    }
    (score > 0).then_some(score)
}

fn candidate_tokens(url: &Url, link_text: Option<&str>) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    if let Some(segments) = url.path_segments() {
        for segment in segments {
            push_tokens(
                &percent_decode_str(segment)
                    .decode_utf8_lossy()
                    .to_lowercase(),
                &mut tokens,
            );
        }
    }
    if let Some(text) = link_text {
        push_tokens(&text.to_lowercase(), &mut tokens);
    }
    tokens
}

fn push_tokens(text: &str, tokens: &mut BTreeSet<String>) {
    for token in text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        tokens.insert(token.to_string());
    }
}

fn url_candidate_score(url: &str) -> Option<i32> {
    Url::parse(url)
        .ok()
        .and_then(|parsed| candidate_score(&parsed, None))
}

fn sort_candidate_urls(candidates: &mut [String]) {
    candidates.sort_by(|a, b| {
        let a_score = url_candidate_score(a).unwrap_or(0);
        let b_score = url_candidate_score(b).unwrap_or(0);
        b_score.cmp(&a_score).then_with(|| a.cmp(b))
    });
}

/// Pull same-domain <loc> URLs from a sitemap.xml and append any keyword-scoring
/// ones to the candidate list, then re-rank the combined pool.
fn merge_sitemap_candidates(xml: &str, seed: &Url, seed_host: &str, candidates: &mut Vec<String>) {
    let loc_re = loc_regex();
    let mut seen: BTreeSet<String> = candidates.iter().cloned().collect();
    for caps in loc_re.captures_iter(xml) {
        let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        let Ok(joined) = seed.join(raw) else {
            continue;
        };
        if !matches!(joined.scheme(), "http" | "https") {
            continue;
        }
        let Some(host) = joined.host_str() else {
            continue;
        };
        if !same_registrable_domain(seed_host, host) {
            continue;
        }
        let mut normalized = joined.clone();
        normalized.set_fragment(None);
        let key = normalized.to_string();
        if seen.contains(&key) {
            continue;
        }
        if candidate_score(&normalized, None).is_some() {
            seen.insert(key.clone());
            candidates.push(key);
        }
    }
    sort_candidate_urls(candidates);
}

// ---------------------------------------------------------------------------
// Deterministic extraction
// ---------------------------------------------------------------------------

/// One extracted value plus its `page:<url>` provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentField {
    pub value: String,
    /// "page:<url>" — the page the value was read from.
    pub provenance: String,
}

impl EnrichmentField {
    fn new(value: impl Into<String>, url: &str) -> Self {
        Self {
            value: value.into(),
            provenance: format!("page:{url}"),
        }
    }
}

/// One page reduced to stripped visible text, for the LLM gap-filler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichedPageText {
    pub url: String,
    pub text: String,
}

/// Deterministically extracted company facts. Every populated field is grounded
/// in a fetched page. Contact-person fields stay sparse here (a website rarely
/// states them structurally); the LLM gap-filler fills them from page text when
/// present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebEnrichment {
    pub company_name: Option<EnrichmentField>,
    pub company_website: Option<EnrichmentField>,
    pub company_phone: Option<EnrichmentField>,
    pub company_address: Option<EnrichmentField>,
    pub company_email: Option<EnrichmentField>,
    pub company_description: Option<EnrichmentField>,
    /// Stripped page text (homepage first), bounded — the gap-filler's input.
    pub page_texts: Vec<EnrichedPageText>,
}

impl WebEnrichment {
    pub fn is_empty(&self) -> bool {
        self.company_name.is_none()
            && self.company_website.is_none()
            && self.company_phone.is_none()
            && self.company_address.is_none()
            && self.company_email.is_none()
            && self.company_description.is_none()
    }
}

/// Run the deterministic extraction pass over the fetched pages. Homepage-first
/// order means homepage values win ties; later pages only fill gaps.
pub fn extract_enrichment(pages: &[FetchedPage], max_text_chars: usize) -> WebEnrichment {
    let mut out = WebEnrichment::default();
    for page in pages {
        // Strongest signal: schema.org JSON-LD Organization/LocalBusiness.
        for org in jsonld_organizations(&page.html) {
            fill_from_jsonld(&mut out, &org, &page.url);
        }
        // OpenGraph fills name/description gaps.
        fill_from_opengraph(&mut out, &page.html, &page.url);
        // mailto:/tel: links and a conservative phone regex.
        fill_from_links_and_text(&mut out, &page.html, &page.url);
        out.page_texts.push(EnrichedPageText {
            url: page.url.clone(),
            text: strip_to_text(&page.html, max_text_chars),
        });
    }
    // Lead the gap-filler (and the operator-facing trace) with the pages most
    // likely to carry company facts — /contact, /about, /team — instead of a
    // listings-heavy homepage. Deterministic extraction above already ran in
    // fetch order (homepage-first wins ties); only the LLM input order changes.
    // Stable sort keeps fetch order among equally-scored pages.
    out.page_texts
        .sort_by_key(|p| std::cmp::Reverse(url_candidate_score(&p.url).unwrap_or(0)));
    out
}

fn set_if_empty(slot: &mut Option<EnrichmentField>, value: Option<String>, url: &str) {
    if slot.is_none() {
        if let Some(value) = value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            *slot = Some(EnrichmentField::new(
                value.chars().take(300).collect::<String>(),
                url,
            ));
        }
    }
}

/// Collect JSON-LD blocks and return the objects whose @type is an
/// Organization/LocalBusiness (handling @graph arrays and type arrays).
fn jsonld_organizations(html: &str) -> Vec<Value> {
    let mut orgs = Vec::new();
    for raw in jsonld_blocks(html) {
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        collect_orgs(&value, &mut orgs);
    }
    orgs
}

fn collect_orgs(value: &Value, orgs: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_orgs(item, orgs);
            }
        }
        Value::Object(map) => {
            if let Some(graph) = map.get("@graph") {
                collect_orgs(graph, orgs);
            }
            if type_is_org(map.get("@type")) {
                orgs.push(value.clone());
            }
        }
        _ => {}
    }
}

fn type_is_org(ty: Option<&Value>) -> bool {
    let is_org = |s: &str| {
        let s = s.to_lowercase();
        s == "organization"
            || s == "localbusiness"
            || s == "corporation"
            || s.ends_with("business")
            || s.ends_with("company")
    };
    match ty {
        Some(Value::String(s)) => is_org(s),
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).any(is_org),
        _ => false,
    }
}

fn fill_from_jsonld(out: &mut WebEnrichment, org: &Value, url: &str) {
    set_if_empty(&mut out.company_name, json_str(org, "name"), url);
    set_if_empty(&mut out.company_website, json_str(org, "url"), url);
    set_if_empty(
        &mut out.company_phone,
        json_phone(org.get("telephone")),
        url,
    );
    set_if_empty(&mut out.company_email, json_email(org.get("email")), url);
    set_if_empty(
        &mut out.company_description,
        json_str(org, "description"),
        url,
    );
    set_if_empty(
        &mut out.company_address,
        jsonld_address(org.get("address")),
        url,
    );
}

fn json_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn json_email(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| s.trim().trim_start_matches("mailto:").trim().to_string())
        .filter(|s| s.contains('@') && !s.contains(char::is_whitespace))
}

fn json_phone(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(format_us_phone)
}

/// Flatten a schema.org PostalAddress (or a plain string address) to one line.
fn jsonld_address(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        Some(Value::Object(_)) => {
            let addr = value.unwrap();
            let parts: Vec<String> = [
                "streetAddress",
                "addressLocality",
                "addressRegion",
                "postalCode",
                "addressCountry",
            ]
            .iter()
            .filter_map(|key| json_str(addr, key))
            .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Some(Value::Array(items)) => items.iter().find_map(|item| jsonld_address(Some(item))),
        _ => None,
    }
}

fn fill_from_opengraph(out: &mut WebEnrichment, html: &str, url: &str) {
    set_if_empty(
        &mut out.company_name,
        meta_property(html, "og:site_name"),
        url,
    );
    set_if_empty(
        &mut out.company_description,
        meta_property(html, "og:description"),
        url,
    );
}

fn fill_from_links_and_text(out: &mut WebEnrichment, html: &str, url: &str) {
    if out.company_email.is_none() {
        set_if_empty(&mut out.company_email, first_mailto(html), url);
    }
    if out.company_phone.is_none() {
        let tel = first_tel(html).or_else(|| first_us_phone(&strip_to_text(html, 20_000)));
        set_if_empty(&mut out.company_phone, tel, url);
    }
}

// --- small HTML/markup readers ---

fn jsonld_blocks(html: &str) -> Vec<String> {
    let re = jsonld_regex();
    re.captures_iter(html)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn meta_property(html: &str, property: &str) -> Option<String> {
    // Match <meta property="og:x" content="..."> in either attribute order.
    let escaped = regex::escape(property);
    let forward = Regex::new(&format!(
        r#"(?is)<meta[^>]*\bproperty\s*=\s*["']{escaped}["'][^>]*\bcontent\s*=\s*["']([^"']*)["']"#
    ))
    .ok()?;
    if let Some(caps) = forward.captures(html) {
        if let Some(m) = caps.get(1) {
            let value = decode_entities(m.as_str().trim());
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    let reverse = Regex::new(&format!(
        r#"(?is)<meta[^>]*\bcontent\s*=\s*["']([^"']*)["'][^>]*\bproperty\s*=\s*["']{escaped}["']"#
    ))
    .ok()?;
    reverse.captures(html).and_then(|caps| {
        caps.get(1)
            .map(|m| decode_entities(m.as_str().trim()))
            .filter(|v| !v.is_empty())
    })
}

fn first_mailto(html: &str) -> Option<String> {
    mailto_regex().captures(html).and_then(|caps| {
        caps.get(1)
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| s.contains('@') && !s.contains(char::is_whitespace))
    })
}

/// Validate a candidate string as a US phone number with libphonenumber (the
/// `phonenumber` crate) and return it in national format ("(843) 882-9224").
/// `None` for anything that isn't a real, valid number — this is what rejects
/// ZIP codes and listing dimensions ("2448 sq ft") that a bare regex would
/// happily mistake for a phone.
pub fn format_us_phone(candidate: &str) -> Option<String> {
    let parsed = phonenumber::parse(Some(phonenumber::country::US), candidate).ok()?;
    phonenumber::is_valid(&parsed).then(|| {
        parsed
            .format()
            .mode(phonenumber::Mode::National)
            .to_string()
    })
}

fn first_tel(html: &str) -> Option<String> {
    tel_regex()
        .captures(html)
        .and_then(|caps| caps.get(1).map(|m| m.as_str()))
        .and_then(format_us_phone)
}

/// First VALID US phone number in free text: a loose candidate scan hands each
/// phone-shaped run to libphonenumber, which keeps only the real ones.
pub fn first_us_phone(text: &str) -> Option<String> {
    phone_candidate_regex()
        .find_iter(text)
        .find_map(|m| format_us_phone(m.as_str()))
}

pub fn normalize_page_text(html: &str, _url: Option<&Url>, max_chars: usize) -> NormalizedPageText {
    NormalizedPageText {
        flat_text: legacy_flat_page_text(html, max_chars),
        layout_text: Arc::from(layout_page_text(html, max_chars)),
        normalizer_version: NORMALIZER_VERSION,
    }
}

/// Strip <script>/<style>, drop tags, decode the legacy entity set, collapse
/// whitespace, and cap length. Compatibility wrapper for existing callers.
pub fn strip_to_text(html: &str, max_chars: usize) -> String {
    legacy_flat_page_text(html, max_chars)
}

fn legacy_flat_page_text(html: &str, max_chars: usize) -> String {
    let without_blocks = script_style_regex().replace_all(html, " ");
    let without_tags = tag_regex().replace_all(&without_blocks, " ");
    let decoded = decode_entities(&without_tags);
    let collapsed = whitespace_regex().replace_all(&decoded, " ");
    collapsed.trim().chars().take(max_chars).collect()
}

fn layout_page_text(html: &str, max_chars: usize) -> String {
    let without_blocks = script_style_regex().replace_all(html, " ");
    let with_breaks = block_break_regex().replace_all(&without_blocks, "\n");
    let document = Html::parse_document(&with_breaks);
    let body_text = Selector::parse("body")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|body| body.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| document.root_element().text().collect::<Vec<_>>().join(" "));
    collapse_layout_text(&decode_layout_entities(&body_text), max_chars)
}

fn collapse_layout_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    let mut pending_newlines = 0usize;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            pending_newlines = 2;
            pending_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if pending_newlines == 0 {
                pending_space = true;
            }
            continue;
        }
        if !out.is_empty() {
            if pending_newlines > 0 {
                out.push('\n');
                if pending_newlines > 1 {
                    out.push('\n');
                }
            } else if pending_space {
                out.push(' ');
            }
        }
        pending_newlines = 0;
        pending_space = false;
        out.push(ch);
    }
    out.trim().chars().take(max_chars).collect()
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

fn decode_layout_entities(input: &str) -> String {
    let legacy_decoded = decode_entities(input);
    numeric_entity_regex()
        .replace_all(&legacy_decoded, |caps: &regex::Captures<'_>| {
            let raw = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            let code = raw
                .strip_prefix('x')
                .or_else(|| raw.strip_prefix('X'))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| raw.parse::<u32>().ok());
            code.and_then(char::from_u32)
                .map(|ch| ch.to_string())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

/// Canonicalize an already-sliced display span for containment matching.
///
/// Order is binding for Tier 4 grounding:
/// 1. decode HTML entities so model/page quoting differences do not matter;
/// 2. NFKC only after slicing, because display byte offsets must stay stable;
/// 3. strip zero-width/control chars that carry no visible evidence;
/// 4. collapse whitespace to one ASCII space and trim;
/// 5. normalize a minimal punctuation set: smart quotes to ASCII quotes and
///    common Unicode dash/minus variants to `-`;
/// 6. ASCII casefold for deterministic, locale-free containment.
pub fn canonicalize(input: &str) -> String {
    let decoded = decode_layout_entities(input);
    let normalized: String = decoded.nfkc().collect();
    let mut out = String::new();
    let mut pending_space = false;
    for ch in normalized.chars() {
        if matches!(
            ch,
            '\u{00ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}'
        ) {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        let normalized = match ch {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            _ => ch,
        };
        out.push(normalized);
    }
    out.trim().to_ascii_lowercase()
}

pub fn canonical_contains(value: &str, span: &str) -> bool {
    let value = canonicalize(value);
    !value.is_empty() && canonicalize(span).contains(&value)
}

#[cfg(test)]
pub(crate) fn old_strip_to_text_for_tests(html: &str, max_chars: usize) -> String {
    let script_style = Regex::new(r"(?is)<(script|style)\b[^>]*>.*?</(script|style)>").unwrap();
    let tag = Regex::new(r"(?is)<[^>]+>").unwrap();
    let whitespace = Regex::new(r"\s+").unwrap();
    let without_blocks = script_style.replace_all(html, " ");
    let without_tags = tag.replace_all(&without_blocks, " ");
    let decoded = without_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");
    let collapsed = whitespace.replace_all(&decoded, " ");
    collapsed.trim().chars().take(max_chars).collect()
}

// --- cached regexes (compiled once) ---

macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect("valid regex"))
        }
    };
}

cached_regex!(loc_regex, r#"(?is)<loc>\s*([^<\s]+)\s*</loc>"#);
cached_regex!(
    jsonld_regex,
    r#"(?is)<script[^>]*type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#
);
cached_regex!(mailto_regex, r#"(?is)mailto:([^"'?\s>]+)"#);
cached_regex!(tel_regex, r#"(?is)tel:([^"'?\s>]+)"#);
// Loose phone-SHAPED candidate: an optional +, a leading digit/paren, then a
// run of digits and common separators. Deliberately permissive — libphonenumber
// (format_us_phone) decides what's actually a valid number.
cached_regex!(
    phone_candidate_regex,
    r"(?x)
        \+?
        \(?\d
        [\d().\-\s]{6,18}
        \d
    "
);
cached_regex!(
    block_break_regex,
    r#"(?is)</?(address|article|aside|blockquote|br|dd|div|dl|dt|figcaption|figure|footer|form|h[1-6]|header|hr|li|main|nav|ol|p|pre|section|table|tbody|td|tfoot|th|thead|tr|ul)\b[^>]*>"#
);
cached_regex!(numeric_entity_regex, r#"(?i)&#(x[0-9a-f]+|\d+);"#);
cached_regex!(
    script_style_regex,
    r"(?is)<(script|style)\b[^>]*>.*?</(script|style)>"
);
cached_regex!(tag_regex, r"(?is)<[^>]+>");
cached_regex!(whitespace_regex, r"\s+");
cached_regex!(
    domain_regex,
    r"(?ix)
        (?:https?://)?                                    # optional scheme
        (?P<host>
            (?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+   # one or more labels
            [a-z]{2,24}                                    # TLD
        )
        (?:[/:?\#][^\s]*)?                                 # optional path/port/query
    "
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    fn migration_html_corpus() -> Vec<&'static str> {
        vec![
            r#"<html><body><h1>Example Company</h1><p>Contact billing@example.test</p></body></html>"#,
            r#"<html><body><a href="mailto:hi@example.test">Email us</a><a href="tel:+14155550199">Call</a></body></html>"#,
            r#"<html><body><p>Reception: (415) 555-0199</p></body></html>"#,
            r#"<html><head><script type="application/ld+json">{"@type":"Organization","name":"Example Company"}</script></head><body>Welcome</body></html>"#,
            r#"<html><head><style>.hidden{display:none}</style><script>var x=1;</script></head><body>Visible &amp; grounded</body></html>"#,
            r#"<html><body><p>Suite&nbsp;200 &#8212; Charleston</p><p>Use &copy; literally</p></body></html>"#,
            r#"<html><body><div><p>Nested <strong>visible</strong> text</div><p>after malformed block</body></html>"#,
            r#"<html><body><![CDATA[Contact section]]><p>Not script text</p><!-- comment --></body></html>"#,
            r#"<html><body><p>Emoji 😀 and café stay visible</p></body></html>"#,
            "<html><body><p>Big\n\n\t whitespace     runs</p></body></html>",
            r#"<html><body><address>1 Market St<br>San Francisco, CA</address></body></html>"#,
            r#"<html><body><nav><a href="/contact">Contact</a></nav><main><h2>About</h2><p>Company facts</p></main></body></html>"#,
        ]
    }

    /// Scripted HTTP: url → response, plus a recorded log of requested urls.
    #[derive(Default)]
    struct ScriptedHttp {
        responses: HashMap<String, WebHttpResponse>,
        requested: Mutex<Vec<String>>,
    }

    impl ScriptedHttp {
        fn page(mut self, url: &str, body: &str) -> Self {
            self.responses.insert(
                url.to_string(),
                WebHttpResponse {
                    status: 200,
                    content_type: Some("text/html".to_string()),
                    location: None,
                    body: body.to_string(),
                },
            );
            self
        }

        fn redirect(mut self, url: &str, to: &str) -> Self {
            self.responses.insert(
                url.to_string(),
                WebHttpResponse {
                    status: 301,
                    content_type: None,
                    location: Some(to.to_string()),
                    body: String::new(),
                },
            );
            self
        }
    }

    impl WebHttp for ScriptedHttp {
        fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError> {
            self.requested.lock().unwrap().push(url.to_string());
            match self.responses.get(url) {
                Some(r) => Ok(WebHttpResponse {
                    status: r.status,
                    content_type: r.content_type.clone(),
                    location: r.location.clone(),
                    body: r.body.clone(),
                }),
                None => Ok(WebHttpResponse {
                    status: 404,
                    content_type: None,
                    location: None,
                    body: String::new(),
                }),
            }
        }
    }

    /// Scripted resolver: host → ips, defaulting to a public address.
    struct ScriptedResolver {
        map: HashMap<String, Vec<IpAddr>>,
        default_public: bool,
    }

    impl ScriptedResolver {
        fn public() -> Self {
            Self {
                map: HashMap::new(),
                default_public: true,
            }
        }

        fn with(mut self, host: &str, ip: IpAddr) -> Self {
            self.map.entry(host.to_string()).or_default().push(ip);
            self
        }
    }

    impl HostResolver for ScriptedResolver {
        fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, WebFetchError> {
            if let Some(ips) = self.map.get(host) {
                return Ok(ips.clone());
            }
            if self.default_public {
                Ok(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
            } else {
                Err(WebFetchError::Blocked {
                    reason: "no record".to_string(),
                })
            }
        }
    }

    fn reader(
        http: ScriptedHttp,
        resolver: ScriptedResolver,
    ) -> WebPageReader<ScriptedHttp, ScriptedResolver> {
        WebPageReader::new(
            Arc::new(http),
            Arc::new(resolver),
            WebCrawlConfig::default(),
        )
    }

    #[test]
    fn public_ip_classification() {
        assert!(is_public_ip(&"93.184.216.34".parse().unwrap()));
        assert!(!is_public_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip(&"169.254.1.1".parse().unwrap()));
        assert!(!is_public_ip(&"100.64.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"192.0.0.8".parse().unwrap()));
        assert!(is_public_ip(&"192.0.78.143".parse().unwrap()));
        assert!(!is_public_ip(&"192.88.99.1".parse().unwrap()));
        assert!(!is_public_ip(&"198.18.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"224.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"::1".parse().unwrap()));
        assert!(!is_public_ip(&"fd00::1".parse().unwrap()));
        assert!(!is_public_ip(&"fe80::1".parse().unwrap()));
        assert!(!is_public_ip(&"ff02::1".parse().unwrap()));
        assert!(!is_public_ip(&"2001:db8::1".parse().unwrap()));
        assert!(!is_public_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_public_ip(&"2606:2800:220:1::1".parse().unwrap()));
    }

    #[test]
    fn registrable_domain_handles_subdomains_and_ccsld() {
        assert_eq!(registrable_domain("example.test"), "example.test");
        assert_eq!(registrable_domain("www.example.test"), "example.test");
        assert_eq!(registrable_domain("shop.eu.example.test"), "example.test");
        assert_eq!(registrable_domain("foo.bar.co.uk"), "bar.co.uk");
        assert!(same_registrable_domain("example.test", "www.example.test"));
        assert!(!same_registrable_domain("example.test", "evil.com"));
    }

    #[test]
    fn canonical_research_fetch_url_uses_fetch_parser_for_policy_host() {
        assert_eq!(
            canonical_research_fetch_url(
                "https://www.example.test/about",
                "example.test",
                std::iter::empty::<&str>(),
            )
            .as_deref(),
            Some("https://www.example.test/about")
        );
        assert!(canonical_research_fetch_url(
            "https://evil.example\\@example.test/",
            "example.test",
            std::iter::empty::<&str>(),
        )
        .is_none());
    }

    #[test]
    fn normalize_seed_forces_https() {
        assert_eq!(
            normalize_seed_url("example.test").unwrap().as_str(),
            "https://example.test/"
        );
        assert_eq!(
            normalize_seed_url("http://example.test/about")
                .unwrap()
                .scheme(),
            "https"
        );
        assert!(normalize_seed_url("   ").is_err());
    }

    #[test]
    fn find_domain_extracts_literal_only() {
        assert_eq!(
            find_domain("Went to example.test HQ — Casey is the contact"),
            Some("example.test".to_string())
        );
        assert_eq!(
            find_domain("see https://www.acme.co.uk/about for details"),
            Some("acme.co.uk".to_string())
        );
        // An email's domain part is contact data, not a mentioned site.
        assert_eq!(find_domain("ping casey@example.test"), None);
        // Common mailbox hosts are ignored.
        assert_eq!(find_domain("mail me at gmail.com"), None);
        // No domain present.
        assert_eq!(find_domain("invoice him $200 for the SEO audit"), None);
    }

    #[test]
    fn crawl_refuses_private_dns() {
        let http = ScriptedHttp::default().page("https://internal.test/", "<html></html>");
        let resolver = ScriptedResolver::public()
            .with("internal.test", IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)));
        let err = reader(http, resolver).crawl("internal.test").unwrap_err();
        assert!(matches!(err, WebFetchError::Blocked { .. }), "{err}");
    }

    #[test]
    fn crawl_refuses_explicit_non_https_port() {
        let http = ScriptedHttp::default();
        let err = reader(http, ScriptedResolver::public())
            .crawl("example.test:8443")
            .unwrap_err();
        assert!(matches!(err, WebFetchError::Blocked { .. }), "{err}");
    }

    #[test]
    fn crawl_follows_homepage_links_within_budget() {
        let home = r#"
            <html><head>
              <meta property="og:site_name" content="Example Company" />
            </head><body>
              <a href="/about-us">About</a>
              <a href="/contact">Contact</a>
              <a href="https://twitter.com/Example">Twitter</a>
              <a href="/blog/post-1">Blog</a>
            </body></html>"#;
        let http = ScriptedHttp::default()
            .page("https://example.test/", home)
            .page(
                "https://example.test/contact",
                r#"<html><body><a href="mailto:hi@example.test">Email</a>
                   Call (415) 555-0100</body></html>"#,
            )
            .page(
                "https://example.test/about-us",
                "<html><body>About us text</body></html>",
            );
        // No sitemap (404) — that consumes one budget slot.
        let pages = reader(http, ScriptedResolver::public())
            .crawl("example.test")
            .unwrap();
        let urls: Vec<&str> = pages.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(urls[0], "https://example.test/");
        // contact scores higher than about-us, both same-domain; twitter is
        // off-domain and dropped.
        assert!(urls.contains(&"https://example.test/contact"));
        assert!(urls.contains(&"https://example.test/about-us"));
        assert!(!urls.iter().any(|u| u.contains("twitter")));
    }

    #[test]
    fn crawl_prefers_company_fact_pages_over_rental_inventory_candidates() {
        let home = r#"
            <html><body>
              <a href="/vacation-rentals/contactless-entry">Contactless rental entry</a>
              <a href="/all-rentals/contact-the-host">Contact the host</a>
              <a href="/blogs/contactless-stays">Contactless stays blog</a>
            </body></html>"#;
        let sitemap = r#"
            <urlset>
              <url><loc>https://example.test/about-us</loc></url>
              <url><loc>https://example.test/contact-us</loc></url>
            </urlset>"#;
        let http = ScriptedHttp::default()
            .page("https://example.test/", home)
            .page("https://example.test/sitemap.xml", sitemap)
            .page(
                "https://example.test/contact-us",
                "<html><body>Contact Example Company</body></html>",
            )
            .page(
                "https://example.test/about-us",
                "<html><body>About Example Company</body></html>",
            )
            .page(
                "https://example.test/vacation-rentals/contactless-entry",
                "<html><body>Door code instructions for a rental</body></html>",
            )
            .page(
                "https://example.test/all-rentals/contact-the-host",
                "<html><body>Host contact on a booking page</body></html>",
            );

        let pages = reader(http, ScriptedResolver::public())
            .crawl("example.test")
            .unwrap();
        let urls: Vec<&str> = pages.iter().map(|p| p.url.as_str()).collect();

        // With a four-request crawl budget, homepage + sitemap leave only two
        // candidate fetches. Spend them on company fact pages, not inventory
        // pages that happen to contain words like "contact".
        assert_eq!(
            urls,
            vec![
                "https://example.test/",
                "https://example.test/contact-us",
                "https://example.test/about-us",
            ]
        );
    }

    #[test]
    fn crawl_follows_redirect_and_reguards() {
        let http = ScriptedHttp::default()
            .redirect("https://example.test/", "https://www.example.test/")
            .page(
                "https://www.example.test/",
                "<html><body>Home</body></html>",
            );
        let pages = reader(http, ScriptedResolver::public())
            .crawl("example.test")
            .unwrap();
        assert_eq!(pages[0].url, "https://www.example.test/");
    }

    #[test]
    fn crawl_blocks_offdomain_redirect() {
        let http =
            ScriptedHttp::default().redirect("https://example.test/", "https://evil.example/");
        // Homepage redirect leaves the registrable domain → crawl errors out.
        let err = reader(http, ScriptedResolver::public())
            .crawl("example.test")
            .unwrap_err();
        assert!(matches!(err, WebFetchError::Blocked { .. }), "{err}");
    }

    #[test]
    fn extraction_pulls_jsonld_and_opengraph() {
        let home = r#"
            <html><head>
              <meta property="og:description" content="Boutique stays" />
              <script type="application/ld+json">
              {"@context":"https://schema.org","@type":"Organization",
               "name":"Example Company LLC","url":"https://example.test",
               "telephone":"+1-415-555-0100","email":"hello@example.test",
               "address":{"@type":"PostalAddress","streetAddress":"1 Market St",
                 "addressLocality":"San Francisco","addressRegion":"CA",
                 "postalCode":"94105"}}
              </script>
            </head><body>Welcome</body></html>"#;
        let pages = vec![FetchedPage {
            url: "https://example.test/".to_string(),
            html: home.to_string(),
        }];
        let enrich = extract_enrichment(&pages, 8_000);
        assert_eq!(
            enrich.company_name.as_ref().unwrap().value,
            "Example Company LLC"
        );
        assert_eq!(
            enrich.company_name.as_ref().unwrap().provenance,
            "page:https://example.test/"
        );
        assert_eq!(
            enrich.company_phone.as_ref().unwrap().value,
            "(415) 555-0100"
        );
        assert_eq!(
            enrich.company_email.as_ref().unwrap().value,
            "hello@example.test"
        );
        assert!(enrich
            .company_address
            .as_ref()
            .unwrap()
            .value
            .contains("San Francisco"));
        assert_eq!(
            enrich.company_description.as_ref().unwrap().value,
            "Boutique stays"
        );
    }

    #[test]
    fn extraction_falls_back_to_mailto_and_phone_regex() {
        let pages = vec![FetchedPage {
            url: "https://example.test/contact".to_string(),
            html: r#"<html><body>
                <a href="mailto:hi@example.test">Email us</a>
                <p>Reception: (415) 555-0199</p>
                </body></html>"#
                .to_string(),
        }];
        let enrich = extract_enrichment(&pages, 8_000);
        assert_eq!(enrich.company_email.unwrap().value, "hi@example.test");
        assert_eq!(enrich.company_phone.unwrap().value, "(415) 555-0199");
    }

    #[test]
    fn phone_extraction_validates_and_rejects_listing_noise() {
        // libphonenumber validates: the header phone is found and normalized,
        // while listing dimensions ("2448 sq ft 14 5") and a bare ZIP are not
        // mistaken for phone numbers.
        let pages = vec![FetchedPage {
            url: "https://example.test/".to_string(),
            html: "<html><body>Contact Us +1 (843) 882-9224 \
                   Beach Belle 2448 sq ft 14 5 4.5 Mount Pleasant SC 29464</body></html>"
                .to_string(),
        }];
        let phone = extract_enrichment(&pages, 8_000)
            .company_phone
            .expect("phone");
        assert_eq!(
            phone.value, "(843) 882-9224",
            "normalized to national format"
        );

        let noise = vec![FetchedPage {
            url: "https://x.test/".to_string(),
            html: "<html><body>2448 sq ft 14 5 4.5 zip 29464</body></html>".to_string(),
        }];
        assert!(extract_enrichment(&noise, 8_000).company_phone.is_none());
    }

    #[test]
    fn jsonld_phone_is_validated() {
        let pages = vec![FetchedPage {
            url: "https://example.test/".to_string(),
            html: r#"<html><head><script type="application/ld+json">
                {"@context":"https://schema.org","@type":"Organization",
                 "name":"Example Company LLC","telephone":"2448 sq ft 14 5"}
                </script></head><body>No phone here</body></html>"#
                .to_string(),
        }];
        assert!(extract_enrichment(&pages, 8_000).company_phone.is_none());
    }

    #[test]
    fn page_texts_lead_with_company_fact_pages() {
        let pages = vec![
            FetchedPage {
                url: "https://example.test/".to_string(),
                html: "<html><body>Homepage listings</body></html>".to_string(),
            },
            FetchedPage {
                url: "https://example.test/about".to_string(),
                html: "<html><body>About text</body></html>".to_string(),
            },
            FetchedPage {
                url: "https://example.test/contact".to_string(),
                html: "<html><body>Contact text</body></html>".to_string(),
            },
        ];
        let urls: Vec<String> = extract_enrichment(&pages, 8_000)
            .page_texts
            .into_iter()
            .map(|page| page.url)
            .collect();
        assert_eq!(
            urls,
            vec![
                "https://example.test/contact".to_string(),
                "https://example.test/about".to_string(),
                "https://example.test/".to_string(),
            ]
        );
    }

    #[test]
    fn strip_to_text_removes_scripts_and_tags() {
        let text = strip_to_text(
            "<html><head><style>.a{}</style><script>var x=1;</script></head>\
             <body><h1>Hi</h1><p>There &amp; here</p></body></html>",
            1_000,
        );
        assert_eq!(text, "Hi There & here");
    }

    #[test]
    fn strip_to_text_matches_normalize_page_text_flat_projection() {
        for html in migration_html_corpus() {
            assert_eq!(
                strip_to_text(html, 20_000),
                normalize_page_text(html, None, 20_000).flat_text
            );
        }
    }

    #[test]
    fn normalize_page_text_flat_projection_matches_legacy_corpus() {
        for html in migration_html_corpus() {
            assert_eq!(
                normalize_page_text(html, None, 20_000).flat_text,
                old_strip_to_text_for_tests(html, 20_000),
                "legacy flat projection changed for {html:?}"
            );
            assert_eq!(
                normalize_page_text(html, None, 24).flat_text,
                old_strip_to_text_for_tests(html, 24),
                "legacy char cap changed for {html:?}"
            );
        }
    }

    #[test]
    fn phone_fallback_scan_is_stable_over_normalized_flat_text() {
        for html in migration_html_corpus() {
            assert_eq!(
                first_us_phone(&normalize_page_text(html, None, 20_000).flat_text),
                first_us_phone(&old_strip_to_text_for_tests(html, 20_000)),
                "phone fallback changed for {html:?}"
            );
        }
    }

    #[test]
    fn normalize_page_text_display_outputs_do_not_nfkc() {
        let html = "<html><body><p>Phone １２３</p><p>Ligature ﬁ and e\u{301}</p></body></html>";
        let normalized = normalize_page_text(html, None, 1_000);
        assert!(normalized.flat_text.contains("１２３"));
        assert!(normalized.flat_text.contains('ﬁ'));
        assert!(normalized.flat_text.contains("e\u{301}"));
        assert!(normalized.layout_text.contains("１２３"));
        assert!(normalized.layout_text.contains('ﬁ'));
        assert!(normalized.layout_text.contains("e\u{301}"));
    }

    #[test]
    fn normalize_page_text_keeps_flat_entity_set_legacy_but_layout_decodes_numeric() {
        let html = "<html><body><p>A&amp;B&nbsp;C &#8212; D &copy;</p></body></html>";
        let normalized = normalize_page_text(html, None, 1_000);
        assert_eq!(normalized.flat_text, "A&B C &#8212; D &copy;");
        assert!(normalized.layout_text.contains("A&B C — D ©"));
    }

    #[test]
    fn layout_text_preserves_visible_tokens_without_link_artifacts() {
        let html = r#"<html><body>
            <nav><a href="https://example.com/contact">Contact</a></nav>
            <main><h1>Example Company</h1><p>Book &amp; stay now.</p></main>
            </body></html>"#;
        let normalized = normalize_page_text(html, None, 1_000);
        let mut flat_tokens: Vec<_> = normalized.flat_text.split_whitespace().collect();
        let mut layout_tokens: Vec<_> = normalized.layout_text.split_whitespace().collect();
        flat_tokens.sort_unstable();
        layout_tokens.sort_unstable();
        assert_eq!(layout_tokens, flat_tokens);
        assert!(normalized.layout_text.contains('\n'));
        assert!(!normalized.layout_text.contains('['));
        assert!(!normalized.layout_text.contains(']'));
        assert!(!normalized.layout_text.contains("(http"));
    }

    #[test]
    fn canonicalize_is_slice_first_nfkc_after_and_idempotent() {
        let display = "A１２B";
        let display_slice = &display[1..7];
        assert_eq!(display_slice, "１２");
        assert_eq!(canonicalize(display_slice), "12");
        assert_eq!(
            canonicalize(&canonicalize(" A&nbsp;１２\u{200b}—B ")),
            "a 12-b"
        );
    }

    #[test]
    fn canonical_contains_matches_value_inside_span_only() {
        assert!(canonical_contains(
            "billing@example.com",
            "Email BILLING@example.com for invoices",
        ));
        assert!(canonical_contains("12-b", "Quote: １２\u{200b}—B"));
        assert!(canonical_contains("\"hello\"", "They said “HELLO” today"));
        assert!(!canonical_contains(
            "billing@example.com",
            "Email support@example.com"
        ));
        assert!(!canonical_contains("", "anything"));
        assert_eq!(canonicalize(" \t\n"), "");
    }

    #[test]
    fn budget_caps_total_requests() {
        // Homepage links to many candidates; only MAX_REQUESTS-1 (after the
        // sitemap 404) candidates are fetched.
        let home = r#"<html><body>
            <a href="/contact">c</a><a href="/about">a</a>
            <a href="/team">t</a><a href="/company">co</a>
            </body></html>"#;
        let http = ScriptedHttp::default()
            .page("https://example.test/", home)
            .page("https://example.test/contact", "x")
            .page("https://example.test/about", "x")
            .page("https://example.test/team", "x")
            .page("https://example.test/company", "x");
        let pages = reader(http, ScriptedResolver::public())
            .crawl("example.test")
            .unwrap();
        // homepage + at most (MAX_REQUESTS - 1 sitemap slot) pages, never more
        // than MAX_REQUESTS network calls total.
        assert!(pages.len() <= MAX_REQUESTS);
    }
}
