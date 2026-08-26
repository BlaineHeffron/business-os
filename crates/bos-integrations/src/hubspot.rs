//! HubSpot CRM note-create integration (minimal port from agent-monitor-rust
//! `hubspot.rs`, free-tier posture): one capability — log a note
//! (`POST /crm/v3/objects/notes`, `hs_note_body` + `hs_timestamp`).
//! NO associations API (restricted on the free tier — agent_monitor's proven
//! posture): the contact reference rides in the note body text.
//!
//! Config-driven like the calendar client: credentials/gating arrive as
//! [`HubSpotWriteConfig`] built by the caller; this module never reads env.
//! `write_enabled = false` (the default) => the dry-run client.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const NOTES_URL: &str = "https://api.hubapi.com/crm/v3/objects/notes";
const HUBSPOT_HTTP_TIMEOUT_SECS: u64 = 20;
const HUBSPOT_DEAL_SEARCH_MAX_PAGES: usize = 10;
const HUBSPOT_DEAL_SAMPLE_MAX_PAGES: usize = 3;

#[derive(Debug, Clone)]
pub struct HubSpotWriteConfig {
    /// Private-app access token. None = unconfigured (dry-run regardless).
    pub access_token: Option<String>,
    /// Execution gate. `false` => [`hubspot_execution_client`] returns the
    /// dry-run client.
    pub write_enabled: bool,
}

