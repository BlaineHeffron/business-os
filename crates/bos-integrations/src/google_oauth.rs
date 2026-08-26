//! Google OAuth refresh-token exchange. Config-driven: credentials are passed in
//! as [`GoogleOAuthConfig`] by the caller — this module never reads env vars.
//! An optional JSON state-file resolver is provided for callers that persist
//! OAuth material on disk; the path is an explicit argument.

use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};
use serde_json::Value;
use std::path::Path;

const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    /// Scopes the refresh token was granted. Empty = unknown (scope checks are
    /// skipped by callers when the list is empty).
    pub scopes: Vec<String>,
    /// Token endpoint override (tests/proxies). `None` = Google's endpoint.
    pub token_url: Option<String>,
}

impl std::fmt::Debug for GoogleOAuthConfig {
    // Hand-written so a stray `{:?}` cannot dump the refresh token / client
    // secret. client_id and scopes are not secret and stay visible.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleOAuthConfig")
            .field("refresh_token", &"[redacted]")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("scopes", &self.scopes)
            .field("token_url", &self.token_url)
            .finish()
    }
}

fn google_oauth_error(code: &'static str, message: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::ExternalDependency,
        code,
        message.to_string(),
        CorrelationId::generate(),
    )
}

fn google_oauth_rejected_error(code: &'static str, message: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::Unauthorized,
        code,
        message.to_string(),
        CorrelationId::generate(),
    )
}

/// Resolve credentials for `service` (e.g. `"gmail"`) from a JSON OAuth state
/// file at `path`. Layout matches the predecessor's `.google_oauth.json`:
/// per-service `refreshToken`/`clientId`/`clientSecret`/`scopes` with top-level
/// `clientId`/`clientSecret`/`scopes` fallbacks. Returns `None` when the file
/// is absent, unparsable, or missing required fields.
pub fn resolve_credentials_from_state_file(
    path: &Path,
    service: &str,
) -> Option<GoogleOAuthConfig> {
    let state = read_json_file(path)?;
    let refresh_token = nested_non_empty_string(&state, &[service, "refreshToken"])?.to_string();
    let client_id = nested_non_empty_string(&state, &[service, "clientId"])
        .or_else(|| nested_non_empty_string(&state, &["clientId"]))?
        .to_string();
    let client_secret = nested_non_empty_string(&state, &[service, "clientSecret"])
        .or_else(|| nested_non_empty_string(&state, &["clientSecret"]))?
        .to_string();
    let scopes = read_scopes(&state, service);
    Some(GoogleOAuthConfig {
        client_id,
        client_secret,
        refresh_token,
        scopes,
        token_url: None,
    })
}

fn read_json_file(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
}

fn nested_non_empty_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in keys {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn read_scopes(state: &Value, service: &str) -> Vec<String> {
    let raw = state
        .get(service)
        .and_then(|svc| svc.get("scopes"))
        .or_else(|| state.get("scopes"));
    match raw {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(text)) => parse_scope_list(text),
        _ => Vec::new(),
    }
}

