//! Live Google Calendar event-create client (create only) + handwritten reqwest
//! transport + deterministic event-id derivation + write-gated factory.
//! This is a narrow handwritten adapter, not the official Google SDK.

use super::{
    DryRunGoogleCalendarClient, GoogleCalendarEventWriteOperation, GoogleCalendarEventWriteRequest,
    GoogleCalendarEventWriteResponse, GoogleCalendarExecutionClient, GoogleCalendarExecutionStatus,
    GoogleCalendarWriteConfig, GoogleCalendarWriteError,
};
use crate::google_api_errors;
use crate::google_oauth::{fetch_access_token, has_scope, GoogleOAuthConfig};
use bos_kernel::RetryClass;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

const CALENDAR_EVENTS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";
const CALENDAR_FULL_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
const CALENDAR_EVENTS_URL_BASE: &str = "https://www.googleapis.com/calendar/v3/calendars";
const CALENDAR_HTTP_TIMEOUT_SECS: u64 = 20;
const URL_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub fn has_calendar_scope(creds: &GoogleOAuthConfig) -> bool {
    has_scope(creds, CALENDAR_EVENTS_SCOPE) || has_scope(creds, CALENDAR_FULL_SCOPE)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarHttpResponse {
    pub status: u16,
    pub body: Value,
    pub retry_after_ms: Option<i64>,
}

/// Narrow HTTP POST surface for Calendar writes. Returns the completed response
/// (any status) so the client can map 409 -> Ok; only network failure is an Err.
pub trait CalendarHttp: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError>;

    /// Read-only GET (calendar list). Default errors so write-only test
    /// transports keep compiling; the live transport overrides.
    fn get_json(
        &self,
        _url: &str,
        _access_token: &str,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError> {
        Err(GoogleCalendarWriteError::Permanent {
            code: "google_calendar_http_get_unsupported".to_string(),
            message: "transport does not implement GET".to_string(),
        })
    }
}

fn calendar_retryable(
    code: &str,
    message: impl std::fmt::Display,
    retry_after_ms: Option<i64>,
) -> GoogleCalendarWriteError {
    GoogleCalendarWriteError::Retryable {
        code: code.to_string(),
        message: message.to_string(),
        retry_after_ms,
    }
}

fn calendar_permanent(code: &str, message: impl std::fmt::Display) -> GoogleCalendarWriteError {
    GoogleCalendarWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn calendar_status_message(status: u16, body: &Value) -> String {
    let reason = google_api_errors::first_error_reason(body)
        .map(|reason| format!(" reason={reason}"))
        .unwrap_or_default();
    let message = google_api_errors::error_message(body)
        .map(|message| format!(" message={message}"))
        .unwrap_or_default();
    format!("status {status}{reason}{message}")
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, URL_COMPONENT_ENCODE_SET).to_string()
}

#[derive(Debug, Clone)]
pub struct ReqwestCalendarHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestCalendarHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(CALENDAR_HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl ReqwestCalendarHttpClient {
    pub fn new(client: reqwest::blocking::Client) -> Self {
        Self { client }
    }
}

impl CalendarHttp for ReqwestCalendarHttpClient {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .map_err(|err| calendar_retryable("google_calendar_http_send_failed", err, None))?;
        let status = response.status().as_u16();
        let retry_after_ms = google_api_errors::retry_after_ms(response.headers())
            .and_then(|ms| i64::try_from(ms).ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(CalendarHttpResponse {
            status,
            body,
            retry_after_ms,
        })
    }

    fn get_json(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| calendar_retryable("google_calendar_http_send_failed", err, None))?;
        let status = response.status().as_u16();
        let retry_after_ms = google_api_errors::retry_after_ms(response.headers())
            .and_then(|ms| i64::try_from(ms).ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(CalendarHttpResponse {
            status,
            body,
            retry_after_ms,
        })
    }
}

const BASE32HEX: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";

/// Deterministic Google-valid event id from an idempotency key.
/// Google event ids: chars in base32hex (0-9a-v), length 5..=1024, lowercase.
/// We SHA-256 the key and base32hex-encode the first 20 bytes (32 chars).
pub fn derive_event_id(idempotency_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(idempotency_key.as_bytes());
    let bytes = &digest[..20];
    let mut out = String::with_capacity(32);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(BASE32HEX[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(BASE32HEX[idx] as char);
    }
    out
}

/// One calendar the connected account can write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarListEntry {
    pub id: String,
    pub summary: String,
    pub primary: bool,
}

pub struct LiveGoogleCalendarClient {
    http: Arc<dyn CalendarHttp>,
    access_token: String,
}

impl LiveGoogleCalendarClient {
    /// Exchanges the refresh token for an access token (one network call to the
    /// token endpoint). Fails without network when a populated scope list lacks
    /// a calendar write-capable scope (empty list = unverifiable, proceed —
    /// matches the Gmail client posture).
    pub fn from_credentials(
        http: Arc<dyn CalendarHttp>,
        creds: &GoogleOAuthConfig,
    ) -> Result<Self, GoogleCalendarWriteError> {
        if !creds.scopes.is_empty() && !has_calendar_scope(creds) {
            return Err(calendar_permanent(
                "google_calendar_scope_missing",
                "calendar.events (or calendar) scope absent from resolved credentials",
            ));
        }
        let access_token = fetch_access_token(creds).map_err(|err| match err.retry() {
            RetryClass::Never => calendar_permanent("google_calendar_token_failed", err),
            _ => calendar_retryable("google_calendar_token_failed", err, None),
        })?;
        Ok(Self { http, access_token })
    }

    #[cfg(test)]
    pub fn for_test(http: Arc<dyn CalendarHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }

    /// Calendars the connected account can write to, for the operator's
    /// "which calendar does this event go on" picker.
    pub fn list_writable_calendars(
        &self,
    ) -> Result<Vec<CalendarListEntry>, GoogleCalendarWriteError> {
        let url = "https://www.googleapis.com/calendar/v3/users/me/calendarList\
                   ?showHidden=true&maxResults=250&fields=items(id,summary,primary,accessRole)";
        let response = self.http.get_json(url, &self.access_token)?;
        match response.status {
            200 => {
                let entries = response
                    .body
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                let access_role =
                                    item.get("accessRole").and_then(Value::as_str).unwrap_or("");
                                if !matches!(
                                    access_role,
                                    "owner" | "writer" | "writerWithoutPrivateAccess"
                                ) {
                                    return None;
                                }
                                Some(CalendarListEntry {
                                    id: item.get("id")?.as_str()?.to_string(),
                                    summary: item
                                        .get("summary")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                    primary: item
                                        .get("primary")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(entries)
            }
            403 if google_api_errors::has_retryable_quota_reason(&response.body) => {
                Err(calendar_retryable(
                    "google_calendar_rate_or_server",
                    calendar_status_message(response.status, &response.body),
                    response.retry_after_ms,
                ))
            }
            401 | 403 => Err(calendar_permanent(
                "google_calendar_auth_failed",
                calendar_status_message(response.status, &response.body),
            )),
            429 | 500..=599 => Err(calendar_retryable(
                "google_calendar_rate_or_server",
                calendar_status_message(response.status, &response.body),
                response.retry_after_ms,
            )),
            other => Err(calendar_permanent(
                "google_calendar_list_rejected",
                format!("status {other}: {}", response.body),
            )),
        }
    }

    fn build_body(&self, request: &GoogleCalendarEventWriteRequest, event_id: &str) -> Value {
        let mut start = serde_json::json!({ "dateTime": request.start_at });
        let mut end = serde_json::json!({ "dateTime": request.end_at });
        if let Some(tz) = request.timezone.as_ref() {
            start["timeZone"] = Value::String(tz.clone());
            end["timeZone"] = Value::String(tz.clone());
        }
        let attendees: Vec<Value> = request
            .attendees
            .iter()
            .map(|email| serde_json::json!({ "email": email }))
            .collect();
        let mut body = serde_json::json!({
            "id": event_id,
            "summary": request.summary,
            "start": start,
            "end": end,
            "attendees": attendees,
        });
        if let Some(desc) = request.description.as_ref() {
            body["description"] = Value::String(desc.clone());
        }
        body
    }
}

impl GoogleCalendarExecutionClient for LiveGoogleCalendarClient {
    fn write_event(
        &self,
        request: &GoogleCalendarEventWriteRequest,
    ) -> Result<GoogleCalendarEventWriteResponse, GoogleCalendarWriteError> {
        super::validate_calendar_write_request(request)?;
        if !matches!(request.operation, GoogleCalendarEventWriteOperation::Create) {
            return Err(calendar_permanent(
                "google_calendar_operation_not_enabled",
                "only event create is enabled in this slice",
            ));
        }
        if !request.approval.is_complete() {
            return Err(calendar_permanent(
                "google_calendar_approval_missing",
                "google calendar write approval metadata is incomplete",
            ));
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(calendar_permanent(
                "google_calendar_idempotency_key_missing",
                "google calendar write idempotency key is required",
            ));
        }
        let event_id = request
            .event_id
            .clone()
            .unwrap_or_else(|| derive_event_id(&request.idempotency_key));
        let calendar_id = encode_path_segment(&request.calendar_id);
        let send_updates = if request.send_invitations {
            "all"
        } else {
            "none"
        };
        let url =
            format!("{CALENDAR_EVENTS_URL_BASE}/{calendar_id}/events?sendUpdates={send_updates}");
        let body = self.build_body(request, &event_id);
        let response = self.http.post_json(&url, &self.access_token, &body)?;

        match response.status {
            200..=299 => {
                let returned_id = response
                    .body
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        calendar_permanent(
                            "google_calendar_missing_event_id",
                            "events.insert response missing id",
                        )
                    })?;
                let etag = response
                    .body
                    .get("etag")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(GoogleCalendarEventWriteResponse {
                    status: GoogleCalendarExecutionStatus::executed("event_created"),
                    calendar_id: request.calendar_id.clone(),
                    event_id: returned_id,
                    etag,
                    revision: None,
                    approval: request.approval.clone(),
                })
            }
            409 => Ok(GoogleCalendarEventWriteResponse {
                status: GoogleCalendarExecutionStatus::executed("already_exists"),
                calendar_id: request.calendar_id.clone(),
                event_id,
                etag: String::new(),
                revision: None,
                approval: request.approval.clone(),
            }),
            403 if google_api_errors::has_retryable_quota_reason(&response.body) => {
                Err(calendar_retryable(
                    "google_calendar_event_create_unavailable",
                    calendar_status_message(response.status, &response.body),
                    response.retry_after_ms,
                ))
            }
            429 | 500..=599 => Err(calendar_retryable(
                "google_calendar_event_create_unavailable",
                calendar_status_message(response.status, &response.body),
                response.retry_after_ms,
            )),
            other => Err(calendar_permanent(
                "google_calendar_event_create_failed",
                format!("google calendar rejected the event (status {other})"),
            )),
        }
    }
}

struct FailedGoogleCalendarClient {
    error: GoogleCalendarWriteError,
}

impl GoogleCalendarExecutionClient for FailedGoogleCalendarClient {
    fn write_event(
        &self,
        _request: &GoogleCalendarEventWriteRequest,
    ) -> Result<GoogleCalendarEventWriteResponse, GoogleCalendarWriteError> {
        Err(self.error.clone())
    }
}

/// Factory: returns [`LiveGoogleCalendarClient`] only when `config.write_enabled`
/// AND the supplied credentials carry a calendar scope. Any other path returns
/// [`DryRunGoogleCalendarClient`]. The caller (bos-app) resolves credentials and
/// the gate; this never reads env.
pub fn google_calendar_execution_client(
    config: &GoogleCalendarWriteConfig,
) -> Box<dyn GoogleCalendarExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunGoogleCalendarClient);
    }

    if !config.oauth.scopes.is_empty() && !has_calendar_scope(&config.oauth) {
        return Box::new(FailedGoogleCalendarClient {
            error: calendar_permanent(
                "google_calendar_scope_missing",
                "calendar.events (or calendar) scope absent from resolved credentials",
            ),
        });
    }

    let http = Arc::new(ReqwestCalendarHttpClient::default());
    match LiveGoogleCalendarClient::from_credentials(http, &config.oauth) {
        Ok(client) => Box::new(client),
        Err(err) => {
            let (code, reason) = match &err {
                GoogleCalendarWriteError::Permanent { code, .. } => (code.as_str(), "permanent"),
                GoogleCalendarWriteError::Retryable { code, .. } => (code.as_str(), "retryable"),
                GoogleCalendarWriteError::Conflict { code, .. } => (code.as_str(), "conflict"),
            };
            tracing::warn!("google_calendar_factory: live_client_error code={code} class={reason}");
            Box::new(FailedGoogleCalendarClient { error: err })
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FakeCalendarHttp {
    responses: std::sync::Mutex<std::collections::VecDeque<(u16, Value)>>,
    pub posts: std::sync::Mutex<Vec<(String, Value)>>,
}

#[cfg(test)]
impl FakeCalendarHttp {
    pub fn new(responses: Vec<(u16, Value)>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
            posts: std::sync::Mutex::new(Vec::new()),
        }
    }
    pub fn post_count(&self) -> usize {
        self.posts.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
impl CalendarHttp for FakeCalendarHttp {
    fn post_json(
        &self,
        url: &str,
        _access_token: &str,
        body: &Value,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError> {
        self.posts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((url.to_string(), body.clone()));
        let (status, resp) = self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                calendar_retryable("fake_calendar_http_exhausted", "no queued response", None)
            })?;
        Ok(CalendarHttpResponse {
            status,
            body: resp,
            retry_after_ms: None,
        })
    }

    fn get_json(
        &self,
        url: &str,
        _access_token: &str,
    ) -> Result<CalendarHttpResponse, GoogleCalendarWriteError> {
        self.posts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((url.to_string(), Value::Null));
        let (status, resp) = self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                calendar_retryable("fake_calendar_http_exhausted", "no queued response", None)
            })?;
        Ok(CalendarHttpResponse {
            status,
            body: resp,
            retry_after_ms: None,
        })
    }
}

