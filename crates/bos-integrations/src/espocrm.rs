//! EspoCRM note-create integration (the self-hosted open-source CRM arm of
//! the provider model — deployed for clients who have no CRM, connected
//! exactly like HubSpot). One capability: log a note
//! (`POST {base_url}/api/v1/Note`, `X-Api-Key` auth; the API user is created
//! in Administration → API Users with a role granting Note create).
//!
//! Config-driven like the hubspot client: credentials/gating arrive as
//! [`EspoCrmWriteConfig`] built by the caller; this module never reads env.
//! `write_enabled = false` (the default) => the dry-run client.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

const ESPOCRM_HTTP_TIMEOUT_SECS: u64 = 20;
const ESPOCRM_STREET_MAX_CHARS: usize = 255;

fn normalize_company_domain(raw: &str) -> Option<String> {
    let mut value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    for prefix in ["https://", "http://"] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            value = stripped.to_string();
            break;
        }
    }
    value = value
        .trim_start_matches("www.")
        .trim_end_matches('/')
        .trim_end_matches('.')
        .to_string();
    if value.contains('/') {
        value = value.split('/').next().unwrap_or_default().to_string();
    }
    (!value.is_empty() && value.contains('.')).then_some(value)
}

#[derive(Debug, Clone)]
pub struct EspoCrmWriteConfig {
    /// Instance base URL (e.g. "http://localhost:4580"). None = unconfigured
    /// (dry-run regardless).
    pub base_url: Option<String>,
    /// API key of the API user. None = unconfigured (dry-run regardless).
    pub api_key: Option<String>,
    /// Execution gate. `false` => [`espocrm_execution_client`] returns the
    /// dry-run client.
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl EspoCrmApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "espocrm", capability = "create_note"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmNoteCreateOutboxPayload {
    pub idempotency_key: String,
    pub approval: EspoCrmApprovalMetadata,
    /// Full note text (already carries the contact/source reference lines).
    pub note_body: String,
    /// RFC3339 timestamp of the source call/email. Espo's createdAt is
    /// server-set, so the client folds this into the note text instead.
    pub occurred_at: String,
    /// The contact's email, when the note named one — the executor resolves it
    /// to a Contact at delivery time and attaches the note to that record
    /// (parentType=Contact). Absent / unresolved => an unattached stream note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmNoteCreateRequest {
    pub idempotency_key: String,
    pub approval: EspoCrmApprovalMetadata,
    pub note_body: String,
    pub occurred_at: String,
    /// Resolved Contact id to attach the note to (parentType=Contact). None =>
    /// an unattached stream note (the prior behavior, and the HubSpot posture).
    pub parent_contact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmNoteCreateResponse {
    pub status: EspoCrmExecutionStatus,
    /// EspoCRM note record id ("dry-run" sentinel when not executed).
    pub note_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EspoCrmWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

pub trait EspoCrmExecutionClient: Send + Sync {
    fn create_note(
        &self,
        request: &EspoCrmNoteCreateRequest,
    ) -> Result<EspoCrmNoteCreateResponse, EspoCrmWriteError>;
}

fn validate_request(request: &EspoCrmNoteCreateRequest) -> Result<(), EspoCrmWriteError> {
    if !request.approval.is_complete() {
        return Err(EspoCrmWriteError::Permanent {
            code: "espocrm_approval_missing".to_string(),
            message: "espocrm write approval metadata is incomplete".to_string(),
        });
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(EspoCrmWriteError::Permanent {
            code: "espocrm_idempotency_key_missing".to_string(),
            message: "espocrm write idempotency key is required".to_string(),
        });
    }
    if request.note_body.trim().is_empty() {
        return Err(EspoCrmWriteError::Permanent {
            code: "espocrm_note_body_empty".to_string(),
            message: "espocrm note body is empty".to_string(),
        });
    }
    Ok(())
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunEspoCrmClient;

impl EspoCrmExecutionClient for DryRunEspoCrmClient {
    fn create_note(
        &self,
        request: &EspoCrmNoteCreateRequest,
    ) -> Result<EspoCrmNoteCreateResponse, EspoCrmWriteError> {
        validate_request(request)?;
        Ok(EspoCrmNoteCreateResponse {
            status: EspoCrmExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("espocrm_write_disabled_dry_run".to_string()),
            },
            note_id: "dry-run".to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmHttpResponse {
    pub status: u16,
    pub body: Value,
}

/// Narrow HTTP POST surface (mirrors the hubspot client's transport seam).
pub trait EspoCrmHttp: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestEspoCrmHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestEspoCrmHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(ESPOCRM_HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl EspoCrmHttp for ReqwestEspoCrmHttpClient {
    fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
        let response = self
            .client
            .post(url)
            .header("X-Api-Key", api_key)
            .json(body)
            .send()
            .map_err(|err| EspoCrmWriteError::Retryable {
                code: "espocrm_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(EspoCrmHttpResponse { status, body })
    }
}

pub struct LiveEspoCrmClient {
    http: Arc<dyn EspoCrmHttp>,
    base_url: String,
    api_key: String,
}

impl LiveEspoCrmClient {
    pub fn new(http: Arc<dyn EspoCrmHttp>, base_url: String, api_key: String) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }
}

impl EspoCrmExecutionClient for LiveEspoCrmClient {
    fn create_note(
        &self,
        request: &EspoCrmNoteCreateRequest,
    ) -> Result<EspoCrmNoteCreateResponse, EspoCrmWriteError> {
        validate_request(request)?;
        // Espo's createdAt is server-set; the source time rides in the text.
        let post = format!(
            "{}\nOccurred at: {}",
            request.note_body, request.occurred_at
        );
        let mut note = serde_json::Map::new();
        note.insert("type".to_string(), Value::String("Post".to_string()));
        note.insert("post".to_string(), Value::String(post));
        // Attach to the contact's record stream when one was resolved.
        if let Some(contact_id) = request.parent_contact_id.as_deref() {
            note.insert(
                "parentType".to_string(),
                Value::String("Contact".to_string()),
            );
            note.insert(
                "parentId".to_string(),
                Value::String(contact_id.to_string()),
            );
        }
        let body = Value::Object(note);
        let url = format!("{}/api/v1/Note", self.base_url);
        let response = self.http.post_json(&url, &self.api_key, &body)?;
        match response.status {
            200 | 201 => {
                let note_id = response
                    .body
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                Ok(EspoCrmNoteCreateResponse {
                    status: EspoCrmExecutionStatus {
                        executed: true,
                        dry_run: false,
                        reason: Some("espocrm_note_created".to_string()),
                    },
                    note_id,
                })
            }
            401 | 403 => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_auth_failed".to_string(),
                message: format!("status {}", response.status),
            }),
            429 | 500..=599 => Err(EspoCrmWriteError::Retryable {
                code: "espocrm_rate_or_server".to_string(),
                message: format!("status {}", response.status),
            }),
            other => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_request_rejected".to_string(),
                message: format!("status {other}: {}", response.body),
            }),
        }
    }
}

/// Write-gated factory: disabled or unconfigured => dry-run client.
pub fn espocrm_execution_client(config: &EspoCrmWriteConfig) -> Box<dyn EspoCrmExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunEspoCrmClient);
    }
    let base_url = config
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let (Some(base_url), Some(api_key)) = (base_url, api_key) else {
        tracing::warn!(
            "espocrm_factory: write enabled but base url or api key missing - dry-run fallback"
        );
        return Box::new(DryRunEspoCrmClient);
    };
    Box::new(LiveEspoCrmClient::new(
        Arc::new(ReqwestEspoCrmHttpClient::default()),
        base_url.to_string(),
        api_key.to_string(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmLeadInput {
    pub title: String,
    pub intent_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmLeadCreateOutboxPayload {
    pub idempotency_key: String,
    pub approval: EspoCrmApprovalMetadata,
    pub lead: EspoCrmLeadInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmLeadCreateResponse {
    pub status: EspoCrmExecutionStatus,
    pub lead_id: String,
}

pub trait EspoCrmLeadExecutionClient: Send + Sync {
    fn create_lead(
        &self,
        payload: &EspoCrmLeadCreateOutboxPayload,
    ) -> Result<EspoCrmLeadCreateResponse, EspoCrmWriteError>;
}

fn validate_lead(payload: &EspoCrmLeadCreateOutboxPayload) -> Result<(), EspoCrmWriteError> {
    let permanent = |code: &str, message: &str| EspoCrmWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if payload.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "espocrm_idempotency_key_required",
            "missing idempotency key",
        ));
    }
    if payload.lead.title.trim().is_empty() {
        return Err(permanent(
            "espocrm_lead_title_required",
            "lead title required",
        ));
    }
    if payload.lead.intent_summary.trim().is_empty() {
        return Err(permanent(
            "espocrm_lead_summary_required",
            "lead summary required",
        ));
    }
    Ok(())
}

pub struct DryRunEspoCrmLeadClient;

impl EspoCrmLeadExecutionClient for DryRunEspoCrmLeadClient {
    fn create_lead(
        &self,
        payload: &EspoCrmLeadCreateOutboxPayload,
    ) -> Result<EspoCrmLeadCreateResponse, EspoCrmWriteError> {
        validate_lead(payload)?;
        Ok(EspoCrmLeadCreateResponse {
            status: EspoCrmExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("espocrm_write_gate_closed".to_string()),
            },
            lead_id: "dry-run".to_string(),
        })
    }
}

pub struct LiveEspoCrmLeadClient {
    http: Arc<dyn EspoCrmHttp>,
    base_url: String,
    api_key: String,
}

impl LiveEspoCrmLeadClient {
    pub fn new(http: Arc<dyn EspoCrmHttp>, base_url: String, api_key: String) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }
}

impl EspoCrmLeadExecutionClient for LiveEspoCrmLeadClient {
    fn create_lead(
        &self,
        payload: &EspoCrmLeadCreateOutboxPayload,
    ) -> Result<EspoCrmLeadCreateResponse, EspoCrmWriteError> {
        validate_lead(payload)?;
        let mut description = payload.lead.intent_summary.trim().to_string();
        if let Some(next_step) = payload
            .lead
            .next_step_text
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            description.push_str("\n\nNext step: ");
            description.push_str(next_step);
        }
        if let Some(source) = payload
            .lead
            .source_ref
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            description.push_str("\n\nSource: ");
            description.push_str(source);
        }
        let mut body = serde_json::Map::new();
        body.insert(
            "name".to_string(),
            Value::String(payload.lead.title.clone()),
        );
        body.insert("description".to_string(), Value::String(description));
        if let Some(account_name) = payload
            .lead
            .company_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            body.insert(
                "accountName".to_string(),
                Value::String(account_name.to_string()),
            );
        }
        if let Some(email) = payload
            .lead
            .contact_email
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            body.insert("emailAddress".to_string(), Value::String(email.to_string()));
        }
        if let Some(name) = payload
            .lead
            .contact_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let mut parts = name.split_whitespace();
            if let Some(first) = parts.next() {
                body.insert("firstName".to_string(), Value::String(first.to_string()));
                let last = parts.collect::<Vec<_>>().join(" ");
                if !last.is_empty() {
                    body.insert("lastName".to_string(), Value::String(last));
                }
            }
        }
        let url = format!("{}/api/v1/Lead", self.base_url);
        let response = self
            .http
            .post_json(&url, &self.api_key, &Value::Object(body))?;
        match response.status {
            200 | 201 => {
                let lead_id = response
                    .body
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                Ok(EspoCrmLeadCreateResponse {
                    status: EspoCrmExecutionStatus {
                        executed: true,
                        dry_run: false,
                        reason: Some("espocrm_lead_created".to_string()),
                    },
                    lead_id,
                })
            }
            401 | 403 => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_auth_failed".to_string(),
                message: format!("status {}", response.status),
            }),
            429 | 500..=599 => Err(EspoCrmWriteError::Retryable {
                code: "espocrm_rate_or_server".to_string(),
                message: format!("status {}", response.status),
            }),
            other => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_request_rejected".to_string(),
                message: format!("status {other}: {}", response.body),
            }),
        }
    }
}

