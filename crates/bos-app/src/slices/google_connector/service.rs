//! Connect-flow logic: consent URL assembly, CSRF state management, and
//! resolution of "which Google credential should a consumer act with" —
//! the acting user's stored credential wins; the env refresh token is the
//! legacy single-account fallback.

use axum::http::HeaderMap;
use bos_contracts::google_connector::{
    ConnectorStatus, GoogleDriveFolderOption, GoogleDriveFolderOptionsResponse,
};
use bos_integrations::google_drive_read::{
    DriveReadClient, LiveDriveReadClient, ReqwestDriveHttpClient, GOOGLE_DRIVE_READONLY_SCOPE,
};
use bos_integrations::google_oauth;
use bos_integrations::GoogleOAuthConfig;
use rusqlite::Connection;

use super::store;
use crate::env_registry;
use crate::store_core::StoreError;

pub const GMAIL_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const GMAIL_COMPOSE_SCOPE: &str = "https://www.googleapis.com/auth/gmail.compose";
pub const GMAIL_MODIFY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";
pub const CALENDAR_EVENTS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";
pub const CALENDAR_LIST_READONLY_SCOPE: &str =
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly";
pub const DRIVE_READONLY_SCOPE: &str = GOOGLE_DRIVE_READONLY_SCOPE;
pub const SEARCH_CONSOLE_READONLY_SCOPE: &str =
    bos_integrations::google_search_console::GOOGLE_SEARCH_CONSOLE_READONLY_SCOPE;
pub const ANALYTICS_READONLY_SCOPE: &str =
    bos_integrations::google_analytics_data::GOOGLE_ANALYTICS_READONLY_SCOPE;

/// All Google scopes the connector knows how to request. The live connect flow
/// narrows this list to the slices enabled for the client.
pub const ALL_KNOWN_SCOPES: &[&str] = &[
    GMAIL_READONLY_SCOPE,
    GMAIL_COMPOSE_SCOPE,
    GMAIL_MODIFY_SCOPE,
    CALENDAR_EVENTS_SCOPE,
    CALENDAR_LIST_READONLY_SCOPE,
    DRIVE_READONLY_SCOPE,
    SEARCH_CONSOLE_READONLY_SCOPE,
    ANALYTICS_READONLY_SCOPE,
];

/// OAuth app credentials (NOT the per-user token) — env-provided.
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
}

pub fn oauth_app_from_env() -> Option<OAuthApp> {
    match (
        env_registry::string(&env_registry::BOS_GMAIL_OAUTH_CLIENT_ID),
        env_registry::string(&env_registry::BOS_GMAIL_OAUTH_CLIENT_SECRET),
    ) {
        (Some(client_id), Some(client_secret)) => Some(OAuthApp {
            client_id,
            client_secret,
        }),
        _ => None,
    }
}

pub fn redirect_uri() -> String {
    let base = env_registry::string(&env_registry::BOS_PUBLIC_BASE_URL)
        .unwrap_or_else(|| "http://127.0.0.1:4400".to_string());
    redirect_uri_for_base(&base)
}

pub(crate) fn redirect_uri_for_base(base: &str) -> String {
    format!(
        "{}/api/connectors/google/callback",
        base.trim_end_matches('/')
    )
}

pub fn redirect_uri_for_request(headers: &HeaderMap) -> String {
    redirect_uri_from_base_or_headers(
        env_registry::string(&env_registry::BOS_PUBLIC_BASE_URL).as_deref(),
        first_header_value(headers, "x-forwarded-proto").as_deref(),
        first_header_value(headers, "x-forwarded-host").as_deref(),
        first_header_value(headers, "host").as_deref(),
    )
}

pub(crate) fn redirect_uri_from_base_or_headers(
    public_base: Option<&str>,
    forwarded_proto: Option<&str>,
    forwarded_host: Option<&str>,
    host_header: Option<&str>,
) -> String {
    if let Some(base) = public_base {
        return redirect_uri_for_base(base);
    }
    let Some(host) = forwarded_host.or(host_header) else {
        return redirect_uri_for_base("http://127.0.0.1:4400");
    };
    let proto = forwarded_proto
        .map(str::to_string)
        .unwrap_or_else(|| default_proto_for_host(host));
    redirect_uri_for_base(&format!("{proto}://{host}"))
}

