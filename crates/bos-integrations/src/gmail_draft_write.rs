//! Gmail draft-create integration (minimal port from agent-monitor-rust
//! `gmail_drafts.rs` / `gmail_draft_live.rs`): ONE capability — create a
//! reply DRAFT in the operator's mailbox (`POST /gmail/v1/users/me/drafts`,
//! raw MIME + threadId so Gmail threads it onto the conversation).
//!
//! SENDING IS NOT HERE. Even with the write gate open, approval only stages
//! a draft in Gmail; the human sends it from Gmail. That matches the Demo
//! agreement's DRAFT→J|D posture for customer-facing email.
//!
//! Config-driven like the other write clients: [`GmailDraftWriteConfig`] is
//! built by the caller; `write_enabled = false` (default) => dry-run client.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::gmail_http::GmailHttp;
use crate::gmail_mime::build_raw_message_with_reply_headers;
use crate::google_oauth::{fetch_access_token, has_scope, GoogleOAuthConfig};

const DRAFTS_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/drafts";
const GMAIL_COMPOSE_SCOPE: &str = "https://www.googleapis.com/auth/gmail.compose";
const GMAIL_MODIFY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";

fn has_draft_scope(creds: &GoogleOAuthConfig) -> bool {
    has_scope(creds, GMAIL_COMPOSE_SCOPE) || has_scope(creds, GMAIL_MODIFY_SCOPE)
}