pub fn espocrm_lead_execution_client(
    config: &EspoCrmWriteConfig,
) -> Box<dyn EspoCrmLeadExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunEspoCrmLeadClient);
    }
    let base_url = config
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let (Some(base_url), Some(api_key)) = (base_url, api_key) else {
        tracing::warn!(
            "espocrm_lead_factory: write enabled but base url or api key missing - dry-run fallback"
        );
        return Box::new(DryRunEspoCrmLeadClient);
    };
    Box::new(LiveEspoCrmLeadClient::new(
        Arc::new(ReqwestEspoCrmHttpClient::default()),
        base_url.to_string(),
        api_key.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeHttp {
        responses: Mutex<VecDeque<(u16, Value)>>,
        posts: Mutex<Vec<(String, String, Value)>>,
    }

    impl FakeHttp {
        fn new(responses: Vec<(u16, Value)>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                posts: Mutex::new(Vec::new()),
            }
        }
    }

    impl EspoCrmHttp for FakeHttp {
        fn post_json(
            &self,
            url: &str,
            api_key: &str,
            body: &Value,
        ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
            self.posts.lock().expect("posts").push((
                url.to_string(),
                api_key.to_string(),
                body.clone(),
            ));
            let (status, body) = self
                .responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("scripted response");
            Ok(EspoCrmHttpResponse { status, body })
        }
    }

    fn request() -> EspoCrmNoteCreateRequest {
        EspoCrmNoteCreateRequest {
            idempotency_key: "idem-1".to_string(),
            approval: EspoCrmApprovalMetadata {
                approval_id: "appr-1".to_string(),
                approved_by: "jordan".to_string(),
                approved_at: "2026-06-10T12:00:00Z".to_string(),
            },
            note_body: "Call from Dana — wants the storefront quote.".to_string(),
            occurred_at: "2026-06-10T11:45:00Z".to_string(),
            parent_contact_id: None,
        }
    }

    #[test]
    fn factory_dry_runs_when_gate_closed_or_config_missing() {
        let gated = espocrm_execution_client(&EspoCrmWriteConfig {
            base_url: Some("http://localhost:4580".to_string()),
            api_key: Some("key".to_string()),
            write_enabled: false,
        });
        let response = gated.create_note(&request()).expect("dry run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);

        let keyless = espocrm_execution_client(&EspoCrmWriteConfig {
            base_url: Some("http://localhost:4580".to_string()),
            api_key: None,
            write_enabled: true,
        });
        assert!(
            keyless
                .create_note(&request())
                .expect("dry run")
                .status
                .dry_run
        );

        let urlless = espocrm_execution_client(&EspoCrmWriteConfig {
            base_url: None,
            api_key: Some("key".to_string()),
            write_enabled: true,
        });
        assert!(
            urlless
                .create_note(&request())
                .expect("dry run")
                .status
                .dry_run
        );
    }

    #[test]
    fn dry_run_still_validates_approval_and_body() {
        let mut incomplete = request();
        incomplete.approval.approved_by = String::new();
        let err = DryRunEspoCrmClient
            .create_note(&incomplete)
            .expect_err("approval required");
        assert!(matches!(err, EspoCrmWriteError::Permanent { ref code, .. }
            if code == "espocrm_approval_missing"));

        let mut empty = request();
        empty.note_body = "  ".to_string();
        assert!(DryRunEspoCrmClient.create_note(&empty).is_err());
    }

    #[test]
    fn live_create_posts_note_with_api_key_and_occurred_at_in_text() {
        let http = Arc::new(FakeHttp::new(vec![(
            200,
            serde_json::json!({"id": "note-42"}),
        )]));
        let client = LiveEspoCrmClient::new(
            http.clone(),
            "http://localhost:4580/".to_string(), // trailing slash normalized
            "secret-key".to_string(),
        );
        let response = client.create_note(&request()).expect("created");
        assert!(response.status.executed);
        assert_eq!(response.note_id, "note-42");

        let posts = http.posts.lock().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "http://localhost:4580/api/v1/Note");
        assert_eq!(posts[0].1, "secret-key");
        assert_eq!(posts[0].2["type"], "Post");
        let post_text = posts[0].2["post"].as_str().expect("post text");
        assert!(post_text.contains("storefront quote"));
        assert!(post_text.contains("Occurred at: 2026-06-10T11:45:00Z"));
        // No parent id => an unattached stream note.
        assert!(posts[0].2.get("parentId").is_none());
    }

    #[test]
    fn live_create_attaches_to_the_resolved_contact() {
        let http = Arc::new(FakeHttp::new(vec![(
            200,
            serde_json::json!({"id": "note-7"}),
        )]));
        let client = LiveEspoCrmClient::new(
            http.clone(),
            "http://localhost:4580".to_string(),
            "secret-key".to_string(),
        );
        let mut request = request();
        request.parent_contact_id = Some("con-9".to_string());
        client.create_note(&request).expect("created");
        let posts = http.posts.lock().expect("posts");
        assert_eq!(posts[0].2["parentType"], "Contact");
        assert_eq!(posts[0].2["parentId"], "con-9");
    }

    #[test]
    fn status_codes_map_to_retry_classes() {
        for (status, retryable) in [(429u16, true), (503, true), (400, false), (401, false)] {
            let http = Arc::new(FakeHttp::new(vec![(status, Value::Null)]));
            let client = LiveEspoCrmClient::new(
                http,
                "http://localhost:4580".to_string(),
                "key".to_string(),
            );
            let err = client.create_note(&request()).expect_err("must fail");
            match (retryable, err) {
                (true, EspoCrmWriteError::Retryable { .. }) => {}
                (false, EspoCrmWriteError::Permanent { .. }) => {}
                (_, other) => panic!("status {status}: wrong class {other:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Records write half: create_records — the approval-gated path that creates
// the CRM Account and/or Contact a note references when they are not already
// present. At-least-once outbox delivery makes idempotency the design center,
// and EspoCRM creates carry no client-supplied id, so the executor is a
// deterministic ENSURE chain — every step searches before it creates:
//   1. ensure_account  (search Account by name; create when allowed + absent)
//   2. ensure_contact  (search Contact by email, then name; create when
//                       allowed + absent, linked to the account)
// A redelivered job re-finds whatever a prior attempt created; nothing is
// double-minted. The account is ensured first so the contact can link to it.
// ---------------------------------------------------------------------------

/// One company to ensure in the CRM (Account). Present whenever a company was
/// identified — even a matched one — so the contact can link to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmCompanyInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One person to ensure in the CRM (Contact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmContactInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl EspoCrmContactInput {
    /// Espo's computed full name ("First Last"), for the name-equals search and
    /// the empty-name guard.
    fn full_name(&self) -> String {
        let mut parts = Vec::new();
        if let Some(first) = self
            .first_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(first);
        }
        if let Some(last) = self
            .last_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(last);
        }
        parts.join(" ")
    }
}

/// Outbox payload for `provider = "espocrm", capability = "create_records"`.
/// A company and/or a contact, each with a `create_*` flag: true = create when
/// absent; false = a matched record carried only so the contact links to it
/// (never created).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspoCrmRecordsCreateOutboxPayload {
    pub idempotency_key: String,
    pub approval: EspoCrmApprovalMetadata,
    /// The BOS draft id (traceability).
    pub draft_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<EspoCrmCompanyInput>,
    pub create_company: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<EspoCrmContactInput>,
    pub create_contact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspoCrmRecordsCreateResponse {
    pub status: EspoCrmExecutionStatus,
    /// Provider ids the chain resolved (None when nothing was found or created
    /// for that record). "dry-run" sentinel on the dry-run client.
    pub account_id: Option<String>,
    pub contact_id: Option<String>,
}

fn validate_records(payload: &EspoCrmRecordsCreateOutboxPayload) -> Result<(), EspoCrmWriteError> {
    let permanent = |code: &str, message: &str| EspoCrmWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if !payload.approval.is_complete() {
        return Err(permanent(
            "espocrm_approval_missing",
            "espocrm write approval metadata is incomplete",
        ));
    }
    if payload.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "espocrm_idempotency_key_missing",
            "espocrm write idempotency key is required",
        ));
    }
    if !payload.create_company && !payload.create_contact {
        return Err(permanent(
            "espocrm_records_nothing_proposed",
            "at least one record must be proposed for creation",
        ));
    }
    if payload.create_company
        && payload
            .company
            .as_ref()
            .map(|c| c.name.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(permanent(
            "espocrm_company_name_missing",
            "a company proposed for creation needs a name",
        ));
    }
    if payload.create_contact
        && payload
            .contact
            .as_ref()
            .map(|c| c.full_name().is_empty())
            .unwrap_or(true)
    {
        return Err(permanent(
            "espocrm_contact_name_missing",
            "a contact proposed for creation needs a name",
        ));
    }
    if payload.create_contact
        && payload
            .contact
            .as_ref()
            .and_then(|c| c.last_name.as_deref())
            .map(|last| last.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(permanent(
            "espocrm_contact_last_name_missing",
            "EspoCRM contact creation requires a last name",
        ));
    }
    Ok(())
}

