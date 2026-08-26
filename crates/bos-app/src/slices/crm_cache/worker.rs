//! CRM cache sync pump. Off unless BOS_CRM_READ_SYNC_ENABLED; manual sync
//! uses the same guarded cycle.

use std::time::Duration;

use bos_integrations::crm_read::{
    CrmDealRecord, CrmPageRequest, CrmReadClient, CrmReadError, CRM_MAX_PAGE_SIZE,
};
use bos_integrations::espocrm::{espocrm_records_search_client, EspoCrmWriteConfig};

use super::store::{self, CrmSyncCursor};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

pub const CRM_CACHE_SYNC_COOLDOWN_MS: u64 = 120_000;

pub struct CrmCachePumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<CrmCachePumpConfig, StoreError> {
    Ok(CrmCachePumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_CRM_READ_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_CRM_READ_SYNC_INTERVAL_SECS,
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
        &env_registry::BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE,
        8,
    )?
    .clamp(1, 20) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!("CRM cache sync not started because the slice is disabled");
        return;
    }
    std::thread::Builder::new()
        .name("crm-cache-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("CRM cache sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "CRM cache sync config read failed");
                            CrmCachePumpConfig {
                                enabled: false,
                                interval: Duration::from_secs(1800),
                                max_requests_per_cycle: 8,
                            }
                        }
                    }
                };
                if config.enabled && try_begin_sync(&state, now_ms()).is_ok() {
                    let summary = run_guarded_cycle(
                        &state,
                        config.max_requests_per_cycle,
                        false,
                        config.interval,
                    );
                    match summary {
                        Ok(summary) if summary.requests_used > 0 => tracing::info!(
                            requests_used = summary.requests_used,
                            written = summary.written,
                            unchanged = summary.unchanged,
                            tombstoned = summary.tombstoned,
                            rate_limited = summary.rate_limited,
                            "CRM cache sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "CRM cache sync failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn crm-cache-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub written: usize,
    pub unchanged: usize,
    pub tombstoned: usize,
    pub rate_limited: bool,
}

pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state.sync_guards.guard(crate::http::Pump::CrmCache).lock();
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

pub fn run_guarded_cycle(
    state: &AppState,
    max_requests: u32,
    force_refresh: bool,
    refresh_interval: Duration,
) -> Result<CycleSummary, String> {
    let result = run_live_cycle(state, max_requests, force_refresh, refresh_interval);
    let mut status = state.sync_guards.guard(crate::http::Pump::CrmCache).lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + CRM_CACHE_SYNC_COOLDOWN_MS;
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

fn run_live_cycle(
    state: &AppState,
    max_requests: u32,
    force_refresh: bool,
    refresh_interval: Duration,
) -> Result<CycleSummary, String> {
    match super::service::configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_HUBSPOT) => {
            let access_token = env_registry::string(&env_registry::BOS_HUBSPOT_ACCESS_TOKEN);
            let client = bos_integrations::hubspot::hubspot_deal_discovery_client(access_token)?;
            run_sync_cycle(
                state,
                &client,
                true,
                max_requests,
                now_ms(),
                force_refresh,
                refresh_interval,
            )
        }
        Ok(crate::slices::crm_drafts::service::PROVIDER_ESPOCRM) => {
            let config = EspoCrmWriteConfig {
                base_url: env_registry::string(&env_registry::BOS_ESPOCRM_BASE_URL),
                api_key: env_registry::string(&env_registry::BOS_ESPOCRM_API_KEY),
                write_enabled: false,
            };
            let Some(client) = espocrm_records_search_client(&config) else {
                return Ok(CycleSummary::default());
            };
            run_sync_cycle(
                state,
                &client,
                false,
                max_requests,
                now_ms(),
                force_refresh,
                refresh_interval,
            )
        }
        Ok(other) => Err(format!("unknown CRM provider: {other}")),
        Err(err) => Err(err),
    }
}

pub fn run_sync_cycle(
    state: &AppState,
    read_client: &dyn CrmReadClient,
    sync_deals: bool,
    max_requests: u32,
    now: u64,
    force_refresh: bool,
    refresh_interval: Duration,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let backoff_until = {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        [store::ENTITY_CONTACT, store::ENTITY_DEAL]
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
    refresh_completed_cursors(state, sync_deals, now, force_refresh, refresh_interval)?;
    let mut budget = max_requests;
    sync_contacts(state, read_client, &mut budget, &mut summary, now)?;
    if sync_deals {
        sync_deals_page(state, read_client, &mut budget, &mut summary, now)?;
    } else {
        mark_deals_skipped(state, now)?;
    }
    Ok(summary)
}

fn refresh_completed_cursors(
    state: &AppState,
    sync_deals: bool,
    now: u64,
    force_refresh: bool,
    refresh_interval: Duration,
) -> Result<(), String> {
    let entities = if sync_deals {
        &[store::ENTITY_CONTACT, store::ENTITY_DEAL][..]
    } else {
        &[store::ENTITY_CONTACT][..]
    };
    let refresh_after_ms = refresh_interval.as_millis().min(u64::MAX as u128) as u64;
    let mut persistence = state.persistence.lock();
    for entity in entities {
        let mut cursor = store::get_cursor(persistence.connection_ref(), &state.client_id, entity)
            .map_err(|err| err.to_string())?;
        if !cursor.backfill_complete {
            continue;
        }
        let stale = cursor
            .last_advanced_at_ms
            .is_none_or(|advanced_at| now.saturating_sub(advanced_at) >= refresh_after_ms);
        if !force_refresh && !stale {
            continue;
        }
        cursor.next_after_cursor = None;
        cursor.backfill_complete = false;
        cursor.sync_started_at_ms = Some(now);
        cursor.last_error = None;
        store::put_cursor(
            persistence.connection(),
            &state.client_id,
            entity,
            &cursor,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn sync_contacts(
    state: &AppState,
    read_client: &dyn CrmReadClient,
    budget: &mut u32,
    summary: &mut CycleSummary,
    now: u64,
) -> Result<(), String> {
    loop {
        if *budget == 0 {
            return Ok(());
        }
        let cursor = ensure_backfill_started(state, store::ENTITY_CONTACT, now)?;
        if cursor.rate_limited_until_ms > now || cursor.backfill_complete {
            return Ok(());
        }
        let sync_started_at_ms = cursor.sync_started_at_ms.unwrap_or(now);
        *budget -= 1;
        summary.requests_used += 1;
        let request = CrmPageRequest {
            cursor: cursor.next_after_cursor.clone(),
            page_size: CRM_MAX_PAGE_SIZE,
        };
        match read_client.list_contacts_page(&request) {
            Ok(page) => {
                let mut persistence = state.persistence.lock();
                let upserted = store::upsert_contact_snapshots(
                    persistence.connection(),
                    &state.client_id,
                    &page.records,
                    now,
                )
                .map_err(|err| err.to_string())?;
                summary.written += upserted.written;
                summary.unchanged += upserted.unchanged;
                summary.tombstoned += upserted.tombstoned;
                let mut advanced = cursor;
                advanced.next_after_cursor = page.next_cursor;
                advanced.backfill_complete = advanced.next_after_cursor.is_none();
                advanced.last_error = None;
                if advanced.backfill_complete {
                    summary.tombstoned += store::tombstone_stale_contact_snapshots(
                        persistence.connection(),
                        &state.client_id,
                        sync_started_at_ms,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                    advanced.sync_started_at_ms = None;
                }
                store::put_cursor(
                    persistence.connection(),
                    &state.client_id,
                    store::ENTITY_CONTACT,
                    &advanced,
                    now,
                )
                .map_err(|err| err.to_string())?;
                if advanced.backfill_complete {
                    return Ok(());
                }
            }
            Err(err) => {
                return handle_fetch_error(state, store::ENTITY_CONTACT, cursor, err, now, summary)
            }
        }
    }
}

fn sync_deals_page(
    state: &AppState,
    read_client: &dyn CrmReadClient,
    budget: &mut u32,
    summary: &mut CycleSummary,
    now: u64,
) -> Result<(), String> {
    loop {
        if *budget == 0 {
            return Ok(());
        }
        let cursor = ensure_backfill_started(state, store::ENTITY_DEAL, now)?;
        if cursor.rate_limited_until_ms > now || cursor.backfill_complete {
            return Ok(());
        }
        let sync_started_at_ms = cursor.sync_started_at_ms.unwrap_or(now);
        *budget -= 1;
        summary.requests_used += 1;
        let request = CrmPageRequest {
            cursor: cursor.next_after_cursor.clone(),
            page_size: CRM_MAX_PAGE_SIZE,
        };
        match read_client.list_deals_page(&request) {
            Ok(mut page) => {
                hydrate_deal_contacts(state, &mut page.records)?;
                let mut persistence = state.persistence.lock();
                let upserted = store::upsert_deal_snapshots(
                    persistence.connection(),
                    &state.client_id,
                    &page.records,
                    now,
                )
                .map_err(|err| err.to_string())?;
                summary.written += upserted.written;
                summary.unchanged += upserted.unchanged;
                summary.tombstoned += upserted.tombstoned;
                let mut advanced = cursor;
                advanced.next_after_cursor = page.next_cursor;
                advanced.backfill_complete = advanced.next_after_cursor.is_none();
                advanced.last_error = None;
                if advanced.backfill_complete {
                    summary.tombstoned += store::tombstone_stale_deal_snapshots(
                        persistence.connection(),
                        &state.client_id,
                        sync_started_at_ms,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                    advanced.sync_started_at_ms = None;
                }
                store::put_cursor(
                    persistence.connection(),
                    &state.client_id,
                    store::ENTITY_DEAL,
                    &advanced,
                    now,
                )
                .map_err(|err| err.to_string())?;
                if advanced.backfill_complete {
                    return Ok(());
                }
            }
            Err(err) => {
                return handle_fetch_error(state, store::ENTITY_DEAL, cursor, err, now, summary)
            }
        }
    }
}

fn hydrate_deal_contacts(state: &AppState, deals: &mut [CrmDealRecord]) -> Result<(), String> {
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    for deal in deals {
        if deal.associated_contact_email.is_some() {
            continue;
        }
        let Some(contact_id) = deal.associated_contact_ids.first() else {
            continue;
        };
        if let Some(contact) = store::contact_by_provider_id(conn, &state.client_id, contact_id)
            .map_err(|err| err.to_string())?
        {
            deal.associated_contact_email = contact.email;
            if deal.associated_contact_company.is_none() {
                deal.associated_contact_company = contact.company;
            }
        }
    }
    Ok(())
}

fn mark_deals_skipped(state: &AppState, now: u64) -> Result<(), String> {
    let mut cursor = current_cursor(state, store::ENTITY_DEAL)?;
    cursor.backfill_complete = true;
    cursor.sync_started_at_ms = None;
    cursor.last_error = None;
    let mut persistence = state.persistence.lock();
    store::put_cursor(
        persistence.connection(),
        &state.client_id,
        store::ENTITY_DEAL,
        &cursor,
        now,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn current_cursor(state: &AppState, entity: &str) -> Result<CrmSyncCursor, String> {
    let persistence = state.persistence.lock();
    store::get_cursor(persistence.connection_ref(), &state.client_id, entity)
        .map_err(|err| err.to_string())
}

fn ensure_backfill_started(
    state: &AppState,
    entity: &str,
    now: u64,
) -> Result<CrmSyncCursor, String> {
    let mut cursor = current_cursor(state, entity)?;
    if !cursor.backfill_complete && cursor.sync_started_at_ms.is_none() {
        cursor.sync_started_at_ms = Some(now);
        let mut persistence = state.persistence.lock();
        store::put_cursor(
            persistence.connection(),
            &state.client_id,
            entity,
            &cursor,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(cursor)
}

fn handle_fetch_error(
    state: &AppState,
    entity: &str,
    mut cursor: CrmSyncCursor,
    err: CrmReadError,
    now: u64,
    summary: &mut CycleSummary,
) -> Result<(), String> {
    match err {
        CrmReadError::RateLimited {
            retry_after_ms,
            message,
        } => {
            summary.rate_limited = true;
            cursor.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
            cursor.last_error = Some(message);
        }
        CrmReadError::Retryable { code, message } | CrmReadError::Permanent { code, message } => {
            cursor.last_error = Some(format!("{code}: {message}"));
        }
    }
    let mut persistence = state.persistence.lock();
    store::put_cursor(
        persistence.connection(),
        &state.client_id,
        entity,
        &cursor,
        now,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}
