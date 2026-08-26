//! Local call-input audio transcription pump. Off unless
//! BOS_CALL_INPUTS_SYNC_ENABLED and BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED
//! are set. This is internal domain work, not an outbox/provider effect: it
//! stages transcript text through the normal call_inputs mutation path after
//! the source consent gate.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use bos_contracts::call_inputs::{
    CallInputKind, CallInputSourceConfig, CallInputStageRequest, CallInputTranscriptionMeta,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::source::{EvidenceRecord, EvidenceSourceRef, EvidenceUsagePolicy, SourceKind};
use bos_integrations::google_drive_read::{
    DriveFileMeta, DriveReadClient, LiveDriveReadClient, ReqwestDriveHttpClient,
};
use bos_integrations::google_oauth;
use sha2::{Digest, Sha256};

use super::{service, store};
use crate::env_registry;
use crate::http::{now_ms, AppState};
use crate::store_core::StoreError;

const ACTOR_ID: &str = "call_inputs_transcription_worker";
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "flac", "ogg"];
const PUMP_COOLDOWN_MS: u64 = 120_000;
const TRANSCRIPT_STDOUT_LIMIT_BYTES: usize = 256 * 1024;
const PROCESS_STDERR_LIMIT_BYTES: usize = 64 * 1024;
const VERSION_STDOUT_LIMIT_BYTES: usize = 4 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct TranscriptionPumpConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub intake_dir: Option<PathBuf>,
    pub tmp_dir: PathBuf,
    pub whisper_bin: Option<PathBuf>,
    pub whisper_model: Option<String>,
    pub timeout: Duration,
    pub max_audio_bytes: u64,
    pub max_concurrency: usize,
}

impl TranscriptionPumpConfig {
    fn runnable_error(&self) -> Option<&'static str> {
        if self.whisper_bin.is_none() {
            return Some("call_inputs_whisper_bin_unset");
        }
        if self.whisper_model.is_none() {
            return Some("call_inputs_whisper_model_unset");
        }
        None
    }
}

pub fn config_from_settings(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<TranscriptionPumpConfig, StoreError> {
    let sync_enabled = crate::slices::admin_settings::service::flag(
        conn,
        client_id,
        &env_registry::BOS_CALL_INPUTS_SYNC_ENABLED,
    )?;
    let transcription_enabled = crate::slices::admin_settings::service::flag(
        conn,
        client_id,
        &env_registry::BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED,
    )?;
    let interval_secs = crate::slices::admin_settings::service::usize_or(
        conn,
        client_id,
        &env_registry::BOS_CALL_INPUTS_SYNC_INTERVAL_SECS,
        300,
    )?
    .max(60) as u64;
    Ok(config_from_env_with_runtime(
        sync_enabled && transcription_enabled,
        interval_secs,
    ))
}

fn config_from_env_with_runtime(enabled: bool, interval_secs: u64) -> TranscriptionPumpConfig {
    TranscriptionPumpConfig {
        enabled,
        interval: Duration::from_secs(interval_secs),
        intake_dir: env_registry::string(&env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR)
            .map(|raw| PathBuf::from(raw.trim())),
        tmp_dir: env_registry::string(&env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR)
            .map(|raw| PathBuf::from(raw.trim()))
            .unwrap_or_else(|| PathBuf::from("var/call-inputs-transcription")),
        whisper_bin: env_registry::string(&env_registry::BOS_CALL_INPUTS_WHISPER_BIN)
            .map(|raw| PathBuf::from(raw.trim())),
        whisper_model: env_registry::string(&env_registry::BOS_CALL_INPUTS_WHISPER_MODEL)
            .map(|raw| raw.trim().to_string()),
        timeout: Duration::from_millis(
            env_registry::string(&env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS)
                .and_then(|raw| raw.trim().parse().ok())
                .unwrap_or(300_000)
                .clamp(5_000, 3_600_000),
        ),
        max_audio_bytes: env_registry::string(&env_registry::BOS_CALL_INPUTS_MAX_AUDIO_BYTES)
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(52_428_800)
            .clamp(1_000_000, 500_000_000),
        max_concurrency: env_registry::string(
            &env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY,
        )
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(1)
        .clamp(1, 4),
    }
}

pub fn spawn(state: AppState) {
    if !state.slice_enabled(super::SLICE.id) {
        tracing::info!(
            "call-input transcription pump not started (call_inputs disabled by client overlay)"
        );
        return;
    }
    std::thread::Builder::new()
        .name("call-input-transcription-pump".to_string())
        .spawn(move || {
            tracing::info!("call-input transcription pump started");
            loop {
                let config = {
                    let persistence = state.persistence.lock();
                    match config_from_settings(persistence.connection_ref(), &state.client_id) {
                        Ok(config) => config,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "call-input transcription config read failed"
                            );
                            config_from_env_with_runtime(false, 300)
                        }
                    }
                };
                if config.enabled && try_begin_transcription(&state, now_ms()).is_ok() {
                    match run_guarded_cycle(&state, &config) {
                        Ok(summary) if summary.files_seen > 0 || summary.staged > 0 => {
                            tracing::info!(
                                files_seen = summary.files_seen,
                                staged = summary.staged,
                                failed = summary.failed,
                                skipped = summary.skipped,
                                "call-input transcription cycle complete"
                            );
                        }
                        Ok(_) => {}
                        Err(err) => tracing::warn!(
                            error = %err,
                            "call-input transcription cycle failed"
                        ),
                    }
                }
                std::thread::sleep(effective_interval(&state, &config));
            }
        })
        .expect("spawn call-input-transcription-pump thread");
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CycleSummary {
    pub files_seen: u32,
    pub staged: u32,
    pub failed: u32,
    pub skipped: u32,
}