pub trait EspoCrmRecordsExecutionClient: Send + Sync {
    fn create_records(
        &self,
        payload: &EspoCrmRecordsCreateOutboxPayload,
    ) -> Result<EspoCrmRecordsCreateResponse, EspoCrmWriteError>;
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunEspoCrmRecordsClient;

impl EspoCrmRecordsExecutionClient for DryRunEspoCrmRecordsClient {
    fn create_records(
        &self,
        payload: &EspoCrmRecordsCreateOutboxPayload,
    ) -> Result<EspoCrmRecordsCreateResponse, EspoCrmWriteError> {
        validate_records(payload)?;
        Ok(EspoCrmRecordsCreateResponse {
            status: EspoCrmExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("espocrm_write_disabled_dry_run".to_string()),
            },
            account_id: payload.company.as_ref().map(|_| "dry-run".to_string()),
            contact_id: payload.contact.as_ref().map(|_| "dry-run".to_string()),
        })
    }
}

/// Records write transport seam: the ensure-chain needs searches (GET) and
/// creates (POST). The note-create client keeps its POST-only seam.
pub trait EspoCrmRecordsHttp: Send + Sync {
    fn get_json(&self, url: &str, api_key: &str) -> Result<EspoCrmHttpResponse, EspoCrmWriteError>;
    fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError>;
}

