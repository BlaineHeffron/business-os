//! HTTP assembly: shared state, operator auth, router composition.
//! Slices contribute routers via their `routes::router()`; this module mounts
//! them. No business logic lives here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once, OnceLock};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{extract::State, Json, Router};
use bos_contracts::operator_users::{
    OperatorSessionLoginRequest, OperatorSessionResponse, OperatorSessionVisibilityResponse,
};
use parking_lot::Mutex as NonPoisoningMutex;
use tower_http::catch_panic::CatchPanicLayer;

use crate::env_registry;
use crate::persistence::PersistencePool;

/// Pending OAuth CSRF states expire after this many ms.
const OAUTH_STATE_TTL_MS: u64 = 10 * 60 * 1000;
const OPERATOR_SESSION_COOKIE: &str = "bos_operator_session";
const OPERATOR_SESSION_COOKIE_PREFIX: &str = "boss2";
/// Browser operator sessions last seven days from sign-in. The cookie is
/// stateless and revalidates the shared/personal token on every request, so
/// process restarts do not log operators out and token/user revocation still
/// takes effect immediately.
const OPERATOR_SESSION_ABSOLUTE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Kept only for pre-stateless in-memory `boss_...` cookies in a running
/// process. New cookies use the absolute seven-day TTL above.
const OPERATOR_SESSION_IDLE_TTL_MS: u64 = 12 * 60 * 60 * 1000;
const SPA_HTML_CACHE_CONTROL: &str = "no-cache, must-revalidate";
const SPA_IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

#[derive(Debug, Clone)]
struct OperatorSession {
    token: String,
    issued_at_ms: u64,
    last_seen_at_ms: u64,
}

// Lock-ordering invariant (all code in this crate must respect this):
//
//   1. Identity / auth  - resolve_actor, require_operator, authenticate_presented_token.
//      With the connection pool, auth borrows its own connection independently of the
//      handler's connection - the re-entrancy deadlock class is gone by construction.
//      Still call auth before borrowing a handler connection as a code-clarity convention.
//
//   2. Secondary locks  - operator_sessions, revoked_operator_sessions,
//      produce_in_flight, sync_guards fields. Short-lived,
//      taken one at a time, never nested.
//      Never acquire while holding a pooled connection across multi-step logic.
//
//   3. Persistence      - borrowed from PersistencePool via persistence_or_busy() in
//      request handlers, or .persistence() in background workers. Pool connection_timeout
//      is 5s; exhaustion -> 503 (request handlers) or panic (workers, should never happen).
//
// Original production deadlock fixed in #114; re-entrancy class eliminated structurally
// in this PR (connection pool). This comment is the canonical reference.
#[derive(Clone)]
pub struct AppState {
    pub persistence: PersistencePool,
    pub schema_version: u32,
    pub client_id: Arc<str>,
    /// Client-facing brand name from the overlay identity ("Example Company");
    /// the SPA titles itself with it. "BusinessOS" without an overlay.
    pub display_name: Arc<str>,
    /// Process start, for uptime in the diagnostics surface.
    pub started_at_ms: u64,
    operator_token: Option<Arc<str>>,
    operator_sessions: Arc<NonPoisoningMutex<HashMap<String, OperatorSession>>>,
    revoked_operator_sessions: Arc<NonPoisoningMutex<std::collections::HashSet<String>>>,
    /// Slice ids enabled by the client overlay; empty = all (dev profile).
    enabled_slices: Arc<[String]>,
    /// (item_id, kind) produces running right now (manual kickoff threads +
    /// the auto-produce pump). Process-local on purpose: a crash just means
    /// the operator clicks again, and the one-active-draft index already
    /// prevents duplicates.
    pub produce_in_flight: Arc<NonPoisoningMutex<std::collections::HashSet<(String, String)>>>,
    /// Per-pump serialization + cooldown guards. Each pump keeps an independent
    /// mutex so unrelated workers never contend on one shared lock.
    pub sync_guards: SyncGuards,
    /// Overlay [drive_corpus] corpus-pointer defaults, pinned at startup
    /// (env BOS_DRIVE_CORPUS_* overrides per field at resolve time).
    pub drive_corpus_overlay: Arc<Option<crate::overlay::DriveCorpusOverlay>>,
    /// Overlay [search_console] defaults, pinned at startup (env overrides
    /// per field at resolve time).
    pub search_console_overlay: Arc<Option<crate::overlay::SearchConsoleOverlay>>,
    /// Overlay [quote_workflows] profile selection, pinned at startup.
    pub quote_workflows_overlay: Arc<crate::overlay::QuoteWorkflowsOverlay>,
    /// Overlay [owner_reports] cadence/recipient defaults, pinned at startup
    /// (env BOS_REPORT_DIGEST_* overrides per field at resolve time).
    pub owner_reports_overlay: Arc<Option<crate::overlay::OwnerReportsOverlay>>,
    /// Overlay [email_triage] dashboard defaults. Ingestion query is still env.
    pub email_triage_overlay: Arc<crate::overlay::EmailTriageOverlay>,
    /// Overlay [work_queue] shared-inbox visibility routing.
    pub work_queue_overlay: Arc<crate::overlay::WorkQueueOverlay>,
    /// Overlay [accounting] defaults, pinned at startup (env overrides per
    /// field at resolve time).
    pub accounting_overlay: Arc<crate::overlay::AccountingOverlay>,
    /// Overlay [accounting] visibility policy default, pinned at startup
    /// (env BOS_ACCOUNTING_VISIBILITY_POLICY overrides at resolve time).
    pub accounting_visibility_policy: crate::overlay::AccountingVisibilityPolicy,
    /// Overlay [lead_discovery] approved source list and criteria.
    pub lead_discovery_overlay: Arc<crate::overlay::LeadDiscoveryOverlay>,
    /// Overlay [call_inputs] consent/fit-gated call source list and routing.
    pub call_inputs_overlay: Arc<crate::overlay::CallInputsOverlay>,
    /// Overlay [customer_tier_sync] non-secret tier mapping defaults.
    pub customer_tier_sync_overlay: Arc<crate::overlay::CustomerTierSyncOverlay>,
}

pub type PersistenceConn = crate::persistence::PersistenceConn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pump {
    Accounting,
    CrmCache,
    Stockforge,
    Drive,
    SearchConsole,
    Claims,
    CallInputTranscription,
    DataRetention,
    LeadDiscoveryAutoscrape,
    EnrichmentFreshness,
    ReportGenerate,
    ShopifySales,
}

/// In-memory pump state for freshness display + overlap/cooldown guards.
/// `units_used` is provider requests or engine runs depending on the pump.
/// `kick_pending` is only used by the Stockforge webhook-deferral pump.
#[derive(Debug, Clone, Default)]
pub struct SyncGuard {
    pub in_flight: bool,
    pub last_attempt_ms: Option<u64>,
    /// Last cycle that completed without rate-limit/auth stand-down.
    /// In-memory only — used so a quiet successful refresh still looks fresh.
    pub last_success_ms: Option<u64>,
    pub last_outcome: Option<String>,
    pub units_used: u32,
    pub next_allowed_at_ms: u64,
    pub kick_pending: bool,
    /// Retention exposes cycle duration without expanding every pump DTO.
    pub last_duration_ms: Option<u64>,
}

#[derive(Clone)]
pub struct SyncGuards {
    accounting: Arc<NonPoisoningMutex<SyncGuard>>,
    crm_cache: Arc<NonPoisoningMutex<SyncGuard>>,
    stockforge: Arc<NonPoisoningMutex<SyncGuard>>,
    drive: Arc<NonPoisoningMutex<SyncGuard>>,
    search_console: Arc<NonPoisoningMutex<SyncGuard>>,
    claims: Arc<NonPoisoningMutex<SyncGuard>>,
    call_input_transcription: Arc<NonPoisoningMutex<SyncGuard>>,
    data_retention: Arc<NonPoisoningMutex<SyncGuard>>,
    lead_discovery_autoscrape: Arc<NonPoisoningMutex<SyncGuard>>,
    enrichment_freshness: Arc<NonPoisoningMutex<SyncGuard>>,
    report_generate: Arc<NonPoisoningMutex<SyncGuard>>,
    shopify_sales: Arc<NonPoisoningMutex<SyncGuard>>,
}

impl Default for SyncGuards {
    fn default() -> Self {
        Self {
            accounting: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            crm_cache: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            stockforge: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            drive: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            search_console: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            claims: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            call_input_transcription: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            data_retention: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            lead_discovery_autoscrape: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            enrichment_freshness: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            report_generate: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
            shopify_sales: Arc::new(NonPoisoningMutex::new(SyncGuard::default())),
        }
    }
}

impl SyncGuards {
    pub fn guard(&self, pump: Pump) -> &Arc<NonPoisoningMutex<SyncGuard>> {
        match pump {
            Pump::Accounting => &self.accounting,
            Pump::CrmCache => &self.crm_cache,
            Pump::Stockforge => &self.stockforge,
            Pump::Drive => &self.drive,
            Pump::SearchConsole => &self.search_console,
            Pump::Claims => &self.claims,
            Pump::CallInputTranscription => &self.call_input_transcription,
            Pump::DataRetention => &self.data_retention,
            Pump::LeadDiscoveryAutoscrape => &self.lead_discovery_autoscrape,
            Pump::EnrichmentFreshness => &self.enrichment_freshness,
            Pump::ReportGenerate => &self.report_generate,
            Pump::ShopifySales => &self.shopify_sales,
        }
    }
}

impl AppState {
    pub fn new(persistence: PersistencePool) -> Self {
        Self::with_overlay(persistence, None)
    }

