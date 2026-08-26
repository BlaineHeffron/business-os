//! Drive corpus sync pump: bounded, incremental, rate-limit-respecting. Off
//! unless BOS_DRIVE_SYNC_ENABLED; the manual Sync-now route runs the same
//! cycle core through the same serialization guard, so there is NEVER more
//! than one Drive request in flight from this process.
//!
//! Cycle shape (every HTTP call costs one unit of the request budget):
//! 1. token refresh (per-user Google credential, drive.readonly required),
//! 2. changes-API startPageToken pinned BEFORE the backfill walk so edits
//!    made during backfill are never missed,
//! 3. backfill: one files.list page per configured folder per request, then
//!    explicit include files — every allowed file lands as a `stale`
//!    snapshot (metadata only, cheap),
//! 4. incremental: changes pages from the cursor; removed/out-of-corpus
//!    files drop their chunks, changed files go `stale`,
//! 5. indexing: stale docs are read (one request each), content-hash
//!    skipped when unchanged, otherwise deterministically chunked and
//!    written to the FTS index in one transaction.
//!
//! 429 honors Retry-After, stamps the cursor deadline, and stops the WHOLE
//! cycle; the walk cursors only advance after their page's rows commit, so
//! failures resume exactly where they stopped — no re-spending.

use std::time::Duration;

use bos_integrations::google_drive_read::{
    document_allowed_for_corpus, DriveError, DriveFileMeta, DriveReadClient,
    GoogleDriveCorpusPointer, LiveDriveReadClient, ReqwestDriveHttpClient,
    GOOGLE_DRIVE_READONLY_SCOPE,
};
use bos_integrations::google_oauth;

use super::service;
use super::store::{self, DriveSyncCursor};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

/// Minimum gap between manual Sync-now requests (also applies after pump
/// cycles). Reference docs change slowly; QBO's cooldown fits.
pub const DRIVE_SYNC_COOLDOWN_MS: u64 = 120_000;

pub struct DrivePumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub max_requests_per_cycle: u32,
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<DrivePumpConfig, StoreError> {
    Ok(DrivePumpConfig {
        enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_DRIVE_SYNC_ENABLED,
        )?,
        interval: Duration::from_secs(
            crate::slices::admin_settings::service::usize_or(
                conn,
                client_id,
                &env_registry::BOS_DRIVE_SYNC_INTERVAL_SECS,
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
        &env_registry::BOS_DRIVE_MAX_REQUESTS_PER_CYCLE,
        8,
    )?
    .clamp(2, 30) as u32)
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!(
            "drive corpus sync pump not started (drive_corpus disabled by client overlay)"
        );
        return;
    }
    std::thread::Builder::new()
        .name("drive-sync-pump".to_string())
        .spawn(move || {
            tracing::info!("drive corpus sync pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(error = %err, "drive corpus sync config read failed");
                            DrivePumpConfig {
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
                            indexed = summary.indexed,
                            unchanged = summary.unchanged,
                            marked_stale = summary.marked_stale,
                            removed = summary.removed,
                            rate_limited = summary.rate_limited,
                            "drive corpus sync cycle complete"
                        ),
                        Ok(_) => {}
                        Err(err) => tracing::warn!(error = %err, "drive corpus sync cycle failed"),
                    }
                }
                std::thread::sleep(config.interval);
            }
        })
        .expect("spawn drive-sync-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub requests_used: u32,
    pub indexed: usize,
    pub unchanged: usize,
    pub marked_stale: usize,
    pub removed: usize,
    pub rate_limited: bool,
}

/// Claim the sync slot. Err = someone else is syncing or cooldown active.
pub fn try_begin_sync(state: &AppState, now: u64) -> Result<(), &'static str> {
    try_begin_sync_inner(state, now, true)
}

/// Claim the sync slot after a settings change. Folder changes should refresh
/// promptly, but still must not overlap an active sync.
pub fn try_begin_sync_ignoring_cooldown(state: &AppState, now: u64) -> Result<(), &'static str> {
    try_begin_sync_inner(state, now, false)
}