impl EspoCrmRecordsHttp for ReqwestEspoCrmHttpClient {
    fn get_json(&self, url: &str, api_key: &str) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
        let response = self
            .client
            .get(url)
            .header("X-Api-Key", api_key)
            .send()
            .map_err(|err| EspoCrmWriteError::Retryable {
                code: "espocrm_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(EspoCrmHttpResponse { status, body })
    }

    fn post_json(
        &self,
        url: &str,
        api_key: &str,
        body: &Value,
    ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
        EspoCrmHttp::post_json(self, url, api_key, body)
    }
}

pub struct LiveEspoCrmRecordsClient<C: EspoCrmRecordsHttp = ReqwestEspoCrmHttpClient> {
    http: Arc<C>,
    base_url: String,
    api_key: String,
}

impl<C: EspoCrmRecordsHttp> LiveEspoCrmRecordsClient<C> {
    pub fn new(http: Arc<C>, base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base_url: String = base_url.into();
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    fn check(
        &self,
        response: EspoCrmHttpResponse,
        context: &str,
    ) -> Result<Value, EspoCrmWriteError> {
        match response.status {
            200..=299 => Ok(response.body),
            401 | 403 => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_auth_failed".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            429 | 500..=599 => Err(EspoCrmWriteError::Retryable {
                code: "espocrm_rate_or_server".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            other => Err(EspoCrmWriteError::Permanent {
                code: "espocrm_request_rejected".to_string(),
                message: format!("{context}: status {other}: {}", response.body),
            }),
        }
    }

    /// First id from an Espo list response (`{ "total", "list": [...] }`).
    fn first_list_id(body: &Value) -> Option<String> {
        body.get("list")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Id from a create response (Espo returns the entity object directly).
    fn created_id(body: &Value, context: &str) -> Result<String, EspoCrmWriteError> {
        body.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| EspoCrmWriteError::Permanent {
                code: "espocrm_response_invalid".to_string(),
                message: format!("{context}: created entity has no id"),
            })
    }

    fn street_value(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.chars().take(ESPOCRM_STREET_MAX_CHARS).collect())
    }