    /// Overlay identity wins over BOS_CLIENT_ID; the env default only covers
    /// overlay-less dev. A non-default env id that disagrees is logged loudly.
    pub fn with_overlay(
        persistence: PersistencePool,
        overlay: Option<&crate::overlay::ClientOverlay>,
    ) -> Self {
        let schema_version = persistence.schema_version();
        let env_client_id =
            env_registry::string(&env_registry::BOS_CLIENT_ID).unwrap_or_else(|| "dev".to_string());
        let client_id = match overlay {
            Some(overlay) => {
                if env_client_id != "dev" && env_client_id != overlay.identity.client_id {
                    tracing::warn!(
                        env = %env_client_id,
                        overlay = %overlay.identity.client_id,
                        "BOS_CLIENT_ID disagrees with the overlay identity; overlay wins"
                    );
                }
                overlay.identity.client_id.clone()
            }
            None => env_client_id,
        };
        let enabled_slices: Arc<[String]> = overlay
            .map(|overlay| overlay.slices.enabled.clone())
            .unwrap_or_default()
            .into();
        let display_name = overlay
            .map(|overlay| overlay.identity.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "BusinessOS".to_string());
        Self {
            persistence,
            schema_version,
            client_id: client_id.into(),
            display_name: display_name.into(),
            started_at_ms: now_ms(),
            operator_token: env_registry::string(&env_registry::BOS_OPERATOR_TOKEN).map(Into::into),
            operator_sessions: Arc::new(NonPoisoningMutex::new(HashMap::new())),
            revoked_operator_sessions: Arc::new(NonPoisoningMutex::new(
                std::collections::HashSet::new(),
            )),
            enabled_slices,
            produce_in_flight: Arc::new(NonPoisoningMutex::new(std::collections::HashSet::new())),
            sync_guards: SyncGuards::default(),
            drive_corpus_overlay: Arc::new(
                overlay.and_then(|overlay| overlay.drive_corpus.clone()),
            ),
            search_console_overlay: Arc::new(
                overlay.and_then(|overlay| overlay.search_console.clone()),
            ),
            quote_workflows_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.quote_workflows.clone())
                    .unwrap_or_default(),
            ),
            owner_reports_overlay: Arc::new(
                overlay.and_then(|overlay| overlay.owner_reports.clone()),
            ),
            email_triage_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.email_triage.clone())
                    .unwrap_or_default(),
            ),
            work_queue_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.work_queue.clone())
                    .unwrap_or_default(),
            ),
            accounting_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.accounting.clone())
                    .unwrap_or_default(),
            ),
            accounting_visibility_policy: overlay
                .and_then(|overlay| overlay.accounting.visibility_policy)
                .unwrap_or_default(),
            lead_discovery_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.lead_discovery.clone())
                    .unwrap_or_default(),
            ),
            call_inputs_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.call_inputs.clone())
                    .unwrap_or_default(),
            ),
            customer_tier_sync_overlay: Arc::new(
                overlay
                    .map(|overlay| overlay.customer_tier_sync.clone())
                    .unwrap_or_default(),
            ),
        }
    }

    /// A disabled slice contributes no routes and no background work.
    pub fn slice_enabled(&self, slice_id: &str) -> bool {
        self.enabled_slices.is_empty() || self.enabled_slices.iter().any(|id| id == slice_id)
    }

    pub fn persistence(&self) -> PersistenceConn {
        self.persistence.lock()
    }

    /// Acquire persistence with a bounded pool wait so exhaustion becomes a 503
    /// instead of an indefinitely hung request.
    pub fn persistence_or_busy(&self) -> Result<PersistenceConn, Box<Response>> {
        self.persistence.get().map_err(|err| {
            tracing::error!("persistence pool exhausted; returning 503: {err}");
            Box::new(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "persistence_busy",
            ))
        })
    }

    /// Effective enabled slice ids (an empty overlay list means all).
    pub fn enabled_slice_ids(&self) -> Vec<String> {
        if self.enabled_slices.is_empty() {
            crate::slices::registry()
                .iter()
                .map(|slice| slice.id.to_string())
                .collect()
        } else {
            self.enabled_slices.to_vec()
        }
    }

    /// Operator gate. Browser UI normally authenticates with the HttpOnly
    /// session cookie; Bearer tokens remain supported for API/dev clients.
    /// When BOS_OPERATOR_TOKEN is unset (local dev), open.
    pub fn require_operator(&self, headers: &HeaderMap) -> Result<(), Box<Response>> {
        self.authenticate_operator(headers).map(|_| ())
    }

    pub fn require_scope(&self, headers: &HeaderMap) -> Result<OperatorScope, Box<Response>> {
        self.authenticate_operator(headers)
            .map(|identity| identity.scope())
    }

    pub fn require_all_scope(&self, headers: &HeaderMap) -> Result<(), Box<Response>> {
        match self.require_scope(headers)? {
            OperatorScope::All => Ok(()),
            OperatorScope::User(_) => Err(Box::new(error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "scope_forbidden",
            ))),
        }
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, Box<Response>> {
        let identity = self.authenticate_operator(headers)?;
        let scope = identity.scope();
        let actor_id = identity.actor_id.clone();
        Ok(AuthContext {
            identity,
            scope,
            actor_id,
        })
    }

    pub fn authenticate_agent_mcp(
        &self,
        headers: &HeaderMap,
    ) -> Result<AuthContext, Box<Response>> {
        let denied = || {
            Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_token_invalid",
            ))
        };
        if !self.operator_credentials_configured()? {
            return Err(denied());
        }
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if bearer.is_none() && self.session_token_from_headers(headers)?.is_none() {
            return Err(denied());
        }
        self.authenticate(headers)
    }

    fn operator_credentials_configured(&self) -> Result<bool, Box<Response>> {
        if self.operator_token.is_some() {
            return Ok(true);
        }
        let persistence = self.persistence_or_busy()?;
        crate::slices::operator_users::store::any_active_token(
            persistence.connection_ref(),
            &self.client_id,
        )
        .map_err(|err| {
            tracing::error!(error = %err, "operator credential lookup failed");
            Box::new(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth_lookup_failed",
            ))
        })
    }

    /// Operator gate that resolves WHO acts: the shared BOS_OPERATOR_TOKEN
    /// (or open dev mode) is the anonymous "operator"; a personal token
    /// resolves to its user. Wrong/unknown tokens are rejected even in open
    /// dev mode — presenting a credential means asking to be identified.
    pub fn authenticate_operator(
        &self,
        headers: &HeaderMap,
    ) -> Result<OperatorIdentity, Box<Response>> {
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if bearer.is_some() {
            return self.authenticate_presented_token(bearer);
        }
        if let Some(token) = self.session_token_from_headers(headers)? {
            return self.authenticate_presented_token(Some(&token));
        }
        self.authenticate_presented_token(None)
    }

    fn authenticate_presented_token(
        &self,
        presented: Option<&str>,
    ) -> Result<OperatorIdentity, Box<Response>> {
        let denied = || {
            Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_token_invalid",
            ))
        };
        match presented {
            None => {
                if self.operator_token.is_none() {
                    Ok(OperatorIdentity::shared())
                } else {
                    Err(denied())
                }
            }
            Some(token) => {
                if self.operator_token.as_deref() == Some(token) {
                    return Ok(OperatorIdentity::shared());
                }
                let persistence = self.persistence_or_busy()?;
                match crate::slices::operator_users::store::find_active_by_token(
                    persistence.connection_ref(),
                    &self.client_id,
                    token,
                ) {
                    Ok(Some(user)) => Ok(OperatorIdentity {
                        actor_id: user.user_id,
                        display_name: user.display_name,
                    }),
                    Ok(None) => Err(denied()),
                    Err(err) => {
                        tracing::error!(error = %err, "operator token lookup failed");
                        Err(Box::new(error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "auth_lookup_failed",
                        )))
                    }
                }
            }
        }
    }

    pub fn create_operator_session_for_token(&self, token: &str) -> Result<String, Box<Response>> {
        self.authenticate_presented_token(Some(token))?;
        let now = now_ms();
        Ok(signed_session_cookie_value(&self.client_id, token, now))
    }

    fn session_token_from_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<String>, Box<Response>> {
        let Some(session_id) = cookie_value(headers, OPERATOR_SESSION_COOKIE) else {
            return Ok(None);
        };
        if let Some(token) = self.signed_session_token(session_id)? {
            return Ok(Some(token));
        }
        let now = now_ms();
        let mut sessions = self.operator_sessions.lock();
        let Some(session) = sessions.get_mut(session_id) else {
            return Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_session_invalid",
            )));
        };
        if now.saturating_sub(session.last_seen_at_ms) > OPERATOR_SESSION_IDLE_TTL_MS
            || now.saturating_sub(session.issued_at_ms) > OPERATOR_SESSION_ABSOLUTE_TTL_MS
        {
            sessions.remove(session_id);
            return Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_session_expired",
            )));
        }
        session.last_seen_at_ms = now;
        Ok(Some(session.token.clone()))
    }

    fn signed_session_token(&self, session_id: &str) -> Result<Option<String>, Box<Response>> {
        let Some(signed) = parse_signed_session_cookie_value(session_id) else {
            return Ok(None);
        };
        if self.revoked_operator_sessions.lock().contains(session_id) {
            return Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_session_invalid",
            )));
        }
        let now = now_ms();
        if now.saturating_sub(signed.issued_at_ms) > OPERATOR_SESSION_ABSOLUTE_TTL_MS {
            return Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_session_expired",
            )));
        }

        let proof_material = session_proof_material(&self.client_id, signed.issued_at_ms);
        if let Some(token) = self.operator_token.as_deref() {
            if crate::slices::operator_users::store::session_token_fingerprint(token)
                == signed.token_fingerprint
                && crate::slices::operator_users::store::session_token_proof(
                    token,
                    signed.token_fingerprint,
                    &proof_material,
                ) == signed.proof
            {
                return Ok(Some(token.to_string()));
            }
        }

        let persistence = self.persistence_or_busy()?;
        match crate::slices::operator_users::store::find_active_token_by_session_proof(
            persistence.connection_ref(),
            &self.client_id,
            signed.token_fingerprint,
            signed.proof,
            &proof_material,
        ) {
            Ok(Some(token)) => Ok(Some(token)),
            Ok(None) => Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_session_invalid",
            ))),
            Err(err) => {
                tracing::error!(error = %err, "operator session token lookup failed");
                Err(Box::new(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_lookup_failed",
                )))
            }
        }
    }

    pub fn clear_operator_session(&self, headers: &HeaderMap) {
        let Some(session_id) = cookie_value(headers, OPERATOR_SESSION_COOKIE) else {
            return;
        };
        if parse_signed_session_cookie_value(session_id).is_some() {
            self.revoked_operator_sessions
                .lock()
                .insert(session_id.to_string());
        } else {
            self.operator_sessions.lock().remove(session_id);
        }
    }

    /// The actor stamped on a mutation's receipts. An authenticated PERSONAL
    /// identity always wins; the shared/open identity falls back to the
    /// request's actor_id field (legacy/dev), then "operator". Call only
    /// after require_operator passed.
    pub fn resolve_actor(&self, headers: &HeaderMap, requested: Option<&str>) -> String {
        match self.authenticate(headers) {
            Ok(auth) => auth.actor_or(requested),
            Err(_) => requested
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(SHARED_OPERATOR_ACTOR)
                .to_string(),
        }
    }

    /// Like [`authenticate_operator`], but also accepts the token as a query
    /// param — for browser-opened URLs (e.g. OAuth connect) where headers are
    /// impractical. A personal token in the query resolves to its user, so
    /// the connect flow knows WHO is connecting.
    pub fn authenticate_operator_or_query_token(
        &self,
        headers: &HeaderMap,
        query_token: Option<&str>,
    ) -> Result<OperatorIdentity, Box<Response>> {
        // An empty query token (open dev mode appends one) is "no token".
        let Some(token) = query_token.map(str::trim).filter(|token| !token.is_empty()) else {
            return self.authenticate_operator(headers);
        };
        if self.operator_token.as_deref() == Some(token) {
            return Ok(OperatorIdentity::shared());
        }
        let lookup = {
            let persistence = self.persistence_or_busy()?;
            crate::slices::operator_users::store::find_active_by_token(
                persistence.connection_ref(),
                &self.client_id,
                token,
            )
        };
        match lookup {
            Ok(Some(user)) => Ok(OperatorIdentity {
                actor_id: user.user_id,
                display_name: user.display_name,
            }),
            // A wrong query token never falls back to "open dev mode":
            // presenting a credential means asking to be identified.
            Ok(None) => Err(Box::new(error_response(
                StatusCode::UNAUTHORIZED,
                "operator_token_invalid",
            ))),
            Err(err) => {
                tracing::error!(error = %err, "operator token lookup failed");
                Err(Box::new(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_lookup_failed",
                )))
            }
        }
    }

    /// Register a single-use OAuth CSRF state bound to the user who initiated
    /// the connect — the callback stores the credential under that user.
    pub fn register_oauth_state(
        &self,
        connector: &str,
        state: &str,
        user_id: &str,
    ) -> Result<(), crate::store_core::StoreError> {
        let mut persistence = self.persistence.lock();
        crate::slices::oauth_state::register_oauth_state(
            persistence.connection(),
            &self.client_id,
            connector,
            state,
            user_id,
            now_ms(),
            OAUTH_STATE_TTL_MS,
        )?;
        Ok(())
    }

    /// Validate AND remove (single-use). Returns the bound user id;
    /// `None` = unknown or expired.
    pub fn consume_oauth_state(
        &self,
        connector: &str,
        state: &str,
    ) -> Result<Option<String>, crate::store_core::StoreError> {
        let mut persistence = self.persistence.lock();
        crate::slices::oauth_state::consume_oauth_state(
            persistence.connection(),
            &self.client_id,
            connector,
            state,
            &crate::slices::google_connector::service::generate_state(),
            now_ms(),
        )
    }
}