fn try_begin_sync_inner(
    state: &AppState,
    now: u64,
    enforce_cooldown: bool,
) -> Result<(), &'static str> {
    let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
    if status.in_flight {
        return Err("sync_in_flight");
    }
    if enforce_cooldown && now < status.next_allowed_at_ms {
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
    let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + DRIVE_SYNC_COOLDOWN_MS;
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
    // Config resolves per cycle so env changes apply without a restart (the
    // overlay part is startup-pinned, like enabled_slices).
    let resolved = {
        let persistence = state.persistence.lock();
        service::corpus_pointer_for_state(state, persistence.connection_ref())
            .map_err(|err| err.to_string())?
    };
    let pointer = resolved.pointer;
    if !pointer.is_configured() {
        // Not configured yet — wait quietly; the status surface says so.
        return Ok(CycleSummary::default());
    }
    let oauth = {
        let persistence = state.persistence.lock();
        crate::slices::google_connector::service::resolve_google_oauth(
            persistence.connection_ref(),
            &state.client_id,
            resolved.credential_user_id.as_deref(),
        )
        .map_err(|err| err.to_string())?
    };
    let Some(oauth) = oauth else {
        // No Google credential connected — wait quietly.
        return Ok(CycleSummary::default());
    };
    if !oauth.scopes.is_empty() && !google_oauth::has_scope(&oauth, GOOGLE_DRIVE_READONLY_SCOPE) {
        // Known scope list without drive.readonly: the operator must
        // reconnect Google (re-consent). Surface on the cursor and wait.
        record_cursor_error(state, "drive_scope_missing_reconnect_google", now_ms())?;
        return Ok(CycleSummary::default());
    }
    let mut summary = CycleSummary::default();
    summary.requests_used += 1; // token refresh spends one unit
    let access_token = google_oauth::fetch_access_token(&oauth).map_err(|err| err.to_string())?;
    let client = LiveDriveReadClient::new(ReqwestDriveHttpClient::default());
    let budget = max_requests.saturating_sub(summary.requests_used).max(1);
    let cycle = run_sync_cycle(state, &client, &access_token, &pointer, budget, now_ms())?;
    summary.requests_used += cycle.requests_used;
    summary.indexed = cycle.indexed;
    summary.unchanged = cycle.unchanged;
    summary.marked_stale = cycle.marked_stale;
    summary.removed = cycle.removed;
    summary.rate_limited = cycle.rate_limited;
    Ok(summary)
}