    fn search_url(&self, entity: &str, attribute: &str, value: &str) -> String {
        let encode = crate::qbo_oauth::encode_query_component;
        format!(
            "{}/api/v1/{entity}?select=id,name&maxSize=1\
             &where[0][type]=equals&where[0][attribute]={attribute}&where[0][value]={}",
            self.base_url,
            encode(value),
        )
    }

    /// Search an Account by exact name. Read-only — used both at produce time
    /// (matched/missing detection, gate-independent) and inside the chain.
    pub fn find_account(&self, name: &str) -> Result<Option<String>, EspoCrmWriteError> {
        let url = self.search_url("Account", "name", name);
        let body = self.check(self.http.get_json(&url, &self.api_key)?, "account lookup")?;
        Ok(Self::first_list_id(&body))
    }

    /// Search an Account by website/domain. Read-only.
    pub fn find_account_by_domain(
        &self,
        domain: &str,
    ) -> Result<Option<String>, EspoCrmWriteError> {
        let Some(domain) = normalize_company_domain(domain) else {
            return Ok(None);
        };
        for candidate in [
            domain.clone(),
            format!("https://{domain}"),
            format!("https://www.{domain}"),
            format!("http://{domain}"),
            format!("http://www.{domain}"),
        ] {
            let url = self.search_url("Account", "website", &candidate);
            let body = self.check(
                self.http.get_json(&url, &self.api_key)?,
                "account domain lookup",
            )?;
            if let Some(id) = Self::first_list_id(&body) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Search a Contact by email (preferred) then full name. Read-only.
    pub fn find_contact(
        &self,
        email: Option<&str>,
        full_name: Option<&str>,
    ) -> Result<Option<String>, EspoCrmWriteError> {
        let lookup = if let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) {
            Some(self.search_url("Contact", "emailAddress", email))
        } else {
            full_name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|name| self.search_url("Contact", "name", name))
        };
        let Some(url) = lookup else { return Ok(None) };
        let body = self.check(self.http.get_json(&url, &self.api_key)?, "contact lookup")?;
        Ok(Self::first_list_id(&body))
    }

    fn list_url(
        &self,
        entity: &str,
        select: &str,
        request: &crate::crm_read::CrmPageRequest,
    ) -> String {
        let encode = crate::qbo_oauth::encode_query_component;
        let offset = request
            .cursor
            .as_deref()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(0);
        format!(
            "{}/api/v1/{entity}?select={}&maxSize={}&offset={offset}",
            self.base_url,
            encode(select),
            request.effective_page_size(),
        )
    }

    fn next_offset_cursor<T>(
        records: &[T],
        request: &crate::crm_read::CrmPageRequest,
    ) -> Option<String> {
        (records.len() as u32 == request.effective_page_size()).then(|| {
            let offset = request
                .cursor
                .as_deref()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                .unwrap_or(0);
            (offset + records.len() as u32).to_string()
        })
    }

    fn string_field(record: &Value, name: &str) -> Option<String> {
        record
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(str::to_string)
    }

    fn record_ids(record: &Value, name: &str) -> Vec<String> {
        record
            .get(name)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    }

    fn money_cents(record: &Value, name: &str) -> Option<i64> {
        let value = record.get(name)?;
        if let Some(number) = value.as_i64() {
            return number.checked_mul(100);
        }
        if let Some(number) = value.as_f64() {
            return Some((number * 100.0).round() as i64);
        }
        let raw = value.as_str()?.trim();
        if raw.is_empty() {
            return None;
        }
        let negative = raw.starts_with('-');
        let unsigned = raw.trim_start_matches('-');
        let mut parts = unsigned.split('.');
        let dollars = parts.next()?.parse::<i64>().ok()?;
        let cents_part = parts.next().unwrap_or("");
        if parts.next().is_some() {
            return None;
        }
        let cents = match cents_part.len() {
            0 => 0,
            1 => cents_part.parse::<i64>().ok()? * 10,
            _ => cents_part.get(0..2)?.parse::<i64>().ok()?,
        };
        let total = dollars.checked_mul(100)?.checked_add(cents)?;
        Some(if negative { -total } else { total })
    }