#[derive(Debug, Clone)]
struct PendingPanicDiagnostic {
    diagnostic_id: String,
    client_id: String,
    message: String,
    location: Option<String>,
    backtrace: String,
    thread_name: Option<String>,
    occurred_at_ms: u64,
}

static PANIC_HOOK_INIT: Once = Once::new();
static PANIC_DIAGNOSTIC_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static PANIC_STATE: OnceLock<NonPoisoningMutex<Option<AppState>>> = OnceLock::new();
static PENDING_PANIC_DIAGNOSTICS: OnceLock<NonPoisoningMutex<Vec<PendingPanicDiagnostic>>> =
    OnceLock::new();

thread_local! {
    static THREAD_PANIC_STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

#[cfg_attr(not(test), allow(dead_code))]
struct ThreadPanicStateGuard(Option<AppState>);

impl Drop for ThreadPanicStateGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        THREAD_PANIC_STATE.with(|state| {
            *state.borrow_mut() = previous;
        });
    }
}

fn panic_state() -> &'static NonPoisoningMutex<Option<AppState>> {
    PANIC_STATE.get_or_init(|| NonPoisoningMutex::new(None))
}

fn pending_panic_diagnostics() -> &'static NonPoisoningMutex<Vec<PendingPanicDiagnostic>> {
    PENDING_PANIC_DIAGNOSTICS.get_or_init(|| NonPoisoningMutex::new(Vec::new()))
}

pub fn install_panic_hook(state: AppState) {
    *panic_state().lock() = Some(state);
    PANIC_HOOK_INIT.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            record_panic_from_hook(info);
        }));
    });
}

#[cfg_attr(not(test), allow(dead_code))]
fn enter_thread_panic_state(state: AppState) -> ThreadPanicStateGuard {
    THREAD_PANIC_STATE.with(|slot| {
        let previous = slot.borrow_mut().replace(state);
        ThreadPanicStateGuard(previous)
    })
}

fn current_panic_state() -> Option<AppState> {
    THREAD_PANIC_STATE.with(|state| state.borrow().clone())
}

fn record_panic_from_hook(info: &PanicHookInfo<'_>) {
    let state = current_panic_state().or_else(|| panic_state().lock().clone());
    let diagnostic = panic_diagnostic_from_info(info, state.as_ref());
    tracing::error!(
        diagnostic_id = %diagnostic.diagnostic_id,
        message = %diagnostic.message,
        location = diagnostic.location.as_deref(),
        thread = diagnostic.thread_name.as_deref(),
        backtrace = %diagnostic.backtrace,
        "thread panicked"
    );

    let Some(state) = state else {
        pending_panic_diagnostics().lock().push(diagnostic);
        return;
    };
    record_or_defer_panic_diagnostic(&state, diagnostic);
}

fn panic_diagnostic_from_info(
    info: &PanicHookInfo<'_>,
    state: Option<&AppState>,
) -> PendingPanicDiagnostic {
    let occurred_at_ms = now_ms();
    let sequence = PANIC_DIAGNOSTIC_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let message = panic_message(info);
    let location = info.location().map(|location| {
        format!(
            "{}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        )
    });
    let thread_name = std::thread::current().name().map(str::to_string);
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    let client_id = state
        .map(|state| state.client_id.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    PendingPanicDiagnostic {
        diagnostic_id: format!("{occurred_at_ms}-{sequence}"),
        client_id,
        message,
        location,
        backtrace,
        thread_name,
        occurred_at_ms,
    }
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "panic payload was not a string".to_string()
    }
}

fn record_or_defer_panic_diagnostic(state: &AppState, diagnostic: PendingPanicDiagnostic) {
    let Some(mut persistence) = state.persistence.try_lock() else {
        pending_panic_diagnostics().lock().push(diagnostic);
        return;
    };
    if let Err(err) = insert_pending_panic_diagnostic(&mut persistence, &diagnostic) {
        tracing::warn!(
            diagnostic_id = %diagnostic.diagnostic_id,
            error = %err,
            "panic diagnostic insert failed"
        );
    }
}

fn insert_pending_panic_diagnostic(
    persistence: &mut PersistenceConn,
    diagnostic: &PendingPanicDiagnostic,
) -> Result<(), crate::store_core::StoreError> {
    crate::slices::debug::store::insert_panic_diagnostic(
        persistence.connection(),
        &crate::slices::debug::store::PanicDiagnosticInsert {
            diagnostic_id: &diagnostic.diagnostic_id,
            client_id: &diagnostic.client_id,
            message: &diagnostic.message,
            location: diagnostic.location.as_deref(),
            backtrace: &diagnostic.backtrace,
            thread_name: diagnostic.thread_name.as_deref(),
            occurred_at_ms: diagnostic.occurred_at_ms,
        },
    )
    .map(|_| ())
}

pub fn flush_pending_panic_diagnostics() {
    let Some(state) = panic_state().lock().clone() else {
        return;
    };
    flush_pending_panic_diagnostics_for(&state);
}

fn flush_pending_panic_diagnostics_for(state: &AppState) {
    let pending = {
        let mut pending = pending_panic_diagnostics().lock();
        if pending.is_empty() {
            return;
        }
        std::mem::take(&mut *pending)
    };
    let mut persistence = state.persistence.lock();
    for diagnostic in pending {
        if let Err(err) = insert_pending_panic_diagnostic(&mut persistence, &diagnostic) {
            tracing::warn!(
                diagnostic_id = %diagnostic.diagnostic_id,
                error = %err,
                "pending panic diagnostic insert failed"
            );
        }
    }
}

fn try_flush_pending_panic_diagnostics_for(state: &AppState) {
    let pending = {
        let mut pending = pending_panic_diagnostics().lock();
        if pending.is_empty() {
            return;
        }
        std::mem::take(&mut *pending)
    };
    let Some(mut persistence) = state.persistence.try_lock() else {
        pending_panic_diagnostics().lock().extend(pending);
        return;
    };
    for diagnostic in pending {
        if let Err(err) = insert_pending_panic_diagnostic(&mut persistence, &diagnostic) {
            tracing::warn!(
                diagnostic_id = %diagnostic.diagnostic_id,
                error = %err,
                "pending panic diagnostic insert failed"
            );
        }
    }
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

struct SignedOperatorSessionCookie<'a> {
    issued_at_ms: u64,
    token_fingerprint: &'a str,
    proof: &'a str,
}

fn signed_session_cookie_value(client_id: &str, token: &str, issued_at_ms: u64) -> String {
    let token_fingerprint = crate::slices::operator_users::store::session_token_fingerprint(token);
    let proof_material = session_proof_material(client_id, issued_at_ms);
    let proof = crate::slices::operator_users::store::session_token_proof(
        token,
        &token_fingerprint,
        &proof_material,
    );
    format!("{OPERATOR_SESSION_COOKIE_PREFIX}_{issued_at_ms}_{token_fingerprint}_{proof}")
}

fn parse_signed_session_cookie_value(value: &str) -> Option<SignedOperatorSessionCookie<'_>> {
    let rest = value.strip_prefix(OPERATOR_SESSION_COOKIE_PREFIX)?;
    let rest = rest.strip_prefix('_')?;
    let mut parts = rest.split('_');
    let issued_at_ms = parts.next()?.parse().ok()?;
    let token_fingerprint = parts.next()?;
    let proof = parts.next()?;
    if parts.next().is_some() || !is_hex_256(token_fingerprint) || !is_hex_256(proof) {
        return None;
    }
    Some(SignedOperatorSessionCookie {
        issued_at_ms,
        token_fingerprint,
        proof,
    })
}