/// The testable cycle core: every external seam is injected.
pub fn run_sync_cycle(
    state: &AppState,
    client: &dyn DriveReadClient,
    access_token: &str,
    pointer: &GoogleDriveCorpusPointer,
    max_requests: u32,
    now: u64,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let mut cursor = {
        let persistence = state.persistence.lock();
        store::get_cursor(persistence.connection_ref(), &state.client_id)
            .map_err(|err| err.to_string())?
    };
    if cursor.rate_limited_until_ms > now {
        return Ok(summary);
    }
    let mut budget = max_requests;

    // Config change: locally re-evaluate stored docs against the new rules
    // (zero API cost) and restart the backfill walk for newly added folders.
    let config_hash = service::corpus_config_hash(pointer);
    if cursor.config_hash != config_hash {
        let snapshots = {
            let persistence = state.persistence.lock();
            store::list_snapshots(persistence.connection_ref(), &state.client_id)
                .map_err(|err| err.to_string())?
        };
        for snapshot in snapshots {
            if snapshot.status == store::STATUS_REMOVED {
                continue;
            }
            if !document_allowed_for_corpus(&snapshot.meta, pointer) {
                let mut persistence = state.persistence.lock();
                if store::mark_removed(
                    persistence.connection(),
                    &state.client_id,
                    &snapshot.meta.file_id,
                    now,
                )
                .map_err(|err| err.to_string())?
                {
                    summary.removed += 1;
                }
            }
        }
        cursor.config_hash = config_hash;
        cursor.backfill_complete = false;
        cursor.backfill_folder_index = 0;
        cursor.backfill_page_token = None;
        cursor.last_error = None;
        put_cursor(state, &cursor, now)?;
    }

    // Pin the changes stream BEFORE backfill so concurrent edits replay.
    if cursor.start_page_token.is_none() {
        if budget == 0 {
            return Ok(summary);
        }
        budget -= 1;
        summary.requests_used += 1;
        match client.fetch_start_page_token(access_token) {
            Ok(token) => {
                cursor.start_page_token = Some(token);
                cursor.last_error = None;
                put_cursor(state, &cursor, now)?;
            }
            Err(err) => return handle_drive_error(state, &mut summary, cursor, err, now),
        }
    }

    // Backfill walk: folders first (one listing page per request), then the
    // explicit include files (one metadata fetch each).
    if !cursor.backfill_complete {
        let folder_count = pointer.folder_ids.len() as u32;
        let target_count = folder_count + pointer.include_file_ids.len() as u32;
        while cursor.backfill_folder_index < target_count {
            if budget == 0 {
                return Ok(summary);
            }
            budget -= 1;
            summary.requests_used += 1;
            if cursor.backfill_folder_index < folder_count {
                let folder = &pointer.folder_ids[cursor.backfill_folder_index as usize];
                match client.list_folder_files(
                    access_token,
                    folder,
                    cursor.backfill_page_token.as_deref(),
                ) {
                    Ok(page) => {
                        summary.marked_stale +=
                            mark_allowed_stale(state, &page.files, pointer, now)?;
                        match page.next_page_token {
                            Some(token) => cursor.backfill_page_token = Some(token),
                            None => {
                                cursor.backfill_page_token = None;
                                cursor.backfill_folder_index += 1;
                            }
                        }
                        cursor.last_error = None;
                        put_cursor(state, &cursor, now)?;
                    }
                    Err(err) => return handle_drive_error(state, &mut summary, cursor, err, now),
                }
            } else {
                let file_id = &pointer.include_file_ids
                    [(cursor.backfill_folder_index - folder_count) as usize];
                match client.fetch_file(access_token, file_id) {
                    Ok(Some(meta)) => {
                        summary.marked_stale +=
                            mark_allowed_stale(state, std::slice::from_ref(&meta), pointer, now)?;
                        cursor.backfill_folder_index += 1;
                        cursor.last_error = None;
                        put_cursor(state, &cursor, now)?;
                    }
                    Ok(None) => {
                        // Included file vanished — tolerate and move on.
                        cursor.backfill_folder_index += 1;
                        put_cursor(state, &cursor, now)?;
                    }
                    Err(err) => return handle_drive_error(state, &mut summary, cursor, err, now),
                }
            }
        }
        cursor.backfill_complete = true;
        cursor.backfill_page_token = None;
        put_cursor(state, &cursor, now)?;
    }

    // Incremental: changes pages from the pinned token.
    while let Some(page_token) = cursor
        .pending_page_token
        .clone()
        .or_else(|| cursor.start_page_token.clone())
    {
        if budget == 0 {
            return Ok(summary);
        }
        budget -= 1;
        summary.requests_used += 1;
        match client.fetch_changes(access_token, &page_token) {
            Ok(page) => {
                for change in &page.changes {
                    if change.removed {
                        let mut persistence = state.persistence.lock();
                        if store::mark_removed(
                            persistence.connection(),
                            &state.client_id,
                            &change.file_id,
                            now,
                        )
                        .map_err(|err| err.to_string())?
                        {
                            summary.removed += 1;
                        }
                        continue;
                    }
                    let Some(meta) = &change.file else { continue };
                    if document_allowed_for_corpus(meta, pointer) {
                        summary.marked_stale +=
                            mark_allowed_stale(state, std::slice::from_ref(meta), pointer, now)?;
                    } else {
                        // Trashed, moved out of the corpus, renamed into an
                        // exclusion, … — drop it if we hold it.
                        let mut persistence = state.persistence.lock();
                        if store::mark_removed(
                            persistence.connection(),
                            &state.client_id,
                            &meta.file_id,
                            now,
                        )
                        .map_err(|err| err.to_string())?
                        {
                            summary.removed += 1;
                        }
                    }
                }
                match (page.next_page_token, page.new_start_page_token) {
                    (Some(next), _) => {
                        cursor.pending_page_token = Some(next);
                        cursor.last_error = None;
                        put_cursor(state, &cursor, now)?;
                    }
                    (None, new_start) => {
                        if let Some(new_start) = new_start {
                            cursor.start_page_token = Some(new_start);
                        }
                        cursor.pending_page_token = None;
                        cursor.last_error = None;
                        put_cursor(state, &cursor, now)?;
                        break;
                    }
                }
            }
            Err(err) => return handle_drive_error(state, &mut summary, cursor, err, now),
        }
    }

    // Indexing: read + chunk + write stale docs with whatever budget remains.
    while budget > 0 {
        let stale = {
            let persistence = state.persistence.lock();
            store::stale_snapshots(
                persistence.connection_ref(),
                &state.client_id,
                budget as usize,
            )
            .map_err(|err| err.to_string())?
        };
        if stale.is_empty() {
            break;
        }
        for snapshot in stale {
            if budget == 0 {
                return Ok(summary);
            }
            budget -= 1;
            summary.requests_used += 1;
            match client.read_text(access_token, &snapshot.meta) {
                Ok(None) => {
                    let mut persistence = state.persistence.lock();
                    store::mark_skipped(
                        persistence.connection(),
                        &state.client_id,
                        &snapshot.meta.file_id,
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                }
                Ok(Some(text)) => {
                    let hash = service::content_hash(&text);
                    let mut persistence = state.persistence.lock();
                    if hash == snapshot.content_hash {
                        store::touch_indexed(
                            persistence.connection(),
                            &state.client_id,
                            &snapshot.meta.file_id,
                            now,
                        )
                        .map_err(|err| err.to_string())?;
                        summary.unchanged += 1;
                    } else {
                        let chunks = service::chunk_document(&text);
                        store::index_document(
                            persistence.connection(),
                            &state.client_id,
                            &snapshot.meta.file_id,
                            &snapshot.meta.name,
                            &hash,
                            &chunks,
                            now,
                        )
                        .map_err(|err| err.to_string())?;
                        summary.indexed += 1;
                    }
                }
                Err(DriveError::RateLimited {
                    retry_after_ms,
                    message,
                }) => {
                    return handle_drive_error(
                        state,
                        &mut summary,
                        cursor,
                        DriveError::RateLimited {
                            retry_after_ms,
                            message,
                        },
                        now,
                    )
                }
                Err(DriveError::AuthRejected { message }) => {
                    return handle_drive_error(
                        state,
                        &mut summary,
                        cursor,
                        DriveError::AuthRejected { message },
                        now,
                    )
                }
                Err(err) => {
                    // Per-document failure (export rejected, too large, …):
                    // record on the doc and keep going.
                    let mut persistence = state.persistence.lock();
                    store::mark_error(
                        persistence.connection(),
                        &state.client_id,
                        &snapshot.meta.file_id,
                        &err.to_string(),
                        now,
                    )
                    .map_err(|err| err.to_string())?;
                }
            }
        }
    }
    Ok(summary)
}

/// Stamp Drive metadata for every corpus-allowed file; returns how many rows
/// actually changed (unchanged revisions write nothing).
fn mark_allowed_stale(
    state: &AppState,
    files: &[DriveFileMeta],
    pointer: &GoogleDriveCorpusPointer,
    now: u64,
) -> Result<usize, String> {
    let mut marked = 0usize;
    for meta in files {
        if !document_allowed_for_corpus(meta, pointer) {
            continue;
        }
        let mut persistence = state.persistence.lock();
        if store::mark_stale_from_meta(persistence.connection(), &state.client_id, meta, now)
            .map_err(|err| err.to_string())?
        {
            marked += 1;
        }
    }
    Ok(marked)
}

/// Cycle-stopping Drive errors: 429 stamps the cursor's backoff deadline and
/// returns the partial summary; auth rejection and permanent listing errors
/// land on the cursor and end the cycle (next cycle resumes from the same
/// committed walk position).
fn handle_drive_error(
    state: &AppState,
    summary: &mut CycleSummary,
    mut cursor: DriveSyncCursor,
    err: DriveError,
    now: u64,
) -> Result<CycleSummary, String> {
    match err {
        DriveError::RateLimited {
            retry_after_ms,
            message,
        } => {
            summary.rate_limited = true;
            cursor.rate_limited_until_ms = now + retry_after_ms.unwrap_or(60_000);
            cursor.last_error = Some(message);
            put_cursor(state, &cursor, now)?;
            Ok(*summary)
        }
        DriveError::AuthRejected { message } => {
            cursor.last_error = Some(format!("auth: {message}"));
            put_cursor(state, &cursor, now)?;
            Err(format!("drive credential rejected: {message}"))
        }
        DriveError::Permanent { code, message } => {
            cursor.last_error = Some(format!("{code}: {message}").chars().take(300).collect());
            put_cursor(state, &cursor, now)?;
            Err(format!("{code}: {message}"))
        }
    }
}

fn put_cursor(state: &AppState, cursor: &DriveSyncCursor, now: u64) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    store::put_cursor(persistence.connection(), &state.client_id, cursor, now)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn record_cursor_error(state: &AppState, error: &str, now: u64) -> Result<(), String> {
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let mut cursor = store::get_cursor(conn, &state.client_id).map_err(|err| err.to_string())?;
    cursor.last_error = Some(error.to_string());
    store::put_cursor(conn, &state.client_id, &cursor, now).map_err(|err| err.to_string())?;
    Ok(())
}