pub fn try_begin_transcription(state: &AppState, now: u64) -> Result<(), &'static str> {
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::CallInputTranscription)
        .lock();
    if status.in_flight {
        return Err("transcription_in_flight");
    }
    if now < status.next_allowed_at_ms {
        return Err("transcription_cooldown");
    }
    status.in_flight = true;
    status.last_attempt_ms = Some(now);
    Ok(())
}

pub fn run_guarded_cycle(
    state: &AppState,
    config: &TranscriptionPumpConfig,
) -> Result<CycleSummary, String> {
    let result = run_intake_cycle(state, config, now_ms());
    let mut status = state
        .sync_guards
        .guard(crate::http::Pump::CallInputTranscription)
        .lock();
    status.in_flight = false;
    status.next_allowed_at_ms = now_ms() + PUMP_COOLDOWN_MS;
    match &result {
        Ok(summary) => {
            status.units_used = summary.staged + summary.failed;
            status.last_outcome = Some("ok".to_string());
        }
        Err(err) => status.last_outcome = Some(format!("error: {err}")),
    }
    result
}

pub fn run_intake_cycle(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    now: u64,
) -> Result<CycleSummary, String> {
    if !config.enabled {
        return Ok(CycleSummary::default());
    }
    if let Some(reason) = config.runnable_error() {
        return Err(reason.to_string());
    }
    let source = first_recording_source(&state.call_inputs_overlay)
        .ok_or_else(|| "call_inputs_recording_source_not_configured".to_string())?;
    service::resolve_enabled_source(&state.call_inputs_overlay, &source.source_id)
        .map_err(|err| err.to_string())?;
    if let Some(summary) = run_drive_intake_cycle(state, config, source, now)? {
        return Ok(summary);
    }
    run_local_intake_cycle(state, config, source, now)
}

enum ProcessOutcome {
    Staged,
    Skipped,
}