fn session_proof_material(client_id: &str, issued_at_ms: u64) -> String {
    format!("{client_id}:{issued_at_ms}")
}

fn is_hex_256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn session_cookie(cookie_value: &str) -> HeaderValue {
    let value = format!(
        "{OPERATOR_SESSION_COOKIE}={cookie_value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
        OPERATOR_SESSION_ABSOLUTE_TTL_MS / 1000
    );
    HeaderValue::from_str(&value).expect("session cookie header")
}

fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "bos_operator_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
    )
}

/// The anonymous actor for the shared BOS_OPERATOR_TOKEN / open dev mode.
pub const SHARED_OPERATOR_ACTOR: &str = "operator";

/// Who a request acts as, resolved from its bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorIdentity {
    /// "operator" (shared/open) or the operator_users user_id.
    pub actor_id: String,
    pub display_name: String,
}

impl OperatorIdentity {
    fn shared() -> Self {
        Self {
            actor_id: SHARED_OPERATOR_ACTOR.to_string(),
            display_name: "Operator".to_string(),
        }
    }

    pub fn scope(&self) -> OperatorScope {
        if self.actor_id == SHARED_OPERATOR_ACTOR {
            OperatorScope::All
        } else {
            OperatorScope::User(self.actor_id.clone())
        }
    }
}

/// Read/mutation scope derived from the authenticated identity.
/// Shared/open "operator" => All (sees every user incl. legacy NULL rows);
/// a named operator user => User(id) (sees only its own source_user_id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorScope {
    All,
    User(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub identity: OperatorIdentity,
    pub scope: OperatorScope,
    pub actor_id: String,
}

impl AuthContext {
    pub fn actor_or(&self, requested: Option<&str>) -> String {
        if self.actor_id != SHARED_OPERATOR_ACTOR {
            self.actor_id.clone()
        } else {
            requested
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(SHARED_OPERATOR_ACTOR)
                .to_string()
        }
    }

    pub fn require_all_scope(&self) -> Result<(), Box<Response>> {
        match &self.scope {
            OperatorScope::All => Ok(()),
            OperatorScope::User(_) => Err(Box::new(error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "scope_forbidden",
            ))),
        }
    }
}

impl OperatorScope {
    /// Bind params for the isolation predicate
    /// `AND (?scope_all = 1 OR <col> = ?scope_user)`.
    pub fn sql_params(&self) -> (i64, String) {
        match self {
            OperatorScope::All => (1, String::new()),
            OperatorScope::User(user_id) => (0, user_id.clone()),
        }
    }

    /// SQL predicate fragment plus bind params for operator source-user scope.
    pub fn sql_filter(&self, col: &str, all_idx: usize, user_idx: usize) -> (String, i64, String) {
        let (scope_all, scope_user) = self.sql_params();
        (
            format!("(?{all_idx} = 1 OR {col} = ?{user_idx})"),
            scope_all,
            scope_user,
        )
    }

    /// All matches anything; User matches only an exact, non-NULL source_user_id.
    pub fn matches_source_user(&self, source_user_id: Option<&str>) -> bool {
        match self {
            OperatorScope::All => true,
            OperatorScope::User(user_id) => source_user_id == Some(user_id.as_str()),
        }
    }

    pub fn require_source_user(
        &self,
        source_user_id: Option<&str>,
    ) -> Result<(), crate::store_core::StoreError> {
        if self.matches_source_user(source_user_id) {
            Ok(())
        } else {
            Err(crate::store_core::StoreError::Domain(
                "scope_forbidden".to_string(),
            ))
        }
    }
}

pub fn scope_sql_params(scope: &OperatorScope) -> (i64, String) {
    scope.sql_params()
}

pub fn matches_source_user(scope: &OperatorScope, source_user_id: Option<&str>) -> bool {
    scope.matches_source_user(source_user_id)
}

pub fn require_source_user(
    scope: &OperatorScope,
    source_user_id: Option<&str>,
) -> Result<(), crate::store_core::StoreError> {
    scope.require_source_user(source_user_id)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn error_response(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// Success page shown after an OAuth connect callback. Operator-facing, so it
/// stays plain ("Google connected", not "the ingestion pump will pick up the
/// credential") and sends the user back into the app automatically instead of
/// asking them to close the tab. `heading` is the connected service (e.g.
/// "Google"); `message` is one friendly reassurance line.
pub fn connector_connected_page(heading: &str, message: &str) -> Response {
    axum::response::Html(connector_connected_html(heading, message)).into_response()
}

fn connector_connected_html(heading: &str, message: &str) -> String {
    let heading = escape_html_text(heading);
    let message = escape_html_text(message);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{heading} connected</title>\
         <meta http-equiv=\"refresh\" content=\"2;url=/\">\
         <style>body{{font-family:system-ui,-apple-system,sans-serif;background:#0a0a0a;\
         color:#e4e4e7;display:flex;min-height:100vh;align-items:center;justify-content:center;\
         margin:0}}.card{{text-align:center;max-width:24rem;padding:2rem}}\
         h1{{font-size:1.25rem;margin:0 0 .5rem}}p{{color:#a1a1aa;margin:.25rem 0}}\
         a{{color:#38bdf8}}</style></head>\
         <body><div class=\"card\"><h1>{heading} connected</h1>\
         <p>{message}</p><p>Taking you back to the app\u{2026} \
         <a href=\"/\">Continue now</a></p></div>\
         <script>setTimeout(function(){{location.replace(\"/\")}},1500)</script>\
         </body></html>"
    )
}

fn escape_html_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Standard HTTP rendering of a store_core mutation outcome (409 on conflict).
/// Every slice route returns mutations through this — one envelope, one place.
pub fn mutation_response(outcome: crate::store_core::MutationOutcome) -> Response {
    use crate::store_core::MutationOutcome;
    use bos_contracts::mutation::{MutationOutcomeKind, MutationResponse};
    let response = match outcome {
        MutationOutcome::Applied {
            receipt_id,
            revision,
        } => MutationResponse {
            outcome: MutationOutcomeKind::Applied,
            receipt_id,
            revision: Some(revision),
        },
        MutationOutcome::ReplayedIdempotent {
            receipt_id,
            revision,
        } => MutationResponse {
            outcome: MutationOutcomeKind::ReplayedIdempotent,
            receipt_id,
            revision,
        },
        MutationOutcome::RevisionConflict {
            receipt_id,
            current_revision,
        } => {
            return (
                StatusCode::CONFLICT,
                Json(MutationResponse {
                    outcome: MutationOutcomeKind::RevisionConflict,
                    receipt_id,
                    revision: current_revision,
                }),
            )
                .into_response()
        }
    };
    Json(response).into_response()
}

/// Standard HTTP rendering of a store error: domain codes are 422 with the
/// code in the envelope; sqlite failures are logged and masked as 500.
pub fn store_error_response(slice: &'static str, err: crate::store_core::StoreError) -> Response {
    use crate::store_core::StoreError;
    match err {
        StoreError::Domain(code) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &code),
        StoreError::Sqlite(message) => {
            tracing::error!(error = %message, slice, "store failure");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "storage_failure")
        }
    }
}

/// The built SPA (frontend/dist), embedded at compile time. When the folder
/// only holds .gitkeep (no `just fe-build` yet), the registry index serves
/// at / instead.
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../frontend/dist"]
struct FrontendAssets;