    /// Search the account by name; create it only when `allow_create`. Returns
    /// the resolved id (None when matched-but-absent and creation is off).
    fn ensure_account(
        &self,
        company: &EspoCrmCompanyInput,
        allow_create: bool,
    ) -> Result<Option<String>, EspoCrmWriteError> {
        if let Some(id) = self.find_account(&company.name)? {
            return Ok(Some(id));
        }
        if !allow_create {
            return Ok(None);
        }
        let mut record = serde_json::Map::new();
        record.insert("name".to_string(), Value::String(company.name.clone()));
        if let Some(website) = company.website.as_deref() {
            record.insert("website".to_string(), Value::String(website.to_string()));
        }
        // Espo's `phoneNumber` is a structured phone field. Sending a display
        // string such as "(843) 882-9224" is rejected with a validation 400, so
        // omit optional phone data until this client writes Espo's structured
        // representation.
        if let Some(address) = company.address.as_deref().and_then(Self::street_value) {
            record.insert("billingAddressStreet".to_string(), Value::String(address));
        }
        if let Some(description) = company.description.as_deref() {
            record.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        let created = self.http.post_json(
            &format!("{}/api/v1/Account", self.base_url),
            &self.api_key,
            &Value::Object(record),
        )?;
        let body = self.check(created, "account create")?;
        Self::created_id(&body, "account create").map(Some)
    }

    /// Search the contact by email (preferred) then full name; create it only
    /// when `allow_create`, linked to `account_id` when one is known.
    fn ensure_contact(
        &self,
        contact: &EspoCrmContactInput,
        allow_create: bool,
        account_id: Option<&str>,
    ) -> Result<Option<String>, EspoCrmWriteError> {
        let full_name = contact.full_name();
        if let Some(id) = self.find_contact(
            contact.email.as_deref(),
            (!full_name.is_empty()).then_some(full_name.as_str()),
        )? {
            return Ok(Some(id));
        }
        if !allow_create {
            return Ok(None);
        }
        let mut record = serde_json::Map::new();
        if let Some(first) = contact.first_name.as_deref() {
            record.insert("firstName".to_string(), Value::String(first.to_string()));
        }
        if let Some(last) = contact.last_name.as_deref() {
            record.insert("lastName".to_string(), Value::String(last.to_string()));
        }
        if let Some(email) = contact.email.as_deref() {
            record.insert("emailAddress".to_string(), Value::String(email.to_string()));
        }
        // See account create: Espo's phone field is not a plain string write.
        // Contact.title is a computed account-contact role field in the stock
        // CRM module, not a writable job-title field.
        if let Some(account_id) = account_id {
            record.insert(
                "accountId".to_string(),
                Value::String(account_id.to_string()),
            );
        }
        let created = self.http.post_json(
            &format!("{}/api/v1/Contact", self.base_url),
            &self.api_key,
            &Value::Object(record),
        )?;
        let body = self.check(created, "contact create")?;
        Self::created_id(&body, "contact create").map(Some)
    }
}

impl<C: EspoCrmRecordsHttp> crate::crm_read::CrmReadClient for LiveEspoCrmRecordsClient<C> {
    fn list_contacts_page(
        &self,
        request: &crate::crm_read::CrmPageRequest,
    ) -> Result<
        crate::crm_read::CrmPage<crate::crm_read::CrmContactRecord>,
        crate::crm_read::CrmReadError,
    > {
        let url = self.list_url(
            "Contact",
            "id,name,emailAddress,accountName,phoneNumber,status,assignedUserName,modifiedAt",
            request,
        );
        let response = self
            .http
            .get_json(&url, &self.api_key)
            .map_err(crm_read_error_from_espocrm)?;
        let body = self
            .check(response, "contact snapshot list")
            .map_err(crm_read_error_from_espocrm)?;
        let records = body
            .get("list")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let provider_contact_id = Self::string_field(record, "id")?;
                Some(crate::crm_read::CrmContactRecord {
                    provider_contact_id,
                    email: Self::string_field(record, "emailAddress"),
                    name: Self::string_field(record, "name"),
                    company: Self::string_field(record, "accountName"),
                    phone: Self::string_field(record, "phoneNumber"),
                    lifecycle_stage: Self::string_field(record, "status"),
                    owner: Self::string_field(record, "assignedUserName"),
                    last_activity_at: Self::string_field(record, "modifiedAt"),
                })
            })
            .collect::<Vec<_>>();
        let next_cursor = Self::next_offset_cursor(&records, request);
        Ok(crate::crm_read::CrmPage {
            records,
            next_cursor,
        })
    }

    fn list_deals_page(
        &self,
        request: &crate::crm_read::CrmPageRequest,
    ) -> Result<
        crate::crm_read::CrmPage<crate::crm_read::CrmDealRecord>,
        crate::crm_read::CrmReadError,
    > {
        let url = self.list_url(
            "Opportunity",
            "id,name,stage,amount,accountName,closeDate,contactsIds,modifiedAt",
            request,
        );
        let response = self
            .http
            .get_json(&url, &self.api_key)
            .map_err(crm_read_error_from_espocrm)?;
        let body = self
            .check(response, "deal snapshot list")
            .map_err(crm_read_error_from_espocrm)?;
        let records = body
            .get("list")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let provider_deal_id = Self::string_field(record, "id")?;
                Some(crate::crm_read::CrmDealRecord {
                    provider_deal_id,
                    name: Self::string_field(record, "name"),
                    stage: Self::string_field(record, "stage"),
                    amount_cents: Self::money_cents(record, "amount"),
                    currency: None,
                    pipeline: None,
                    close_date: Self::string_field(record, "closeDate"),
                    associated_contact_ids: Self::record_ids(record, "contactsIds"),
                    associated_contact_email: None,
                    associated_contact_company: Self::string_field(record, "accountName"),
                })
            })
            .collect::<Vec<_>>();
        let next_cursor = Self::next_offset_cursor(&records, request);
        Ok(crate::crm_read::CrmPage {
            records,
            next_cursor,
        })
    }
}