#[cfg(test)]
mod deal_read_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ScriptedDealHttp {
        script: Mutex<VecDeque<(u16, Value)>>,
        bodies: Mutex<Vec<Value>>,
    }

    impl ScriptedDealHttp {
        fn new(script: Vec<(u16, Value)>) -> Self {
            Self {
                script: Mutex::new(script.into_iter().collect()),
                bodies: Mutex::new(Vec::new()),
            }
        }
    }

    impl HubSpotReadHttp for ScriptedDealHttp {
        fn get_json(
            &self,
            url: &str,
            _access_token: &str,
        ) -> Result<HubSpotHttpResponse, HubSpotReadError> {
            panic!("unexpected GET in scripted deal search test: {url}");
        }

        fn post_json(
            &self,
            url: &str,
            _access_token: &str,
            body: &Value,
        ) -> Result<HubSpotHttpResponse, HubSpotReadError> {
            assert!(url.ends_with("/crm/v3/objects/deals/search"));
            self.bodies.lock().expect("bodies").push(body.clone());
            let (status, body) = self
                .script
                .lock()
                .expect("script")
                .pop_front()
                .expect("unexpected request");
            Ok(HubSpotHttpResponse { status, body })
        }
    }

    fn config() -> HubSpotDealReadConfig {
        HubSpotDealReadConfig {
            access_token: Some("tok".to_string()),
            pipeline_id: Some("pipe-1".to_string()),
            open_stage_ids: vec!["open-1".to_string()],
            won_stage_ids: vec!["won-1".to_string()],
            lost_stage_ids: vec!["lost-1".to_string()],
            started_date_property: Some("createdate".to_string()),
            closed_date_property: Some("closedate".to_string()),
            segment_properties: vec!["dealtype".to_string()],
        }
    }

    #[test]
    fn deal_reporting_snapshot_computes_close_rate_duration_and_segments() {
        let http = Arc::new(ScriptedDealHttp::new(vec![
            (
                200,
                serde_json::json!({
                    "results": [
                        { "id": "d1", "properties": {
                            "dealstage": "won-1",
                            "createdate": "2026-06-01",
                            "closedate": "2026-06-11",
                            "dealtype": "commercial"
                        }},
                        { "id": "d2", "properties": {
                            "dealstage": "lost-1",
                            "createdate": "1717200000000",
                            "closedate": "1718064000000",
                            "dealtype": "residential"
                        }}
                    ],
                    "paging": { "next": { "after": "page-2" } }
                }),
            ),
            (
                200,
                serde_json::json!({
                    "results": [
                        { "id": "d3", "properties": {
                            "dealstage": "won-1",
                            "createdate": "2026-06-02",
                            "closedate": "2026-06-12",
                            "dealtype": "commercial"
                        }}
                    ]
                }),
            ),
        ]));
        let client = LiveHubSpotDealReadClient::new(http.clone(), "tok");
        let snapshot = client
            .reporting_snapshot(&config(), 1_717_824_000_000, 1_718_688_000_000)
            .expect("snapshot");
        assert_eq!(snapshot.closed_deals, 3);
        assert_eq!(snapshot.won_deals, 2);
        assert_eq!(snapshot.lost_deals, 1);
        assert_eq!(snapshot.close_rate_bps, Some(6_666));
        assert_eq!(snapshot.avg_contact_to_close_days, Some(10));
        assert_eq!(snapshot.contact_to_close_sample, 3);
        assert!(snapshot
            .segment_cuts
            .contains(&"dealtype:commercial=2".to_string()));
        assert_eq!(http.bodies.lock().expect("bodies").len(), 2);
    }

    #[test]
    fn missing_mapping_is_pending_before_network() {
        let mut config = config();
        config.pipeline_id = None;
        assert!(config
            .missing_reason()
            .expect("reason")
            .contains("pipeline"));
    }

    #[test]
    fn deal_search_stops_at_page_budget() {
        let http = Arc::new(ScriptedDealHttp::new(
            (0..=HUBSPOT_DEAL_SEARCH_MAX_PAGES)
                .map(|page| {
                    (
                        200,
                        serde_json::json!({
                            "results": [],
                            "paging": { "next": { "after": format!("page-{page}") } }
                        }),
                    )
                })
                .collect(),
        ));
        let client = LiveHubSpotDealReadClient::new(http.clone(), "tok");
        let err = client
            .search_deals(
                "pipe-1",
                &["won-1".to_string()],
                "closedate",
                1_717_824_000_000,
                1_718_688_000_000,
                &["dealstage".to_string(), "closedate".to_string()],
            )
            .expect_err("page budget should stop search");
        assert_eq!(
            err,
            HubSpotReadError::Limited {
                code: "hubspot_deals_page_budget_exceeded".to_string(),
                message: "deal search exceeded 10 pages".to_string(),
            }
        );
        assert_eq!(
            http.bodies.lock().expect("bodies").len(),
            HUBSPOT_DEAL_SEARCH_MAX_PAGES
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl HubSpotApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "hubspot", capability = "create_note"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotNoteCreateOutboxPayload {
    pub idempotency_key: String,
    pub approval: HubSpotApprovalMetadata,
    /// Full note text (already carries the contact/source reference lines).
    pub note_body: String,
    /// RFC3339 timestamp the note is logged at (the call/email time).
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotNoteCreateRequest {
    pub idempotency_key: String,
    pub approval: HubSpotApprovalMetadata,
    pub note_body: String,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotNoteCreateResponse {
    pub status: HubSpotExecutionStatus,
    /// HubSpot note object id ("dry-run" sentinel when not executed).
    pub note_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubSpotWriteError {
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

pub trait HubSpotExecutionClient: Send + Sync {
    fn create_note(
        &self,
        request: &HubSpotNoteCreateRequest,
    ) -> Result<HubSpotNoteCreateResponse, HubSpotWriteError>;
}

impl HubSpotExecutionClient for Box<dyn HubSpotExecutionClient> {
    fn create_note(
        &self,
        request: &HubSpotNoteCreateRequest,
    ) -> Result<HubSpotNoteCreateResponse, HubSpotWriteError> {
        (**self).create_note(request)
    }
}

fn validate_request(request: &HubSpotNoteCreateRequest) -> Result<(), HubSpotWriteError> {
    if !request.approval.is_complete() {
        return Err(HubSpotWriteError::Permanent {
            code: "hubspot_approval_missing".to_string(),
            message: "hubspot write approval metadata is incomplete".to_string(),
        });
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(HubSpotWriteError::Permanent {
            code: "hubspot_idempotency_key_missing".to_string(),
            message: "hubspot write idempotency key is required".to_string(),
        });
    }
    if request.note_body.trim().is_empty() {
        return Err(HubSpotWriteError::Permanent {
            code: "hubspot_note_body_empty".to_string(),
            message: "hubspot note body is empty".to_string(),
        });
    }
    Ok(())
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunHubSpotClient;

impl HubSpotExecutionClient for DryRunHubSpotClient {
    fn create_note(
        &self,
        request: &HubSpotNoteCreateRequest,
    ) -> Result<HubSpotNoteCreateResponse, HubSpotWriteError> {
        validate_request(request)?;
        Ok(HubSpotNoteCreateResponse {
            status: HubSpotExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("hubspot_write_disabled_dry_run".to_string()),
            },
            note_id: "dry-run".to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotHttpResponse {
    pub status: u16,
    pub body: Value,
}

/// Narrow HTTP POST surface (mirrors the calendar client's transport seam).
pub trait HubSpotHttp: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestHubSpotHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestHubSpotHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HUBSPOT_HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl HubSpotHttp for ReqwestHubSpotHttpClient {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .map_err(|err| HubSpotWriteError::Retryable {
                code: "hubspot_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(HubSpotHttpResponse { status, body })
    }
}

pub trait HubSpotReadHttp: Send + Sync {
    fn get_json(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError>;
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError>;
}

impl HubSpotReadHttp for ReqwestHubSpotHttpClient {
    fn get_json(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| HubSpotReadError::Retryable {
                code: "hubspot_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(HubSpotHttpResponse { status, body })
    }

    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError> {
        HubSpotHttp::post_json(self, url, access_token, body).map_err(|err| match err {
            HubSpotWriteError::Retryable { code, message } => {
                HubSpotReadError::Retryable { code, message }
            }
            HubSpotWriteError::Permanent { code, message } => {
                HubSpotReadError::Limited { code, message }
            }
        })
    }
}

pub struct LiveHubSpotClient {
    http: Arc<dyn HubSpotHttp>,
    access_token: String,
}

impl LiveHubSpotClient {
    pub fn new(http: Arc<dyn HubSpotHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }
}

impl HubSpotExecutionClient for LiveHubSpotClient {
    fn create_note(
        &self,
        request: &HubSpotNoteCreateRequest,
    ) -> Result<HubSpotNoteCreateResponse, HubSpotWriteError> {
        validate_request(request)?;
        let body = serde_json::json!({
            "properties": {
                "hs_note_body": request.note_body,
                "hs_timestamp": request.occurred_at,
            }
        });
        let response = self.http.post_json(NOTES_URL, &self.access_token, &body)?;
        match response.status {
            200 | 201 => {
                let note_id = response
                    .body
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                Ok(HubSpotNoteCreateResponse {
                    status: HubSpotExecutionStatus {
                        executed: true,
                        dry_run: false,
                        reason: Some("hubspot_note_created".to_string()),
                    },
                    note_id,
                })
            }
            401 | 403 => Err(HubSpotWriteError::Permanent {
                code: "hubspot_auth_failed".to_string(),
                message: format!("status {}", response.status),
            }),
            429 | 500..=599 => Err(HubSpotWriteError::Retryable {
                code: "hubspot_rate_or_server".to_string(),
                message: format!("status {}", response.status),
            }),
            other => Err(HubSpotWriteError::Permanent {
                code: "hubspot_request_rejected".to_string(),
                message: format!("status {other}: {}", response.body),
            }),
        }
    }
}

/// Write-gated factory: disabled or unconfigured => dry-run client.
pub fn hubspot_execution_client(config: &HubSpotWriteConfig) -> Box<dyn HubSpotExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunHubSpotClient);
    }
    let Some(access_token) = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        tracing::warn!("hubspot_factory: write enabled but no access token - dry-run fallback");
        return Box::new(DryRunHubSpotClient);
    };
    Box::new(LiveHubSpotClient::new(
        Arc::new(ReqwestHubSpotHttpClient::default()),
        access_token.to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Records arm (crm_record_create): the HubSpot equivalent of the EspoCRM
// ensure-chain. Search a Company by name and a Contact by email/name; create
// only the missing ones; associate the contact to the company. Mirrors
// espocrm.rs's records client and rides the same BOS_HUBSPOT_WRITE_ENABLED gate.
// ---------------------------------------------------------------------------

const HUBSPOT_API_BASE: &str = "https://api.hubapi.com";

/// One company to ensure in HubSpot (CRM Company object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotCompanyInput {
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

/// One person to ensure in HubSpot (CRM Contact object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotContactInput {
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

impl HubSpotContactInput {
    fn full_name(&self) -> String {
        [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Outbox payload for `provider = "hubspot", capability = "create_records"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSpotRecordsCreateOutboxPayload {
    pub idempotency_key: String,
    pub approval: HubSpotApprovalMetadata,
    pub draft_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<HubSpotCompanyInput>,
    pub create_company: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<HubSpotContactInput>,
    pub create_contact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotRecordsCreateResponse {
    pub status: HubSpotExecutionStatus,
    pub company_id: Option<String>,
    pub contact_id: Option<String>,
}

fn validate_records(payload: &HubSpotRecordsCreateOutboxPayload) -> Result<(), HubSpotWriteError> {
    let permanent = |code: &str, message: &str| HubSpotWriteError::Permanent {
        code: code.to_string(),
        message: message.to_string(),
    };
    if !payload.approval.is_complete() {
        return Err(permanent(
            "hubspot_approval_missing",
            "hubspot write approval metadata is incomplete",
        ));
    }
    if payload.idempotency_key.trim().is_empty() {
        return Err(permanent(
            "hubspot_idempotency_key_missing",
            "hubspot write idempotency key is required",
        ));
    }
    if !payload.create_company && !payload.create_contact {
        return Err(permanent(
            "hubspot_records_nothing_proposed",
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
            "hubspot_company_name_missing",
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
            "hubspot_contact_name_missing",
            "a contact proposed for creation needs a name",
        ));
    }
    Ok(())
}

pub trait HubSpotRecordsExecutionClient: Send + Sync {
    fn create_records(
        &self,
        payload: &HubSpotRecordsCreateOutboxPayload,
    ) -> Result<HubSpotRecordsCreateResponse, HubSpotWriteError>;
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunHubSpotRecordsClient;

impl HubSpotRecordsExecutionClient for DryRunHubSpotRecordsClient {
    fn create_records(
        &self,
        payload: &HubSpotRecordsCreateOutboxPayload,
    ) -> Result<HubSpotRecordsCreateResponse, HubSpotWriteError> {
        validate_records(payload)?;
        Ok(HubSpotRecordsCreateResponse {
            status: HubSpotExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("hubspot_write_disabled_dry_run".to_string()),
            },
            company_id: payload.company.as_ref().map(|_| "dry-run".to_string()),
            contact_id: payload.contact.as_ref().map(|_| "dry-run".to_string()),
        })
    }
}

/// Records transport seam: searches + creates are POSTs; the association is a
/// PUT with no body. Split from the note client's POST-only seam.
pub trait HubSpotRecordsHttp: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError>;
    fn put_empty(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError>;
}

impl HubSpotRecordsHttp for ReqwestHubSpotHttpClient {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
        HubSpotHttp::post_json(self, url, access_token, body)
    }

    fn put_empty(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
        let response = self
            .client
            .put(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| HubSpotWriteError::Retryable {
                code: "hubspot_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(HubSpotHttpResponse { status, body })
    }
}

pub struct LiveHubSpotRecordsClient<C: HubSpotRecordsHttp = ReqwestHubSpotHttpClient> {
    http: Arc<C>,
    access_token: String,
}

impl<C: HubSpotRecordsHttp> LiveHubSpotRecordsClient<C> {
    pub fn new(http: Arc<C>, access_token: impl Into<String>) -> Self {
        Self {
            http,
            access_token: access_token.into(),
        }
    }

    fn check(
        &self,
        response: HubSpotHttpResponse,
        context: &str,
    ) -> Result<Value, HubSpotWriteError> {
        match response.status {
            200 | 201 | 204 => Ok(response.body),
            401 | 403 => Err(HubSpotWriteError::Permanent {
                code: "hubspot_auth_failed".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            429 | 500..=599 => Err(HubSpotWriteError::Retryable {
                code: "hubspot_rate_or_server".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            other => Err(HubSpotWriteError::Permanent {
                code: "hubspot_request_rejected".to_string(),
                message: format!("{context}: status {other}: {}", response.body),
            }),
        }
    }

    /// First id from a CRM search response (`{ "total", "results": [...] }`).
    fn first_result_id(body: &Value) -> Option<String> {
        body.get("results")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn created_id(body: &Value, context: &str) -> Result<String, HubSpotWriteError> {
        body.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| HubSpotWriteError::Permanent {
                code: "hubspot_response_invalid".to_string(),
                message: format!("{context}: created object has no id"),
            })
    }

    fn company_domain(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let candidate = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        let parsed = Url::parse(&candidate).ok()?;
        let host = parsed
            .host_str()?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let domain = host.strip_prefix("www.").unwrap_or(&host);
        (!domain.is_empty()).then(|| domain.to_string())
    }

    fn search(
        &self,
        object: &str,
        filters: Value,
        context: &str,
    ) -> Result<Option<String>, HubSpotWriteError> {
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/{object}/search");
        let body = serde_json::json!({
            "filterGroups": [{ "filters": filters }],
            "limit": 1,
        });
        let response = HubSpotRecordsHttp::post_json(&*self.http, &url, &self.access_token, &body)?;
        Ok(Self::first_result_id(&self.check(response, context)?))
    }

    /// Search a Company by exact name (read-only; produce-time + chain).
    pub fn find_company(&self, name: &str) -> Result<Option<String>, HubSpotWriteError> {
        self.search(
            "companies",
            serde_json::json!([{ "propertyName": "name", "operator": "EQ", "value": name }]),
            "company search",
        )
    }

    /// Search a Company by its normalized domain property. Read-only.
    pub fn find_company_by_domain(
        &self,
        domain: &str,
    ) -> Result<Option<String>, HubSpotWriteError> {
        let Some(domain) = Self::company_domain(domain) else {
            return Ok(None);
        };
        self.search(
            "companies",
            serde_json::json!([{ "propertyName": "domain", "operator": "EQ", "value": domain }]),
            "company search by domain",
        )
    }

    /// Search a Contact by email (preferred) then exact first+last name.
    pub fn find_contact(
        &self,
        email: Option<&str>,
        full_name: Option<&str>,
    ) -> Result<Option<String>, HubSpotWriteError> {
        if let Some(email) = email.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(id) = self.search(
                "contacts",
                serde_json::json!([{ "propertyName": "email", "operator": "EQ", "value": email }]),
                "contact search by email",
            )? {
                return Ok(Some(id));
            }
        }
        if let Some(full) = full_name.map(str::trim).filter(|s| !s.is_empty()) {
            let (first, last) = full.split_once(' ').unwrap_or((full, ""));
            let mut filters = vec![
                serde_json::json!({ "propertyName": "firstname", "operator": "EQ", "value": first }),
            ];
            if !last.is_empty() {
                filters.push(
                    serde_json::json!({ "propertyName": "lastname", "operator": "EQ", "value": last }),
                );
            }
            return self.search("contacts", Value::Array(filters), "contact search by name");
        }
        Ok(None)
    }

    fn ensure_company(
        &self,
        company: &HubSpotCompanyInput,
        allow_create: bool,
    ) -> Result<Option<String>, HubSpotWriteError> {
        if let Some(id) = self.find_company(&company.name)? {
            return Ok(Some(id));
        }
        if !allow_create {
            return Ok(None);
        }
        let mut props = serde_json::Map::new();
        props.insert("name".to_string(), Value::String(company.name.clone()));
        if let Some(domain) = company.website.as_deref().and_then(Self::company_domain) {
            props.insert("domain".to_string(), Value::String(domain));
        }
        if let Some(phone) = company.phone.as_deref() {
            props.insert("phone".to_string(), Value::String(phone.to_string()));
        }
        if let Some(address) = company.address.as_deref() {
            props.insert("address".to_string(), Value::String(address.to_string()));
        }
        if let Some(description) = company.description.as_deref() {
            props.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/companies");
        let body = serde_json::json!({ "properties": Value::Object(props) });
        let response = HubSpotRecordsHttp::post_json(&*self.http, &url, &self.access_token, &body)?;
        Self::created_id(&self.check(response, "company create")?, "company create").map(Some)
    }

    fn ensure_contact(
        &self,
        contact: &HubSpotContactInput,
        allow_create: bool,
        company_id: Option<&str>,
    ) -> Result<Option<String>, HubSpotWriteError> {
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
        let mut props = serde_json::Map::new();
        if let Some(first) = contact.first_name.as_deref() {
            props.insert("firstname".to_string(), Value::String(first.to_string()));
        }
        if let Some(last) = contact.last_name.as_deref() {
            props.insert("lastname".to_string(), Value::String(last.to_string()));
        }
        if let Some(email) = contact.email.as_deref() {
            props.insert("email".to_string(), Value::String(email.to_string()));
        }
        if let Some(phone) = contact.phone.as_deref() {
            props.insert("phone".to_string(), Value::String(phone.to_string()));
        }
        if let Some(title) = contact.title.as_deref() {
            props.insert("jobtitle".to_string(), Value::String(title.to_string()));
        }
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/contacts");
        let body = serde_json::json!({ "properties": Value::Object(props) });
        let response = HubSpotRecordsHttp::post_json(&*self.http, &url, &self.access_token, &body)?;
        let contact_id =
            Self::created_id(&self.check(response, "contact create")?, "contact create")?;
        // Default-associate the new contact to the company (v4 association).
        if let Some(company_id) = company_id {
            let assoc_url = format!(
                "{HUBSPOT_API_BASE}/crm/v4/objects/contact/{contact_id}/associations/default/company/{company_id}"
            );
            let response = self.http.put_empty(&assoc_url, &self.access_token)?;
            self.check(response, "contact-company association")?;
        }
        Ok(Some(contact_id))
    }
}

impl<C: HubSpotRecordsHttp> HubSpotRecordsExecutionClient for LiveHubSpotRecordsClient<C> {
    fn create_records(
        &self,
        payload: &HubSpotRecordsCreateOutboxPayload,
    ) -> Result<HubSpotRecordsCreateResponse, HubSpotWriteError> {
        validate_records(payload)?;
        let company_id = match payload.company.as_ref() {
            Some(company) => self.ensure_company(company, payload.create_company)?,
            None => None,
        };
        let contact_id = match payload.contact.as_ref() {
            Some(contact) => {
                self.ensure_contact(contact, payload.create_contact, company_id.as_deref())?
            }
            None => None,
        };
        Ok(HubSpotRecordsCreateResponse {
            status: HubSpotExecutionStatus {
                executed: true,
                dry_run: false,
                reason: Some("hubspot_records_created".to_string()),
            },
            company_id,
            contact_id,
        })
    }
}

/// Read-only produce-time search client (find_company / find_contact),
/// independent of the write gate. None when unconfigured.
pub fn hubspot_records_search_client(
    config: &HubSpotWriteConfig,
) -> Option<LiveHubSpotRecordsClient> {
    let access_token = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())?;
    Some(LiveHubSpotRecordsClient::new(
        Arc::new(ReqwestHubSpotHttpClient::default()),
        access_token.to_string(),
    ))
}

/// Write-gated factory: disabled or unconfigured => dry-run client.
pub fn hubspot_records_execution_client(
    config: &HubSpotWriteConfig,
) -> Box<dyn HubSpotRecordsExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunHubSpotRecordsClient);
    }
    let Some(access_token) = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        tracing::warn!(
            "hubspot_records_factory: write enabled but no access token - dry-run fallback"
        );
        return Box::new(DryRunHubSpotRecordsClient);
    };
    Box::new(LiveHubSpotRecordsClient::new(
        Arc::new(ReqwestHubSpotHttpClient::default()),
        access_token.to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Deal reporting read arm: read-only HubSpot deal search support for owner
// reporting. No env reads and no writes; callers provide a pipeline/stage/date
// mapping from env or overlay config and receive explicit pending/limited states.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealReadConfig {
    pub access_token: Option<String>,
    pub pipeline_id: Option<String>,
    pub open_stage_ids: Vec<String>,
    pub won_stage_ids: Vec<String>,
    pub lost_stage_ids: Vec<String>,
    pub started_date_property: Option<String>,
    pub closed_date_property: Option<String>,
    pub segment_properties: Vec<String>,
}

impl HubSpotDealReadConfig {
    pub fn missing_reason(&self) -> Option<String> {
        if self
            .access_token
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Some("hubspot access token is not configured".to_string());
        }
        if self
            .pipeline_id
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Some("HubSpot deal pipeline id is not configured".to_string());
        }
        if self.won_stage_ids.is_empty() || self.lost_stage_ids.is_empty() {
            return Some("HubSpot won/lost deal stage ids are not configured".to_string());
        }
        if self
            .started_date_property
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            || self
                .closed_date_property
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return Some("HubSpot started/closed date properties are not configured".to_string());
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HubSpotDealStage {
    pub stage_id: String,
    pub label: String,
    pub display_order: i32,
    pub probability: Option<f64>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HubSpotDealPipeline {
    pub pipeline_id: String,
    pub label: String,
    pub display_order: i32,
    pub archived: bool,
    pub stages: Vec<HubSpotDealStage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealDateProperty {
    pub name: String,
    pub label: String,
    pub field_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealStageCount {
    pub stage_id: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealSample {
    pub deal_id: String,
    pub name: String,
    pub stage_id: Option<String>,
    pub amount_cents: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealRecord {
    pub deal_id: String,
    pub stage_id: Option<String>,
    pub started_at_ms: Option<i64>,
    pub closed_at_ms: Option<i64>,
    pub segment_values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSpotDealReportingSnapshot {
    pub closed_deals: u64,
    pub won_deals: u64,
    pub lost_deals: u64,
    pub close_rate_bps: Option<u32>,
    pub avg_contact_to_close_days: Option<u32>,
    pub contact_to_close_sample: u64,
    pub segment_cuts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubSpotReadError {
    Limited { code: String, message: String },
    Retryable { code: String, message: String },
}

/// Deal-read transport seam. HubSpot CRM search is POST-only.
pub trait HubSpotDealReadHttp: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError>;
}

impl HubSpotDealReadHttp for ReqwestHubSpotHttpClient {
    fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<HubSpotHttpResponse, HubSpotReadError> {
        HubSpotHttp::post_json(self, url, access_token, body).map_err(|err| match err {
            HubSpotWriteError::Retryable { code, message } => {
                HubSpotReadError::Retryable { code, message }
            }
            HubSpotWriteError::Permanent { code, message } => {
                HubSpotReadError::Limited { code, message }
            }
        })
    }
}

pub struct LiveHubSpotDealReadClient<C: HubSpotReadHttp = ReqwestHubSpotHttpClient> {
    http: Arc<C>,
    access_token: String,
}

impl<C: HubSpotReadHttp> LiveHubSpotDealReadClient<C> {
    pub fn new(http: Arc<C>, access_token: impl Into<String>) -> Self {
        Self {
            http,
            access_token: access_token.into(),
        }
    }

    fn check(
        &self,
        response: HubSpotHttpResponse,
        context: &str,
    ) -> Result<Value, HubSpotReadError> {
        match response.status {
            200 => Ok(response.body),
            401 | 403 => Err(HubSpotReadError::Limited {
                code: "hubspot_deals_auth_or_plan_limited".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            429 | 500..=599 => Err(HubSpotReadError::Retryable {
                code: "hubspot_deals_rate_or_server".to_string(),
                message: format!("{context}: status {}", response.status),
            }),
            other => Err(HubSpotReadError::Limited {
                code: "hubspot_deals_request_rejected".to_string(),
                message: format!("{context}: status {other}: {}", response.body),
            }),
        }
    }

    pub fn discover_pipelines(&self) -> Result<Vec<HubSpotDealPipeline>, HubSpotReadError> {
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/pipelines/deals");
        let body = self.check(
            self.http.get_json(&url, &self.access_token)?,
            "deal pipelines",
        )?;
        let mut pipelines = body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_pipeline)
            .collect::<Vec<_>>();
        pipelines.sort_by_key(|pipeline| pipeline.display_order);
        Ok(pipelines)
    }

    pub fn discover_date_properties(
        &self,
    ) -> Result<Vec<HubSpotDealDateProperty>, HubSpotReadError> {
        let url = format!("{HUBSPOT_API_BASE}/crm/v3/properties/deals");
        let body = self.check(
            self.http.get_json(&url, &self.access_token)?,
            "deal properties",
        )?;
        let mut properties = body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_date_property)
            .collect::<Vec<_>>();
        properties.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then(left.name.cmp(&right.name))
        });
        Ok(properties)
    }

    fn property_string(record: &Value, name: &str) -> Option<String> {
        record
            .get("properties")
            .and_then(|props| props.get(name))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(str::to_string)
    }

    fn property_ms(record: &Value, name: &str) -> Option<i64> {
        let raw = Self::property_string(record, name)?;
        raw.parse::<i64>()
            .ok()
            .or_else(|| raw.get(0..10).and_then(yyyy_mm_dd_to_epoch_ms))
    }

    fn parse_records(
        body: &Value,
        started_property: &str,
        closed_property: &str,
        segment_properties: &[String],
    ) -> Vec<HubSpotDealRecord> {
        body.get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let deal_id = record.get("id").and_then(Value::as_str)?.to_string();
                let mut segment_values = BTreeMap::new();
                for property in segment_properties {
                    if let Some(value) = Self::property_string(record, property) {
                        segment_values.insert(property.clone(), value);
                    }
                }
                Some(HubSpotDealRecord {
                    deal_id,
                    stage_id: Self::property_string(record, "dealstage"),
                    started_at_ms: Self::property_ms(record, started_property),
                    closed_at_ms: Self::property_ms(record, closed_property),
                    segment_values,
                })
            })
            .collect()
    }

    pub fn search_deals(
        &self,
        pipeline_id: &str,
        stage_ids: &[String],
        date_property: &str,
        start_ms: i64,
        end_exclusive_ms: i64,
        properties: &[String],
    ) -> Result<Vec<Value>, HubSpotReadError> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        let mut page_count = 0_usize;
        loop {
            if page_count >= HUBSPOT_DEAL_SEARCH_MAX_PAGES {
                return Err(HubSpotReadError::Limited {
                    code: "hubspot_deals_page_budget_exceeded".to_string(),
                    message: format!("deal search exceeded {HUBSPOT_DEAL_SEARCH_MAX_PAGES} pages"),
                });
            }
            let mut body = serde_json::json!({
                "filterGroups": [{
                    "filters": [
                        { "propertyName": "pipeline", "operator": "EQ", "value": pipeline_id },
                        { "propertyName": "dealstage", "operator": "IN", "values": stage_ids },
                        { "propertyName": date_property, "operator": "GTE", "value": start_ms.to_string() },
                        { "propertyName": date_property, "operator": "LT", "value": end_exclusive_ms.to_string() }
                    ]
                }],
                "properties": properties,
                "limit": 100,
            });
            if let Some(after) = after.as_deref() {
                body["after"] = Value::String(after.to_string());
            }
            let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/deals/search");
            let response = self.http.post_json(&url, &self.access_token, &body)?;
            page_count += 1;
            let checked = self.check(response, "deal search")?;
            if let Some(results) = checked.get("results").and_then(Value::as_array) {
                all.extend(results.iter().cloned());
            }
            after = checked
                .get("paging")
                .and_then(|paging| paging.get("next"))
                .and_then(|next| next.get("after"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if after.is_none() {
                break;
            }
        }
        Ok(all)
    }

    pub fn open_stage_counts(
        &self,
        pipeline_id: &str,
        open_stage_ids: &[String],
    ) -> Result<Vec<HubSpotDealStageCount>, HubSpotReadError> {
        let deals = self.search_deals_unbounded(
            pipeline_id,
            open_stage_ids,
            &["dealstage".to_string()],
            HUBSPOT_DEAL_SEARCH_MAX_PAGES,
        )?;
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for deal in deals {
            if let Some(stage) = Self::property_string(&deal, "dealstage") {
                *counts.entry(stage).or_default() += 1;
            }
        }
        Ok(counts
            .into_iter()
            .map(|(stage_id, count)| HubSpotDealStageCount { stage_id, count })
            .collect())
    }

    pub fn sample_open_deals(
        &self,
        pipeline_id: &str,
        open_stage_ids: &[String],
        limit: usize,
    ) -> Result<Vec<HubSpotDealSample>, HubSpotReadError> {
        let deals = self.search_deals_unbounded(
            pipeline_id,
            open_stage_ids,
            &[
                "dealname".to_string(),
                "dealstage".to_string(),
                "amount".to_string(),
            ],
            HUBSPOT_DEAL_SAMPLE_MAX_PAGES,
        )?;
        Ok(deals
            .into_iter()
            .take(limit)
            .filter_map(|deal| {
                let deal_id = deal.get("id").and_then(Value::as_str)?.to_string();
                let name = Self::property_string(&deal, "dealname")
                    .unwrap_or_else(|| format!("Deal {deal_id}"));
                Some(HubSpotDealSample {
                    deal_id,
                    name,
                    stage_id: Self::property_string(&deal, "dealstage"),
                    amount_cents: Self::property_string(&deal, "amount")
                        .and_then(|raw| decimal_dollars_to_cents(&raw)),
                })
            })
            .collect())
    }

    fn search_deals_unbounded(
        &self,
        pipeline_id: &str,
        stage_ids: &[String],
        properties: &[String],
        max_pages: usize,
    ) -> Result<Vec<Value>, HubSpotReadError> {
        if stage_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        let mut page_count = 0_usize;
        loop {
            if page_count >= max_pages {
                return Err(HubSpotReadError::Limited {
                    code: "hubspot_deals_page_budget_exceeded".to_string(),
                    message: format!("deal search exceeded {max_pages} pages"),
                });
            }
            let mut body = serde_json::json!({
                "filterGroups": [{
                    "filters": [
                        { "propertyName": "pipeline", "operator": "EQ", "value": pipeline_id },
                        { "propertyName": "dealstage", "operator": "IN", "values": stage_ids }
                    ]
                }],
                "properties": properties,
                "limit": 100,
            });
            if let Some(after) = after.as_deref() {
                body["after"] = Value::String(after.to_string());
            }
            let url = format!("{HUBSPOT_API_BASE}/crm/v3/objects/deals/search");
            let response = self.http.post_json(&url, &self.access_token, &body)?;
            page_count += 1;
            let checked = self.check(response, "deal search")?;
            if let Some(results) = checked.get("results").and_then(Value::as_array) {
                all.extend(results.iter().cloned());
            }
            after = checked
                .get("paging")
                .and_then(|paging| paging.get("next"))
                .and_then(|next| next.get("after"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if after.is_none() {
                break;
            }
        }
        Ok(all)
    }

    pub fn reporting_snapshot(
        &self,
        config: &HubSpotDealReadConfig,
        start_ms: i64,
        end_exclusive_ms: i64,
    ) -> Result<HubSpotDealReportingSnapshot, HubSpotReadError> {
        if let Some(reason) = config.missing_reason() {
            return Err(HubSpotReadError::Limited {
                code: "hubspot_deals_config_missing".to_string(),
                message: reason,
            });
        }
        let pipeline_id = config.pipeline_id.as_deref().unwrap_or_default().trim();
        let started_property = config
            .started_date_property
            .as_deref()
            .unwrap_or_default()
            .trim();
        let closed_property = config
            .closed_date_property
            .as_deref()
            .unwrap_or_default()
            .trim();
        let mut stage_ids = config.won_stage_ids.clone();
        stage_ids.extend(config.lost_stage_ids.iter().cloned());
        stage_ids.sort();
        stage_ids.dedup();
        let mut properties = vec![
            "pipeline".to_string(),
            "dealstage".to_string(),
            started_property.to_string(),
            closed_property.to_string(),
        ];
        properties.extend(config.segment_properties.iter().cloned());
        properties.sort();
        properties.dedup();

        let raw = self.search_deals(
            pipeline_id,
            &stage_ids,
            closed_property,
            start_ms,
            end_exclusive_ms,
            &properties,
        )?;
        let deals = Self::parse_records(
            &serde_json::json!({ "results": raw }),
            started_property,
            closed_property,
            &config.segment_properties,
        );
        let mut won = 0_u64;
        let mut lost = 0_u64;
        let mut duration_days = Vec::new();
        let mut segment_counts: BTreeMap<String, u64> = BTreeMap::new();
        for deal in &deals {
            if let Some(stage) = deal.stage_id.as_deref() {
                if config.won_stage_ids.iter().any(|id| id == stage) {
                    won += 1;
                } else if config.lost_stage_ids.iter().any(|id| id == stage) {
                    lost += 1;
                }
            }
            if let (Some(started), Some(closed)) = (deal.started_at_ms, deal.closed_at_ms) {
                if closed >= started {
                    duration_days.push(((closed - started) / 86_400_000) as u64);
                }
            }
            for (property, value) in &deal.segment_values {
                let key = format!("{property}:{value}");
                *segment_counts.entry(key).or_default() += 1;
            }
        }
        let closed = won + lost;
        let close_rate_bps = (closed > 0).then(|| ((won * 10_000) / closed) as u32);
        let avg_contact_to_close_days = (!duration_days.is_empty())
            .then(|| (duration_days.iter().sum::<u64>() / duration_days.len() as u64) as u32);
        Ok(HubSpotDealReportingSnapshot {
            closed_deals: closed,
            won_deals: won,
            lost_deals: lost,
            close_rate_bps,
            avg_contact_to_close_days,
            contact_to_close_sample: duration_days.len() as u64,
            segment_cuts: segment_counts
                .into_iter()
                .map(|(segment, count)| format!("{segment}={count}"))
                .collect(),
        })
    }

    fn next_after(body: &Value) -> Option<String> {
        body.get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(|next| next.get("after"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn association_ids(record: &Value, association: &str) -> Vec<String> {
        record
            .get("associations")
            .and_then(|associations| associations.get(association))
            .and_then(|assoc| assoc.get("results"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }
}

impl<C: HubSpotReadHttp> crate::crm_read::CrmReadClient for LiveHubSpotDealReadClient<C> {
    fn list_contacts_page(
        &self,
        request: &crate::crm_read::CrmPageRequest,
    ) -> Result<
        crate::crm_read::CrmPage<crate::crm_read::CrmContactRecord>,
        crate::crm_read::CrmReadError,
    > {
        let encode = crate::qbo_oauth::encode_query_component;
        let page_size = request.effective_page_size();
        let properties = encode(
            "email,firstname,lastname,company,phone,lifecyclestage,hubspot_owner_id,notes_last_updated,lastmodifieddate",
        );
        let mut url = format!(
            "{HUBSPOT_API_BASE}/crm/v3/objects/contacts?limit={page_size}&properties={properties}"
        );
        if let Some(cursor) = request.cursor.as_deref() {
            url.push_str("&after=");
            url.push_str(&encode(cursor));
        }
        let response = self
            .http
            .get_json(&url, &self.access_token)
            .map_err(crm_read_error_from_hubspot)?;
        let body = self
            .check(response, "contact snapshot list")
            .map_err(crm_read_error_from_hubspot)?;
        let records = body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let provider_contact_id = record.get("id").and_then(Value::as_str)?.to_string();
                let first = Self::property_string(record, "firstname");
                let last = Self::property_string(record, "lastname");
                let name = match (first.as_deref(), last.as_deref()) {
                    (Some(first), Some(last)) => Some(format!("{first} {last}")),
                    (Some(first), None) => Some(first.to_string()),
                    (None, Some(last)) => Some(last.to_string()),
                    (None, None) => None,
                };
                Some(crate::crm_read::CrmContactRecord {
                    provider_contact_id,
                    email: Self::property_string(record, "email"),
                    name,
                    company: Self::property_string(record, "company"),
                    phone: Self::property_string(record, "phone"),
                    lifecycle_stage: Self::property_string(record, "lifecyclestage"),
                    owner: Self::property_string(record, "hubspot_owner_id"),
                    last_activity_at: Self::property_string(record, "notes_last_updated")
                        .or_else(|| Self::property_string(record, "lastmodifieddate")),
                })
            })
            .collect();
        Ok(crate::crm_read::CrmPage {
            records,
            next_cursor: Self::next_after(&body),
        })
    }

    fn list_deals_page(
        &self,
        request: &crate::crm_read::CrmPageRequest,
    ) -> Result<
        crate::crm_read::CrmPage<crate::crm_read::CrmDealRecord>,
        crate::crm_read::CrmReadError,
    > {
        let encode = crate::qbo_oauth::encode_query_component;
        let page_size = request.effective_page_size();
        let properties = encode("dealname,dealstage,amount,deal_currency_code,pipeline,closedate");
        let mut url = format!(
            "{HUBSPOT_API_BASE}/crm/v3/objects/deals?limit={page_size}&properties={properties}&associations=contacts"
        );
        if let Some(cursor) = request.cursor.as_deref() {
            url.push_str("&after=");
            url.push_str(&encode(cursor));
        }
        let response = self
            .http
            .get_json(&url, &self.access_token)
            .map_err(crm_read_error_from_hubspot)?;
        let body = self
            .check(response, "deal snapshot list")
            .map_err(crm_read_error_from_hubspot)?;
        let records = body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let provider_deal_id = record.get("id").and_then(Value::as_str)?.to_string();
                Some(crate::crm_read::CrmDealRecord {
                    provider_deal_id,
                    name: Self::property_string(record, "dealname"),
                    stage: Self::property_string(record, "dealstage"),
                    amount_cents: Self::property_string(record, "amount")
                        .and_then(|raw| decimal_dollars_to_cents(&raw)),
                    currency: Self::property_string(record, "deal_currency_code"),
                    pipeline: Self::property_string(record, "pipeline"),
                    close_date: Self::property_string(record, "closedate"),
                    associated_contact_ids: Self::association_ids(record, "contacts"),
                    associated_contact_email: None,
                    associated_contact_company: None,
                })
            })
            .collect();
        Ok(crate::crm_read::CrmPage {
            records,
            next_cursor: Self::next_after(&body),
        })
    }
}

fn crm_read_error_from_hubspot(err: HubSpotReadError) -> crate::crm_read::CrmReadError {
    match err {
        HubSpotReadError::Limited { code, message } => {
            crate::crm_read::CrmReadError::Permanent { code, message }
        }
        HubSpotReadError::Retryable { code, message } if code.contains("rate") => {
            crate::crm_read::CrmReadError::RateLimited {
                retry_after_ms: None,
                message,
            }
        }
        HubSpotReadError::Retryable { code, message } => {
            crate::crm_read::CrmReadError::Retryable { code, message }
        }
    }
}

pub fn hubspot_deal_read_client(
    config: &HubSpotDealReadConfig,
) -> Result<LiveHubSpotDealReadClient, String> {
    if let Some(reason) = config.missing_reason() {
        return Err(reason);
    }
    let access_token = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "hubspot access token is not configured".to_string())?;
    Ok(LiveHubSpotDealReadClient::new(
        Arc::new(ReqwestHubSpotHttpClient::default()),
        access_token.to_string(),
    ))
}

pub fn hubspot_deal_discovery_client(
    access_token: Option<String>,
) -> Result<LiveHubSpotDealReadClient, String> {
    let access_token = access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "hubspot access token is not configured".to_string())?;
    Ok(LiveHubSpotDealReadClient::new(
        Arc::new(ReqwestHubSpotHttpClient::default()),
        access_token.to_string(),
    ))
}

fn parse_pipeline(raw: &Value) -> Option<HubSpotDealPipeline> {
    let pipeline_id = raw.get("id").and_then(Value::as_str)?.to_string();
    let label = raw
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(&pipeline_id)
        .to_string();
    let display_order = raw.get("displayOrder").and_then(Value::as_i64).unwrap_or(0) as i32;
    let archived = raw
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut stages = raw
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_stage)
        .collect::<Vec<_>>();
    stages.sort_by_key(|stage| stage.display_order);
    Some(HubSpotDealPipeline {
        pipeline_id,
        label,
        display_order,
        archived,
        stages,
    })
}

fn parse_stage(raw: &Value) -> Option<HubSpotDealStage> {
    let stage_id = raw.get("id").and_then(Value::as_str)?.to_string();
    let label = raw
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(&stage_id)
        .to_string();
    let display_order = raw.get("displayOrder").and_then(Value::as_i64).unwrap_or(0) as i32;
    let archived = raw
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probability = raw
        .get("metadata")
        .and_then(|metadata| metadata.get("probability"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok());
    Some(HubSpotDealStage {
        stage_id,
        label,
        display_order,
        probability,
        archived,
    })
}

fn parse_date_property(raw: &Value) -> Option<HubSpotDealDateProperty> {
    let name = raw.get("name").and_then(Value::as_str)?.to_string();
    let field_type = raw.get("fieldType").and_then(Value::as_str)?.to_string();
    let property_type = raw.get("type").and_then(Value::as_str).unwrap_or_default();
    if property_type != "date" && property_type != "datetime" {
        return None;
    }
    let label = raw
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_string();
    Some(HubSpotDealDateProperty {
        name,
        label,
        field_type,
    })
}

fn decimal_dollars_to_cents(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let negative = trimmed.starts_with('-');
    let unsigned = trimmed.trim_start_matches('-');
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

fn yyyy_mm_dd_to_epoch_ms(raw: &str) -> Option<i64> {
    let mut parts = raw.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000)
}

// Howard Hinnant's days-from-civil algorithm, returning days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod records_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

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
        fn next(&self, url: &str) -> HubSpotHttpResponse {
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
            HubSpotHttpResponse { status, body }
        }
        fn exhausted(&self) {
            assert!(
                self.script.lock().expect("lock").is_empty(),
                "script not fully consumed"
            );
        }
    }

    impl HubSpotRecordsHttp for ScriptedRecordsHttp {
        fn post_json(
            &self,
            url: &str,
            _token: &str,
            body: &Value,
        ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
            self.post_bodies
                .lock()
                .expect("lock")
                .push((url.to_string(), body.clone()));
            Ok(self.next(url))
        }
        fn put_empty(
            &self,
            url: &str,
            _token: &str,
        ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
            Ok(self.next(url))
        }
    }

    fn approval() -> HubSpotApprovalMetadata {
        HubSpotApprovalMetadata {
            approval_id: "appr_crd_1".to_string(),
            approved_by: "jordan".to_string(),
            approved_at: "2026-06-12T12:00:00Z".to_string(),
        }
    }

    fn both_missing() -> HubSpotRecordsCreateOutboxPayload {
        HubSpotRecordsCreateOutboxPayload {
            idempotency_key: "crmrecords:crd_1".to_string(),
            approval: approval(),
            draft_ref: "crd_1".to_string(),
            company: Some(HubSpotCompanyInput {
                name: "example".to_string(),
                website: Some("https://www.example.test/about?utm=note".to_string()),
                phone: None,
                address: None,
                description: None,
            }),
            create_company: true,
            contact: Some(HubSpotContactInput {
                first_name: Some("Casey".to_string()),
                last_name: Some("Sullivan".to_string()),
                email: Some("casey@example.test".to_string()),
                phone: None,
                title: None,
            }),
            create_contact: true,
        }
    }

    #[test]
    fn ensure_chain_creates_both_and_associates() {
        // company search miss → create; contact search miss (email) → create →
        // associate.
        let http = Arc::new(ScriptedRecordsHttp::new(vec![
            (
                "companies/search",
                200,
                serde_json::json!({ "total": 0, "results": [] }),
            ),
            (
                "crm/v3/objects/companies",
                201,
                serde_json::json!({ "id": "comp-1" }),
            ),
            // contact lookup: email miss, then name miss.
            (
                "contacts/search",
                200,
                serde_json::json!({ "total": 0, "results": [] }),
            ),
            (
                "contacts/search",
                200,
                serde_json::json!({ "total": 0, "results": [] }),
            ),
            (
                "crm/v3/objects/contacts",
                201,
                serde_json::json!({ "id": "cont-1" }),
            ),
            (
                "crm/v4/objects/contact/cont-1/associations/default/company/comp-1",
                200,
                Value::Null,
            ),
        ]));
        let client = LiveHubSpotRecordsClient::new(http.clone(), "token");
        let out = client.create_records(&both_missing()).expect("created");
        assert_eq!(out.company_id.as_deref(), Some("comp-1"));
        assert_eq!(out.contact_id.as_deref(), Some("cont-1"));
        let company_body = http
            .post_bodies()
            .into_iter()
            .find_map(|(url, body)| url.ends_with("/crm/v3/objects/companies").then_some(body))
            .expect("company body");
        assert_eq!(
            company_body
                .get("properties")
                .and_then(|props| props.get("domain"))
                .and_then(Value::as_str),
            Some("example.test")
        );
        http.exhausted();
    }

    #[test]
    fn matched_company_is_not_recreated() {
        // company search HIT (create_company false in payload) → no company POST;
        // contact missing → created and associated to the matched company.
        let mut payload = both_missing();
        payload.create_company = false;
        let http = Arc::new(ScriptedRecordsHttp::new(vec![
            (
                "companies/search",
                200,
                serde_json::json!({ "total": 1, "results": [{ "id": "comp-existing" }] }),
            ),
            (
                "contacts/search",
                200,
                serde_json::json!({ "total": 0, "results": [] }),
            ),
            (
                "contacts/search",
                200,
                serde_json::json!({ "total": 0, "results": [] }),
            ),
            (
                "crm/v3/objects/contacts",
                201,
                serde_json::json!({ "id": "cont-2" }),
            ),
            (
                "crm/v4/objects/contact/cont-2/associations/default/company/comp-existing",
                200,
                Value::Null,
            ),
        ]));
        let client = LiveHubSpotRecordsClient::new(http.clone(), "token");
        let out = client.create_records(&payload).expect("created");
        assert_eq!(out.company_id.as_deref(), Some("comp-existing"));
        assert_eq!(out.contact_id.as_deref(), Some("cont-2"));
        // No company-create POST was made.
        assert!(!http
            .seen()
            .iter()
            .any(|u| u.ends_with("/objects/companies")));
        http.exhausted();
    }

    #[test]
    fn dry_run_validates_without_requests() {
        let out = DryRunHubSpotRecordsClient
            .create_records(&both_missing())
            .expect("dry-run ok");
        assert!(out.status.dry_run);
        assert_eq!(out.company_id.as_deref(), Some("dry-run"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeHttp {
        responses: Mutex<VecDeque<(u16, Value)>>,
        posts: Mutex<Vec<(String, Value)>>,
    }

    impl FakeHttp {
        fn new(responses: Vec<(u16, Value)>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                posts: Mutex::new(Vec::new()),
            }
        }
    }

    impl HubSpotHttp for FakeHttp {
        fn post_json(
            &self,
            url: &str,
            _access_token: &str,
            body: &Value,
        ) -> Result<HubSpotHttpResponse, HubSpotWriteError> {
            self.posts
                .lock()
                .expect("posts")
                .push((url.to_string(), body.clone()));
            let (status, body) = self
                .responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("scripted response");
            Ok(HubSpotHttpResponse { status, body })
        }
    }

    fn request() -> HubSpotNoteCreateRequest {
        HubSpotNoteCreateRequest {
            idempotency_key: "idem-1".to_string(),
            approval: HubSpotApprovalMetadata {
                approval_id: "appr-1".to_string(),
                approved_by: "jordan".to_string(),
                approved_at: "2026-06-10T12:00:00Z".to_string(),
            },
            note_body: "Call from Dana — wants the storefront quote.".to_string(),
            occurred_at: "2026-06-10T11:45:00Z".to_string(),
        }
    }

    #[test]
    fn factory_dry_runs_when_gate_closed_or_token_missing() {
        let gated = hubspot_execution_client(&HubSpotWriteConfig {
            access_token: Some("tok".to_string()),
            write_enabled: false,
        });
        let response = gated.create_note(&request()).expect("dry run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);

        let tokenless = hubspot_execution_client(&HubSpotWriteConfig {
            access_token: None,
            write_enabled: true,
        });
        let response = tokenless.create_note(&request()).expect("dry run");
        assert!(response.status.dry_run);
    }

    #[test]
    fn dry_run_still_validates_approval_and_body() {
        let mut incomplete = request();
        incomplete.approval.approved_by = String::new();
        let err = DryRunHubSpotClient
            .create_note(&incomplete)
            .expect_err("approval required");
        assert!(matches!(err, HubSpotWriteError::Permanent { ref code, .. }
            if code == "hubspot_approval_missing"));

        let mut empty = request();
        empty.note_body = "  ".to_string();
        assert!(DryRunHubSpotClient.create_note(&empty).is_err());
    }

    #[test]
    fn live_create_posts_note_properties_without_associations() {
        let http = Arc::new(FakeHttp::new(vec![(
            201,
            serde_json::json!({"id": "note-77"}),
        )]));
        let client = LiveHubSpotClient::new(http.clone(), "tok".to_string());
        let response = client.create_note(&request()).expect("created");
        assert!(response.status.executed);
        assert_eq!(response.note_id, "note-77");

        let posts = http.posts.lock().expect("posts");
        assert_eq!(posts.len(), 1);
        assert!(posts[0].0.ends_with("/crm/v3/objects/notes"));
        let body = posts[0].1.to_string();
        assert!(body.contains("hs_note_body"));
        assert!(body.contains("hs_timestamp"));
        // Free-tier posture (agent_monitor-proven): never touch the associations API.
        assert!(!body.contains("associations"));
    }

    #[test]
    fn status_codes_map_to_retry_classes() {
        for (status, retryable) in [(429u16, true), (503, true), (400, false), (401, false)] {
            let http = Arc::new(FakeHttp::new(vec![(status, Value::Null)]));
            let client = LiveHubSpotClient::new(http, "tok".to_string());
            let err = client.create_note(&request()).expect_err("must fail");
            match (retryable, err) {
                (true, HubSpotWriteError::Retryable { .. }) => {}
                (false, HubSpotWriteError::Permanent { .. }) => {}
                (_, other) => panic!("status {status}: wrong class {other:?}"),
            }
        }
    }
}