fn process_audio_file(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    source: &CallInputSourceConfig,
    path: &Path,
    now: u64,
) -> Result<ProcessOutcome, String> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    if metadata.len() > config.max_audio_bytes {
        let content_hash = hash_file(path).unwrap_or_else(|_| "sha256:unreadable".to_string());
        record_failure_and_move_aside(
            state,
            path,
            &content_hash,
            "call_input_audio_too_large",
            now,
        )?;
        return Ok(ProcessOutcome::Skipped);
    }
    let content_hash = hash_file(path).map_err(|err| err.to_string())?;
    let source_ref = source_ref_for_file(path, &content_hash);
    {
        let persistence = state.persistence.lock();
        if store::get_input(
            persistence.connection_ref(),
            &state.client_id,
            &format!("call_{source_ref}"),
        )
        .map_err(|err| err.to_string())?
        .is_some()
        {
            move_to_subdir(path, "processed", &content_hash)
                .map_err(|err| format!("move duplicate processed: {err}"))?;
            return Ok(ProcessOutcome::Skipped);
        }
    }
    let job_dir =
        prepare_job_dir(&config.tmp_dir, &source_ref, now).map_err(|err| err.to_string())?;
    let temp_audio = job_dir.join(path.file_name().unwrap_or_default());
    if let Err(err) = fs::copy(path, &temp_audio) {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(format!("copy temp audio: {err}"));
    }
    let run = run_whisper_inner(config, &temp_audio, &content_hash, metadata.len());
    let _ = fs::remove_dir_all(&job_dir);
    let transcript = match run {
        Ok(run) => run,
        Err(err) => {
            record_failure_and_move_aside(state, path, &content_hash, err.code, now)?;
            return Err(err.message);
        }
    };
    let request = stage_request_for_transcript(
        source,
        path,
        &source_ref,
        transcript,
        now,
        Some(path.display().to_string()),
    );
    let input = service::input_from_stage(&request, source, now).map_err(|err| err.to_string())?;
    {
        let mut persistence = state.persistence.lock();
        store::insert_input(
            persistence.connection(),
            &state.client_id,
            ACTOR_ID,
            ActorKindDto::System,
            &input,
            &request.idempotency_key,
        )
        .map_err(|err| err.to_string())?;
    }
    move_to_subdir(path, "processed", &content_hash)
        .map_err(|err| format!("move processed: {err}"))?;
    Ok(ProcessOutcome::Staged)
}

fn run_local_intake_cycle(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    source: &CallInputSourceConfig,
    now: u64,
) -> Result<CycleSummary, String> {
    let Some(intake_dir) = config.intake_dir.as_ref() else {
        return Err("call_inputs_transcription_intake_dir_unset".to_string());
    };
    let files = audio_files(intake_dir).map_err(|err| err.to_string())?;
    let mut summary = CycleSummary {
        files_seen: files.len() as u32,
        ..CycleSummary::default()
    };
    for path in files.into_iter().take(config.max_concurrency) {
        match process_audio_file(state, config, source, &path, now) {
            Ok(ProcessOutcome::Staged) => summary.staged += 1,
            Ok(ProcessOutcome::Skipped) => summary.skipped += 1,
            Err(err) => {
                summary.failed += 1;
                tracing::warn!(path = %path.display(), error = %err, "call-input transcription failed");
            }
        }
    }
    Ok(summary)
}