fn crm_read_error_from_espocrm(err: EspoCrmWriteError) -> crate::crm_read::CrmReadError {
    match err {
        EspoCrmWriteError::Retryable { code, message } if code.contains("rate") => {
            crate::crm_read::CrmReadError::RateLimited {
                retry_after_ms: None,
                message,
            }
        }
        EspoCrmWriteError::Retryable { code, message } => {
            crate::crm_read::CrmReadError::Retryable { code, message }
        }
        EspoCrmWriteError::Permanent { code, message } => {
            crate::crm_read::CrmReadError::Permanent { code, message }
        }
    }
}

impl<C: EspoCrmRecordsHttp> EspoCrmRecordsExecutionClient for LiveEspoCrmRecordsClient<C> {
    fn create_records(
        &self,
        payload: &EspoCrmRecordsCreateOutboxPayload,
    ) -> Result<EspoCrmRecordsCreateResponse, EspoCrmWriteError> {
        validate_records(payload)?;
        let account_id = match payload.company.as_ref() {
            Some(company) => self.ensure_account(company, payload.create_company)?,
            None => None,
        };
        let contact_id = match payload.contact.as_ref() {
            Some(contact) => {
                self.ensure_contact(contact, payload.create_contact, account_id.as_deref())?
            }
            None => None,
        };
        Ok(EspoCrmRecordsCreateResponse {
            status: EspoCrmExecutionStatus {
                executed: true,
                dry_run: false,
                reason: Some("espocrm_records_created".to_string()),
            },
            account_id,
            contact_id,
        })
    }
}

/// A live records client for READ-ONLY produce-time search (find_account /
/// find_contact). Independent of the write gate — the operator must see
/// accurate matched/missing proposals before any gate opens. None when the
/// instance is unconfigured (the caller then treats every record as missing).
pub fn espocrm_records_search_client(
    config: &EspoCrmWriteConfig,
) -> Option<LiveEspoCrmRecordsClient> {
    let base_url = config
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())?;
    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())?;
    Some(LiveEspoCrmRecordsClient::new(
        Arc::new(ReqwestEspoCrmHttpClient::default()),
        base_url.to_string(),
        api_key.to_string(),
    ))
}

/// Write-gated factory: disabled or unconfigured => dry-run client.
pub fn espocrm_records_execution_client(
    config: &EspoCrmWriteConfig,
) -> Box<dyn EspoCrmRecordsExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunEspoCrmRecordsClient);
    }
    let base_url = config
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let (Some(base_url), Some(api_key)) = (base_url, api_key) else {
        tracing::warn!(
            "espocrm_records_factory: write enabled but base url or api key missing - dry-run fallback"
        );
        return Box::new(DryRunEspoCrmRecordsClient);
    };
    Box::new(LiveEspoCrmRecordsClient::new(
        Arc::new(ReqwestEspoCrmHttpClient::default()),
        base_url.to_string(),
        api_key.to_string(),
    ))
}

