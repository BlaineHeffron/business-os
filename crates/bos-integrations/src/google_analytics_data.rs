//! Google Analytics Data API read-only connector. Config and OAuth tokens are
//! provided by bos-app; this module never reads env vars.

use serde_json::{json, Value};
use std::time::Duration;

pub const GOOGLE_ANALYTICS_READONLY_SCOPE: &str =
    "https://www.googleapis.com/auth/analytics.readonly";

const ANALYTICS_DATA_URL_PREFIX: &str = "https://analyticsdata.googleapis.com/v1beta/properties/";
const HTTP_TIMEOUT_SECS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum AnalyticsDataError {
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

impl std::fmt::Display for AnalyticsDataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthRejected { message } => write!(formatter, "auth_rejected: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalyticsMetrics {
    pub sessions: i64,
    pub total_users: i64,
    pub event_count: i64,
    pub conversions: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticsRow {
    pub keys: Vec<String>,
    pub metrics: AnalyticsMetrics,
}

pub trait AnalyticsDataClient: Send + Sync {
    fn run_report(
        &self,
        access_token: &str,
        property_id: &str,
        start_date: &str,
        end_date: &str,
        dimensions: &[&str],
        limit: u32,
    ) -> Result<Vec<AnalyticsRow>, AnalyticsDataError>;
}

#[derive(Debug, Clone)]
pub struct LiveAnalyticsDataClient {
    client: reqwest::blocking::Client,
}

impl Default for LiveAnalyticsDataClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl AnalyticsDataClient for LiveAnalyticsDataClient {
    fn run_report(
        &self,
        access_token: &str,
        property_id: &str,
        start_date: &str,
        end_date: &str,
        dimensions: &[&str],
        limit: u32,
    ) -> Result<Vec<AnalyticsRow>, AnalyticsDataError> {
        let url = format!("{ANALYTICS_DATA_URL_PREFIX}{property_id}:runReport");
        let body = json!({
            "dateRanges": [{"startDate": start_date, "endDate": end_date}],
            "dimensions": dimensions
                .iter()
                .map(|name| json!({"name": name}))
                .collect::<Vec<_>>(),
            "metrics": [
                {"name": "sessions"},
                {"name": "totalUsers"},
                {"name": "eventCount"},
                {"name": "conversions"}
            ],
            "limit": limit.to_string(),
        });
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .map_err(|err| AnalyticsDataError::Permanent {
                code: "analytics_network_error".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status();
        let retry_after_ms = crate::google_api_errors::retry_after_ms(response.headers());
        let payload: Value = response
            .json()
            .map_err(|err| AnalyticsDataError::Permanent {
                code: "analytics_response_parse_error".to_string(),
                message: err.to_string(),
            })?;
        if status.is_success() {
            return parse_rows(&payload);
        }
        let message = crate::google_api_errors::error_message(&payload)
            .unwrap_or("google analytics returned an error")
            .to_string();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(AnalyticsDataError::AuthRejected { message });
        }
        if status.as_u16() == 429 || crate::google_api_errors::has_retryable_quota_reason(&payload)
        {
            return Err(AnalyticsDataError::RateLimited {
                retry_after_ms,
                message,
            });
        }
        Err(AnalyticsDataError::Permanent {
            code: crate::google_api_errors::first_error_reason(&payload)
                .unwrap_or_else(|| format!("analytics_http_{}", status.as_u16())),
            message,
        })
    }
}

fn parse_rows(payload: &Value) -> Result<Vec<AnalyticsRow>, AnalyticsDataError> {
    let rows = payload
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let keys = row
                .get("dimensionValues")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.get("value").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let metric_values = row
                .get("metricValues")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(AnalyticsRow {
                keys,
                metrics: AnalyticsMetrics {
                    sessions: metric_at(&metric_values, 0),
                    total_users: metric_at(&metric_values, 1),
                    event_count: metric_at(&metric_values, 2),
                    conversions: metric_at(&metric_values, 3),
                },
            })
        })
        .collect()
}

fn metric_at(values: &[Value], index: usize) -> i64 {
    values
        .get(index)
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value.round() as i64)
        .unwrap_or(0)
}
