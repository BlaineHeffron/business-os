//! Accounting sync pump: bounded, incremental, rate-limit-respecting. Off
//! unless BOS_ACCOUNTING_SYNC_ENABLED; the manual Sync-now route runs the
//! same cycle core through the same serialization guard, so there is NEVER
//! more than one provider request in flight from this process.
//!
//! Read-limit posture (the reason this slice exists in this shape):
//! - hard request budget per cycle (BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE)
//! - incremental updated-at walks with positional paging; the cursor only
//!   advances after its page's rows commit, so failures resume exactly
//!   where they stopped — no re-spending
//! - 429 honors Retry-After, stamps a backoff deadline, and stops the WHOLE
//!   cycle (providers throttle the account, not one entity)
//! - the persistence lock is never held across an HTTP call
//!
//! The provider resolves per cycle (BOS_ACCOUNTING_PROVIDER): each builds an
//! [`AccountingReadClient`] plus an [`AuthRecovery`] for mid-cycle credential
//! expiry (QBO refreshes + persists its rotated grant; static-token
//! providers can't recover).

use std::sync::Arc;
use std::time::Duration;

use bos_integrations::accounting_read::{
    AccountingError, AccountingReadClient, BillRecord, CustomerRecord, InvoiceRecord, Page,
    PageRequest, PnlReportRequest, ACCOUNTING_MAX_PAGE_SIZE,
};
use bos_integrations::qbo_oauth::{LiveQboTokenRefresher, QboTokenGrant, QboTokenRefresher};
use bos_integrations::qbo_read::{LiveQboReadClient, QboHttp, ReqwestQboHttpClient};
use parking_lot::Mutex;

use super::store::{self, QboSyncCursor, StoredQboCredential};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

/// Mid-cycle credential recovery seam: called at most once per cycle when
/// the provider reports an expired credential. Implementations refresh,
/// PERSIST the rotated grant, and install the fresh token into the read
/// client. Static-token providers use [`NoAuthRecovery`].
pub trait AuthRecovery: Send + Sync {
    fn recover(&self, state: &AppState, now: u64) -> Result<(), String>;
}

/// For providers whose credential cannot be refreshed mid-cycle.
pub struct NoAuthRecovery;

impl AuthRecovery for NoAuthRecovery {
    fn recover(&self, _state: &AppState, _now: u64) -> Result<(), String> {
        Err("credential expired and this provider has no refresh path".to_string())
    }
}

/// QBO recovery: refresh via the rotating grant, persist it immediately,
/// install the new access token into the live client's token cell.
struct QboAuthRecovery<'a, C: QboHttp> {
    refresher: &'a dyn QboTokenRefresher,
    client: &'a LiveQboReadClient<C>,
    refresh_token: Mutex<String>,
}

impl<C: QboHttp> AuthRecovery for QboAuthRecovery<'_, C> {
    fn recover(&self, state: &AppState, now: u64) -> Result<(), String> {
        let current = self.refresh_token.lock().clone();
        let grant = refresh_and_persist(state, self.refresher, &current, now)
            .map_err(|err| err.to_string())?;
        *self.refresh_token.lock() = grant.refresh_token.clone();
        self.client.set_access_token(&grant.access_token);
        Ok(())
    }
}

/// Minimum gap between manual Sync-now requests (also applies after pump
/// cycles). A const, not an env var — fewer knobs.
pub const ACCOUNTING_SYNC_COOLDOWN_MS: u64 = 120_000;
/// Refresh the access token when it expires within this window.
const ACCESS_TOKEN_SLACK_MS: u64 = 5 * 60 * 1000;
const TOKEN_PERSIST_RETRY_DELAYS_MS: [u64; 3] = [50, 150, 300];
/// Pseudo-entity name for the P&L period cache's backoff/error state.
pub const ENTITY_PNL: &str = "pnl";
pub const ENTITY_BALANCE_SHEET: &str = "balance_sheet";