pub fn build_router(state: AppState) -> Router {
    // One (id, router) pair per slice; enablement filters them. A new slice
    // missing from this list is unreachable — keep it in registry order.
    let slice_routers: Vec<(&str, Router<AppState>)> = vec![
        ("accounting", crate::slices::accounting::routes::router()),
        (
            "admin_settings",
            crate::slices::admin_settings::routes::router(),
        ),
        ("agent_mcp", crate::slices::agent_mcp::routes::router()),
        ("ai_usage", crate::slices::ai_usage::routes::router()),
        (
            "calendar_drafts",
            crate::slices::calendar_drafts::routes::router(),
        ),
        ("call_inputs", crate::slices::call_inputs::routes::router()),
        (
            "claim_drafts",
            crate::slices::claim_drafts::routes::router(),
        ),
        (
            "content_drafts",
            crate::slices::content_drafts::routes::router(),
        ),
        (
            "content_plans",
            crate::slices::content_plans::routes::router(),
        ),
        ("crm_cache", crate::slices::crm_cache::routes::router()),
        ("crm_drafts", crate::slices::crm_drafts::routes::router()),
        (
            "crm_record_drafts",
            crate::slices::crm_record_drafts::routes::router(),
        ),
        (
            "crm_sales_intent",
            crate::slices::crm_sales_intent::routes::router(),
        ),
        (
            "customer_tier_sync",
            crate::slices::customer_tier_sync::routes::router(),
        ),
        (
            "data_retention",
            crate::slices::data_retention::routes::router(),
        ),
        ("debug", crate::slices::debug::routes::router()),
        (
            "drive_corpus",
            crate::slices::drive_corpus::routes::router(),
        ),
        (
            "email_drafts",
            crate::slices::email_drafts::routes::router(),
        ),
        (
            "email_triage",
            crate::slices::email_triage::routes::router(),
        ),
        ("enrichment", crate::slices::enrichment::routes::router()),
        (
            "follow_up_tasks",
            crate::slices::follow_up_tasks::routes::router(),
        ),
        (
            "google_connector",
            crate::slices::google_connector::routes::router(),
        ),
        (
            "home_dashboard",
            crate::slices::home_dashboard::routes::router(),
        ),
        (
            "instance_diagnostics",
            crate::slices::instance_diagnostics::routes::router(),
        ),
        ("inventory", crate::slices::inventory::routes::router()),
        (
            "invoice_drafts",
            crate::slices::invoice_drafts::routes::router(),
        ),
        (
            "lead_discovery",
            crate::slices::lead_discovery::routes::router(),
        ),
        (
            "ledger_drafts",
            crate::slices::ledger_drafts::routes::router(),
        ),
        (
            "operator_notes",
            crate::slices::operator_notes::routes::router(),
        ),
        (
            "operator_users",
            crate::slices::operator_users::routes::router(),
        ),
        (
            "owner_reports",
            crate::slices::owner_reports::routes::router(),
        ),
        (
            "packet_proposals",
            crate::slices::packet_proposals::routes::router(),
        ),
        (
            "quote_workflows",
            crate::slices::quote_workflows::routes::router(),
        ),
        (
            "release_notes",
            crate::slices::release_notes::routes::router(),
        ),
        (
            "search_console",
            crate::slices::search_console::routes::router(),
        ),
        (
            "shopify_sales",
            crate::slices::shopify_sales::routes::router(),
        ),
        (
            "social_publishing",
            crate::slices::social_publishing::routes::router(),
        ),
        ("work_queue", crate::slices::work_queue::routes::router()),
    ];
    // /readyz lives beside /livez, outside slice enablement: the support hub
    // must get structured liveness from every instance, always. Common probe
    // aliases (/health, /healthz, trailing slashes) share the same handlers so
    // status monitors do not get a 200 SPA document (false-green).
    let livez_route = get(livez);
    let readyz_route = get(crate::slices::instance_diagnostics::routes::readyz);
    let mut router = Router::new()
        .route("/livez", livez_route.clone())
        .route("/livez/", livez_route.clone())
        .route("/health", livez_route.clone())
        .route("/health/", livez_route.clone())
        .route("/healthz", livez_route.clone())
        .route("/healthz/", livez_route)
        .route("/readyz", readyz_route.clone())
        .route("/readyz/", readyz_route)
        .route("/api/session", post(login_session))
        .route("/api/session/visibility", get(session_visibility))
        .route("/api/session/logout", post(logout_session))
        .merge(crate::produce::router())
        .merge(crate::outbox::router());
    #[cfg(test)]
    {
        router = router.route("/__test/panic-while-holding-db-lock", get(test_panic_route));
    }
    for (slice_id, slice_router) in slice_routers {
        if state.slice_enabled(slice_id) {
            router = router.merge(slice_router);
        } else {
            tracing::info!(slice = slice_id, "slice disabled by client overlay");
        }
    }
    install_panic_hook(state.clone());
    let panic_response_state = state.clone();
    let flush_state = state.clone();
    router
        .fallback(get(spa_or_index))
        .layer(CatchPanicLayer::custom(move |err| {
            panic_response(err, panic_response_state.clone())
        }))
        .layer(middleware::from_fn(move |request, next| {
            let state = flush_state.clone();
            async move { flush_pending_panic_diagnostics_middleware(state, request, next).await }
        }))
        .with_state(state)
}

fn panic_response(_: Box<dyn std::any::Any + Send + 'static>, state: AppState) -> Response {
    flush_pending_panic_diagnostics_for(&state);
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "handler_panicked")
}

async fn flush_pending_panic_diagnostics_middleware(
    state: AppState,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    try_flush_pending_panic_diagnostics_for(&state);
    next.run(request).await
}

#[cfg(test)]
async fn test_panic_route(State(state): State<AppState>) -> Response {
    let _panic_state = enter_thread_panic_state(state.clone());
    let _persistence = state.persistence.lock();
    panic!("test handler panic while holding persistence lock");
}

async fn login_session(
    State(state): axum::extract::State<AppState>,
    Json(body): Json<OperatorSessionLoginRequest>,
) -> Response {
    let token = body.token.trim();
    if token.is_empty() {
        return error_response(StatusCode::UNAUTHORIZED, "operator_token_invalid");
    }
    match state.create_operator_session_for_token(token) {
        Ok(session_id) => (
            [(header::SET_COOKIE, session_cookie(&session_id))],
            Json(OperatorSessionResponse { ok: true }),
        )
            .into_response(),
        Err(denied) => *denied,
    }
}

async fn logout_session(
    State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
) -> Response {
    state.clear_operator_session(&headers);
    (
        [(header::SET_COOKIE, clear_session_cookie())],
        Json(OperatorSessionResponse { ok: true }),
    )
        .into_response()
}

async fn session_visibility(
    State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
) -> Response {
    let auth = match state.authenticate(&headers) {
        Ok(auth) => auth,
        Err(denied) => return *denied,
    };
    match crate::operator_visibility::visible_slice_ids(&state, &auth.scope, &auth.actor_id) {
        Ok(visible_slices) => {
            Json(OperatorSessionVisibilityResponse { visible_slices }).into_response()
        }
        Err(err) => store_error_response("operator_visibility", err),
    }
}

async fn livez() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "ok",
    )
        .into_response()
}

fn is_infra_probe_path(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/').to_ascii_lowercase().as_str(),
        "livez" | "readyz" | "health" | "healthz"
    )
}

/// Serve the embedded SPA: exact asset match first, then index.html for any
/// non-API GET (client-side routing), then the registry index when no bundle
/// is embedded. Infra probe paths never fall through to HTML — a 200 SPA
/// document is a false-green for status monitors.
async fn spa_or_index(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || is_infra_probe_path(path) {
        return error_response(StatusCode::NOT_FOUND, "route_not_found");
    }
    if !path.is_empty() {
        if let Some(asset) = FrontendAssets::get(path) {
            let mime = mime_for(path);
            return (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, cache_control_for_asset(path)),
                ],
                asset.data,
            )
                .into_response();
        }
    }
    if let Some(index) = FrontendAssets::get("index.html") {
        return (
            [(header::CACHE_CONTROL, SPA_HTML_CACHE_CONTROL)],
            axum::response::Html(index.data),
        )
            .into_response();
    }
    let mut response = index().await.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(SPA_HTML_CACHE_CONTROL),
    );
    response
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn cache_control_for_asset(path: &str) -> &'static str {
    if path.starts_with("assets/") && is_vite_hashed_asset(path) {
        SPA_IMMUTABLE_CACHE_CONTROL
    } else {
        SPA_HTML_CACHE_CONTROL
    }
}