fn run_drive_intake_cycle(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    source: &CallInputSourceConfig,
    now: u64,
) -> Result<Option<CycleSummary>, String> {
    let (folder_id, oauth) = {
        let persistence = state.persistence.lock();
        let settings = store::get_drive_settings(persistence.connection_ref(), &state.client_id)
            .map_err(|err| err.to_string())?;
        let Some(settings) = settings else {
            return Ok(None);
        };
        if !settings.ingestion_enabled {
            return Ok(Some(CycleSummary::default()));
        }
        let Some(folder_id) = settings.drive_folder_id else {
            return Ok(None);
        };
        let credential_user_id = settings.credential_user_id;
        let oauth = crate::slices::google_connector::service::resolve_google_oauth_for_owner(
            persistence.connection_ref(),
            &state.client_id,
            credential_user_id.as_deref(),
        )
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "google_credential_not_connected".to_string())?;
        (folder_id, oauth)
    };
    if !oauth.scopes.is_empty()
        && !google_oauth::has_scope(
            &oauth,
            crate::slices::google_connector::service::DRIVE_READONLY_SCOPE,
        )
    {
        return Err("google_drive_scope_missing".to_string());
    }
    let access_token = google_oauth::fetch_access_token(&oauth).map_err(|err| err.to_string())?;
    let client = LiveDriveReadClient::new(ReqwestDriveHttpClient::default());
    run_drive_intake_cycle_with_client(
        state,
        config,
        source,
        now,
        &client,
        &access_token,
        &folder_id,
    )
    .map(Some)
}

fn effective_interval(state: &AppState, config: &TranscriptionPumpConfig) -> Duration {
    let persistence = state.persistence.lock();
    store::get_drive_settings(persistence.connection_ref(), &state.client_id)
        .ok()
        .flatten()
        .and_then(|settings| settings.interval_secs)
        .map(Duration::from_secs)
        .unwrap_or(config.interval)
}

pub(super) fn run_drive_intake_cycle_with_client(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    source: &CallInputSourceConfig,
    now: u64,
    client: &dyn DriveReadClient,
    access_token: &str,
    folder_id: &str,
) -> Result<CycleSummary, String> {
    let mut summary = CycleSummary::default();
    let mut page_token: Option<String> = None;
    let mut attempted = 0usize;
    loop {
        let page = client
            .list_folder_files(access_token, folder_id, page_token.as_deref())
            .map_err(|err| err.to_string())?;
        for file in page.files.into_iter().filter(is_audio_drive_file) {
            summary.files_seen = summary.files_seen.saturating_add(1);
            if drive_input_already_staged(state, &source_ref_for_drive_file(&file))? {
                summary.skipped = summary.skipped.saturating_add(1);
                continue;
            }
            if attempted >= config.max_concurrency {
                return Ok(summary);
            }
            attempted += 1;
            match process_drive_audio_file(state, config, source, client, access_token, &file, now)
            {
                Ok(ProcessOutcome::Staged) => summary.staged += 1,
                Ok(ProcessOutcome::Skipped) => summary.skipped += 1,
                Err(err) => {
                    summary.failed += 1;
                    tracing::warn!(file_id = %file.file_id, name = %file.name, error = %err, "call-input Drive transcription failed");
                }
            }
        }
        if attempted >= config.max_concurrency {
            return Ok(summary);
        }
        let Some(next_page_token) = page.next_page_token else {
            return Ok(summary);
        };
        page_token = Some(next_page_token);
    }
}

fn drive_input_already_staged(state: &AppState, source_ref: &str) -> Result<bool, String> {
    let persistence = state.persistence.lock();
    store::get_input(
        persistence.connection_ref(),
        &state.client_id,
        &format!("call_{source_ref}"),
    )
    .map(|row| row.is_some())
    .map_err(|err| err.to_string())
}