pub struct AccountingPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<AccountingPumpConfig, StoreError> {
    Ok(AccountingPumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_ACCOUNTING_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_ACCOUNTING_SYNC_INTERVAL_SECS,
                1800,
            )?
            .max(300) as u64,
        ),
        max_requests_per_cycle: max_requests_from_settings(conn, client_id)?,
    })
}

pub fn max_requests_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<u32, StoreError> {
    Ok(crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE,
        8,
    )?
    .clamp(1, 20) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("accounting sync pump not started (accounting disabled by client overlay)");
        return;
    }
    std::thread::Builder::new()
        .name("accounting-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("accounting sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "accounting sync config read failed");
                            AccountingPumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                max_requests_per_cycle: 8,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(&state, config.max_requests_per_cycle);
                    match summary {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            written = summary.written,
                            unchanged = summary.unchanged,
                            rate_limited = summary.rate_limited,
                            "accounting sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "accounting sync cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn accounting-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub written: usize,
    pub unchanged: usize,
    pub rate_limited: bool,
}

/// Claim the sync slot. Err = someone else is syncing or cooldown active.
pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock();
    if status.in_flight {
        return Err("sync_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("sync_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

/// Run one cycle with the LIVE client and release the slot. Caller must hold
/// the slot via [`try_begin_sync`].
pub fn run_guarded_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_requests);
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::Accounting)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + ACCOUNTING_SYNC_COOLDOWN_MS;
    match &result {
        Ok(summary) => {
            status.units_used = summary.requests_used;
            status.last_outcome = Some(if summary.rate_limited {
                "rate_limited".to_string()
            } else {
                "ok".to_string()
            });
        }
        Err(err) => status.last_outcome = Some(format!("error: {err}")),
    }
    result
}

fn run_live_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    // Provider + config resolve per cycle so env changes apply on the next
    // run without a restart.
    match super::service::configured_accounting_provider().as_deref() {
        Ok("qbo") => run_live_qbo_cycle(state, max_requests),
        Ok("invoice_ninja") => run_live_invoice_ninja_cycle(state, max_requests),
        Ok("stripe") => run_live_stripe_cycle(state, max_requests),
        Ok(other) => Err(format!("unknown accounting provider: {other}")),
        Err(err) => Err(err.to_string()),
    }
}

fn run_live_stripe_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let Some(secret_key) = super::service::stripe_config_from_env() else {
        return Err("stripe unconfigured: set BOS_STRIPE_SECRET_KEY".to_string());
    };
    let read_client = bos_integrations::stripe::LiveStripeReadClient::new(
        Arc::new(bos_integrations::stripe::ReqwestStripeHttpClient::default()),
        secret_key,
    );
    // Static secret key: nothing to recover mid-cycle.
    run_sync_cycle(state, &read_client, &NoAuthRecovery, max_requests, now_ms())
}

fn run_live_invoice_ninja_cycle(
    state: &AppState,
    max_requests: u32,
) -> Result<CycleSummary, String> {
    let Some((base_url, api_token)) = super::service::invoice_ninja_config_from_env() else {
        return Err(
            "invoice ninja unconfigured: set BOS_INVOICE_NINJA_BASE_URL and              BOS_INVOICE_NINJA_API_TOKEN"
                .to_string(),
        );
    };
    let read_client = bos_integrations::invoice_ninja::LiveInvoiceNinjaReadClient::new(
        Arc::new(bos_integrations::invoice_ninja::ReqwestInvoiceNinjaHttpClient::default()),
        base_url,
        api_token,
    );
    // Static token: nothing to recover mid-cycle.
    run_sync_cycle(state, &read_client, &NoAuthRecovery, max_requests, now_ms())
}