#[cfg(test)]
mod calendar_list_tests {
    use super::*;

    #[test]
    fn list_writable_calendars_parses_entries_and_filters_write_roles() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            200,
            serde_json::json!({
                "items": [
                    {
                        "id": "bauser@gmail.com",
                        "summary": "bauser@gmail.com",
                        "primary": true,
                        "accessRole": "owner"
                    },
                    {
                        "id": "abc123@group.calendar.google.com",
                        "summary": "external-ops",
                        "accessRole": "writerWithoutPrivateAccess"
                    },
                    {
                        "id": "readonly@group.calendar.google.com",
                        "summary": "Readonly",
                        "accessRole": "reader"
                    }
                ]
            }),
        )]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".to_string());
        let calendars = client.list_writable_calendars().expect("list");
        assert_eq!(calendars.len(), 2);
        assert!(calendars[0].primary);
        assert_eq!(calendars[1].summary, "external-ops");
        assert_eq!(calendars[1].id, "abc123@group.calendar.google.com");
        let calls = http.posts.lock().unwrap_or_else(|e| e.into_inner());
        assert!(calls[0].0.contains("showHidden=true"));
        assert!(calls[0].0.contains("accessRole"));
    }

    #[test]
    fn list_writable_calendars_maps_auth_failure_permanent() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(403, Value::Null)]));
        let client = LiveGoogleCalendarClient::for_test(http, "tok".to_string());
        assert!(matches!(
            client.list_writable_calendars(),
            Err(GoogleCalendarWriteError::Permanent { .. })
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::GoogleCalendarApprovalMetadata;
    use super::*;

    #[test]
    fn derive_event_id_is_base32hex_and_deterministic() {
        let a = derive_event_id("idem-key-1");
        let b = derive_event_id("idem-key-1");
        let c = derive_event_id("idem-key-2");
        assert_eq!(a, b, "same key => same id");
        assert_ne!(a, c, "different key => different id");
        assert!(a.len() >= 5 && a.len() <= 1024);
        assert!(
            a.chars()
                .all(|ch| "0123456789abcdefghijklmnopqrstuv".contains(ch)),
            "must be base32hex lowercase per Google event-id rules"
        );
    }

    #[test]
    fn fake_calendar_http_records_post_and_returns_status_body() {
        let http =
            FakeCalendarHttp::new(vec![(200, serde_json::json!({"id":"evt-1","etag":"e1"}))]);
        let body = serde_json::json!({"id":"evt-1"});
        let resp = http
            .post_json("https://cal.example/events", "tok", &body)
            .expect("ok");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body["id"], "evt-1");
        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "https://cal.example/events");
    }

    fn write_req(op: GoogleCalendarEventWriteOperation) -> GoogleCalendarEventWriteRequest {
        GoogleCalendarEventWriteRequest {
            operation: op,
            calendar_id: "primary".into(),
            event_id: None,
            idempotency_key: "idem-1".into(),
            approval: GoogleCalendarApprovalMetadata {
                approval_id: "appr-1".into(),
                approved_by: "jordan".into(),
                approved_at: "2026-05-31T10:00:00Z".into(),
            },
            summary: "Follow up with customer".into(),
            description: Some("notes".into()),
            start_at: "2026-06-01T15:00:00Z".into(),
            end_at: "2026-06-01T15:30:00Z".into(),
            timezone: Some("America/New_York".into()),
            attendees: vec!["operator@example.test".into()],
            send_invitations: false,
            expected_etag: None,
            expected_revision: None,
        }
    }

    fn oauth_with_scopes(scopes: Vec<String>) -> GoogleOAuthConfig {
        GoogleOAuthConfig {
            refresh_token: "r".to_string(),
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            scopes,
            token_url: None,
        }
    }

    #[test]
    fn create_event_posts_no_notifications_and_returns_executed() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            200,
            serde_json::json!({"id":"evt-9","etag":"etag-9"}),
        )]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".into());
        let resp = client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .expect("ok");
        assert_eq!(resp.event_id, "evt-9");
        assert!(resp.status.executed && !resp.status.dry_run);
        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(posts.len(), 1);
        assert!(posts[0]
            .0
            .starts_with("https://www.googleapis.com/calendar/v3/calendars/primary/events"));
        assert!(posts[0].0.contains("sendUpdates=none"));
        assert_eq!(posts[0].1["id"], derive_event_id("idem-1"));
        assert_eq!(posts[0].1["start"]["dateTime"], "2026-06-01T15:00:00Z");
        assert_eq!(posts[0].1["attendees"][0]["email"], "operator@example.test");
    }

    #[test]
    fn create_event_sends_invitations_only_when_explicitly_requested() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            409,
            serde_json::json!({"error":{"message":"duplicate"}}),
        )]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".into());
        let mut request = write_req(GoogleCalendarEventWriteOperation::Create);
        request.send_invitations = true;
        let response = client
            .write_event(&request)
            .expect("409 is idempotent success");
        assert_eq!(response.status.reason.as_deref(), Some("already_exists"));
        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(posts.len(), 1);
        assert!(posts[0].0.contains("sendUpdates=all"));
    }

    #[test]
    fn create_event_percent_encodes_calendar_id_path_segment() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            200,
            serde_json::json!({"id":"evt-9","etag":"etag-9"}),
        )]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".into());
        let mut req = write_req(GoogleCalendarEventWriteOperation::Create);
        req.calendar_id = "team/calendar#ops@example.test".into();

        client.write_event(&req).expect("ok");

        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(posts[0]
            .0
            .contains("/team%2Fcalendar%23ops%40example.test/events"));
    }

    #[test]
    fn conflict_409_is_idempotent_success_not_error() {
        let http = Arc::new(FakeCalendarHttp::new(vec![(
            409,
            serde_json::json!({"error":{"message":"duplicate"}}),
        )]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".into());
        let resp = client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .expect("409 maps to Ok");
        assert_eq!(resp.event_id, derive_event_id("idem-1"));
        assert!(resp.status.executed);
        assert_eq!(resp.status.reason.as_deref(), Some("already_exists"));
    }

    #[test]
    fn update_operation_is_permanent_without_network() {
        let http = Arc::new(FakeCalendarHttp::new(vec![]));
        let client = LiveGoogleCalendarClient::for_test(http.clone(), "tok".into());
        let err = client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Update))
            .unwrap_err();
        assert!(matches!(err, GoogleCalendarWriteError::Permanent { .. }));
        assert_eq!(http.post_count(), 0);
    }

    #[test]
    fn missing_calendar_scope_is_permanent_without_network() {
        let http = Arc::new(FakeCalendarHttp::new(vec![]));
        let creds = oauth_with_scopes(vec![
            "https://www.googleapis.com/auth/gmail.readonly".to_string()
        ]);
        let result = LiveGoogleCalendarClient::from_credentials(http.clone(), &creds);
        assert!(matches!(
            result,
            Err(GoogleCalendarWriteError::Permanent { .. })
        ));
        assert_eq!(http.post_count(), 0);
    }

    #[test]
    fn server_error_is_retryable_client_error_is_permanent() {
        let http5 = Arc::new(FakeCalendarHttp::new(vec![(503, Value::Null)]));
        let c5 = LiveGoogleCalendarClient::for_test(http5, "tok".into());
        assert!(matches!(
            c5.write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
                .unwrap_err(),
            GoogleCalendarWriteError::Retryable { .. }
        ));
        let http4 = Arc::new(FakeCalendarHttp::new(vec![(
            400,
            serde_json::json!({"error":{"message":"bad"}}),
        )]));
        let c4 = LiveGoogleCalendarClient::for_test(http4, "tok".into());
        match c4
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .unwrap_err()
        {
            GoogleCalendarWriteError::Permanent { message, .. } => {
                assert!(message.contains("status 400"));
                assert!(!message.contains("bad"));
            }
            other => panic!("expected permanent error, got {other:?}"),
        }
    }

    #[test]
    fn quota_403_is_retryable_but_policy_403_is_permanent() {
        let quota = Arc::new(FakeCalendarHttp::new(vec![(
            403,
            serde_json::json!({
                "error": {
                    "errors": [{
                        "domain": "usageLimits",
                        "reason": "concurrentLimitExceeded",
                        "message": "Concurrent Limit Exceeded"
                    }],
                    "code": 403,
                    "message": "Concurrent Limit Exceeded"
                }
            }),
        )]));
        let quota_client = LiveGoogleCalendarClient::for_test(quota, "tok".into());
        match quota_client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .unwrap_err()
        {
            GoogleCalendarWriteError::Retryable { message, .. } => {
                assert!(message.contains("reason=concurrentLimitExceeded"));
            }
            other => panic!("expected retryable, got {other:?}"),
        }

        let policy = Arc::new(FakeCalendarHttp::new(vec![(
            403,
            serde_json::json!({
                "error": {
                    "errors": [{
                        "domain": "calendar",
                        "reason": "forbiddenForNonOrganizer",
                        "message": "forbidden"
                    }],
                    "code": 403,
                    "message": "forbidden"
                }
            }),
        )]));
        let policy_client = LiveGoogleCalendarClient::for_test(policy, "tok".into());
        assert!(matches!(
            policy_client
                .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
                .unwrap_err(),
            GoogleCalendarWriteError::Permanent { .. }
        ));
    }

    #[test]
    fn factory_defaults_to_dry_run_when_writes_disabled() {
        let config = GoogleCalendarWriteConfig::new(oauth_with_scopes(vec![
            CALENDAR_EVENTS_SCOPE.to_string()
        ]));
        assert!(!config.write_enabled, "constructor defaults writes off");
        assert_eq!(config.calendar_id, "primary");
        let client = google_calendar_execution_client(&config);
        // Must be dry-run: write a valid approved request with status.dry_run=true.
        let resp = client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .expect("dry-run should not error on valid request");
        assert!(
            resp.status.dry_run,
            "expected dry-run when write_enabled is false"
        );
        assert!(!resp.status.executed);
    }

    #[test]
    fn factory_fails_when_enabled_credential_explicitly_lacks_calendar_scope() {
        let mut config = GoogleCalendarWriteConfig::new(oauth_with_scopes(vec![
            "https://www.googleapis.com/auth/gmail.readonly".to_string(),
        ]));
        config.write_enabled = true;
        let client = google_calendar_execution_client(&config);
        let error = client
            .write_event(&write_req(GoogleCalendarEventWriteOperation::Create))
            .expect_err("live gate with known missing scope must fail");
        assert!(matches!(
            error,
            GoogleCalendarWriteError::Permanent { code, .. }
                if code == "google_calendar_scope_missing"
        ));
    }
}
