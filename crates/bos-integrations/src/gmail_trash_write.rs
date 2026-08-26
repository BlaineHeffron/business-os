//! Write-gated Gmail message trashing. This is intentionally separate from
//! draft creation: opening the draft gate must never authorize mailbox cleanup.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::gmail_http::GmailHttp;
use crate::google_oauth::{fetch_access_token, has_scope, GoogleOAuthConfig};

const GMAIL_MODIFY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";

#[derive(Debug, Clone)]
pub struct GmailTrashWriteConfig {
    pub oauth: GoogleOAuthConfig,
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GmailTrashOutboxPayload {
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_user_id: Option<String>,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailTrashResponse {
    pub executed: bool,
    pub dry_run: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmailTrashWriteError {
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

pub trait GmailTrashExecutionClient: Send + Sync {
    fn trash_message(&self, message_id: &str) -> Result<GmailTrashResponse, GmailTrashWriteError>;
}

#[derive(Debug, Clone, Default)]
pub struct DryRunGmailTrashClient;

impl GmailTrashExecutionClient for DryRunGmailTrashClient {
    fn trash_message(&self, message_id: &str) -> Result<GmailTrashResponse, GmailTrashWriteError> {
        validate_message_id(message_id)?;
        Ok(GmailTrashResponse {
            executed: false,
            dry_run: true,
            reason: "gmail_trash_write_disabled_dry_run".to_string(),
        })
    }
}

pub struct LiveGmailTrashClient {
    http: Arc<dyn GmailHttp>,
    access_token: String,
}

impl LiveGmailTrashClient {
    pub fn from_credentials(
        http: Arc<dyn GmailHttp>,
        creds: &GoogleOAuthConfig,
    ) -> Result<Self, GmailTrashWriteError> {
        if !creds.scopes.is_empty() && !has_scope(creds, GMAIL_MODIFY_SCOPE) {
            return Err(GmailTrashWriteError::Permanent {
                code: "gmail_trash_scope_missing".to_string(),
                message: "gmail.modify scope absent from credentials".to_string(),
            });
        }
        let access_token =
            fetch_access_token(creds).map_err(|err| GmailTrashWriteError::Retryable {
                code: "gmail_trash_token_failed".to_string(),
                message: format!("{err:?}"),
                retry_after_ms: None,
            })?;
        Ok(Self { http, access_token })
    }

    #[cfg(test)]
    fn for_test(http: Arc<dyn GmailHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }
}

impl GmailTrashExecutionClient for LiveGmailTrashClient {
    fn trash_message(&self, message_id: &str) -> Result<GmailTrashResponse, GmailTrashWriteError> {
        validate_message_id(message_id)?;
        let url =
            format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/trash");
        self.http
            .post_json_with_meta(&url, &self.access_token, &serde_json::json!({}))
            .map_err(|failure| {
                let code = failure.error.code().to_string();
                if matches!(
                    code.as_str(),
                    "gmail_http_post_unavailable" | "gmail_http_post_send_failed"
                ) {
                    GmailTrashWriteError::Retryable {
                        code,
                        message: failure.error.message().to_string(),
                        retry_after_ms: failure.retry_after_ms,
                    }
                } else {
                    GmailTrashWriteError::Permanent {
                        code,
                        message: failure.error.message().to_string(),
                    }
                }
            })?;
        Ok(GmailTrashResponse {
            executed: true,
            dry_run: false,
            reason: "gmail_message_trashed".to_string(),
        })
    }
}

fn validate_message_id(message_id: &str) -> Result<(), GmailTrashWriteError> {
    if message_id.trim().is_empty() {
        return Err(GmailTrashWriteError::Permanent {
            code: "gmail_trash_message_id_missing".to_string(),
            message: "Gmail message id is required".to_string(),
        });
    }
    Ok(())
}

pub fn gmail_trash_execution_client(
    config: &GmailTrashWriteConfig,
) -> Result<Box<dyn GmailTrashExecutionClient>, GmailTrashWriteError> {
    if !config.write_enabled {
        return Ok(Box::new(DryRunGmailTrashClient));
    }
    LiveGmailTrashClient::from_credentials(
        Arc::new(crate::gmail_http::ReqwestGmailHttpClient::default()),
        &config.oauth,
    )
    .map(|client| Box::new(client) as Box<dyn GmailTrashExecutionClient>)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail_http::FakeGmailHttp;

    #[test]
    fn dry_run_never_calls_provider() {
        let response = DryRunGmailTrashClient
            .trash_message("msg-1")
            .expect("dry run");
        assert!(response.dry_run);
        assert!(!response.executed);
    }

    #[test]
    fn live_client_posts_to_gmail_trash_endpoint() {
        let http = Arc::new(FakeGmailHttp::new(vec![serde_json::json!({
            "id": "msg-1",
            "labelIds": ["TRASH"]
        })]));
        let client = LiveGmailTrashClient::for_test(http.clone(), "tok".to_string());
        let response = client.trash_message("msg-1").expect("trash");
        assert!(response.executed);
        let posts = http.posts.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(posts.len(), 1);
        assert!(posts[0].0.ends_with("/messages/msg-1/trash"));
    }

    #[test]
    fn factory_only_dry_runs_when_gate_is_closed() {
        let oauth = GoogleOAuthConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            refresh_token: "refresh".to_string(),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".to_string()],
            token_url: None,
        };
        let client = gmail_trash_execution_client(&GmailTrashWriteConfig {
            oauth: oauth.clone(),
            write_enabled: false,
        })
        .expect("closed gate dry-run client");
        assert!(client.trash_message("msg-1").expect("dry run").dry_run);

        let err = match gmail_trash_execution_client(&GmailTrashWriteConfig {
            oauth,
            write_enabled: true,
        }) {
            Ok(_) => panic!("open gate with missing scope must fail"),
            Err(err) => err,
        };
        assert_eq!(
            err,
            GmailTrashWriteError::Permanent {
                code: "gmail_trash_scope_missing".to_string(),
                message: "gmail.modify scope absent from credentials".to_string(),
            }
        );
    }
}
