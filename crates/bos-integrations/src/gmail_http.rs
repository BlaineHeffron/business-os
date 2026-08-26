//! Minimal blocking HTTP surface for Gmail JSON calls, with bounded timeouts.

use crate::gmail_inbox_read::gmail_inbox_read_error;
use crate::google_api_errors;
use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};
use serde_json::Value;
use std::time::Duration;

/// Overall per-request timeout for Gmail HTTP calls.
const GMAIL_HTTP_TIMEOUT_SECS: u64 = 20;
/// Connection-establishment timeout (a hung TCP connect must not pin a worker).
const GMAIL_HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Minimal HTTP surface for Gmail JSON calls.
pub trait GmailHttp: Send + Sync {
    fn get_json(&self, url: &str, access_token: &str) -> AppResult<Value>;
    fn post_json(&self, url: &str, access_token: &str, body: &Value) -> AppResult<Value>;
    fn post_json_with_meta(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<GmailJsonResponse, GmailHttpFailure> {
        self.post_json(url, access_token, body)
            .map(|body| GmailJsonResponse {
                body,
                retry_after_ms: None,
            })
            .map_err(|error| GmailHttpFailure {
                error,
                retry_after_ms: None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailJsonResponse {
    pub body: Value,
    pub retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailHttpFailure {
    pub error: AppError,
    pub retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ReqwestGmailHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestGmailHttpClient {
    fn default() -> Self {
        // A missing timeout lets a hung Gmail endpoint pin the calling blocking
        // worker thread indefinitely. Always bound both connect and total time.
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(GMAIL_HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(GMAIL_HTTP_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl ReqwestGmailHttpClient {
    pub fn new(client: reqwest::blocking::Client) -> Self {
        Self { client }
    }
}

impl GmailHttp for ReqwestGmailHttpClient {
    fn get_json(&self, url: &str, access_token: &str) -> AppResult<Value> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| gmail_inbox_read_error("gmail_http_send_failed", err))?;
        let status = response.status().as_u16();
        if status >= 400 {
            let retry_after_ms = google_api_errors::retry_after_ms(response.headers());
            let body = response.json::<Value>().unwrap_or(Value::Null);
            if (status == 403 && google_api_errors::has_retryable_quota_reason(&body))
                || status == 429
            {
                return Err(gmail_inbox_read_error(
                    "gmail_http_unavailable",
                    google_status_message(status, retry_after_ms, &body),
                ));
            }
            if status == 401 || status == 403 {
                return Err(gmail_inbox_read_error(
                    "gmail_http_unauthorized",
                    google_status_message(status, retry_after_ms, &body),
                ));
            }
            if status >= 500 {
                return Err(gmail_inbox_read_error(
                    "gmail_http_unavailable",
                    google_status_message(status, retry_after_ms, &body),
                ));
            }
            return Err(gmail_inbox_read_error(
                "gmail_http_client_error",
                google_status_message(status, retry_after_ms, &body),
            ));
        }
        response
            .json::<Value>()
            .map_err(|err| gmail_inbox_read_error("gmail_http_parse_failed", err))
    }

    fn post_json(&self, url: &str, access_token: &str, body: &Value) -> AppResult<Value> {
        self.post_json_with_meta(url, access_token, body)
            .map(|response| response.body)
            .map_err(|failure| failure.error)
    }

    fn post_json_with_meta(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<GmailJsonResponse, GmailHttpFailure> {
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .map_err(|err| GmailHttpFailure {
                error: post_http_error(
                    ErrorCode::ExternalDependency,
                    "gmail_http_post_send_failed",
                    err,
                ),
                retry_after_ms: None,
            })?;
        let status = response.status().as_u16();
        if status >= 400 {
            let retry_after_ms = google_api_errors::retry_after_ms(response.headers());
            let body = response.json::<Value>().unwrap_or(Value::Null);
            if (status == 403 && google_api_errors::has_retryable_quota_reason(&body))
                || status == 429
                || status >= 500
            {
                return Err(GmailHttpFailure {
                    error: post_http_error(
                        ErrorCode::ExternalDependency,
                        "gmail_http_post_unavailable",
                        google_status_message(status, retry_after_ms, &body),
                    ),
                    retry_after_ms,
                });
            }
            if status == 401 || status == 403 {
                return Err(GmailHttpFailure {
                    error: post_http_error(
                        ErrorCode::Unauthorized,
                        "gmail_http_post_unauthorized",
                        google_status_message(status, retry_after_ms, &body),
                    ),
                    retry_after_ms: None,
                });
            }
            return Err(GmailHttpFailure {
                error: post_http_error(
                    ErrorCode::InvalidState,
                    "gmail_http_post_client_error",
                    google_status_message(status, retry_after_ms, &body),
                ),
                retry_after_ms: None,
            });
        }
        response
            .json::<Value>()
            .map(|body| GmailJsonResponse {
                body,
                retry_after_ms: None,
            })
            .map_err(|err| GmailHttpFailure {
                error: post_http_error(
                    ErrorCode::InvalidState,
                    "gmail_http_post_parse_failed",
                    err,
                ),
                retry_after_ms: None,
            })
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FakeGmailHttp {
    responses: std::sync::Mutex<std::collections::VecDeque<Value>>,
    pub calls: std::sync::Mutex<Vec<String>>,
    pub posts: std::sync::Mutex<Vec<(String, Value)>>,
}

#[cfg(test)]
impl FakeGmailHttp {
    pub fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
            posts: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
impl GmailHttp for FakeGmailHttp {
    fn get_json(&self, url: &str, _access_token: &str) -> AppResult<Value> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
        self.responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                gmail_inbox_read_error("fake_gmail_http_exhausted", "no queued response")
            })
    }

    fn post_json(&self, url: &str, _access_token: &str, body: &Value) -> AppResult<Value> {
        self.posts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((url.to_string(), body.clone()));
        self.responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                gmail_inbox_read_error("fake_gmail_http_exhausted", "no queued response")
            })
    }
}

fn post_http_error(
    kind: ErrorCode,
    code: &'static str,
    message: impl std::fmt::Display,
) -> AppError {
    AppError::new(
        kind,
        code,
        message.to_string(),
        CorrelationId::new("corr_gmail_http_post"),
    )
}

fn google_status_message(status: u16, retry_after_ms: Option<u64>, body: &Value) -> String {
    let reason = google_api_errors::first_error_reason(body)
        .map(|reason| format!(" reason={reason}"))
        .unwrap_or_default();
    let retry_after = retry_after_ms
        .map(|ms| format!(" retry_after_ms={ms}"))
        .unwrap_or_default();
    let message = google_api_errors::error_message(body)
        .map(|message| format!(" message={message}"))
        .unwrap_or_default();
    format!("status {status}{reason}{retry_after}{message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_gmail_http_records_post_and_returns_response() -> Result<(), String> {
        let http = FakeGmailHttp::new(vec![
            serde_json::json!({"id":"draft-1","message":{"threadId":"t1"}}),
        ]);
        let body = serde_json::json!({"message":{"raw":"abc","threadId":"t1"}});
        let resp = http
            .post_json("https://gmail.example/drafts", "tok", &body)
            .map_err(|e| e.to_string())?;
        assert_eq!(resp["id"], "draft-1");
        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "https://gmail.example/drafts");
        assert_eq!(posts[0].1["message"]["threadId"], "t1");
        Ok(())
    }

    // A hung upstream (accepts the connection, never replies) must surface as a
    // timeout error within the bound rather than pinning the worker forever.
    // Uses a short-timeout client through the same send path as the timeout-
    // configured `Default`; without a timeout this request would never return.
    #[test]
    fn reqwest_client_times_out_against_hung_upstream() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // Accept and then stall, never sending an HTTP response.
        let _accepter = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 64];
                let _ = stream.read(&mut buf);
                std::thread::sleep(Duration::from_secs(5));
            }
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(250))
            .connect_timeout(Duration::from_millis(250))
            .build()
            .expect("build client");
        let http = ReqwestGmailHttpClient::new(client);

        let started = Instant::now();
        let result = http.get_json(&format!("http://{addr}/v1/x"), "tok");
        let elapsed = started.elapsed();

        assert!(result.is_err(), "hung upstream must yield an error");
        assert!(
            elapsed < Duration::from_secs(3),
            "request must abort on timeout, took {elapsed:?}"
        );
    }

    fn one_response_server(status: u16, headers: &[(&str, &str)], body: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let headers = headers
            .iter()
            .map(|(key, value)| format!("{key}: {value}\r\n"))
            .collect::<String>();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                let reason = if status == 403 {
                    "Forbidden"
                } else {
                    "Too Many Requests"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/gmail")
    }

    #[test]
    fn quota_403_maps_to_retryable_get_error() {
        let url = one_response_server(
            403,
            &[],
            r#"{"error":{"errors":[{"domain":"usageLimits","reason":"dailyLimitExceeded","message":"Daily Limit Exceeded"}],"code":403,"message":"Daily Limit Exceeded"}}"#,
        );
        let http = ReqwestGmailHttpClient::default();
        let err = http.get_json(&url, "tok").expect_err("quota 403");

        assert_eq!(err.code(), "gmail_http_unavailable");
        assert!(err.message().contains("reason=dailyLimitExceeded"));
    }

    #[test]
    fn non_quota_403_stays_unauthorized() {
        let url = one_response_server(
            403,
            &[],
            r#"{"error":{"errors":[{"domain":"global","reason":"domainPolicy","message":"blocked"}],"code":403,"message":"blocked"}}"#,
        );
        let http = ReqwestGmailHttpClient::default();
        let err = http.get_json(&url, "tok").expect_err("policy 403");

        assert_eq!(err.code(), "gmail_http_unauthorized");
        assert!(err.message().contains("reason=domainPolicy"));
    }

    #[test]
    fn retry_after_is_preserved_in_post_error_message() {
        let url = one_response_server(
            429,
            &[("Retry-After", "7")],
            r#"{"error":{"errors":[{"domain":"usageLimits","reason":"rateLimitExceeded","message":"Rate Limit Exceeded"}],"code":429,"message":"Rate Limit Exceeded"}}"#,
        );
        let http = ReqwestGmailHttpClient::default();
        let err = http
            .post_json(&url, "tok", &serde_json::json!({}))
            .expect_err("429");

        assert_eq!(err.code(), "gmail_http_post_unavailable");
        assert!(err.message().contains("retry_after_ms=7000"));
    }
}