fn first_header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn default_proto_for_host(host: &str) -> String {
    if host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]") {
        "http".to_string()
    } else {
        "https".to_string()
    }
}

pub fn requested_scopes_for_enabled_slices(
    slice_enabled: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut scopes = if slice_enabled("email_triage") {
        // gmail.modify includes read access and is required for the explicit
        // Move to Trash action. Avoid requesting redundant readonly access.
        vec![GMAIL_MODIFY_SCOPE]
    } else {
        vec![GMAIL_READONLY_SCOPE]
    };
    if (slice_enabled("email_drafts")
        || slice_enabled("claim_drafts")
        || slice_enabled("owner_reports"))
        && !scopes.contains(&GMAIL_MODIFY_SCOPE)
    {
        scopes.push(GMAIL_COMPOSE_SCOPE);
    }
    if slice_enabled("calendar_drafts") {
        scopes.push(CALENDAR_EVENTS_SCOPE);
        scopes.push(CALENDAR_LIST_READONLY_SCOPE);
    }
    if slice_enabled("drive_corpus")
        || slice_enabled("content_drafts")
        || slice_enabled("call_inputs")
    {
        scopes.push(DRIVE_READONLY_SCOPE);
    }
    if slice_enabled("search_console") || slice_enabled("owner_reports") {
        scopes.push(SEARCH_CONSOLE_READONLY_SCOPE);
    }
    if slice_enabled("search_console") || slice_enabled("owner_reports") {
        scopes.push(ANALYTICS_READONLY_SCOPE);
    }
    scopes
}

pub fn consent_url(app: &OAuthApp, redirect_uri: &str, scopes: &[&str], state: &str) -> String {
    let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
    google_oauth::authorization_consent_url(&app.client_id, redirect_uri, &scopes, state)
}

/// Requested scopes the credential does not carry. An empty stored scope list
/// means "unknown" (e.g. env-provided token) — nothing is reported missing.
fn missing_scopes(granted: &[String], requested_scopes: &[&str]) -> Vec<String> {
    if granted.is_empty() {
        return Vec::new();
    }
    requested_scopes
        .iter()
        .filter(|needed| !granted.iter().any(|have| have == *needed))
        .map(|needed| needed.to_string())
        .collect()
}

/// Connection status for ONE operator user. The user's own stored credential
/// wins; the env refresh token (legacy single-account mode) is the fallback.
pub fn gmail_status(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
    requested_scopes: &[&str],
) -> Result<ConnectorStatus, StoreError> {
    if let Some(stored) = store::get_credential(conn, client_id, user_id, super::SERVICE_GMAIL)? {
        let missing = missing_scopes(&stored.scopes, requested_scopes);
        // Reconnecting re-runs consent with the full scope list, so offer the
        // connect URL whenever something is missing.
        let connect_url = (!missing.is_empty() && oauth_app_from_env().is_some())
            .then(|| "/api/connectors/google/connect".to_string());
        return Ok(ConnectorStatus {
            service: super::SERVICE_GMAIL.to_string(),
            connected: true,
            source: Some("stored".to_string()),
            scopes: stored.scopes,
            missing_scopes: missing,
            connect_url,
            blocked_reason: None,
        });
    }
    if env_registry::string(&env_registry::BOS_GMAIL_OAUTH_REFRESH_TOKEN).is_some() {
        let scopes = env_registry::string(&env_registry::BOS_GMAIL_OAUTH_SCOPES)
            .map(|raw| google_oauth::parse_scope_list(&raw))
            .unwrap_or_default();
        let missing = missing_scopes(&scopes, requested_scopes);
        return Ok(ConnectorStatus {
            service: super::SERVICE_GMAIL.to_string(),
            connected: true,
            source: Some("env".to_string()),
            scopes,
            missing_scopes: missing,
            connect_url: None,
            blocked_reason: None,
        });
    }
    let (connect_url, blocked_reason) = if oauth_app_from_env().is_some() {
        (Some("/api/connectors/google/connect".to_string()), None)
    } else {
        (
            None,
            Some(
                "oauth_app_unconfigured: set BOS_GMAIL_OAUTH_CLIENT_ID and \
                 BOS_GMAIL_OAUTH_CLIENT_SECRET"
                    .to_string(),
            ),
        )
    };
    Ok(ConnectorStatus {
        service: super::SERVICE_GMAIL.to_string(),
        connected: false,
        source: None,
        scopes: Vec::new(),
        missing_scopes: Vec::new(),
        connect_url,
        blocked_reason,
    })
}