fn run_live_qbo_cycle(state: &AppState, max_requests: u32) -> Result<CycleSummary, String> {
    let Some(app) = super::service::oauth_app_from_env() else {
        return Err("qbo oauth app unconfigured".to_string());
    };
    let api_base_url = app.environment.api_base_url().to_string();
    let now = now_ms();
    let mut budget = max_requests;
    let refresher = LiveQboTokenRefresher { app };
    let Some((credential, access_token, prep_requests)) =
        prepare_qbo_credentials(state, &refresher, &mut budget, now)?
    else {
        return Ok(CycleSummary::default()); // not connected; quietly wait
    };
    let read_client = LiveQboReadClient::new(
        Arc::new(ReqwestQboHttpClient::default()),
        api_base_url,
        credential.realm_id.clone(),
        access_token,
    );
    let recovery = QboAuthRecovery {
        refresher: &refresher,
        client: &read_client,
        refresh_token: Mutex::new(credential.refresh_token.clone()),
    };
    let mut summary = run_sync_cycle(state, &read_client, &recovery, budget, now)?;
    summary.requests_used += prep_requests;
    Ok(summary)
}

/// Load the QBO credential and ensure a fresh access token, refreshing —
/// and PERSISTING the rotated grant — when it expires within the slack
/// window. Returns None when not connected; the u32 is requests spent.
pub fn prepare_qbo_credentials(
    state: &AppState,
    refresher: &dyn QboTokenRefresher,
    budget: &mut u32,
    now: u64,
) -> Result<Option<(StoredQboCredential, String, u32)>, String> {
    let credential = {
        let persistence = state.persistence.lock();
        store::get_credential(persistence.connection_ref(), &state.client_id)
            .map_err(|err| err.to_string())?
    };
    let Some(mut credential) = credential else {
        return Ok(None);
    };
    if credential.reconnect_required || store::reconnect_latched(&state.client_id) {
        return Ok(None);
    }
    if let Some(token) = credential.access_token.clone() {
        if credential.access_token_expires_at_ms > now + ACCESS_TOKEN_SLACK_MS {
            return Ok(Some((credential, token, 0)));
        }
    }
    if *budget == 0 {
        return Err("request budget exhausted before token refresh".to_string());
    }
    *budget -= 1;
    let grant = refresh_and_persist(state, refresher, &credential.refresh_token, now)
        .map_err(|err| format!("token refresh failed: {err}"))?;
    credential.refresh_token = grant.refresh_token.clone();
    Ok(Some((credential, grant.access_token, 1)))
}

