//! Google Search Console read-only connector. Config and OAuth tokens are
//! provided by bos-app; this module never reads env vars.

use serde_json::{json, Value};
use std::time::Duration;

pub const GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE: &str =
    "https://www.googleapis.com/auth/webmasters.readonly";

const SEARCH_ANALYTICS_URL_PREFIX: &str =
    "https://searchconsole.googleapis.com/webmasters/v3/sites/";
const SITES_URL: &str = "https://searchconsole.googleapis.com/webmasters/v3/sites";
const HTTP_TIMEOUT_SECS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum SearchConsoleError {
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    AuthRejected {
        message: String,
    },
    Permanent {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for SearchConsoleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthRejected { message } => write!(formatter, "auth_rejected: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchConsoleMetrics {
    pub clicks: i64,
    pub impressions: i64,
    pub ctr: f64,
    pub position: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchConsoleRow {
    pub keys: Vec<String>,
    pub metrics: SearchConsoleMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConsoleSite {
    pub site_url: String,
    pub permission_level: String,
}

pub trait SearchConsoleClient: Send + Sync {
    fn list_sites(&self, access_token: &str) -> Result<Vec<SearchConsoleSite>, SearchConsoleError>;

    fn query(
        &self,
        access_token: &str,
        property_url: &str,
        start_date: &str,
        end_date: &str,
        dimensions: &[&str],
        row_limit: u32,
    ) -> Result<Vec<SearchConsoleRow>, SearchConsoleError>;
}

#[derive(Debug, Clone)]
pub struct LiveSearchConsoleClient {
    client: reqwest::blocking::Client,
}

impl Default for LiveSearchConsoleClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl SearchConsoleClient for LiveSearchConsoleClient {
    fn list_sites(&self, access_token: &str) -> Result<Vec<SearchConsoleSite>, SearchConsoleError> {
        let response = self
            .client
            .get(SITES_URL)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| SearchConsoleError::Permanent {
                code: "search_console_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let value = json_response(response)?;
        Ok(parse_sites(&value))
    }

    fn query(
        &self,
        access_token: &str,
        property_url: &str,
        start_date: &str,
        end_date: &str,
        dimensions: &[&str],
        row_limit: u32,
    ) -> Result<Vec<SearchConsoleRow>, SearchConsoleError> {
        let encoded_site =
            percent_encoding::utf8_percent_encode(property_url, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let url = format!("{SEARCH_ANALYTICS_URL_PREFIX}{encoded_site}/searchAnalytics/query");
        let body = json!({
            "startDate": start_date,
            "endDate": end_date,
            "dimensions": dimensions,
            "rowLimit": row_limit,
            "dataState": "final",
        });
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .map_err(|err| SearchConsoleError::Permanent {
                code: "search_console_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let value = json_response(response)?;
        Ok(parse_rows(&value))
    }
}

fn json_response(response: reqwest::blocking::Response) -> Result<Value, SearchConsoleError> {
    let status = response.status().as_u16();
    if status == 429 {
        let retry_after_ms = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|secs| secs * 1000);
        return Err(SearchConsoleError::RateLimited {
            retry_after_ms,
            message: "search console returned 429".to_string(),
        });
    }
    if status == 401 || status == 403 {
        return Err(SearchConsoleError::AuthRejected {
            message: format!("search console returned {status}"),
        });
    }
    if status >= 400 {
        return Err(SearchConsoleError::Permanent {
            code: "search_console_http_status".to_string(),
            message: format!("search console returned {status}"),
        });
    }
    response
        .json::<Value>()
        .map_err(|err| SearchConsoleError::Permanent {
            code: "search_console_json_parse_failed".to_string(),
            message: err.to_string(),
        })
}

pub fn parse_sites(value: &Value) -> Vec<SearchConsoleSite> {
    value
        .get("siteEntry")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|site| {
            Some(SearchConsoleSite {
                site_url: site.get("siteUrl")?.as_str()?.to_string(),
                permission_level: site
                    .get("permissionLevel")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            })
        })
        .collect()
}

pub fn parse_rows(value: &Value) -> Vec<SearchConsoleRow> {
    value
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|row| SearchConsoleRow {
            keys: row
                .get("keys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            metrics: SearchConsoleMetrics {
                clicks: number_to_i64(row.get("clicks")),
                impressions: number_to_i64(row.get("impressions")),
                ctr: row.get("ctr").and_then(Value::as_f64).unwrap_or(0.0),
                position: row.get("position").and_then(Value::as_f64).unwrap_or(0.0),
            },
        })
        .collect()
}

fn number_to_i64(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_i64)
        .or_else(|| value.and_then(Value::as_f64).map(|n| n.round() as i64))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sites_list_response() {
        let sites = parse_sites(&json!({
            "siteEntry": [
                {"siteUrl": "sc-domain:example.com", "permissionLevel": "siteOwner"},
                {"siteUrl": "https://www.example.com/", "permissionLevel": "siteFullUser"}
            ]
        }));
        assert_eq!(
            sites,
            vec![
                SearchConsoleSite {
                    site_url: "sc-domain:example.com".to_string(),
                    permission_level: "siteOwner".to_string(),
                },
                SearchConsoleSite {
                    site_url: "https://www.example.com/".to_string(),
                    permission_level: "siteFullUser".to_string(),
                }
            ]
        );
    }
}