#[derive(Debug, Clone)]
pub struct GmailDraftWriteConfig {
    pub oauth: GoogleOAuthConfig,
    /// Execution gate. `false` => [`gmail_draft_execution_client`] returns
    /// the dry-run client.
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GmailDraftApprovalMetadata {
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: String,
}

impl GmailDraftApprovalMetadata {
    fn is_complete(&self) -> bool {
        !self.approval_id.trim().is_empty()
            && !self.approved_by.trim().is_empty()
            && !self.approved_at.trim().is_empty()
    }
}

/// Outbox payload for `provider = "gmail", capability = "create_draft"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GmailDraftCreateOutboxPayload {
    pub idempotency_key: String,
    /// Operator user whose Google credential delivers this write. 0e-2
    /// stamps the approver; 0e-3 will stamp the SOURCE account (the reply
    /// must be drafted in the mailbox that received the message). None =
    /// legacy jobs from before per-user credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_user_id: Option<String>,
    pub approval: GmailDraftApprovalMetadata,
    pub to: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    pub subject: String,
    pub body_text: String,
    /// Gmail thread to attach the draft to (reply threading).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// RFC Message-ID of the source message for the reply's In-Reply-To.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_message_id: Option<String>,
    /// RFC References chain for the reply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_message_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailDraftCreateRequest {
    pub idempotency_key: String,
    pub approval: GmailDraftApprovalMetadata,
    pub to: String,
    pub cc: Vec<String>,
    pub subject: String,
    pub body_text: String,
    pub thread_id: Option<String>,
    pub reply_message_id: Option<String>,
    pub reference_message_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailDraftExecutionStatus {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailDraftCreateResponse {
    pub status: GmailDraftExecutionStatus,
    /// Gmail draft object id ("dry-run" sentinel when not executed).
    pub draft_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmailDraftWriteError {
    Retryable {
        code: String,
        message: String,
        retry_after_ms: Option<u64>,
    },
    Permanent {
        code: String,
        message: String,
    },
}

pub trait GmailDraftExecutionClient: Send + Sync {
    fn create_draft(
        &self,
        request: &GmailDraftCreateRequest,
    ) -> Result<GmailDraftCreateResponse, GmailDraftWriteError>;
}

impl GmailDraftExecutionClient for Box<dyn GmailDraftExecutionClient> {
    fn create_draft(
        &self,
        request: &GmailDraftCreateRequest,
    ) -> Result<GmailDraftCreateResponse, GmailDraftWriteError> {
        (**self).create_draft(request)
    }
}

fn validate_request(request: &GmailDraftCreateRequest) -> Result<(), GmailDraftWriteError> {
    if !request.approval.is_complete() {
        return Err(GmailDraftWriteError::Permanent {
            code: "gmail_draft_approval_missing".to_string(),
            message: "gmail draft approval metadata is incomplete".to_string(),
        });
    }
    if request.idempotency_key.trim().is_empty() {
        return Err(GmailDraftWriteError::Permanent {
            code: "gmail_draft_idempotency_key_missing".to_string(),
            message: "gmail draft idempotency key is required".to_string(),
        });
    }
    if request.to.trim().is_empty() || !request.to.contains('@') {
        return Err(GmailDraftWriteError::Permanent {
            code: "gmail_draft_recipient_invalid".to_string(),
            message: "gmail draft recipient is missing or invalid".to_string(),
        });
    }
    if request.body_text.trim().is_empty() {
        return Err(GmailDraftWriteError::Permanent {
            code: "gmail_draft_body_empty".to_string(),
            message: "gmail draft body is empty".to_string(),
        });
    }
    Ok(())
}

/// Validates like the live client but never touches the network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunGmailDraftClient;

impl GmailDraftExecutionClient for DryRunGmailDraftClient {
    fn create_draft(
        &self,
        request: &GmailDraftCreateRequest,
    ) -> Result<GmailDraftCreateResponse, GmailDraftWriteError> {
        validate_request(request)?;
        Ok(GmailDraftCreateResponse {
            status: GmailDraftExecutionStatus {
                executed: false,
                dry_run: true,
                reason: Some("gmail_write_disabled_dry_run".to_string()),
            },
            draft_id: "dry-run".to_string(),
        })
    }
}

pub struct LiveGmailDraftClient {
    http: Arc<dyn GmailHttp>,
    access_token: String,
}

impl LiveGmailDraftClient {
    /// Exchanges the refresh token for an access token. Fails without network
    /// when a populated scope list lacks a draft-capable scope.
    pub fn from_credentials(
        http: Arc<dyn GmailHttp>,
        creds: &GoogleOAuthConfig,
    ) -> Result<Self, GmailDraftWriteError> {
        if !creds.scopes.is_empty() && !has_draft_scope(creds) {
            return Err(GmailDraftWriteError::Permanent {
                code: "gmail_draft_scope_missing".to_string(),
                message: "gmail.compose (or gmail.modify) scope absent from credentials"
                    .to_string(),
            });
        }
        let access_token =
            fetch_access_token(creds).map_err(|err| GmailDraftWriteError::Retryable {
                code: "gmail_draft_token_failed".to_string(),
                message: format!("{err:?}"),
                retry_after_ms: None,
            })?;
        Ok(Self { http, access_token })
    }

    pub fn for_test(http: Arc<dyn GmailHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }
}

impl GmailDraftExecutionClient for LiveGmailDraftClient {
    fn create_draft(
        &self,
        request: &GmailDraftCreateRequest,
    ) -> Result<GmailDraftCreateResponse, GmailDraftWriteError> {
        validate_request(request)?;
        let raw = build_raw_message_with_reply_headers(
            None,
            std::slice::from_ref(&request.to),
            &request.cc,
            &request.subject,
            &request.body_text,
            request.reply_message_id.as_deref(),
            &request.reference_message_ids,
        );
        let mut message = serde_json::json!({ "raw": raw });
        if let Some(thread_id) = request.thread_id.as_deref() {
            message["threadId"] = serde_json::Value::String(thread_id.to_string());
        }
        let body = serde_json::json!({ "message": message });
        let response = self
            .http
            .post_json_with_meta(DRAFTS_URL, &self.access_token, &body)
            .map_err(|failure| {
                // gmail_http classifies status into stable codes; 429/5xx and
                // transport failures are the retryable ones.
                let code = failure.error.code().to_string();
                let retryable = matches!(
                    code.as_str(),
                    "gmail_http_post_unavailable" | "gmail_http_post_send_failed"
                );
                if retryable {
                    GmailDraftWriteError::Retryable {
                        code,
                        retry_after_ms: failure.retry_after_ms,
                        message: failure.error.message().to_string(),
                    }
                } else {
                    GmailDraftWriteError::Permanent {
                        code,
                        message: failure.error.message().to_string(),
                    }
                }
            })?
            .body;
        let draft_id = response
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        Ok(GmailDraftCreateResponse {
            status: GmailDraftExecutionStatus {
                executed: true,
                dry_run: false,
                reason: Some("gmail_draft_created".to_string()),
            },
            draft_id,
        })
    }
}

/// Write-gated factory: disabled, scope-less, or token-failed => dry-run.
pub fn gmail_draft_execution_client(
    config: &GmailDraftWriteConfig,
) -> Box<dyn GmailDraftExecutionClient> {
    if !config.write_enabled {
        return Box::new(DryRunGmailDraftClient);
    }
    if !config.oauth.scopes.is_empty() && !has_draft_scope(&config.oauth) {
        tracing::warn!("gmail_draft_factory: compose scope missing - dry-run fallback");
        return Box::new(DryRunGmailDraftClient);
    }
    let http = Arc::new(crate::gmail_http::ReqwestGmailHttpClient::default());
    match LiveGmailDraftClient::from_credentials(http, &config.oauth) {
        Ok(client) => Box::new(client),
        Err(err) => {
            tracing::warn!(error = ?err, "gmail_draft_factory: live client failed - dry-run");
            Box::new(DryRunGmailDraftClient)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail_http::{FakeGmailHttp, GmailHttpFailure, GmailJsonResponse};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};

    fn request() -> GmailDraftCreateRequest {
        GmailDraftCreateRequest {
            idempotency_key: "idem-1".to_string(),
            approval: GmailDraftApprovalMetadata {
                approval_id: "appr-1".to_string(),
                approved_by: "casey".to_string(),
                approved_at: "2026-06-10T12:00:00Z".to_string(),
            },
            to: "dana@example.test".to_string(),
            cc: vec!["ops@example.test".to_string()],
            subject: "Re: storefront quote".to_string(),
            body_text: "Hi Dana — happy to quote that; could you share the square footage?"
                .to_string(),
            thread_id: Some("thread-9".to_string()),
            reply_message_id: Some("<source-message@example.test>".to_string()),
            reference_message_ids: vec![
                "<root-message@example.test>".to_string(),
                "<source-message@example.test>".to_string(),
            ],
        }
    }

    #[test]
    fn dry_run_validates_and_reports_gate() {
        let response = DryRunGmailDraftClient
            .create_draft(&request())
            .expect("dry run");
        assert!(response.status.dry_run);
        assert!(!response.status.executed);

        let mut no_recipient = request();
        no_recipient.to = "not-an-email".to_string();
        assert!(DryRunGmailDraftClient.create_draft(&no_recipient).is_err());

        let mut no_approval = request();
        no_approval.approval.approved_by = String::new();
        assert!(DryRunGmailDraftClient.create_draft(&no_approval).is_err());
    }

    #[test]
    fn live_create_posts_raw_mime_with_thread_id() {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![serde_json::json!({
            "id": "draft-42",
            "message": {"threadId": "thread-9"},
        })]));
        let client = LiveGmailDraftClient::for_test(http.clone(), "tok".to_string());
        let response = client.create_draft(&request()).expect("created");
        assert!(response.status.executed);
        assert_eq!(response.draft_id, "draft-42");

        let posts = http.posts.lock().expect("posts");
        assert_eq!(posts.len(), 1);
        assert!(posts[0].0.ends_with("/users/me/drafts"));
        assert_eq!(posts[0].1["message"]["threadId"], "thread-9");
        let raw = posts[0].1["message"]["raw"].as_str().expect("raw");
        let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(raw).expect("b64")).expect("utf8");
        assert!(decoded.contains("To: dana@example.test"));
        assert!(decoded.contains("Cc: ops@example.test"));
        assert!(decoded.contains("Subject: Re: storefront quote"));
        assert!(decoded.contains("In-Reply-To: <source-message@example.test>"));
        assert!(decoded
            .contains("References: <root-message@example.test> <source-message@example.test>"));
        assert!(decoded.contains("square footage"));
    }

    struct RetryAfterHttp;

    impl GmailHttp for RetryAfterHttp {
        fn get_json(&self, _url: &str, _access_token: &str) -> AppResult<serde_json::Value> {
            unreachable!("draft create does not GET")
        }

        fn post_json(
            &self,
            _url: &str,
            _access_token: &str,
            _body: &serde_json::Value,
        ) -> AppResult<serde_json::Value> {
            unreachable!("draft create uses post_json_with_meta")
        }

        fn post_json_with_meta(
            &self,
            _url: &str,
            _access_token: &str,
            _body: &serde_json::Value,
        ) -> Result<GmailJsonResponse, GmailHttpFailure> {
            Err(GmailHttpFailure {
                error: AppError::new(
                    ErrorCode::ExternalDependency,
                    "gmail_http_post_unavailable",
                    "status 429 reason=quotaExceeded",
                    CorrelationId::new("corr_test"),
                ),
                retry_after_ms: Some(17_000),
            })
        }
    }

    #[test]
    fn live_create_preserves_structural_retry_after() {
        let client =
            LiveGmailDraftClient::for_test(std::sync::Arc::new(RetryAfterHttp), "tok".to_string());
        match client.create_draft(&request()).expect_err("rate limited") {
            GmailDraftWriteError::Retryable {
                code,
                retry_after_ms,
                ..
            } => {
                assert_eq!(code, "gmail_http_post_unavailable");
                assert_eq!(retry_after_ms, Some(17_000));
            }
            other => panic!("expected retryable, got {other:?}"),
        }
    }

    #[test]
    fn factory_dry_runs_when_gate_closed_or_scope_missing() {
        let oauth = GoogleOAuthConfig {
            client_id: "app".to_string(),
            client_secret: "secret".to_string(),
            refresh_token: "refresh".to_string(),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".to_string()],
            token_url: None,
        };
        // Gate closed.
        let gated = gmail_draft_execution_client(&GmailDraftWriteConfig {
            oauth: oauth.clone(),
            write_enabled: false,
        });
        assert!(
            gated
                .create_draft(&request())
                .expect("dry run")
                .status
                .dry_run
        );

        // Gate open but readonly-only scopes: dry-run, no token fetch.
        let scopeless = gmail_draft_execution_client(&GmailDraftWriteConfig {
            oauth,
            write_enabled: true,
        });
        assert!(
            scopeless
                .create_draft(&request())
                .expect("dry run")
                .status
                .dry_run
        );
    }
}