/// The testable cycle core: every external seam is injected. The client
/// carries its own credentials; `auth` recovers (at most once) from a
/// mid-cycle credential expiry.
pub fn run_sync_cycle(
    state: &AppState,
    read_client: &dyn AccountingReadClient,
    auth: &dyn AuthRecovery,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();

    // A 429 deadline is ACCOUNT-wide (providers throttle the company, not
    // the entity): if any cursor carries a future backoff deadline, the
    // whole cycle stands down.
    let backoff_until = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        [
            store::ENTITY_CUSTOMER,
            store::ENTITY_BILL,
            store::ENTITY_INVOICE,
            ENTITY_PNL,
            ENTITY_BALANCE_SHEET,
        ]
        .iter()
        .map(|entity| {
            store::get_cursor(conn, &state.client_id, entity)
                .map(|cursor| cursor.rate_limited_until_ms)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
        .into_iter()
        .max()
        .unwrap_or(0)
    };
    if backoff_until > now {
        return Ok(summary);
    }

    let mut budget = max_requests;
    // At most one mid-cycle expiry recovery: a second expiry after a fresh
    // credential means something is wrong — bail, don't loop.
    let mut recovered_after_expiry = false;

    // Customers first: there are few, and invoice rows render their names.
    // Bills are before invoices so a long invoice backfill cannot starve A/P.
    let mut entities = vec![
        store::ENTITY_CUSTOMER,
        store::ENTITY_BILL,
        store::ENTITY_INVOICE,
    ];
    if !read_client.supports_bills() {
        entities.retain(|entity| *entity != store::ENTITY_BILL);
    }
    for entity in entities {
        loop {
            if budget == 0 {
                return Ok(summary);
            }
            let cursor = {
                let persistence = state.persistence.lock();
                store::get_cursor(persistence.connection_ref(), &state.client_id, entity)
                    .map_err(|err| err.to_string())?
            };
            if cursor.rate_limited_until_ms > now {
                break; // entity is backing off; try the next one
            }
            // A fresh walk starts from the committed high water; an
            // in-progress walk keeps its pinned filter.
            let walk_since = if cursor.next_start_position > 1 {
                cursor.walk_since.clone()
            } else {
                cursor.high_water_updated_at.clone()
            };
            let page_request = PageRequest {
                since_updated_at: walk_since.clone(),
                start_position: cursor.next_start_position,
                page_size: ACCOUNTING_MAX_PAGE_SIZE,
            };
            budget -= 1;
            summary.requests_used += 1;
            let fetched = fetch_entity_page(read_client, entity, &page_request);
            match fetched {
                Ok(page) => {
                    let written = apply_page(state, entity, &page, cursor, walk_since, now)
                        .map_err(|err| err.to_string())?;
                    summary.written += written.written;
                    summary.unchanged += written.unchanged;
                    if written.walk_complete {
                        break; // entity is caught up this cycle
                    }
                }
                Err(AccountingError::RateLimited {
                    retry_after_ms,
                    message,
                }) => {
                    // Stop the WHOLE cycle; stamp the deadline so even the
                    // next cycle skips this entity until it passes.
                    summary.rate_limited = true;
                    let mut stamped = cursor;
                    stamped.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
                    stamped.last_error = Some(message);
                    let mut persistence = state.persistence.lock();
                    store::put_cursor(
                        persistence.connection(),
                        &state.client_id,
                        entity,
                        &stamped,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                    return Ok(summary);
                }
                Err(AccountingError::AuthExpired { message }) => {
                    // Recover once and retry the same page; a second expiry
                    // with a fresh credential ends the cycle.
                    if recovered_after_expiry {
                        record_entity_error(state, entity, &format!("auth: {message}"), now)?;
                        return Err("provider rejected a freshly refreshed credential".to_string());
                    }
                    if budget == 0 {
                        return Ok(summary);
                    }
                    budget -= 1;
                    summary.requests_used += 1;
                    match auth.recover(state, now) {
                        Ok(()) => {
                            recovered_after_expiry = true;
                            continue;
                        }
                        Err(err) => {
                            record_entity_error(state, entity, &format!("auth: {err}"), now)?;
                            return Err(format!("credential recovery failed: {err}"));
                        }
                    }
                }
                Err(err) => {
                    record_entity_error(state, entity, &err.to_string(), now)?;
                    break; // cursor untouched; next cycle resumes here
                }
            }
        }
    }
    if read_client.supports_pnl() {
        sync_pnl_periods(state, read_client, &mut budget, &mut summary, now)?;
        sync_daily_revenue(state, read_client, &mut budget, &mut summary, now)?;
    }
    if read_client.supports_balance_sheet() {
        sync_balance_sheet(state, read_client, &mut budget, &mut summary, now)?;
    }
    Ok(summary)
}

/// Keep the ProfitAndLoss period cache current: complete periods (immutable
/// books) are fetched exactly once; the in-progress month/week re-fetch each
/// cycle. Shares the cycle's request budget and 429 discipline (entity
/// "pnl" carries the backoff deadline).
fn sync_pnl_periods(
    state: &AppState,
    read_client: &dyn AccountingReadClient,
    budget: &mut u32,
    summary: &mut CycleSummary,
    now: u64,
) -> Result<(), String> {
    let today = super::service::today_string(now);
    let needed = super::service::needed_pnl_periods(&today);
    let (cached_complete, cursor) = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        (
            store::complete_pnl_period_starts(conn, &state.client_id)
                .map_err(|err| err.to_string())?,
            store::get_cursor(conn, &state.client_id, ENTITY_PNL).map_err(|err| err.to_string())?,
        )
    };
    if cursor.rate_limited_until_ms > now {
        return Ok(());
    }
    // Current (incomplete) periods first: freshest cards win when the
    // budget runs short; missing historical months backfill behind them.
    let mut queue: Vec<&super::service::PnlPeriod> =
        needed.iter().filter(|period| !period.is_complete).collect();
    queue.extend(needed.iter().filter(|period| {
        period.is_complete
            && !cached_complete.contains(&(period.kind.to_string(), period.start.clone()))
    }));
    let mut all_fetched = true;
    for period in queue {
        if *budget == 0 {
            all_fetched = false;
            break;
        }
        *budget -= 1;
        summary.requests_used += 1;
        match read_client
            .fetch_profit_and_loss(&PnlReportRequest::total(&period.start, &period.end))
        {
            Ok(report) => {
                let snapshot = store::PnlSnapshotRow {
                    period_kind: period.kind.to_string(),
                    period_start: period.start.clone(),
                    period_end: period.end.clone(),
                    total_income_cents: report.summary.total_income_cents,
                    total_cogs_cents: report.summary.total_cogs_cents,
                    gross_profit_cents: report.summary.gross_profit_cents,
                    is_complete: period.is_complete,
                };
                let mut persistence = state.persistence.lock();
                let written = store::upsert_pnl_snapshot(
                    persistence.connection(),
                    &state.client_id,
                    &snapshot,
                    now,
                )
                .map_err(|err| err.to_string())?;
                if written {
                    summary.written += 1;
                } else {
                    summary.unchanged += 1;
                }
            }
            Err(AccountingError::RateLimited {
                retry_after_ms,
                message,
            }) => {
                summary.rate_limited = true;
                let mut stamped = cursor.clone();
                stamped.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
                stamped.last_error = Some(message);
                let mut persistence = state.persistence.lock();
                store::put_cursor(
                    persistence.connection(),
                    &state.client_id,
                    ENTITY_PNL,
                    &stamped,
                    now,
                )
                .map_err(|err| err.to_string())?;
                return Ok(());
            }
            Err(err) => {
                record_entity_error(state, ENTITY_PNL, &err.to_string(), now)?;
                return Ok(());
            }
        }
    }
    // Clear a stale error once everything needed is cached this cycle.
    if all_fetched && (cursor.last_error.is_some() || !cursor.backfill_complete) {
        let mut cleared = cursor;
        cleared.last_error = None;
        cleared.backfill_complete = true;
        let mut persistence = state.persistence.lock();
        store::put_cursor(
            persistence.connection(),
            &state.client_id,
            ENTITY_PNL,
            &cleared,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn sync_daily_revenue(
    state: &AppState,
    read_client: &dyn AccountingReadClient,
    budget: &mut u32,
    summary: &mut CycleSummary,
    now: u64,
) -> Result<(), String> {
    let today = super::service::today_string(now);
    let Some((start, _)) = super::service::prior_month_to_date_window(&today) else {
        return Ok(());
    };
    let end = today.clone();
    let cursor = {
        let persistence = state.persistence.lock();
        store::get_cursor(persistence.connection_ref(), &state.client_id, ENTITY_PNL)
            .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now || *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    summary.requests_used += 1;
    match read_client.fetch_profit_and_loss(&PnlReportRequest::days(&start, &end)) {
        Ok(report) => {
            let mut written_any = false;
            let mut persistence = state.persistence.lock();
            for day in report.daily_income {
                let snapshot = store::PnlSnapshotRow {
                    period_kind: "day".to_string(),
                    period_start: day.date.clone(),
                    period_end: day.date,
                    total_income_cents: day.total_income_cents,
                    total_cogs_cents: 0,
                    gross_profit_cents: day.total_income_cents,
                    is_complete: true,
                };
                let written = store::upsert_pnl_snapshot(
                    persistence.connection(),
                    &state.client_id,
                    &snapshot,
                    now,
                )
                .map_err(|err| err.to_string())?;
                if written {
                    summary.written += 1;
                    written_any = true;
                } else {
                    summary.unchanged += 1;
                }
            }
            if written_any && cursor.last_error.is_some() {
                let mut cleared = cursor;
                cleared.last_error = None;
                store::put_cursor(
                    persistence.connection(),
                    &state.client_id,
                    ENTITY_PNL,
                    &cleared,
                    now,
                )
                .map_err(|err| err.to_string())?;
            }
        }
        Err(AccountingError::RateLimited {
            retry_after_ms,
            message,
        }) => {
            summary.rate_limited = true;
            let mut stamped = cursor;
            stamped.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
            stamped.last_error = Some(message);
            let mut persistence = state.persistence.lock();
            store::put_cursor(
                persistence.connection(),
                &state.client_id,
                ENTITY_PNL,
                &stamped,
                now,
            )
            .map_err(|err| err.to_string())?;
        }
        Err(err) => {
            record_entity_error(state, ENTITY_PNL, &err.to_string(), now)?;
        }
    }
    Ok(())
}

fn sync_balance_sheet(
    state: &AppState,
    read_client: &dyn AccountingReadClient,
    budget: &mut u32,
    summary: &mut CycleSummary,
    now: u64,
) -> Result<(), String> {
    let today = super::service::today_string(now);
    let cursor = {
        let persistence = state.persistence.lock();
        store::get_cursor(
            persistence.connection_ref(),
            &state.client_id,
            ENTITY_BALANCE_SHEET,
        )
        .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now || *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    summary.requests_used += 1;
    match read_client.fetch_balance_sheet(&today) {
        Ok(report) => {
            let mut persistence = state.persistence.lock();
            let written = store::upsert_balance_sheet_snapshot(
                persistence.connection(),
                &state.client_id,
                &today,
                report,
                now,
            )
            .map_err(|err| err.to_string())?;
            if written {
                summary.written += 1;
            } else {
                summary.unchanged += 1;
            }
            if cursor.last_error.is_some() || !cursor.backfill_complete {
                let mut cleared = cursor;
                cleared.last_error = None;
                cleared.backfill_complete = true;
                store::put_cursor(
                    persistence.connection(),
                    &state.client_id,
                    ENTITY_BALANCE_SHEET,
                    &cleared,
                    now,
                )
                .map_err(|err| err.to_string())?;
            }
        }
        Err(AccountingError::RateLimited {
            retry_after_ms,
            message,
        }) => {
            summary.rate_limited = true;
            let mut stamped = cursor;
            stamped.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
            stamped.last_error = Some(message);
            let mut persistence = state.persistence.lock();
            store::put_cursor(
                persistence.connection(),
                &state.client_id,
                ENTITY_BALANCE_SHEET,
                &stamped,
                now,
            )
            .map_err(|err| err.to_string())?;
        }
        Err(err) => {
            record_entity_error(state, ENTITY_BALANCE_SHEET, &err.to_string(), now)?;
        }
    }
    Ok(())
}

fn fetch_entity_page(
    client: &dyn AccountingReadClient,
    entity: &str,
    page: &PageRequest,
) -> Result<PageRecords, AccountingError> {
    if entity == store::ENTITY_CUSTOMER {
        client.fetch_customers(page).map(PageRecords::Customers)
    } else if entity == store::ENTITY_BILL {
        client.fetch_bills(page).map(PageRecords::Bills)
    } else {
        client.fetch_invoices(page).map(PageRecords::Invoices)
    }
}

enum PageRecords {
    Invoices(Page<InvoiceRecord>),
    Bills(Page<BillRecord>),
    Customers(Page<CustomerRecord>),
}

struct AppliedPage {
    written: usize,
    unchanged: usize,
    walk_complete: bool,
}

/// Persist one fetched page + the advanced cursor under a single lock hold.
fn apply_page(
    state: &AppState,
    entity: &str,
    page: &PageRecords,
    mut cursor: QboSyncCursor,
    walk_since: Option<String>,
    now: u64,
) -> Result<AppliedPage, crate::store_core::StoreError> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let (summary, record_count, page_size, max_updated) = match page {
        PageRecords::Invoices(page) => (
            store::upsert_invoice_snapshots(conn, &state.client_id, &page.records, now)?,
            page.records.len(),
            page.requested_page_size as usize,
            page.records
                .iter()
                .map(|record| record.updated_at.clone())
                .max(),
        ),
        PageRecords::Bills(page) => (
            store::upsert_bill_snapshots(conn, &state.client_id, &page.records, now)?,
            page.records.len(),
            page.requested_page_size as usize,
            page.records
                .iter()
                .map(|record| record.updated_at.clone())
                .max(),
        ),
        PageRecords::Customers(page) => (
            store::upsert_customer_snapshots(conn, &state.client_id, &page.records, now)?,
            page.records.len(),
            page.requested_page_size as usize,
            page.records
                .iter()
                .filter_map(|record| record.updated_at.clone())
                .max(),
        ),
    };
    cursor.last_error = None;
    cursor.rate_limited_until_ms = 0;
    cursor.walk_max_updated_at = match (cursor.walk_max_updated_at.take(), max_updated) {
        (Some(current), Some(seen)) => Some(if seen > current { seen } else { current }),
        (current, seen) => seen.or(current),
    };
    let walk_complete = record_count < page_size;
    if walk_complete {
        // Promote the walk: everything updated up to walk_max is now cached.
        if let Some(walk_max) = cursor.walk_max_updated_at.take() {
            cursor.high_water_updated_at = Some(walk_max);
        }
        cursor.walk_since = None;
        cursor.next_start_position = 1;
        cursor.backfill_complete = true;
    } else {
        cursor.walk_since = walk_since;
        cursor.next_start_position += page_size as u32;
    }
    store::put_cursor(conn, &state.client_id, entity, &cursor, now)?;
    Ok(AppliedPage {
        written: summary.written,
        unchanged: summary.unchanged,
        walk_complete,
    })
}

fn refresh_and_persist(
    state: &AppState,
    refresher: &dyn QboTokenRefresher,
    refresh_token: &str,
    now: u64,
) -> Result<QboTokenGrant, AccountingError> {
    let grant = match refresher.refresh(refresh_token, now) {
        Ok(grant) => grant,
        Err(error) => {
            if matches!(
                &error,
                AccountingError::Permanent { code, .. } if code == "qbo_token_rejected"
            ) {
                // Stop further refresh attempts even if the durable flag
                // cannot be written on this cycle.
                store::latch_reconnect_required(&state.client_id);
                if let Err(store_error) = persist_with_lock_retry(|| {
                    let mut persistence = state.persistence.lock();
                    store::mark_reconnect_required(
                        persistence.connection(),
                        &state.client_id,
                        "qbo_token_rejected",
                        now,
                    )
                }) {
                    tracing::warn!(
                        error = %store_error,
                        "failed to save QuickBooks reconnect requirement"
                    );
                }
            }
            return Err(error);
        }
    };
    persist_with_lock_retry(|| {
        let mut persistence = state.persistence.lock();
        store::update_tokens_after_refresh(persistence.connection(), &state.client_id, &grant, now)
    })
    .map_err(|err| AccountingError::Permanent {
        code: "qbo_token_persist_failed".to_string(),
        message: err.to_string(),
    })?;
    Ok(grant)
}

/// Retries only SQLite busy/locked errors. Sleeps on the calling thread;
/// `refresh_and_persist` runs on the accounting sync worker, never on an
/// HTTP request task.
pub(super) fn persist_with_lock_retry<T>(
    mut operation: impl FnMut() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    for delay_ms in TOKEN_PERSIST_RETRY_DELAYS_MS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if error.is_sqlite_busy() => {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(error) => return Err(error),
        }
    }
    operation()
}

fn record_entity_error(
    state: &AppState,
    entity: &str,
    error: &str,
    now: u64,
) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let mut cursor =
        store::get_cursor(conn, &state.client_id, entity).map_err(|err| err.to_string())?;
    cursor.last_error = Some(error.chars().take(300).collect());
    store::put_cursor(conn, &state.client_id, entity, &cursor, now)
        .map_err(|err| err.to_string())?;
    Ok(())
}
