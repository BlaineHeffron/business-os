//! QuickBooks Online OAuth2: consent URL, authorization-code exchange, and
//! refresh-token grants. Config-driven — this module never reads env vars.
//!
//! TWO QBO-specific facts shape every caller:
//! - Intuit ROTATES the refresh token on every token response. The grant
//!   returned here must be persisted IMMEDIATELY; the previous refresh token
//!   stays valid only ~24h after rotation (crash-window grace, not a feature).
//! - The token response does NOT carry the company (realm) id. It arrives as
//!   a `realmId` query parameter on the OAuth callback redirect — the route
//!   layer must capture and store it alongside the credential.

use serde_json::Value;

use crate::accounting_read::AccountingError;

const QBO_AUTHORIZATION_ENDPOINT: &str = "https://appcenter.intuit.com/connect/oauth2";
const QBO_TOKEN_ENDPOINT: &str = "https://oauth.platform.intuit.com/oauth2/v1/tokens/bearer";
/// The full accounting read scope — QBO has no finer-grained read-only scope;
/// read-only posture is enforced by our client (GET-only transport).
pub const QBO_ACCOUNTING_SCOPE: &str = "com.intuit.quickbooks.accounting";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QboEnvironment {
    Sandbox,
    Production,
}

impl QboEnvironment {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sandbox" => Some(Self::Sandbox),
            "production" => Some(Self::Production),
            _ => None,
        }
    }

    pub fn api_base_url(self) -> &'static str {
        match self {
            Self::Sandbox => "https://sandbox-quickbooks.api.intuit.com",
            Self::Production => "https://quickbooks.api.intuit.com",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Production => "production",
        }
    }
}

/// OAuth app credentials (NOT per-company tokens) — caller-supplied.
#[derive(Clone)]
pub struct QboOAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub environment: QboEnvironment,
    /// Token endpoint override (tests). `None` = Intuit's endpoint.
    pub token_url: Option<String>,
}

impl std::fmt::Debug for QboOAuthApp {
    // Hand-written so a stray `{:?}` cannot dump the client secret.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QboOAuthApp")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("environment", &self.environment)
            .field("token_url", &self.token_url)
            .finish()
    }
}

/// One token response. `refresh_token` is ROTATED — persist before anything
/// else touches the grant.
#[derive(Clone, PartialEq, Eq)]
pub struct QboTokenGrant {
    pub access_token: String,
    pub access_token_expires_at_ms: u64,
    pub refresh_token: String,
    pub refresh_token_expires_at_ms: u64,
}

impl std::fmt::Debug for QboTokenGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QboTokenGrant")
            .field("access_token", &"[redacted]")
            .field(
                "access_token_expires_at_ms",
                &self.access_token_expires_at_ms,
            )
            .field("refresh_token", &"[redacted]")
            .field(
                "refresh_token_expires_at_ms",
                &self.refresh_token_expires_at_ms,
            )
            .finish()
    }
}

/// Query-component encoding that leaves RFC 3986 unreserved characters
/// (alphanumeric plus `-._~`) literal. Intuit compares the redirect_uri
/// LITERALLY against the registered value — over-encoding dots/hyphens
/// (`%2E`/`%2D`) is spec-equivalent but gets rejected as invalid_redirect_uri.
const QUERY_COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub(crate) fn encode_query_component(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, QUERY_COMPONENT).to_string()
}

/// Consent URL for the connect flow. The callback receives `code`, `state`,
/// AND `realmId`.
pub fn authorization_consent_url(app: &QboOAuthApp, redirect_uri: &str, state: &str) -> String {
    format!(
        "{QBO_AUTHORIZATION_ENDPOINT}?client_id={}&response_type=code&scope={}&redirect_uri={}&state={}",
        encode_query_component(&app.client_id),
        encode_query_component(QBO_ACCOUNTING_SCOPE),
        encode_query_component(redirect_uri),
        encode_query_component(state),
    )
}

pub fn exchange_authorization_code(
    app: &QboOAuthApp,
    redirect_uri: &str,
    code: &str,
    now_ms: u64,
) -> Result<QboTokenGrant, AccountingError> {
    token_request(
        app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ],
        now_ms,
    )
}

pub fn refresh_access_token(
    app: &QboOAuthApp,
    refresh_token: &str,
    now_ms: u64,
) -> Result<QboTokenGrant, AccountingError> {
    token_request(
        app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
        now_ms,
    )
}

/// Seam so the sync pump's cycle core is testable without HTTP.
pub trait QboTokenRefresher: Send + Sync {
    fn refresh(&self, refresh_token: &str, now_ms: u64) -> Result<QboTokenGrant, AccountingError>;
}

pub struct LiveQboTokenRefresher {
    pub app: QboOAuthApp,
}

impl QboTokenRefresher for LiveQboTokenRefresher {
    fn refresh(&self, refresh_token: &str, now_ms: u64) -> Result<QboTokenGrant, AccountingError> {
        refresh_access_token(&self.app, refresh_token, now_ms)
    }
}

