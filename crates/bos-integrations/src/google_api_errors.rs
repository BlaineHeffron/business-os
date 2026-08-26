use reqwest::header::HeaderMap;
use serde_json::Value;

const MAX_RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

pub fn retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_secs)
        .map(|secs| secs.saturating_mul(1000).min(MAX_RETRY_AFTER_MS))
}

fn parse_retry_after_secs(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(trimmed.bytes().fold(0_u64, |acc, byte| {
        acc.saturating_mul(10)
            .saturating_add(u64::from(byte - b'0'))
    }))
}

pub fn error_reasons(body: &Value) -> Vec<String> {
    body.get("error")
        .and_then(|error| error.get("errors"))
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("reason").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn first_error_reason(body: &Value) -> Option<String> {
    error_reasons(body).into_iter().next()
}

pub fn is_retryable_quota_reason(reason: &str) -> bool {
    matches!(
        reason,
        "rateLimitExceeded"
            | "userRateLimitExceeded"
            | "quotaExceeded"
            | "dailyLimitExceeded"
            | "limitExceeded"
            | "servingLimitExceeded"
            | "concurrentLimitExceeded"
    )
}

pub fn has_retryable_quota_reason(body: &Value) -> bool {
    error_reasons(body)
        .iter()
        .any(|reason| is_retryable_quota_reason(reason))
}

pub fn error_message(body: &Value) -> Option<&str> {
    body.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .filter(|message| !message.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn retryable_quota_reasons_cover_google_workspace_limit_shapes() {
        for reason in [
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "quotaExceeded",
            "dailyLimitExceeded",
            "limitExceeded",
            "servingLimitExceeded",
            "concurrentLimitExceeded",
        ] {
            let body = serde_json::json!({
                "error": {
                    "errors": [{"domain": "usageLimits", "reason": reason}],
                    "message": "quota"
                }
            });
            assert!(
                has_retryable_quota_reason(&body),
                "{reason} should be retryable"
            );
        }

        let body = serde_json::json!({
            "error": {
                "errors": [{"domain": "global", "reason": "domainPolicy"}],
                "message": "blocked"
            }
        });
        assert!(!has_retryable_quota_reason(&body));
    }

    #[test]
    fn retry_after_ms_saturates_and_clamps() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            HeaderValue::from_static("99999999999999999999"),
        );
        assert_eq!(retry_after_ms(&headers), Some(3_600_000));

        headers.insert("retry-after", HeaderValue::from_static("12"));
        assert_eq!(retry_after_ms(&headers), Some(12_000));
    }
}