/// Split a scope list on both ',' and ' ', trimming and dropping empties.
pub fn parse_scope_list(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn has_scope(creds: &GoogleOAuthConfig, scope: &str) -> bool {
    creds.scopes.iter().any(|s| s == scope)
}

pub fn fetch_access_token(creds: &GoogleOAuthConfig) -> AppResult<String> {
    // Bound connect + total time so a hung token endpoint cannot pin the
    // calling blocking worker thread indefinitely.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let token_url = creds.token_url.as_deref().unwrap_or(GOOGLE_TOKEN_URI);
    // `scope` is intentionally omitted: refresh-token grants reuse the previously-approved scopes.
    let response = client
        .post(token_url)
        .form(&[
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("refresh_token", creds.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(|err| google_oauth_error("google_oauth_token_request_failed", err))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.json::<Value>().unwrap_or(Value::Null);
        let provider_error = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let provider_description = body
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message = if provider_error.is_empty() {
            format!("token endpoint returned {status}")
        } else if provider_description.is_empty() {
            format!("token endpoint returned {status} {provider_error}")
        } else {
            format!("token endpoint returned {status} {provider_error}: {provider_description}")
        };
        if provider_error == "invalid_grant" {
            return Err(google_oauth_rejected_error(
                "google_oauth_invalid_grant",
                message,
            ));
        }
        return Err(google_oauth_error("google_oauth_token_status", message));
    }
    let body = response
        .json::<Value>()
        .map_err(|err| google_oauth_error("google_oauth_token_parse_failed", err))?;
    body.get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            google_oauth_error(
                "google_oauth_token_missing",
                "access_token absent in response",
            )
        })
}

const GOOGLE_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Build the Google consent URL for the authorization-code "connect" flow.
/// `access_type=offline` + `prompt=select_account consent` makes the operator
/// choose the intended Google account and forces a refresh token in the
/// exchange response.
pub fn authorization_consent_url(
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let encode = |raw: &str| utf8_percent_encode(raw, NON_ALPHANUMERIC).to_string();
    format!(
        "{GOOGLE_AUTH_URI}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&access_type=offline&prompt={}",
        encode(client_id),
        encode(redirect_uri),
        encode(&scopes.join(" ")),
        encode(state),
        encode("select_account consent"),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCodeGrant {
    pub refresh_token: String,
    pub scopes: Vec<String>,
}

/// Exchange an authorization code (from the consent redirect) for a refresh
/// token. `token_url` override is for tests.
pub fn exchange_authorization_code(
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
    token_url: Option<&str>,
) -> AppResult<AuthorizationCodeGrant> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let response = client
        .post(token_url.unwrap_or(GOOGLE_TOKEN_URI))
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect_uri),
            ("code", code),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .map_err(|err| google_oauth_error("google_oauth_code_exchange_failed", err))?;
    if !response.status().is_success() {
        return Err(google_oauth_error(
            "google_oauth_code_exchange_status",
            format!("token endpoint returned {}", response.status().as_u16()),
        ));
    }
    let body = response
        .json::<Value>()
        .map_err(|err| google_oauth_error("google_oauth_code_exchange_parse_failed", err))?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            google_oauth_error(
                "google_oauth_refresh_token_missing",
                "refresh_token absent in exchange response (was prompt=consent used?)",
            )
        })?;
    let scopes = body
        .get("scope")
        .and_then(Value::as_str)
        .map(parse_scope_list)
        .unwrap_or_default();
    Ok(AuthorizationCodeGrant {
        refresh_token,
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_oauth_secrets() {
        let creds = GoogleOAuthConfig {
            refresh_token: "rt-super-secret".to_string(),
            client_id: "cid-public-123".to_string(),
            client_secret: "cs-super-secret".to_string(),
            scopes: vec!["scope".to_string()],
            token_url: None,
        };
        let rendered = format!("{creds:?}");
        assert!(
            !rendered.contains("rt-super-secret"),
            "refresh_token leaked: {rendered}"
        );
        assert!(
            !rendered.contains("cs-super-secret"),
            "client_secret leaked: {rendered}"
        );
        // Non-secret identifier is retained for debuggability.
        assert!(rendered.contains("cid-public-123"));
    }

    #[test]
    fn state_file_resolves_service_credentials_with_top_level_fallbacks() {
        let dir = std::env::temp_dir().join(format!(
            "bos_integrations_oauth_state_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("google_oauth.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "clientId": "top-client-id",
                "clientSecret": "top-client-secret",
                "gmail": {
                    "refreshToken": "gmail-refresh",
                    "scopes": ["https://www.googleapis.com/auth/gmail.readonly"]
                }
            })
            .to_string(),
        )
        .expect("write state file");

        let creds =
            resolve_credentials_from_state_file(&path, "gmail").expect("resolve credentials");

        assert_eq!(creds.refresh_token, "gmail-refresh");
        assert_eq!(creds.client_id, "top-client-id");
        assert_eq!(creds.client_secret, "top-client-secret");
        assert!(has_scope(
            &creds,
            "https://www.googleapis.com/auth/gmail.readonly"
        ));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn missing_state_file_resolves_none() {
        let missing = Path::new("/nonexistent/path/google_oauth.json");
        assert!(resolve_credentials_from_state_file(missing, "gmail").is_none());
    }

    #[test]
    fn state_file_string_scopes_accept_commas() {
        let state = serde_json::json!({
            "gmail": {
                "scopes": "https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.send"
            }
        });

        let scopes = read_scopes(&state, "gmail");

        assert_eq!(
            scopes,
            vec![
                "https://www.googleapis.com/auth/gmail.readonly".to_string(),
                "https://www.googleapis.com/auth/gmail.send".to_string(),
            ]
        );
    }

    #[test]
    fn scope_list_splits_on_commas_and_spaces() {
        assert_eq!(
            parse_scope_list("a,b c , ,d"),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn has_scope_checks_membership() {
        let creds = GoogleOAuthConfig {
            refresh_token: "tok".to_string(),
            client_id: "cid".to_string(),
            client_secret: "cs".to_string(),
            scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".to_string()],
            token_url: None,
        };

        assert!(has_scope(
            &creds,
            "https://www.googleapis.com/auth/gmail.readonly"
        ));
        assert!(!has_scope(
            &creds,
            "https://www.googleapis.com/auth/gmail.send"
        ));
    }

    #[test]
    fn consent_url_forces_account_selection() {
        let url = authorization_consent_url(
            "cid",
            "https://ops.example.test/api/connectors/google/callback",
            &["https://www.googleapis.com/auth/gmail.readonly".to_string()],
            "st_test",
        );

        assert!(url.contains("prompt=select%5Faccount%20consent"), "{url}");
        assert!(url.contains("access_type=offline"), "{url}");
    }

    // Exercises the token_url override end-to-end against a local one-shot HTTP
    // responder — no real Google endpoint involved.
    #[test]
    fn fetch_access_token_uses_token_url_override() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"access_token":"local-access-token"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let creds = GoogleOAuthConfig {
            refresh_token: "rt".to_string(),
            client_id: "cid".to_string(),
            client_secret: "cs".to_string(),
            scopes: Vec::new(),
            token_url: Some(format!("http://{addr}/token")),
        };

        let token = fetch_access_token(&creds).expect("token from local endpoint");
        assert_eq!(token, "local-access-token");
        let _ = server.join();
    }

    #[test]
    fn fetch_access_token_preserves_invalid_grant_code() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let creds = GoogleOAuthConfig {
            refresh_token: "rt".to_string(),
            client_id: "cid".to_string(),
            client_secret: "cs".to_string(),
            scopes: Vec::new(),
            token_url: Some(format!("http://{addr}/token")),
        };

        let error = fetch_access_token(&creds).expect_err("invalid grant is rejected");
        assert_eq!(error.code(), "google_oauth_invalid_grant");
        assert_eq!(error.kind(), ErrorCode::Unauthorized);
        assert!(error.message().contains("expired or revoked"));
        let _ = server.join();
    }
}