fn is_vite_hashed_asset(path: &str) -> bool {
    let Some(file_name) = path.rsplit('/').next() else {
        return false;
    };
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    const VITE_HASH_LEN: usize = 8;
    let Some(hash_start) = stem.len().checked_sub(VITE_HASH_LEN) else {
        return false;
    };
    if hash_start == 0 || stem.as_bytes().get(hash_start - 1) != Some(&b'-') {
        return false;
    }
    stem.as_bytes()[hash_start..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
}

/// Minimal operator landing page: which slices are live and where their
/// routes are. No data and no secrets — just orientation.
async fn index() -> axum::response::Html<String> {
    let mut body = String::from(
        "<!doctype html><title>BusinessOS</title>\
         <h1>BusinessOS</h1><p>Server is up. Operator routes (Bearer \
         <code>BOS_OPERATOR_TOKEN</code> required when set):</p>",
    );
    for slice in crate::slices::registry() {
        body.push_str(&format!("<h3>{}</h3><ul>", slice.title));
        for route in slice.routes {
            body.push_str(&format!(
                "<li><code>{} {}</code> — {}</li>",
                route.method, route.path, route.summary
            ));
        }
        body.push_str("</ul>");
    }
    body.push_str(
        "<p><code>GET /livez</code> — health, <code>GET /readyz</code> — structured liveness</p>",
    );
    axum::response::Html(body)
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    // code-shape: test-env-access begin
    pub struct EnvGuard {
        old: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        pub fn set(key: &'static str, value: &str) -> Self {
            Self::set_many(&[(key, value)])
        }

        pub fn set_many(entries: &[(&'static str, &str)]) -> Self {
            let lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let mut old = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                old.push((*key, std::env::var_os(*key)));
                unsafe {
                    std::env::set_var(*key, *value);
                }
            }
            Self { old, _lock: lock }
        }

        pub fn unset(key: &'static str) -> Self {
            let lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let old = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self {
                old: vec![(key, old)],
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                for (key, old) in self.old.iter().rev() {
                    match old {
                        Some(old) => std::env::set_var(key, old),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
    // code-shape: test-env-access end

    /// test_state with an operator token and/or an explicit enabled-slices
    /// list (for auth and enablement route tests).
    pub fn test_state_configured(
        operator_token: Option<&str>,
        enabled_slices: &[&str],
    ) -> AppState {
        let mut state = test_state();
        state.operator_token = operator_token.map(Into::into);
        state.enabled_slices = enabled_slices
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .into();
        state
    }

    pub fn test_state() -> AppState {
        let persistence = PersistencePool::open_in_memory().expect("in-memory db");
        let schema_version = persistence.schema_version();
        AppState {
            persistence,
            schema_version,
            client_id: "test-client".into(),
            display_name: "BusinessOS".into(),
            started_at_ms: now_ms(),
            operator_token: None,
            operator_sessions: Arc::new(NonPoisoningMutex::new(HashMap::new())),
            revoked_operator_sessions: Arc::new(NonPoisoningMutex::new(
                std::collections::HashSet::new(),
            )),
            enabled_slices: Vec::new().into(),
            produce_in_flight: Arc::new(NonPoisoningMutex::new(std::collections::HashSet::new())),
            sync_guards: SyncGuards::default(),
            drive_corpus_overlay: Arc::new(None),
            search_console_overlay: Arc::new(None),
            quote_workflows_overlay: Arc::new(crate::overlay::QuoteWorkflowsOverlay::default()),
            owner_reports_overlay: Arc::new(None),
            email_triage_overlay: Arc::new(crate::overlay::EmailTriageOverlay::default()),
            work_queue_overlay: Arc::new(crate::overlay::WorkQueueOverlay::default()),
            accounting_overlay: Arc::new(crate::overlay::AccountingOverlay::default()),
            accounting_visibility_policy: crate::overlay::AccountingVisibilityPolicy::default(),
            lead_discovery_overlay: Arc::new(crate::overlay::LeadDiscoveryOverlay::default()),
            call_inputs_overlay: Arc::new(crate::overlay::CallInputsOverlay::default()),
            customer_tier_sync_overlay: Arc::new(crate::overlay::CustomerTierSyncOverlay::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::test_state_configured;
    use super::*;
    use axum::body::Body;
    use bos_contracts::calendar_drafts::{CalendarDraftStatus, CalendarEventDraft};
    use bos_contracts::crm_drafts::{CrmDraftStatus, CrmNoteDraft};
    use bos_contracts::email_drafts::{EmailDraftStatus, EmailReplyDraft};
    use bos_contracts::email_triage::FALLBACK_CATEGORY_ID;
    use bos_contracts::operator_users::OperatorUser;
    use bos_contracts::work_queue::{WorkItem, WorkItemStatus};
    use bos_integrations::accounting_read::InvoiceRecord;
    use std::sync::OnceLock;
    use tower::ServiceExt;

    fn panic_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[test]
    fn operator_identity_scope_maps_shared_and_named_users() {
        assert_eq!(OperatorIdentity::shared().scope(), OperatorScope::All);

        let identity = OperatorIdentity {
            actor_id: "u1".to_string(),
            display_name: "User One".to_string(),
        };
        assert_eq!(identity.scope(), OperatorScope::User("u1".to_string()));
    }

    #[test]
    fn operator_scope_sql_params_bind_all_flag_and_user_id() {
        assert_eq!(OperatorScope::All.sql_params(), (1, String::new()));
        assert_eq!(
            scope_sql_params(&OperatorScope::User("u1".to_string())),
            (0, "u1".to_string())
        );
    }

    #[test]
    fn operator_scope_matches_source_user_truth_table() {
        assert!(OperatorScope::All.matches_source_user(None));
        assert!(OperatorScope::All.matches_source_user(Some("u1")));
        assert!(OperatorScope::All.matches_source_user(Some("u2")));

        let scope = OperatorScope::User("u1".to_string());
        assert!(scope.matches_source_user(Some("u1")));
        assert!(!scope.matches_source_user(Some("u2")));
        assert!(!scope.matches_source_user(None));

        assert!(matches_source_user(&scope, Some("u1")));
        assert!(!matches_source_user(&scope, None));
    }

    #[test]
    fn operator_scope_require_source_user_rejects_cross_scope() {
        assert!(OperatorScope::All.require_source_user(None).is_ok());
        assert!(OperatorScope::User("u1".to_string())
            .require_source_user(Some("u1"))
            .is_ok());

        for source_user_id in [None, Some("u2")] {
            let err = require_source_user(&OperatorScope::User("u1".to_string()), source_user_id)
                .expect_err("cross-scope source should be rejected");
            assert!(matches!(
                err,
                crate::store_core::StoreError::Domain(code) if code == "scope_forbidden"
            ));
        }
    }

    fn e2e_operator(user_id: &str) -> OperatorUser {
        OperatorUser {
            user_id: user_id.to_string(),
            display_name: user_id.to_string(),
            active: true,
            archived_at_ms: None,
            default_calendar_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    fn e2e_invoice(id: &str) -> InvoiceRecord {
        InvoiceRecord {
            invoice_id: id.to_string(),
            doc_number: Some(format!("DOC-{id}")),
            customer_id: Some("c1".to_string()),
            customer_name: Some("Customer".to_string()),
            txn_date: Some("2026-06-01".to_string()),
            due_date: Some("2026-07-01".to_string()),
            total_amt_cents: 10_000,
            balance_cents: 10_000,
            voided: false,
            updated_at: "2026-06-01T00:00:00-07:00".to_string(),
        }
    }

    fn e2e_message(
        message_id: &str,
        source_user_id: Option<&str>,
    ) -> crate::slices::email_triage::store::InboundMessageRecord {
        crate::slices::email_triage::store::InboundMessageRecord {
            source_key: message_id.to_string(),
            message_id: message_id.to_string(),
            thread_id: Some(format!("thr-{message_id}")),
            internal_date_ms: Some(1_000),
            from_addr: Some(format!("{message_id}@example.test")),
            to_addr: Some("ops@example.test".to_string()),
            subject: Some(format!("Subject {message_id}")),
            body_excerpt: "Body".to_string(),
            body_full: "Body".to_string(),
            headers: Vec::new(),
            labels: Vec::new(),
            resolved_category: FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: source_user_id.map(str::to_string),
        }
    }

    fn e2e_item(item_id: &str, message_id: &str, source_user_id: Option<&str>) -> WorkItem {
        WorkItem {
            item_id: item_id.to_string(),
            source_kind: "email".to_string(),
            source_ref: message_id.to_string(),
            category_id: FALLBACK_CATEGORY_ID.to_string(),
            title: format!("Work {message_id}"),
            summary: "Summary".to_string(),
            packet_kinds: vec![
                "calendar_event_draft".to_string(),
                "email_draft_reply".to_string(),
                "crm_activity".to_string(),
            ],
            status: WorkItemStatus::Open,
            accept_actor: None,
            ai_suggested: false,
            rationale: String::new(),
            produce_guidance: String::new(),
            source_user_id: source_user_id.map(str::to_string),
            assignee_user_id: None,
            visible_to_user_ids: Vec::new(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    fn e2e_calendar_draft(draft_id: &str, item: &WorkItem) -> CalendarEventDraft {
        CalendarEventDraft {
            draft_id: draft_id.to_string(),
            item_id: item.item_id.clone(),
            source_kind: item.source_kind.clone(),
            source_ref: item.source_ref.clone(),
            source_user_id: item.source_user_id.clone(),
            status: CalendarDraftStatus::Staged,
            title: format!("Event {}", item.source_ref),
            start_at: "2026-06-12T16:00:00-04:00".to_string(),
            end_at: "2026-06-12T17:00:00-04:00".to_string(),
            timezone: Some("America/New_York".to_string()),
            location: None,
            description: None,
            calendar_id: None,
            attendees: Vec::new(),
            send_invitations: false,
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            outbox_job_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    fn e2e_email_draft(draft_id: &str, item: &WorkItem) -> EmailReplyDraft {
        EmailReplyDraft {
            draft_id: draft_id.to_string(),
            item_id: item.item_id.clone(),
            source_kind: item.source_kind.clone(),
            source_ref: item.source_ref.clone(),
            source_user_id: item.source_user_id.clone(),
            status: EmailDraftStatus::Staged,
            to_addr: "customer@example.test".to_string(),
            cc_addrs: Vec::new(),
            subject: format!("Re: {}", item.source_ref),
            body_text: "Draft body".to_string(),
            thread_id: Some(format!("thr-{}", item.source_ref)),
            reply_message_id: None,
            reference_message_ids: Vec::new(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            outbox_job_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    fn e2e_crm_draft(draft_id: &str, item: &WorkItem) -> CrmNoteDraft {
        CrmNoteDraft {
            draft_id: draft_id.to_string(),
            item_id: item.item_id.clone(),
            source_kind: item.source_kind.clone(),
            source_ref: item.source_ref.clone(),
            source_user_id: item.source_user_id.clone(),
            status: CrmDraftStatus::Staged,
            note_body: "Customer called about a quote.".to_string(),
            contact_email: Some("customer@example.test".to_string()),
            occurred_at: "2026-06-10T12:34:56Z".to_string(),
            provenance: Vec::new(),
            model: "test-model".to_string(),
            confidence: "high".to_string(),
            outbox_job_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    fn e2e_terminal_job(conn: &mut rusqlite::Connection, job_id: &str) {
        let job = crate::outbox::NewOutboxJob {
            job_id: job_id.to_string(),
            provider: "google_calendar".to_string(),
            capability: "create_event".to_string(),
            payload_json: "{}".to_string(),
            source_entity_kind: "calendar_event_draft".to_string(),
            source_entity_id: "ced_null".to_string(),
            correlation_id: None,
            causation_id: None,
            idempotency_key: format!("enqueue:{job_id}"),
        };
        crate::store_core::mutate(
            conn,
            crate::store_core::MutationRequest {
                client_id: "test-client",
                entity_kind: "calendar_event_draft",
                entity_id: "ced_null",
                change_kind: "approve",
                actor_id: "operator",
                actor_kind: bos_contracts::receipt::ActorKindDto::Operator,
                expected_revision: None,
                idempotency_key: &format!("approve:{job_id}"),
                correlation_id: None,
                causation_id: None,
                before_json: None,
                after_json: None,
                now_ms: 1_000,
            },
            move |tx| crate::outbox::enqueue_within(tx, "test-client", &job, 1_000),
        )
        .expect("enqueue");
        let claimed = crate::outbox::claim_due_jobs(conn, "test-client", None, 60_000, 10, 2_000)
            .expect("claim");
        crate::outbox::record_attempt(
            conn,
            "test-client",
            &claimed[0],
            &crate::outbox::AttemptOutcome::Terminal {
                error: "auth_failed".to_string(),
                result_json: None,
            },
            3_000,
        )
        .expect("terminal");
    }

    fn seed_e2e_isolation_data(state: &AppState) {
        let mut persistence = state.persistence.lock();
        let conn = persistence.connection();
        crate::slices::operator_users::store::create_user(
            conn,
            "test-client",
            "operator",
            &e2e_operator("jordan"),
            "tok_jordan",
            "create_jordan",
        )
        .expect("jordan");
        crate::slices::operator_users::store::create_user(
            conn,
            "test-client",
            "operator",
            &e2e_operator("dana"),
            "tok_dana",
            "create_dana",
        )
        .expect("dana");

        let rows = [
            (
                "m-jordan",
                "wi_jordan",
                Some("jordan"),
                "ced_jordan",
                "erd_jordan",
                "cnd_jordan",
            ),
            (
                "m-dana",
                "wi_dana",
                Some("dana"),
                "ced_dana",
                "erd_dana",
                "cnd_dana",
            ),
            (
                "m-null", "wi_null", None, "ced_null", "erd_null", "cnd_null",
            ),
        ];
        for (message_id, item_id, source_user_id, calendar_id, email_id, crm_id) in rows {
            crate::slices::email_triage::store::record_inbound_message(
                conn,
                "test-client",
                &e2e_message(message_id, source_user_id),
            )
            .expect("message");
            let item = e2e_item(item_id, message_id, source_user_id);
            crate::slices::work_queue::store::insert_item(conn, "test-client", &item)
                .expect("item");
            crate::slices::calendar_drafts::store::insert_draft(
                conn,
                "test-client",
                "operator",
                &e2e_calendar_draft(calendar_id, &item),
                &format!("stage:{calendar_id}"),
            )
            .expect("calendar draft");
            crate::slices::email_drafts::store::insert_draft(
                conn,
                "test-client",
                "operator",
                &e2e_email_draft(email_id, &item),
                &format!("stage:{email_id}"),
            )
            .expect("email draft");
            crate::slices::crm_drafts::store::insert_draft(
                conn,
                "test-client",
                "operator",
                &e2e_crm_draft(crm_id, &item),
                &format!("stage:{crm_id}"),
            )
            .expect("crm draft");
        }
        e2e_terminal_job(conn, "job_retry_e2e");
    }

    async fn json_request(
        router: axum::Router,
        method: axum::http::Method,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> axum::response::Response {
        let mut builder = axum::http::Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        router
            .oneshot(builder.body(body).expect("request"))
            .await
            .expect("response")
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("json response")
    }

    #[tokio::test]
    async fn users_route_rejects_self_archive() {
        let state = test_state_configured(None, &[]);
        {
            let mut persistence = state.persistence.lock();
            crate::slices::operator_users::store::create_user(
                persistence.connection(),
                "test-client",
                "operator",
                &e2e_operator("jordan"),
                "tok_jordan",
                "create_jordan",
            )
            .expect("create user");
        }
        let router = build_router(state.clone());

        let response = json_request(
            router,
            axum::http::Method::POST,
            "/api/users/jordan/action",
            Some("tok_jordan"),
            Some(serde_json::json!({
                "action": "archive",
                "expected_revision": null,
                "idempotency_key": "archive_jordan",
                "actor_id": null
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let user = {
            let persistence = state.persistence.lock();
            crate::slices::operator_users::store::get_user(
                persistence.connection_ref(),
                "test-client",
                "jordan",
            )
            .expect("get user")
            .expect("user")
        };
        assert!(user.archived_at_ms.is_none());
    }

    fn json_string_set(value: &serde_json::Value, pointer: &str, field: &str) -> Vec<String> {
        let mut ids: Vec<String> = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .expect("array")
            .iter()
            .map(|entry| {
                entry
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .expect("string field")
                    .to_string()
            })
            .collect();
        ids.sort();
        ids
    }

    fn response_error_code(body: serde_json::Value) -> String {
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .expect("error code")
            .to_string()
    }

    fn draft_id_set(value: &serde_json::Value) -> Vec<String> {
        let mut ids: Vec<String> = value
            .get("drafts")
            .and_then(serde_json::Value::as_array)
            .expect("drafts")
            .iter()
            .map(|entry| {
                entry
                    .get("draft")
                    .and_then(|draft| draft.get("draft_id"))
                    .and_then(serde_json::Value::as_str)
                    .expect("draft id")
                    .to_string()
            })
            .collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn api_e2e_composes_per_user_isolation_across_operator_surfaces() {
        let state = test_state_configured(None, &[]);
        seed_e2e_isolation_data(&state);
        let router = build_router(state.clone());

        let inbox_for = |router: axum::Router, token: Option<&'static str>| async move {
            let response = json_request(
                router,
                axum::http::Method::GET,
                "/api/email-triage/inbox",
                token,
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            json_string_set(&response_json(response).await, "/messages", "message_id")
        };
        assert_eq!(
            inbox_for(router.clone(), Some("tok_jordan")).await,
            vec!["m-jordan"]
        );
        assert_eq!(
            inbox_for(router.clone(), Some("tok_dana")).await,
            vec!["m-dana"]
        );
        assert_eq!(
            inbox_for(router.clone(), None).await,
            vec!["m-dana", "m-jordan", "m-null"]
        );

        let queue_for = |router: axum::Router, token: Option<&'static str>| async move {
            let response = json_request(
                router,
                axum::http::Method::GET,
                "/api/work-queue",
                token,
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let mut ids: Vec<String> = response_json(response)
                .await
                .get("items")
                .and_then(serde_json::Value::as_array)
                .expect("items")
                .iter()
                .map(|entry| {
                    entry
                        .get("item")
                        .and_then(|item| item.get("item_id"))
                        .and_then(serde_json::Value::as_str)
                        .expect("item_id")
                        .to_string()
                })
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(
            queue_for(router.clone(), Some("tok_jordan")).await,
            vec!["wi_jordan"]
        );
        assert_eq!(
            queue_for(router.clone(), Some("tok_dana")).await,
            vec!["wi_dana"]
        );
        assert_eq!(
            queue_for(router.clone(), None).await,
            vec!["wi_dana", "wi_jordan", "wi_null"]
        );

        for (path, jordan_id, dana_id, null_id) in [
            ("/api/calendar-drafts", "ced_jordan", "ced_dana", "ced_null"),
            ("/api/email-drafts", "erd_jordan", "erd_dana", "erd_null"),
            ("/api/crm-drafts", "cnd_jordan", "cnd_dana", "cnd_null"),
        ] {
            let response = json_request(
                router.clone(),
                axum::http::Method::GET,
                path,
                Some("tok_jordan"),
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{path} jordan");
            assert_eq!(
                draft_id_set(&response_json(response).await),
                vec![jordan_id]
            );

            let response = json_request(
                router.clone(),
                axum::http::Method::GET,
                path,
                Some("tok_dana"),
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{path} dana");
            assert_eq!(draft_id_set(&response_json(response).await), vec![dana_id]);

            let response =
                json_request(router.clone(), axum::http::Method::GET, path, None, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{path} all");
            assert_eq!(
                draft_id_set(&response_json(response).await),
                vec![dana_id, jordan_id, null_id]
            );
        }

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/work-queue/wi_dana/action",
            Some("tok_jordan"),
            Some(serde_json::json!({
                "action": "accept",
                "expected_revision": null,
                "idempotency_key": "jordan_accept_dana",
                "actor_id": "jordan"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_error_code(response_json(response).await),
            "scope_forbidden"
        );

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/email-drafts/erd_dana/action",
            Some("tok_jordan"),
            Some(serde_json::json!({
                "action": "reject",
                "expected_revision": null,
                "idempotency_key": "jordan_reject_dana_email",
                "actor_id": "jordan"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_error_code(response_json(response).await),
            "scope_forbidden"
        );

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/email-triage/reclassify",
            Some("tok_jordan"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_error_code(response_json(response).await),
            "scope_forbidden"
        );

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/email-triage/ai-retriage-reset",
            Some("tok_jordan"),
            Some(serde_json::json!({
                "scope": "all",
                "idempotency_key": "reset_named",
                "actor_id": "jordan"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_error_code(response_json(response).await),
            "scope_forbidden"
        );

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/outbox-jobs/job_retry_e2e/retry",
            Some("tok_jordan"),
            Some(serde_json::json!({
                "idempotency_key": "retry_named_e2e",
                "actor_id": "jordan"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_error_code(response_json(response).await),
            "scope_forbidden"
        );

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/email-triage/reclassify",
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = json_request(
            router.clone(),
            axum::http::Method::POST,
            "/api/email-triage/ai-retriage-reset",
            None,
            Some(serde_json::json!({
                "scope": "all",
                "idempotency_key": "reset_all",
                "actor_id": "operator"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = json_request(
            router,
            axum::http::Method::POST,
            "/api/outbox-jobs/job_retry_e2e/retry",
            None,
            Some(serde_json::json!({
                "idempotency_key": "retry_all_e2e",
                "actor_id": "operator"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn connector_connected_page_is_humanized_and_auto_redirects() {
        let html = connector_connected_html(
            "Google",
            "Your inbox and calendar are linked. New email will start arriving in a moment.",
        );
        // Operator-facing: plain success, no internal mechanics.
        assert!(html.contains("Google connected"));
        for jargon in ["pump", "credential", "close this tab", "next cycle"] {
            assert!(!html.contains(jargon), "leaked operator jargon: {jargon}");
        }
        // Auto-redirect back into the app (meta refresh + JS), with a manual fallback.
        assert!(html.contains("http-equiv=\"refresh\""));
        assert!(html.contains("location.replace(\"/\")"));
        assert!(html.contains("href=\"/\""));
    }

    #[test]
    fn connector_connected_page_escapes_copy_fields() {
        let html =
            connector_connected_html("Google <script>", "Inbox & calendar \"ready\" <now> 'soon'");

        assert!(html.contains("Google &lt;script&gt; connected"));
        assert!(html.contains("Inbox &amp; calendar &quot;ready&quot; &lt;now&gt; &#39;soon&#39;"));
        assert!(!html.contains("Google <script> connected"));
        assert!(!html.contains("calendar \"ready\" <now>"));
    }

    #[test]
    fn secondary_locks_do_not_poison_on_panic() {
        let state = super::test_support::test_state();

        macro_rules! assert_lock_survives_panic {
            ($state:expr, $lock:expr, $label:literal) => {{
                let lock = $lock.clone();
                let panicking_state = $state.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _panic_state = enter_thread_panic_state(panicking_state.clone());
                    let _guard = lock.lock();
                    panic!("deliberate panic: {}", $label);
                }));
                assert!(result.is_err(), "panic should be caught for {}", $label);
                drop($lock.lock());
            }};
        }

        assert_lock_survives_panic!(state, state.operator_sessions, "operator_sessions");
        assert_lock_survives_panic!(
            state,
            state.revoked_operator_sessions,
            "revoked_operator_sessions"
        );
        assert_lock_survives_panic!(state, state.produce_in_flight, "produce_in_flight");
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::Accounting),
            "accounting_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::CrmCache),
            "crm_cache_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::Stockforge),
            "stockforge_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::Drive),
            "drive_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::SearchConsole),
            "search_console_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::Claims),
            "claims_sync_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::CallInputTranscription),
            "call_input_transcription_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::LeadDiscoveryAutoscrape),
            "lead_discovery_autoscrape_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::EnrichmentFreshness),
            "enrichment_freshness_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::ReportGenerate),
            "report_generate_status"
        );
        assert_lock_survives_panic!(
            state,
            state.sync_guards.guard(Pump::ShopifySales),
            "shopify_sales_sync_status"
        );
    }

    #[tokio::test]
    async fn persistence_or_busy_returns_503_when_pool_is_exhausted() {
        let pool =
            PersistencePool::open_in_memory_with_config(1, std::time::Duration::from_millis(100))
                .expect("pool");
        let schema_version = pool.schema_version();
        let held = pool.lock();
        let mut state = super::test_support::test_state();
        state.schema_version = schema_version;
        state.persistence = pool;

        let result = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            state.persistence_or_busy()
        })
        .await
        .expect("pool acquisition should time out promptly");
        assert!(result.is_err(), "pool exhaustion should return 503");
        drop(held);
    }

    #[tokio::test]
    async fn liveness_probes_are_plain_ok_not_spa_html() {
        let router = build_router(test_state_configured(None, &[]));

        for path in ["/livez", "/livez/", "/health", "/health/", "/healthz", "/healthz/"] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::get(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                content_type.starts_with("text/plain"),
                "{path} must be text/plain, got {content_type}"
            );
            let body = http_body_util::BodyExt::collect(response.into_body())
                .await
                .expect("body")
                .to_bytes();
            assert_eq!(&body[..], b"ok", "{path}");
        }

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::head("/livez")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("text/plain"),
            "HEAD /livez must be text/plain, got {content_type}"
        );

        let response = router
            .oneshot(
                axum::http::Request::get("/readyz/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("application/json"),
            "/readyz/ must be JSON, got {content_type}"
        );
    }

    #[tokio::test]
    async fn embedded_spa_index_uses_revalidation_cache_policy() {
        let router = build_router(test_state_configured(None, &[]));

        for path in ["/", "/queue"] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::get(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&HeaderValue::from_static(SPA_HTML_CACHE_CONTROL)),
                "{path} should revalidate the SPA bootstrap document"
            );
        }

        let response = router
            .oneshot(
                axum::http::Request::head("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static(SPA_HTML_CACHE_CONTROL))
        );
    }

    #[tokio::test]
    async fn embedded_vite_assets_use_immutable_cache_policy_when_bundle_is_present() {
        let Some(asset_path) = FrontendAssets::iter()
            .find(|path| path.starts_with("assets/") && path.ends_with(".js"))
        else {
            // Clean source checkouts intentionally commit only frontend/dist/.gitkeep.
            // The policy helper below covers classification; this route-level
            // assertion runs when a local frontend build has embedded assets.
            eprintln!("skipping embedded asset route assertion: frontend/dist/assets is empty");
            return;
        };
        let router = build_router(test_state_configured(None, &[]));

        let response = router
            .oneshot(
                axum::http::Request::get(format!("/{asset_path}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static(SPA_IMMUTABLE_CACHE_CONTROL))
        );
    }

    #[test]
    fn spa_asset_cache_policy_distinguishes_hashed_assets() {
        assert_eq!(
            cache_control_for_asset("assets/index-AbCdEf12.js"),
            SPA_IMMUTABLE_CACHE_CONTROL
        );
        assert_eq!(
            cache_control_for_asset("assets/index-djzAN5oK.css"),
            SPA_IMMUTABLE_CACHE_CONTROL
        );
        assert_eq!(
            cache_control_for_asset("assets/index-BUg-gWZZ.js"),
            SPA_IMMUTABLE_CACHE_CONTROL
        );
        assert_eq!(
            cache_control_for_asset("assets/index.css"),
            SPA_HTML_CACHE_CONTROL
        );
        assert_eq!(
            cache_control_for_asset("images/logo-AbCdEf12.png"),
            SPA_HTML_CACHE_CONTROL
        );
        assert_eq!(
            cache_control_for_asset("manifest.json"),
            SPA_HTML_CACHE_CONTROL
        );
    }

    #[tokio::test]
    async fn session_login_sets_httponly_cookie_and_logout_clears_it() {
        let router = build_router(test_state_configured(Some("secret"), &[]));

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::post("/api/session")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("set-cookie")
            .to_string();
        assert!(cookie.starts_with("bos_operator_session="));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=604800"));
        assert!(!cookie.contains("secret"));

        let session_pair = cookie.split(';').next().expect("cookie pair").to_string();
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/api/diagnostics/health")
                    .header(header::COOKIE, &session_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::post("/api/session/logout")
                    .header(header::COOKIE, &session_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let clear_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("clear cookie");
        assert!(clear_cookie.contains("Max-Age=0"));

        let response = router
            .oneshot(
                axum::http::Request::get("/api/diagnostics/health")
                    .header(header::COOKIE, &session_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_visibility_is_core_and_uses_cached_accounting_policy() {
        let mut state = test_state_configured(Some("tok_all"), &["accounting", "admin_settings"]);
        state.accounting_visibility_policy =
            crate::overlay::AccountingVisibilityPolicy::AuthorizerOnly;
        {
            let mut persistence = state.persistence.lock();
            let conn = persistence.connection();
            crate::slices::operator_users::store::create_user(
                conn,
                &state.client_id,
                "operator",
                &e2e_operator("user_casey"),
                "tok_casey",
                "create_casey",
            )
            .expect("create casey");
            crate::slices::accounting::store::upsert_invoice_snapshots(
                conn,
                &state.client_id,
                &[e2e_invoice("i1")],
                1_000,
            )
            .expect("seed stale accounting cache");
        }
        let router = build_router(state);

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/api/session/visibility")
                    .header(header::AUTHORIZATION, "Bearer tok_casey")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "session visibility must not depend on instance_diagnostics being enabled"
        );
        let body = response_json(response).await;
        assert_eq!(
            body["visible_slices"],
            serde_json::json!([]),
            "user-scope operators should not see all-scope-only admin settings or stale authorizer-only accounting"
        );

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/api/session/visibility")
                    .header(header::AUTHORIZATION, "Bearer tok_all")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(
            body["visible_slices"],
            serde_json::json!(["accounting", "admin_settings"]),
            "all-scope operators should see admin settings"
        );

        let health = router
            .oneshot(
                axum::http::Request::get("/api/diagnostics/health")
                    .header(header::AUTHORIZATION, "Bearer tok_casey")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("health response");
        assert_eq!(
            health.status(),
            StatusCode::NOT_FOUND,
            "diagnostics remains disabled for this overlay"
        );
    }

    #[tokio::test]
    async fn session_cookie_survives_empty_process_session_map() {
        let state = test_state_configured(Some("secret"), &[]);
        let router = build_router(state.clone());

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::post("/api/session")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("set-cookie");
        let session_pair = cookie.split(';').next().expect("cookie pair").to_string();
        state.operator_sessions.lock().clear();

        let response = router
            .oneshot(
                axum::http::Request::get("/api/diagnostics/health")
                    .header(header::COOKIE, &session_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn panic_layer_records_diagnostic_and_keeps_db_usable() {
        let _guard = panic_test_lock().lock().await;
        let state = test_state_configured(None, &[]);
        let router = build_router(state.clone());

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/__test/panic-while-holding-db-lock")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("panic response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/readyz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("ready response");
        assert_eq!(response.status(), StatusCode::OK);

        let rows = {
            let persistence = state.persistence.lock();
            crate::slices::debug::store::list_recent(
                persistence.connection_ref(),
                &state.client_id,
                200,
            )
            .expect("debug rows")
        };
        let row = rows
            .iter()
            .find(|row| {
                row.source == "panic"
                    && row.error_message.as_deref().is_some_and(|message| {
                        message.contains("test handler panic while holding persistence lock")
                    })
            })
            .expect("panic diagnostic");
        assert_eq!(row.category, "panic");
        let message = row.error_message.as_deref().expect("panic message");
        assert!(message.contains("test handler panic while holding persistence lock"));
        assert!(message.contains("backtrace:"));

        let receipts = {
            let persistence = state.persistence.lock();
            crate::store_core::receipts_for_entity(
                persistence.connection_ref(),
                &state.client_id,
                "panic_diagnostic",
                row.reference_id.as_deref().expect("panic diagnostic id"),
                10,
            )
            .expect("panic diagnostic receipts")
        };
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].change_kind, "record");
    }

    #[tokio::test]
    async fn normal_request_flushes_deferred_panic_diagnostic() {
        let _guard = panic_test_lock().lock().await;
        let state = test_state_configured(None, &[]);
        let router = build_router(state.clone());
        let panic_state = state.clone();

        std::thread::Builder::new()
            .name("panic-diagnostic-test-worker".to_string())
            .spawn(move || {
                let _panic_state = enter_thread_panic_state(panic_state.clone());
                let _persistence = panic_state.persistence.lock();
                panic!("test worker panic while holding persistence lock");
            })
            .expect("spawn panic diagnostic test worker")
            .join()
            .expect_err("worker should panic");

        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get("/readyz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("ready response");
        assert_eq!(response.status(), StatusCode::OK);

        let rows = {
            let persistence = state.persistence.lock();
            crate::slices::debug::store::list_recent(
                persistence.connection_ref(),
                &state.client_id,
                200,
            )
            .expect("debug rows")
        };
        let row = rows
            .iter()
            .find(|row| {
                row.source == "panic"
                    && row.error_message.as_deref().is_some_and(|message| {
                        message.contains("test worker panic while holding persistence lock")
                    })
            })
            .expect("deferred panic diagnostic");
        assert_eq!(row.category, "panic");
    }
}