pub fn drive_folder_options(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
    query: Option<&str>,
) -> Result<GoogleDriveFolderOptionsResponse, StoreError> {
    let oauth = resolve_google_oauth(conn, client_id, Some(user_id))?
        .ok_or_else(|| StoreError::Domain("google_credential_not_connected".to_string()))?;
    if !oauth.scopes.is_empty() && !google_oauth::has_scope(&oauth, DRIVE_READONLY_SCOPE) {
        return Err(StoreError::Domain("google_drive_scope_missing".to_string()));
    }
    let access_token = google_oauth::fetch_access_token(&oauth)
        .map_err(|err| StoreError::Domain(format!("google_token_refresh_failed: {err}")))?;
    let client = LiveDriveReadClient::new(ReqwestDriveHttpClient::default());
    let page = client
        .list_folders(&access_token, query, None)
        .map_err(|err| StoreError::Domain(err.to_string()))?;
    let mut folders: Vec<GoogleDriveFolderOption> = page
        .files
        .into_iter()
        .map(|file| GoogleDriveFolderOption {
            folder_id: file.file_id,
            name: file.name,
            parent_folder_ids: file.parent_folder_ids,
            web_view_link: file.web_view_link,
        })
        .collect();
    folders.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.folder_id.cmp(&b.folder_id))
    });
    Ok(GoogleDriveFolderOptionsResponse { folders })
}