fn process_drive_audio_file(
    state: &AppState,
    config: &TranscriptionPumpConfig,
    source: &CallInputSourceConfig,
    client: &dyn DriveReadClient,
    access_token: &str,
    file: &DriveFileMeta,
    now: u64,
) -> Result<ProcessOutcome, String> {
    let source_ref = source_ref_for_drive_file(file);
    {
        let persistence = state.persistence.lock();
        if store::get_input(
            persistence.connection_ref(),
            &state.client_id,
            &format!("call_{source_ref}"),
        )
        .map_err(|err| err.to_string())?
        .is_some()
        {
            return Ok(ProcessOutcome::Skipped);
        }
    }
    let bytes = client
        .download_file(access_token, file, config.max_audio_bytes)
        .map_err(|err| err.to_string())?;
    let content_hash = hash_bytes(&bytes);
    let job_dir =
        prepare_job_dir(&config.tmp_dir, &source_ref, now).map_err(|err| err.to_string())?;
    let temp_audio = job_dir.join(&file.name);
    if let Err(err) = fs::write(&temp_audio, &bytes) {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(format!("write temp Drive audio: {err}"));
    }
    let run = run_whisper_inner(config, &temp_audio, &content_hash, bytes.len() as u64);
    let _ = fs::remove_dir_all(&job_dir);
    let transcript = match run {
        Ok(run) => run,
        Err(err) => {
            record_transcription_failure(state, &source_ref, err.code, now)?;
            return Err(err.message);
        }
    };
    let request = stage_request_for_transcript(
        source,
        Path::new(&file.name),
        &source_ref,
        transcript,
        now,
        file.web_view_link.clone(),
    );
    let input = service::input_from_stage(&request, source, now).map_err(|err| err.to_string())?;
    {
        let mut persistence = state.persistence.lock();
        store::insert_input(
            persistence.connection(),
            &state.client_id,
            ACTOR_ID,
            ActorKindDto::System,
            &input,
            &request.idempotency_key,
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(ProcessOutcome::Staged)
}

fn first_recording_source(
    overlay: &crate::overlay::CallInputsOverlay,
) -> Option<&CallInputSourceConfig> {
    overlay.sources.iter().find(|source| {
        matches!(
            source.kind,
            bos_contracts::call_inputs::CallInputSourceKind::FolderRecording
                | bos_contracts::call_inputs::CallInputSourceKind::DriveRecording
                | bos_contracts::call_inputs::CallInputSourceKind::Other
        )
    })
}

#[derive(Debug, Clone)]
pub struct WhisperTranscript {
    pub text: String,
    pub meta: CallInputTranscriptionMeta,
}

#[derive(Debug, Clone)]
struct WhisperError {
    code: &'static str,
    message: String,
}

pub fn run_whisper(
    config: &TranscriptionPumpConfig,
    audio_path: &Path,
    source_content_hash: &str,
    audio_bytes: u64,
) -> Result<WhisperTranscript, String> {
    run_whisper_inner(config, audio_path, source_content_hash, audio_bytes)
        .map_err(|err| err.message)
}

fn run_whisper_inner(
    config: &TranscriptionPumpConfig,
    audio_path: &Path,
    source_content_hash: &str,
    audio_bytes: u64,
) -> Result<WhisperTranscript, WhisperError> {
    let bin = config.whisper_bin.as_ref().ok_or(WhisperError {
        code: "call_inputs_whisper_bin_unset",
        message: "whisper bin unset".to_string(),
    })?;
    let model = config.whisper_model.as_ref().ok_or(WhisperError {
        code: "call_inputs_whisper_model_unset",
        message: "whisper model unset".to_string(),
    })?;
    let started = Instant::now();
    let output = run_command_with_timeout(bin, model, audio_path, config.timeout)?;
    let runtime_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    if !output.status.success() {
        return Err(WhisperError {
            code: "call_input_transcription_exit_nonzero",
            message: format!("whisper exited with {}", exit_status_string(output.status)),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err(WhisperError {
            code: "call_input_transcription_empty",
            message: "whisper produced empty transcript".to_string(),
        });
    }
    let executable_version = whisper_version(bin).unwrap_or_else(|| "unknown".to_string());
    Ok(WhisperTranscript {
        text,
        meta: CallInputTranscriptionMeta {
            engine: "whisper.cpp".to_string(),
            executable: bin.display().to_string(),
            executable_version,
            model: model.clone(),
            model_hash: model_hash(model),
            source_content_hash: Some(source_content_hash.to_string()),
            audio_bytes,
            runtime_ms,
            exit_status: exit_status_string(output.status),
        },
    })
}

fn run_command_with_timeout(
    bin: &Path,
    model: &str,
    audio_path: &Path,
    timeout: Duration,
) -> Result<std::process::Output, WhisperError> {
    let mut command = Command::new(bin);
    command
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(audio_path)
        .arg("-nt");
    run_process_with_timeout(
        &mut command,
        timeout,
        TRANSCRIPT_STDOUT_LIMIT_BYTES,
        PROCESS_STDERR_LIMIT_BYTES,
        "call_input_transcription",
    )
}

fn run_process_with_timeout(
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    code_prefix: &'static str,
) -> Result<std::process::Output, WhisperError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| WhisperError {
            code: "call_input_transcription_spawn_failed",
            message: err.to_string(),
        })?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| read_pipe_bounded(pipe, stdout_limit));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| read_pipe_bounded(pipe, stderr_limit));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return collect_process_output(status, stdout, stderr, code_prefix),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_pipe(stdout, code_prefix);
                let _ = join_pipe(stderr, code_prefix);
                return Err(WhisperError {
                    code: "call_input_transcription_timeout",
                    message: format!("{code_prefix} timed out after {}ms", timeout.as_millis()),
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WhisperError {
                    code: "call_input_transcription_wait_failed",
                    message: err.to_string(),
                });
            }
        }
    }
}