#[cfg(test)]
mod records_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted records transport: each entry is (expected url substring,
    /// status, body). Panics on an unexpected call — proving skipped steps make
    /// NO requests. Hit urls are recorded for negative assertions.
    struct ScriptedRecordsHttp {
        script: Mutex<VecDeque<(&'static str, u16, Value)>>,
        seen: Mutex<Vec<String>>,
        post_bodies: Mutex<Vec<(String, Value)>>,
    }

    impl ScriptedRecordsHttp {
        fn new(script: Vec<(&'static str, u16, Value)>) -> Self {
            Self {
                script: Mutex::new(script.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
                post_bodies: Mutex::new(Vec::new()),
            }
        }
        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("lock").clone()
        }
        fn post_bodies(&self) -> Vec<(String, Value)> {
            self.post_bodies.lock().expect("lock").clone()
        }
        fn next(&self, url: &str) -> EspoCrmHttpResponse {
            self.seen.lock().expect("lock").push(url.to_string());
            let (fragment, status, body) = self
                .script
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected request: {url}"));
            assert!(
                url.contains(fragment),
                "expected url containing {fragment}, got {url}"
            );
            EspoCrmHttpResponse { status, body }
        }
        fn exhausted(&self) {
            assert!(
                self.script.lock().expect("lock").is_empty(),
                "script not fully consumed"
            );
        }
    }

    impl EspoCrmRecordsHttp for ScriptedRecordsHttp {
        fn get_json(
            &self,
            url: &str,
            _key: &str,
        ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
            Ok(self.next(url))
        }
        fn post_json(
            &self,
            url: &str,
            _key: &str,
            body: &Value,
        ) -> Result<EspoCrmHttpResponse, EspoCrmWriteError> {
            self.post_bodies
                .lock()
                .expect("lock")
                .push((url.to_string(), body.clone()));
            Ok(self.next(url))
        }
    }

    fn client(http: Arc<ScriptedRecordsHttp>) -> LiveEspoCrmRecordsClient<ScriptedRecordsHttp> {
        LiveEspoCrmRecordsClient::new(http, "http://localhost:4580/", "key-1")
    }

    fn both_missing() -> EspoCrmRecordsCreateOutboxPayload {
        EspoCrmRecordsCreateOutboxPayload {
            idempotency_key: "crmrecords:crd_1".to_string(),
            approval: EspoCrmApprovalMetadata {
                approval_id: "appr_crd_1".to_string(),
                approved_by: "jordan".to_string(),
                approved_at: "2026-06-11T12:00:00Z".to_string(),
            },
            draft_ref: "crd_1".to_string(),
            company: Some(EspoCrmCompanyInput {
                name: "example".to_string(),
                website: Some("example.test".to_string()),
                phone: Some("(843) 882-9224".to_string()),
                address: Some("123 ".to_string() + &"Long Street ".repeat(40)),
                description: Some("Boutique vacation rentals in the Lowcountry.".to_string()),
            }),
            create_company: true,
            contact: Some(EspoCrmContactInput {
                first_name: Some("Casey".to_string()),
                last_name: Some("Sullivan".to_string()),
                email: Some("casey@example.test".to_string()),
                phone: Some("(843) 882-9224".to_string()),
                title: Some("Operations Manager".to_string()),
            }),
            create_contact: true,
        }
    }

    fn empty_list() -> Value {
        serde_json::json!({ "total": 0, "list": [] })
    }

    #[test]
    fn both_missing_creates_account_then_linked_contact() {
        let http = Arc::new(ScriptedRecordsHttp::new(vec![
            ("Account?select=id,name", 200, empty_list()),
            ("Account", 200, serde_json::json!({ "id": "acc-1" })),
            ("Contact?select=id,name", 200, empty_list()),
            ("Contact", 200, serde_json::json!({ "id": "con-1" })),
        ]));
        let response = client(http.clone())
            .create_records(&both_missing())
            .expect("chain");
        http.exhausted();
        assert_eq!(response.account_id.as_deref(), Some("acc-1"));
        assert_eq!(response.contact_id.as_deref(), Some("con-1"));
        assert!(response.status.executed);
        // The contact links to the account the chain just created.
        let contact_post = http
            .seen()
            .into_iter()
            .find(|url| url.ends_with("/api/v1/Contact"))
            .expect("contact create");
        assert!(contact_post.contains("/Contact"));
        let account_body = http
            .post_bodies()
            .into_iter()
            .find_map(|(url, body)| url.ends_with("/api/v1/Account").then_some(body))
            .expect("account create body");
        assert!(
            account_body.get("phoneNumber").is_none(),
            "raw display phone strings must not block Espo account creation"
        );
        assert_eq!(
            account_body.get("description").and_then(Value::as_str),
            Some("Boutique vacation rentals in the Lowcountry.")
        );
        let street = account_body
            .get("billingAddressStreet")
            .and_then(Value::as_str)
            .expect("street");
        assert_eq!(street.chars().count(), ESPOCRM_STREET_MAX_CHARS);
        let contact_body = http
            .post_bodies()
            .into_iter()
            .find_map(|(url, body)| url.ends_with("/api/v1/Contact").then_some(body))
            .expect("contact create body");
        assert!(
            contact_body.get("phoneNumber").is_none(),
            "raw display phone strings must not block Espo contact creation"
        );
        assert!(
            contact_body.get("title").is_none(),
            "stock Espo Contact.title is not a writable job-title field"
        );
    }

    #[test]
    fn matched_company_links_missing_contact_without_creating_account() {
        // create_company false (matched): the account is searched + linked but
        // never created; only the contact is created.
        let mut payload = both_missing();
        payload.create_company = false;
        let http = Arc::new(ScriptedRecordsHttp::new(vec![
            (
                "Account?select=id,name",
                200,
                serde_json::json!({ "total": 1, "list": [{ "id": "acc-existing" }] }),
            ),
            ("Contact?select=id,name", 200, empty_list()),
            ("Contact", 200, serde_json::json!({ "id": "con-2" })),
        ]));
        let response = client(http.clone())
            .create_records(&payload)
            .expect("chain");
        http.exhausted();
        assert_eq!(response.account_id.as_deref(), Some("acc-existing"));
        assert_eq!(response.contact_id.as_deref(), Some("con-2"));
    }

    #[test]
    fn redelivery_finds_existing_records_and_creates_nothing() {
        let http = Arc::new(ScriptedRecordsHttp::new(vec![
            (
                "Account?select=id,name",
                200,
                serde_json::json!({ "total": 1, "list": [{ "id": "acc-1" }] }),
            ),
            (
                "Contact?select=id,name",
                200,
                serde_json::json!({ "total": 1, "list": [{ "id": "con-1" }] }),
            ),
        ]));
        let response = client(http.clone())
            .create_records(&both_missing())
            .expect("resume");
        http.exhausted();
        assert_eq!(response.account_id.as_deref(), Some("acc-1"));
        assert_eq!(response.contact_id.as_deref(), Some("con-1"));
    }

    #[test]
    fn factory_and_dry_run_validate_without_network() {
        let dry = espocrm_records_execution_client(&EspoCrmWriteConfig {
            base_url: Some("http://localhost:4580".to_string()),
            api_key: Some("k".to_string()),
            write_enabled: false,
        });
        let response = dry.create_records(&both_missing()).expect("dry run");
        assert!(response.status.dry_run);
        assert_eq!(response.account_id.as_deref(), Some("dry-run"));

        // Nothing proposed is a permanent rejection.
        let mut nothing = both_missing();
        nothing.create_company = false;
        nothing.create_contact = false;
        assert!(matches!(
            DryRunEspoCrmRecordsClient.create_records(&nothing),
            Err(EspoCrmWriteError::Permanent { code, .. })
                if code == "espocrm_records_nothing_proposed"
        ));

        // A company proposed for creation needs a name.
        let mut nameless = both_missing();
        nameless.company = Some(EspoCrmCompanyInput {
            name: "   ".to_string(),
            website: None,
            phone: None,
            address: None,
            description: None,
        });
        assert!(matches!(
            DryRunEspoCrmRecordsClient.create_records(&nameless),
            Err(EspoCrmWriteError::Permanent { code, .. })
                if code == "espocrm_company_name_missing"
        ));

        // Stock EspoCRM requires Contact.lastName on create.
        let mut first_name_only = both_missing();
        first_name_only.contact = Some(EspoCrmContactInput {
            first_name: Some("Trevor".to_string()),
            last_name: None,
            email: None,
            phone: None,
            title: None,
        });
        assert!(matches!(
            DryRunEspoCrmRecordsClient.create_records(&first_name_only),
            Err(EspoCrmWriteError::Permanent { code, .. })
                if code == "espocrm_contact_last_name_missing"
        ));
    }
}