/// Resolve which stored operator credential would back a Google action for
/// `user_id`. `None` means the env refresh token is the active credential.
pub fn google_oauth_owner(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<Option<String>, StoreError> {
    let Some(app) = oauth_app_from_env() else {
        return Ok(None);
    };
    if store::get_credential(conn, client_id, user_id, super::SERVICE_GMAIL)?.is_some() {
        return Ok(Some(user_id.to_string()));
    }
    if env_oauth_config(app).is_some() {
        return Ok(None);
    }
    let all = store::list_credentials(conn, client_id, super::SERVICE_GMAIL)?;
    Ok((all.len() == 1).then(|| all[0].user_id.clone()))
}

pub fn resolve_google_oauth_for_owner(
    conn: &Connection,
    client_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Option<GoogleOAuthConfig>, StoreError> {
    let Some(owner_user_id) = owner_user_id else {
        return resolve_google_oauth(conn, client_id, None);
    };
    let Some(app) = oauth_app_from_env() else {
        return Ok(None);
    };
    let Some(stored) = store::get_credential(conn, client_id, owner_user_id, super::SERVICE_GMAIL)?
    else {
        return Ok(None);
    };
    Ok(Some(config_with_token(
        app,
        stored.refresh_token,
        stored.scopes,
    )))
}

fn config_with_token(
    app: OAuthApp,
    refresh_token: String,
    scopes: Vec<String>,
) -> GoogleOAuthConfig {
    GoogleOAuthConfig {
        client_id: app.client_id,
        client_secret: app.client_secret,
        refresh_token,
        scopes,
        token_url: None,
    }
}

fn env_oauth_config(app: OAuthApp) -> Option<GoogleOAuthConfig> {
    env_registry::string(&env_registry::BOS_GMAIL_OAUTH_REFRESH_TOKEN).map(|refresh_token| {
        config_with_token(
            app,
            refresh_token,
            env_registry::string(&env_registry::BOS_GMAIL_OAUTH_SCOPES)
                .map(|raw| google_oauth::parse_scope_list(&raw))
                .unwrap_or_default(),
        )
    })
}

/// Resolve the Google OAuth credential a consumer should act with, given the
/// user whose behalf the action runs on. Order: that user's stored credential
/// → env refresh token (legacy single-account mode) → the ONLY stored
/// credential when exactly one exists (so single-account deployments keep
/// working whichever identity acts). Two or more stored credentials with no
/// exact match resolve to `None` — picking someone else's account silently
/// would be wrong. `None` user = no personal binding (legacy outbox jobs).
pub fn resolve_google_oauth(
    conn: &Connection,
    client_id: &str,
    user_id: Option<&str>,
) -> Result<Option<GoogleOAuthConfig>, StoreError> {
    let Some(app) = oauth_app_from_env() else {
        return Ok(None);
    };
    if let Some(user_id) = user_id {
        if let Some(stored) = store::get_credential(conn, client_id, user_id, super::SERVICE_GMAIL)?
        {
            return Ok(Some(config_with_token(
                app,
                stored.refresh_token,
                stored.scopes,
            )));
        }
    }
    if let Some(config) = env_oauth_config(OAuthApp {
        client_id: app.client_id.clone(),
        client_secret: app.client_secret.clone(),
    }) {
        return Ok(Some(config));
    }
    let mut all = store::list_credentials(conn, client_id, super::SERVICE_GMAIL)?;
    if all.len() == 1 {
        let only = all.remove(0);
        if user_id.is_some_and(|user| user != only.user_id) {
            tracing::debug!(
                requested = user_id.unwrap_or_default(),
                using = %only.user_id,
                "google credential fallback: acting user not connected, using the only credential"
            );
        }
        return Ok(Some(config_with_token(
            app,
            only.refresh_token,
            only.scopes,
        )));
    }
    Ok(None)
}

/// Resolve a credential explicitly bound to `user_id`. Unlike
/// `resolve_google_oauth`, this never substitutes another stored user's
/// credential. The deployment-wide env token remains a valid legacy fallback.
pub fn resolve_bound_google_oauth(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<Option<GoogleOAuthConfig>, StoreError> {
    let Some(app) = oauth_app_from_env() else {
        return Ok(None);
    };
    if let Some(stored) = store::get_credential(conn, client_id, user_id, super::SERVICE_GMAIL)? {
        return Ok(Some(config_with_token(
            app,
            stored.refresh_token,
            stored.scopes,
        )));
    }
    Ok(env_oauth_config(app))
}

/// A connected Gmail account the ingest pump should poll: the owning user
/// (None = env credential, single-account mode) and its OAuth config.
pub struct GmailAccount {
    pub user_id: Option<String>,
    pub oauth: GoogleOAuthConfig,
}

/// Every Gmail account the ingest pump polls this cycle: ALL stored per-user
/// credentials; the env refresh token only when none are stored (it would
/// otherwise double-fetch the same mailbox it was migrated from).
pub fn list_gmail_accounts(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<GmailAccount>, StoreError> {
    let Some(app) = oauth_app_from_env() else {
        return Ok(Vec::new());
    };
    let stored = store::list_credentials(conn, client_id, super::SERVICE_GMAIL)?;
    if stored.is_empty() {
        return Ok(env_oauth_config(app)
            .map(|oauth| GmailAccount {
                user_id: None,
                oauth,
            })
            .into_iter()
            .collect());
    }
    Ok(stored
        .into_iter()
        .map(|credential| GmailAccount {
            user_id: Some(credential.user_id),
            oauth: config_with_token(
                OAuthApp {
                    client_id: app.client_id.clone(),
                    client_secret: app.client_secret.clone(),
                },
                credential.refresh_token,
                credential.scopes,
            ),
        })
        .collect())
}

/// Random, single-use CSRF state token. Sourced from the OS CSPRNG; the
/// time/pid mix is only a last-resort fallback if /dev/urandom is unreadable.
pub fn generate_state() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let read_ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !read_ok {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
            .unwrap_or(0)
            ^ u64::from(std::process::id()).rotate_left(32);
        bytes[..8].copy_from_slice(&nanos.to_le_bytes());
    }
    let mut out = String::from("st_");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