fn whisper_version(bin: &Path) -> Option<String> {
    let mut command = Command::new(bin);
    command.arg("--version");
    run_process_with_timeout(
        &mut command,
        VERSION_TIMEOUT,
        VERSION_STDOUT_LIMIT_BYTES,
        PROCESS_STDERR_LIMIT_BYTES,
        "call_input_transcription_version",
    )
    .ok()
    .and_then(|output| {
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .chars()
                .take(200)
                .collect::<String>()
        })
    })
    .filter(|value| {
        !value.is_empty()
            && !value.starts_with("error:")
            && !value.starts_with("usage:")
            && !value.contains("unknown argument")
    })
}

fn read_pipe_bounded<R>(mut pipe: R, limit: usize) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = pipe.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            if kept.len() < limit {
                let remaining = limit - kept.len();
                kept.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        }
        Ok(kept)
    })
}

fn collect_process_output(
    status: std::process::ExitStatus,
    stdout: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    code_prefix: &'static str,
) -> Result<std::process::Output, WhisperError> {
    let stdout = join_pipe(stdout, code_prefix)?;
    let stderr = join_pipe(stderr, code_prefix)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn join_pipe(
    handle: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    code_prefix: &'static str,
) -> Result<Vec<u8>, WhisperError> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| WhisperError {
                code: "call_input_transcription_wait_failed",
                message: format!("{code_prefix} output reader panicked"),
            })?
            .map_err(|err| WhisperError {
                code: "call_input_transcription_wait_failed",
                message: err.to_string(),
            }),
        None => Ok(Vec::new()),
    }
}

fn stage_request_for_transcript(
    source: &CallInputSourceConfig,
    path: &Path,
    source_ref: &str,
    transcript: WhisperTranscript,
    now: u64,
    item_url: Option<String>,
) -> CallInputStageRequest {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("call recording");
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .trim()
        .to_string();
    let title = if title.is_empty() {
        "Call recording".to_string()
    } else {
        format!("Call recording: {title}")
    };
    let content_hash = transcript.meta.source_content_hash.clone();
    CallInputStageRequest {
        source_id: source.source_id.clone(),
        source_ref: source_ref.to_string(),
        input_kind: CallInputKind::Recording,
        title,
        summary: transcript.text.chars().take(500).collect(),
        caller_name: None,
        caller_phone: None,
        caller_email: None,
        transcript_text: transcript.text,
        recording_ref: EvidenceRecord {
            evidence_id: format!("ev_{source_ref}"),
            source: EvidenceSourceRef {
                source_id: source.source_id.clone(),
                kind: SourceKind::Call,
                display_name: source.display_name.clone(),
                url: None,
            },
            policy: EvidenceUsagePolicy::approved_source_import(),
            item_url,
            captured_at_ms: Some(now),
            evidence_quote: String::new(),
            content_hash,
        },
        transcription_meta: Some(transcript.meta),
        occurred_at_ms: None,
        captured_at_ms: Some(now),
        idempotency_key: source_ref.to_string(),
        actor_id: Some(ACTOR_ID.to_string()),
    }
}