fn token_request(
    app: &QboOAuthApp,
    form: &[(&str, &str)],
    now_ms: u64,
) -> Result<QboTokenGrant, AccountingError> {
    // Bound connect + total time so a hung token endpoint cannot pin the
    // calling blocking worker thread indefinitely.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let token_url = app.token_url.as_deref().unwrap_or(QBO_TOKEN_ENDPOINT);
    let response = client
        .post(token_url)
        .basic_auth(&app.client_id, Some(&app.client_secret))
        .header("Accept", "application/json")
        .form(form)
        .send()
        .map_err(|err| AccountingError::Retryable {
            code: "qbo_token_request_failed".to_string(),
            message: err.to_string(),
        })?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().unwrap_or(Value::Null);
    if status == 429 {
        return Err(AccountingError::RateLimited {
            retry_after_ms: None,
            message: "token endpoint rate limited".to_string(),
        });
    }
    if !(200..300).contains(&status) {
        // 400 invalid_grant = the refresh token is dead (revoked/expired):
        // the operator must reconnect — permanent, not retryable.
        let error_code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let err = format!("token endpoint returned {status} {error_code}");
        return Err(if (500..600).contains(&status) {
            AccountingError::Retryable {
                code: "qbo_token_status".to_string(),
                message: err,
            }
        } else {
            AccountingError::Permanent {
                code: "qbo_token_rejected".to_string(),
                message: err,
            }
        });
    }
    grant_from_response(&body, now_ms).ok_or_else(|| AccountingError::Permanent {
        code: "qbo_token_parse_failed".to_string(),
        message: "token response missing access_token/refresh_token".to_string(),
    })
}

fn grant_from_response(body: &Value, now_ms: u64) -> Option<QboTokenGrant> {
    let non_empty = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let seconds = |key: &str, fallback: u64| {
        body.get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    Some(QboTokenGrant {
        access_token: non_empty("access_token")?,
        // QBO access tokens last 3600s; refresh tokens ~100 days.
        access_token_expires_at_ms: now_ms + seconds("expires_in", 3600) * 1000,
        refresh_token: non_empty("refresh_token")?,
        refresh_token_expires_at_ms: now_ms
            + seconds("x_refresh_token_expires_in", 100 * 24 * 3600) * 1000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_url_carries_scope_state_and_redirect() {
        let app = QboOAuthApp {
            client_id: "abc 123".to_string(),
            client_secret: "secret".to_string(),
            environment: QboEnvironment::Sandbox,
            token_url: None,
        };
        let url = authorization_consent_url(&app, "https://ops.example-host.com/cb", "st_x");
        assert!(url.starts_with("https://appcenter.intuit.com/connect/oauth2?"));
        assert!(url.contains("client_id=abc%20123"));
        // Unreserved characters (dots, hyphens, underscores) stay LITERAL —
        // Intuit rejects over-encoded redirect URIs as invalid_redirect_uri.
        assert!(url.contains("scope=com.intuit.quickbooks.accounting"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fops.example-host.com%2Fcb"));
        assert!(url.contains("state=st_x"));
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn grant_parses_expiries_with_defaults() {
        let body = serde_json::json!({
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 3600,
            "x_refresh_token_expires_in": 8640000,
        });
        let grant = grant_from_response(&body, 1_000).expect("grant");
        assert_eq!(grant.access_token, "at-1");
        assert_eq!(grant.refresh_token, "rt-1");
        assert_eq!(grant.access_token_expires_at_ms, 1_000 + 3_600_000);
        assert_eq!(grant.refresh_token_expires_at_ms, 1_000 + 8_640_000_000);

        let missing_refresh = serde_json::json!({ "access_token": "at-1" });
        assert!(grant_from_response(&missing_refresh, 0).is_none());
    }

    #[test]
    fn debug_never_dumps_secrets() {
        let app = QboOAuthApp {
            client_id: "id".to_string(),
            client_secret: "super-secret".to_string(),
            environment: QboEnvironment::Production,
            token_url: None,
        };
        let grant = QboTokenGrant {
            access_token: "at-secret".to_string(),
            access_token_expires_at_ms: 0,
            refresh_token: "rt-secret".to_string(),
            refresh_token_expires_at_ms: 0,
        };
        let dump = format!("{app:?}{grant:?}");
        assert!(!dump.contains("super-secret"));
        assert!(!dump.contains("at-secret"));
        assert!(!dump.contains("rt-secret"));
    }

    #[test]
    fn environment_parses_and_maps_base_urls() {
        assert_eq!(
            QboEnvironment::parse(" Sandbox ").unwrap().api_base_url(),
            "https://sandbox-quickbooks.api.intuit.com"
        );
        assert_eq!(
            QboEnvironment::parse("production").unwrap().api_base_url(),
            "https://quickbooks.api.intuit.com"
        );
        assert!(QboEnvironment::parse("staging").is_none());
    }
}
