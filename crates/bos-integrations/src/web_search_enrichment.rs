//! Bounded web-search evidence collection for draft enrichment.
//!
//! Callers provide purpose-scoped queries. The collector runs a configured
//! search endpoint, caps query/result/page budgets, fetches public result pages
//! through the guarded website reader, and returns curated evidence for typed
//! transforms. LLMs never receive arbitrary browser/tool access.

use std::sync::Arc;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;

use crate::web_page_read::{
    FetchedPage, HostResolver, WebCrawlConfig, WebFetchError, WebHttp, WebPageReader,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchProvider {
    Generic,
    Tavily,
}

impl WebSearchProvider {
    pub fn from_name(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None => None,
            // SearXNG is a generic JSON search endpoint; `searxng` is an alias
            // for the Generic provider so config reads self-documentingly.
            Some(value)
                if value.eq_ignore_ascii_case("generic")
                    || value.eq_ignore_ascii_case("searxng") =>
            {
                Some(Self::Generic)
            }
            Some(value) if value.eq_ignore_ascii_case("tavily") => Some(Self::Tavily),
            Some(_) => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Tavily => "tavily",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebSearchConfig {
    pub enabled: bool,
    pub provider: Option<WebSearchProvider>,
    pub endpoint_url: Option<String>,
    pub api_key: Option<String>,
    /// Keyless fallback search endpoint (e.g. a self-hosted SearXNG `?format=json`
    /// URL) queried as a `Generic` provider when the primary search errors or
    /// rate-limits. Unset = no fallback.
    pub fallback_endpoint_url: Option<String>,
    pub max_queries: usize,
    pub max_results_per_query: usize,
    pub max_fetched_pages: usize,
    pub timeout_ms: u64,
    pub cost_budget_micros: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            endpoint_url: None,
            api_key: None,
            fallback_endpoint_url: None,
            max_queries: 1,
            max_results_per_query: 3,
            max_fetched_pages: 2,
            timeout_ms: 10_000,
            cost_budget_micros: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub query: String,
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEvidencePage {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchEvidence {
    pub purpose: String,
    pub reason: String,
    pub queries: Vec<String>,
    pub results: Vec<SearchResult>,
    pub pages: Vec<SearchEvidencePage>,
    pub failures: Vec<String>,
}

impl SearchEvidence {
    pub fn search_was_attempted(&self) -> bool {
        !self.queries.is_empty()
    }

    pub fn text_for_llm(&self, max_chars: usize) -> String {
        let mut out = String::new();
        for (idx, page) in self.pages.iter().enumerate() {
            out.push_str(&format!(
                "SEARCH_PAGE_{idx}\nURL: {}\nTitle: {}\nSnippet: {}\n{}\n\n",
                page.url, page.title, page.snippet, page.text
            ));
            if out.chars().count() >= max_chars {
                return out.chars().take(max_chars).collect();
            }
        }
        out
    }
}

pub trait WebSearchApi: Send + Sync {
    fn search(
        &self,
        config: &WebSearchConfig,
        query: &str,
        timeout_ms: u64,
    ) -> Result<Vec<SearchResult>, WebFetchError>;
}

#[derive(Default)]
pub struct ReqwestWebSearchApi;

impl WebSearchApi for ReqwestWebSearchApi {
    fn search(
        &self,
        config: &WebSearchConfig,
        query: &str,
        timeout_ms: u64,
    ) -> Result<Vec<SearchResult>, WebFetchError> {
        match config.provider.unwrap_or(WebSearchProvider::Generic) {
            WebSearchProvider::Generic => self.search_generic(config, query, timeout_ms),
            WebSearchProvider::Tavily => self.search_tavily(config, query, timeout_ms),
        }
    }
}

impl ReqwestWebSearchApi {
    fn search_generic(
        &self,
        config: &WebSearchConfig,
        query: &str,
        timeout_ms: u64,
    ) -> Result<Vec<SearchResult>, WebFetchError> {
        let endpoint_url =
            config
                .endpoint_url
                .as_deref()
                .ok_or_else(|| WebFetchError::Transport {
                    message: "search_endpoint_unset".to_string(),
                })?;
        let encoded = utf8_percent_encode(query, NON_ALPHANUMERIC).to_string();
        let url = if endpoint_url.contains("{query}") {
            endpoint_url.replace("{query}", &encoded)
        } else if endpoint_url.contains('?') {
            format!("{endpoint_url}&q={encoded}")
        } else {
            format!("{endpoint_url}?q={encoded}")
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms.max(1)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("BusinessOS-SearchEnrichment/1.0")
            .build()
            .map_err(|err| WebFetchError::Transport {
                message: err.to_string(),
            })?;
        let mut request = client.get(url).header("Accept", "application/json");
        if let Some(key) = config
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
        {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let response = request.send().map_err(|err| WebFetchError::Transport {
            message: err.to_string(),
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(WebFetchError::Transport {
                message: format!("search status {status}"),
            });
        }
        let body = response.text().map_err(|err| WebFetchError::Transport {
            message: err.to_string(),
        })?;
        let value: Value = serde_json::from_str(&body).map_err(|err| WebFetchError::Transport {
            message: format!("search json parse failed: {err}"),
        })?;
        Ok(parse_search_results(query, &value))
    }

    fn search_tavily(
        &self,
        config: &WebSearchConfig,
        query: &str,
        timeout_ms: u64,
    ) -> Result<Vec<SearchResult>, WebFetchError> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| WebFetchError::Transport {
                message: "search_api_key_unset".to_string(),
            })?;
        let endpoint = config
            .endpoint_url
            .as_deref()
            .unwrap_or("https://api.tavily.com/search");
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms.max(1)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("BusinessOS-SearchEnrichment/1.0")
            .build()
            .map_err(|err| WebFetchError::Transport {
                message: err.to_string(),
            })?;
        let request = client
            .post(endpoint)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&serde_json::json!({
                "query": query,
                "search_depth": "basic",
                "max_results": config.max_results_per_query.max(1),
                "include_raw_content": false
            }));
        let response = request.send().map_err(|err| WebFetchError::Transport {
            message: err.to_string(),
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(WebFetchError::Transport {
                message: format!("search status {status}"),
            });
        }
        let body = response.text().map_err(|err| WebFetchError::Transport {
            message: err.to_string(),
        })?;
        let value: Value = serde_json::from_str(&body).map_err(|err| WebFetchError::Transport {
            message: format!("search json parse failed: {err}"),
        })?;
        Ok(parse_tavily_search_results(query, &value))
    }
}

pub struct WebSearchCollector<S: WebSearchApi, H: WebHttp, R: HostResolver> {
    search: Arc<S>,
    page_reader: WebPageReader<H, R>,
    config: WebSearchConfig,
}

impl<S: WebSearchApi, H: WebHttp, R: HostResolver> WebSearchCollector<S, H, R> {
    pub fn new(search: Arc<S>, http: Arc<H>, resolver: Arc<R>, config: WebSearchConfig) -> Self {
        let page_reader = WebPageReader::new(
            http,
            resolver,
            WebCrawlConfig {
                max_requests: config.max_fetched_pages,
                max_candidate_pages: 0,
                max_text_chars: 8_000,
            },
        );
        Self {
            search,
            page_reader,
            config,
        }
    }

    pub fn collect(&self, purpose: &str, reason: &str, queries: &[String]) -> SearchEvidence {
        self.collect_with_mode(purpose, reason, queries, true)
    }

    /// Run search/fallback collection without fetching any result pages.
    ///
    /// This is intentionally not `max_fetched_pages = 0`: callers that only need
    /// surfaced URL candidates should not exercise the page HTTP/resolver path at
    /// all.
    pub fn search_results_only(
        &self,
        purpose: &str,
        reason: &str,
        queries: &[String],
    ) -> SearchEvidence {
        self.collect_with_mode(purpose, reason, queries, false)
    }

    fn collect_with_mode(
        &self,
        purpose: &str,
        reason: &str,
        queries: &[String],
        fetch_pages: bool,
    ) -> SearchEvidence {
        let mut evidence = SearchEvidence {
            purpose: purpose.to_string(),
            reason: reason.to_string(),
            ..SearchEvidence::default()
        };
        if let Some(failure) = self.preflight_failure() {
            evidence.failures.push(failure.to_string());
            return evidence;
        }
        let mut page_budget = self.config.max_fetched_pages;
        for query in queries
            .iter()
            .map(|q| q.trim())
            .filter(|q| !q.is_empty())
            .take(self.config.max_queries)
        {
            evidence.queries.push(query.to_string());
            // Primary search; on any error (rate-limit, timeout, 5xx) fall back to
            // the keyless fallback endpoint (e.g. self-hosted SearXNG) when set.
            let outcome = match self
                .search
                .search(&self.config, query, self.config.timeout_ms)
            {
                Ok(results) => Some(results),
                Err(primary_err) => match self.fallback_config() {
                    Some(fallback) => {
                        evidence
                            .failures
                            .push(format!("primary_search_failed:{query}:{primary_err}"));
                        match self.search.search(&fallback, query, self.config.timeout_ms) {
                            Ok(results) => Some(results),
                            Err(fallback_err) => {
                                evidence
                                    .failures
                                    .push(format!("fallback_search_failed:{query}:{fallback_err}"));
                                None
                            }
                        }
                    }
                    None => {
                        evidence
                            .failures
                            .push(format!("search_failed:{query}:{primary_err}"));
                        None
                    }
                },
            };
            let Some(results) = outcome else {
                continue;
            };
            for result in results.into_iter().take(self.config.max_results_per_query) {
                if evidence
                    .results
                    .iter()
                    .any(|existing| existing.url == result.url)
                {
                    continue;
                }
                let result = SearchResult {
                    query: query.to_string(),
                    ..result
                };
                if fetch_pages
                    && page_budget > 0
                    && evidence.pages.len() < self.config.max_fetched_pages
                {
                    match self
                        .page_reader
                        .fetch_public_page(&result.url, &mut page_budget)
                    {
                        Ok(page) => evidence.pages.push(page_to_evidence(&result, page)),
                        Err(err) => evidence
                            .failures
                            .push(format!("fetch_failed:{}:{err}", result.url)),
                    }
                }
                evidence.results.push(result);
            }
        }
        evidence
    }

    fn preflight_failure(&self) -> Option<&'static str> {
        if !self.config.enabled {
            return Some("search_disabled");
        }
        if self.config.cost_budget_micros == 0 {
            return Some("search_cost_budget_zero");
        }
        let provider = self.config.provider.unwrap_or(WebSearchProvider::Generic);
        if provider == WebSearchProvider::Generic && self.config.endpoint_url.is_none() {
            return Some("search_endpoint_unset");
        }
        if provider == WebSearchProvider::Tavily
            && self
                .config
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .is_none()
        {
            return Some("search_api_key_unset");
        }
        None
    }

    /// A `Generic` search config aimed at the keyless fallback endpoint, used when
    /// the primary search errors. Carries no api_key and no further fallback.
    fn fallback_config(&self) -> Option<WebSearchConfig> {
        let endpoint = self
            .config
            .fallback_endpoint_url
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())?;
        Some(WebSearchConfig {
            provider: Some(WebSearchProvider::Generic),
            endpoint_url: Some(endpoint.to_string()),
            api_key: None,
            fallback_endpoint_url: None,
            ..self.config.clone()
        })
    }
}

fn page_to_evidence(result: &SearchResult, page: FetchedPage) -> SearchEvidencePage {
    SearchEvidencePage {
        url: page.url,
        title: result.title.clone(),
        snippet: result.snippet.clone(),
        text: crate::web_page_read::strip_to_text(&page.html, 8_000),
    }
}

pub fn parse_search_results(query: &str, value: &Value) -> Vec<SearchResult> {
    let candidates = value
        .get("webPages")
        .and_then(|v| v.get("value"))
        .or_else(|| value.get("organic"))
        .or_else(|| value.get("results"))
        .or_else(|| value.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    candidates
        .iter()
        .filter_map(|item| {
            let url = first_string(item, &["url", "link"])?;
            let title = first_string(item, &["name", "title"]).unwrap_or_default();
            // `content` covers SearXNG result objects (used as the keyless fallback).
            let snippet =
                first_string(item, &["snippet", "description", "content"]).unwrap_or_default();
            Some(SearchResult {
                query: query.to_string(),
                title: title.chars().take(300).collect(),
                url,
                snippet: snippet.chars().take(500).collect(),
            })
        })
        .collect()
}

pub fn parse_tavily_search_results(query: &str, value: &Value) -> Vec<SearchResult> {
    value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = first_string(item, &["url"])?;
            let title = first_string(item, &["title"]).unwrap_or_default();
            let snippet =
                first_string(item, &["content", "raw_content", "snippet"]).unwrap_or_default();
            Some(SearchResult {
                query: query.to_string(),
                title: title.chars().take(300).collect(),
                url,
                snippet: snippet.chars().take(500).collect(),
            })
        })
        .collect()
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str())
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_page_read::{old_strip_to_text_for_tests, WebFetchError, WebHttpResponse};
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    struct ScriptSearch {
        results: Vec<SearchResult>,
    }

    impl WebSearchApi for ScriptSearch {
        fn search(
            &self,
            _config: &WebSearchConfig,
            query: &str,
            _timeout_ms: u64,
        ) -> Result<Vec<SearchResult>, WebFetchError> {
            Ok(self
                .results
                .iter()
                .cloned()
                .map(|mut result| {
                    result.query = query.to_string();
                    result
                })
                .collect())
        }
    }

    struct ScriptHttp {
        pages: BTreeMap<String, String>,
    }

    impl WebHttp for ScriptHttp {
        fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError> {
            let body = self
                .pages
                .get(url)
                .cloned()
                .ok_or_else(|| WebFetchError::Transport {
                    message: format!("missing script page {url}"),
                })?;
            Ok(WebHttpResponse {
                status: 200,
                content_type: Some("text/html".to_string()),
                location: None,
                body,
            })
        }
    }

    struct PublicResolver;

    impl HostResolver for PublicResolver {
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, WebFetchError> {
            Ok(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
        }
    }

    struct RecordingHttp {
        requested: Arc<Mutex<Vec<String>>>,
    }

    impl WebHttp for RecordingHttp {
        fn get(&self, url: &str) -> Result<WebHttpResponse, WebFetchError> {
            self.requested.lock().unwrap().push(url.to_string());
            Ok(WebHttpResponse {
                status: 200,
                content_type: Some("text/html".to_string()),
                location: None,
                body: "<html>would fetch</html>".to_string(),
            })
        }
    }

    #[test]
    fn page_to_evidence_uses_legacy_stable_flat_text() {
        let html = r#"<html><head><style>.x{}</style><script>bad()</script></head>
            <body><h1>Example&nbsp;Stays</h1><p>Book &amp; stay &#8212; today.</p></body></html>"#;
        let result = SearchResult {
            query: "Example".to_string(),
            title: "Example Company".to_string(),
            url: "https://example.test/about".to_string(),
            snippet: "About Example".to_string(),
        };
        let page = FetchedPage {
            url: result.url.clone(),
            html: html.to_string(),
        };
        let evidence = page_to_evidence(&result, page);
        assert_eq!(evidence.text, old_strip_to_text_for_tests(html, 8_000));
    }

    #[test]
    fn disabled_search_records_failure_without_queries() {
        let collector = WebSearchCollector::new(
            Arc::new(ScriptSearch { results: vec![] }),
            Arc::new(ScriptHttp {
                pages: BTreeMap::new(),
            }),
            Arc::new(PublicResolver),
            WebSearchConfig::default(),
        );
        let evidence = collector.collect("crm_record_drafts", "weak company", &["q".to_string()]);
        assert_eq!(evidence.failures, vec!["search_disabled"]);
        assert!(evidence.queries.is_empty());
        assert!(!evidence.search_was_attempted());
    }

    #[test]
    fn budget_caps_queries_results_and_fetched_pages() {
        let results = vec![
            SearchResult {
                query: String::new(),
                title: "A".to_string(),
                url: "https://example.com/a".to_string(),
                snippet: "Alpha".to_string(),
            },
            SearchResult {
                query: String::new(),
                title: "B".to_string(),
                url: "https://example.com/b".to_string(),
                snippet: "Beta".to_string(),
            },
        ];
        let mut pages = BTreeMap::new();
        pages.insert(
            "https://example.com/a".to_string(),
            "<html>Example Company vacation homes</html>".to_string(),
        );
        pages.insert(
            "https://example.com/b".to_string(),
            "<html>Second page</html>".to_string(),
        );
        let collector = WebSearchCollector::new(
            Arc::new(ScriptSearch { results }),
            Arc::new(ScriptHttp { pages }),
            Arc::new(PublicResolver),
            WebSearchConfig {
                enabled: true,
                endpoint_url: Some("https://search.example?q={query}".to_string()),
                max_queries: 1,
                max_results_per_query: 1,
                max_fetched_pages: 1,
                ..WebSearchConfig::default()
            },
        );
        let evidence = collector.collect(
            "crm_record_drafts",
            "weak company",
            &["first".to_string(), "second".to_string()],
        );
        assert_eq!(evidence.queries, vec!["first"]);
        assert!(evidence.search_was_attempted());
        assert_eq!(evidence.results.len(), 1);
        assert_eq!(evidence.pages.len(), 1);
        assert!(evidence.text_for_llm(500).contains("Example Company"));
    }

    #[test]
    fn results_only_search_never_fetches_pages() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let collector = WebSearchCollector::new(
            Arc::new(ScriptSearch {
                results: vec![SearchResult {
                    query: String::new(),
                    title: "A".to_string(),
                    url: "https://example.com/a".to_string(),
                    snippet: "Alpha".to_string(),
                }],
            }),
            Arc::new(RecordingHttp {
                requested: requested.clone(),
            }),
            Arc::new(PublicResolver),
            WebSearchConfig {
                enabled: true,
                endpoint_url: Some("https://search.example?q={query}".to_string()),
                max_fetched_pages: 1,
                ..WebSearchConfig::default()
            },
        );

        let evidence =
            collector.search_results_only("crm_record_drafts", "weak company", &["a".to_string()]);

        assert_eq!(evidence.results.len(), 1);
        assert!(evidence.pages.is_empty());
        assert!(requested.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_common_search_json_shapes() {
        let value = serde_json::json!({
            "webPages": { "value": [
                { "name": "Example Company", "url": "https://example.test/about", "snippet": "Vacation rentals" }
            ]}
        });
        let results = parse_search_results("Example", &value);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Company");
    }

    #[test]
    fn parses_tavily_search_results() {
        let value = serde_json::json!({
            "query": "example.test official company name example.test",
            "results": [
                {
                    "title": "Example Company",
                    "url": "https://www.houfy.com/lodging/Example Company/123",
                    "content": "Example Company offers waterfront vacation rentals.",
                    "score": 0.91
                }
            ],
            "request_id": "req_123",
            "usage": { "credits": 1 }
        });
        let results = parse_tavily_search_results("Example", &value);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Company");
        assert_eq!(
            results[0].snippet,
            "Example Company offers waterfront vacation rentals."
        );
    }

    #[test]
    fn tavily_missing_key_records_failure() {
        let collector = WebSearchCollector::new(
            Arc::new(ScriptSearch { results: vec![] }),
            Arc::new(ScriptHttp {
                pages: BTreeMap::new(),
            }),
            Arc::new(PublicResolver),
            WebSearchConfig {
                enabled: true,
                provider: Some(WebSearchProvider::Tavily),
                ..WebSearchConfig::default()
            },
        );
        let evidence = collector.collect("crm_record_drafts", "weak company", &["q".to_string()]);
        assert_eq!(evidence.failures, vec!["search_api_key_unset"]);
        assert!(evidence.queries.is_empty());
    }

    struct FailoverSearch;

    impl WebSearchApi for FailoverSearch {
        fn search(
            &self,
            config: &WebSearchConfig,
            query: &str,
            _timeout_ms: u64,
        ) -> Result<Vec<SearchResult>, WebFetchError> {
            // Primary (Tavily) is rate-limited; the Generic keyless fallback succeeds.
            match config.provider {
                Some(WebSearchProvider::Generic) => Ok(vec![SearchResult {
                    query: query.to_string(),
                    title: "Example Company".to_string(),
                    url: "https://example.com/Example".to_string(),
                    snippet: "Waterfront rentals".to_string(),
                }]),
                _ => Err(WebFetchError::Transport {
                    message: "search status 429 Too Many Requests".to_string(),
                }),
            }
        }
    }

    #[test]
    fn primary_rate_limit_falls_back_to_keyless_endpoint() {
        let collector = WebSearchCollector::new(
            Arc::new(FailoverSearch),
            Arc::new(ScriptHttp {
                pages: BTreeMap::new(),
            }),
            Arc::new(PublicResolver),
            WebSearchConfig {
                enabled: true,
                provider: Some(WebSearchProvider::Tavily),
                api_key: Some("tvly-key".to_string()),
                fallback_endpoint_url: Some(
                    "https://searxng.local/search?q={query}&format=json".to_string(),
                ),
                // Skip page fetching; this test asserts the search-layer failover.
                max_fetched_pages: 0,
                ..WebSearchConfig::default()
            },
        );
        let evidence = collector.collect(
            "crm_record_drafts",
            "weak company",
            &["Example".to_string()],
        );
        assert_eq!(evidence.results.len(), 1);
        assert_eq!(evidence.results[0].url, "https://example.com/Example");
        assert!(evidence
            .failures
            .iter()
            .any(|f| f.starts_with("primary_search_failed:")));
        assert_eq!(evidence.failures.len(), 1);
    }

    #[test]
    fn no_fallback_endpoint_records_primary_failure_only() {
        let collector = WebSearchCollector::new(
            Arc::new(FailoverSearch),
            Arc::new(ScriptHttp {
                pages: BTreeMap::new(),
            }),
            Arc::new(PublicResolver),
            WebSearchConfig {
                enabled: true,
                provider: Some(WebSearchProvider::Tavily),
                api_key: Some("tvly-key".to_string()),
                ..WebSearchConfig::default()
            },
        );
        let evidence = collector.collect(
            "crm_record_drafts",
            "weak company",
            &["Example".to_string()],
        );
        assert!(evidence.results.is_empty());
        assert!(evidence
            .failures
            .iter()
            .any(|f| f.starts_with("search_failed:Example:")));
    }

    #[test]
    fn parses_searxng_content_as_snippet() {
        let value = serde_json::json!({
            "query": "Example Company",
            "results": [
                {
                    "url": "https://example.com/r",
                    "title": "Example Company",
                    "content": "Waterfront vacation rentals.",
                    "engine": "google"
                }
            ]
        });
        let results = parse_search_results("Example", &value);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/r");
        assert_eq!(results[0].snippet, "Waterfront vacation rentals.");
    }

    #[test]
    fn provider_from_name_maps_searxng_to_generic() {
        assert_eq!(
            WebSearchProvider::from_name(Some("searxng")),
            Some(WebSearchProvider::Generic)
        );
        assert_eq!(
            WebSearchProvider::from_name(Some("SearXNG")),
            Some(WebSearchProvider::Generic)
        );
        assert_eq!(
            WebSearchProvider::from_name(Some("tavily")),
            Some(WebSearchProvider::Tavily)
        );
        assert_eq!(WebSearchProvider::from_name(Some("nope")), None);
    }
}