fn audio_files(intake_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    if !intake_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(intake_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && is_audio_file(&path) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn prepare_job_dir(tmp_root: &Path, source_ref: &str, now: u64) -> std::io::Result<PathBuf> {
    let sanitized = source_ref
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect::<String>();
    let job_dir = tmp_root.join(format!("{sanitized}-{now}"));
    fs::create_dir_all(&job_dir)?;
    Ok(job_dir)
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(is_audio_extension)
        .unwrap_or(false)
}

fn is_audio_drive_file(file: &DriveFileMeta) -> bool {
    Path::new(&file.name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(is_audio_extension)
        .unwrap_or(false)
}

fn is_audio_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    AUDIO_EXTENSIONS.iter().any(|allowed| *allowed == ext)
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn model_hash(model: &str) -> Option<String> {
    let path = Path::new(model);
    path.is_file().then(|| hash_file(path).ok()).flatten()
}

fn source_ref_for_file(path: &Path, content_hash: &str) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("audio")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect::<String>();
    let hash = content_hash.trim_start_matches("sha256:");
    format!(
        "local-audio:{}:{}",
        stem,
        hash.chars().take(16).collect::<String>()
    )
}

fn source_ref_for_drive_file(file: &DriveFileMeta) -> String {
    let version = file
        .version
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(file.modified_time.as_str())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(80)
        .collect::<String>();
    format!("drive-audio:{}:{}", file.file_id, version)
}

fn record_transcription_failure(
    state: &AppState,
    source_ref: &str,
    error_code: &str,
    now: u64,
) -> Result<(), String> {
    let idempotency_key = format!("transcription-failure:{source_ref}:{now}");
    let mut persistence = state.persistence.lock();
    store::record_transcription_failure(
        persistence.connection(),
        &state.client_id,
        ACTOR_ID,
        source_ref,
        &idempotency_key,
        error_code,
        now,
    )
    .map_err(|err| err.to_string())
}

fn record_failure_and_move_aside(
    state: &AppState,
    path: &Path,
    content_hash: &str,
    error_code: &str,
    now: u64,
) -> Result<(), String> {
    let idempotency_key = format!(
        "transcription-failure:{}:{}:{}",
        source_ref_for_file(path, content_hash),
        content_hash
            .trim_start_matches("sha256:")
            .chars()
            .take(16)
            .collect::<String>(),
        now
    );
    {
        let mut persistence = state.persistence.lock();
        store::record_transcription_failure(
            persistence.connection(),
            &state.client_id,
            ACTOR_ID,
            &path.display().to_string(),
            &idempotency_key,
            error_code,
            now,
        )
        .map_err(|err| err.to_string())?;
    }
    let moved = move_to_subdir(path, "failed", content_hash)
        .map_err(|err| format!("move failed aside: {err}"))?;
    tracing::warn!(
        source = %path.display(),
        moved_to = %moved.display(),
        error_code,
        "call-input audio moved aside after transcription failure"
    );
    Ok(())
}

fn move_to_subdir(path: &Path, subdir: &str, content_hash: &str) -> std::io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let dir = parent.join(subdir);
    fs::create_dir_all(&dir)?;
    let file_name = path.file_name().unwrap_or_default();
    let hash = content_hash.trim_start_matches("sha256:");
    let dest = dir.join(format!(
        "{}.{}",
        hash.chars().take(16).collect::<String>(),
        file_name.to_string_lossy()
    ));
    fs::rename(path, &dest)?;
    Ok(dest)
}

fn exit_status_string(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| format!("exit:{code}"))
        .unwrap_or_else(|| "signal".to_string())
}
