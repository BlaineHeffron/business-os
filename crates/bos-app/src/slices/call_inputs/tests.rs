use bos_contracts::call_inputs::{
    CallInputKind, CallInputSourceConfig, CallInputSourceKind, CallInputStageRequest,
    CallInputStatus, CallInputsDriveSettingsUpdateRequest, CallInputsRouting,
};
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::source::{EvidenceRecord, EvidenceSourceRef, EvidenceUsagePolicy, SourceKind};
use bos_contracts::work_queue::WorkItemStatus;
use bos_integrations::google_drive_read::{
    DriveChange, DriveChangesPage, DriveError, DriveFileMeta, DriveFilePage, DriveReadClient,
};
use std::collections::HashMap;
use std::sync::Arc;

use super::{service, store, worker};
use crate::overlay::CallInputsOverlay;
use crate::persistence::PersistencePool;
use crate::slices::mutation_context::MutationContext;
use crate::store_core::MutationOutcome;

fn ready_source() -> CallInputSourceConfig {
    CallInputSourceConfig {
        source_id: "demo_selected_transcripts".to_string(),
        display_name: "Demo selected transcripts".to_string(),
        kind: CallInputSourceKind::DriveTranscript,
        location_hint: Some("Drive folder id".to_string()),
        enabled: true,
        consent_basis: Some("Written consent/fit attestation".to_string()),
    }
}

fn overlay() -> CallInputsOverlay {
    CallInputsOverlay {
        sources: vec![ready_source()],
        routing: CallInputsRouting {
            packet_kinds: vec!["crm_activity".to_string(), "follow_up_task".to_string()],
        },
    }
}

fn evidence() -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: "ev_call_1".to_string(),
        source: EvidenceSourceRef {
            source_id: "demo_selected_transcripts".to_string(),
            kind: SourceKind::Call,
            display_name: "Demo selected transcripts".to_string(),
            url: None,
        },
        policy: EvidenceUsagePolicy::approved_source_import(),
        item_url: Some("drive://recording-1.m4a".to_string()),
        captured_at_ms: Some(1_700_000),
        evidence_quote: "Caller asked for a follow-up next Tuesday.".to_string(),
        content_hash: Some("sha256:test".to_string()),
    }
}

fn stage_request() -> CallInputStageRequest {
    CallInputStageRequest {
        source_id: "demo_selected_transcripts".to_string(),
        source_ref: "drive-file-1".to_string(),
        input_kind: CallInputKind::Transcript,
        title: "Call with Dana".to_string(),
        summary: "Dana asked about primer and requested a follow-up.".to_string(),
        caller_name: Some("Dana".to_string()),
        caller_phone: Some("+1 555 0100".to_string()),
        caller_email: None,
        transcript_text: "Dana: Can you follow up next Tuesday about primer?".to_string(),
        recording_ref: evidence(),
        transcription_meta: None,
        occurred_at_ms: Some(1_650_000),
        captured_at_ms: Some(1_700_000),
        idempotency_key: "stage-call-1".to_string(),
        actor_id: None,
    }
}

fn recording_overlay() -> CallInputsOverlay {
    let mut source = ready_source();
    source.kind = CallInputSourceKind::FolderRecording;
    CallInputsOverlay {
        sources: vec![source],
        routing: CallInputsRouting {
            packet_kinds: vec!["crm_activity".to_string(), "follow_up_task".to_string()],
        },
    }
}

fn drive_recording_overlay() -> CallInputsOverlay {
    let mut source = ready_source();
    source.kind = CallInputSourceKind::DriveRecording;
    CallInputsOverlay {
        sources: vec![source],
        routing: CallInputsRouting {
            packet_kinds: vec!["crm_activity".to_string(), "follow_up_task".to_string()],
        },
    }
}

#[test]
fn status_is_pending_until_source_enabled() {
    let mut pending = ready_source();
    pending.enabled = false;
    let status = service::status(&CallInputsOverlay {
        sources: vec![pending],
        routing: CallInputsRouting::default(),
    });
    assert!(!status.configured);
    assert_eq!(status.enabled_sources, 0);
    assert_eq!(status.pending_sources, 1);
}

#[test]
fn source_gate_reports_missing_and_disabled_errors() {
    let err =
        service::resolve_enabled_source(&CallInputsOverlay::default(), "demo_selected_transcripts")
            .expect_err("missing source refused");
    assert!(err.to_string().contains("call_source_not_configured"));

    let mut source = ready_source();
    source.enabled = false;
    let err = service::resolve_enabled_source(
        &CallInputsOverlay {
            sources: vec![source],
            routing: CallInputsRouting::default(),
        },
        "demo_selected_transcripts",
    )
    .expect_err("disabled source refused");
    assert!(err.to_string().contains("call_source_not_enabled"));

    let mut source = ready_source();
    source.consent_basis = None;
    let err = service::resolve_enabled_source(
        &CallInputsOverlay {
            sources: vec![source],
            routing: CallInputsRouting::default(),
        },
        "demo_selected_transcripts",
    )
    .expect_err("missing consent refused");
    assert!(err.to_string().contains("call_source_consent_missing"));
}

#[test]
fn call_input_can_be_staged_and_accepted_into_queue() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay();
    let source = service::resolve_enabled_source(&overlay, "demo_selected_transcripts").unwrap();
    let input = service::input_from_stage(&stage_request(), source, 2_000_000).unwrap();
    store::insert_input(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &input,
        "stage-call-1",
    )
    .expect("input staged");

    let staged = store::get_input(conn.connection_ref(), "client", &input.call_input_id)
        .unwrap()
        .expect("call input");
    assert_eq!(staged.input.status, CallInputStatus::Staged);
    assert_eq!(staged.input.source_ref, "drive-file-1");
    assert_eq!(
        staged.input.recording_ref.policy,
        EvidenceUsagePolicy::approved_source_import()
    );

    store::accept_input(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-call-1",
            now_ms: 2_000_100,
        },
        &input.call_input_id,
        &[
            "calendar_event_draft".to_string(),
            "email_draft_reply".to_string(),
        ],
    )
    .expect("input accepted");

    let accepted = store::get_input(conn.connection_ref(), "client", &input.call_input_id)
        .unwrap()
        .expect("call input");
    assert_eq!(accepted.input.status, CallInputStatus::Accepted);
    let item_id = accepted.input.work_item_id.expect("queue item");
    let item = crate::slices::work_queue::store::get_item_unscoped(
        conn.connection_ref(),
        "client",
        &item_id,
    )
    .unwrap()
    .expect("work item");
    assert_eq!(item.item.status, WorkItemStatus::Open);
    assert_eq!(item.item.source_kind, super::SOURCE_KIND_CALL_INPUT);
    assert_eq!(
        item.item.packet_kinds,
        vec!["calendar_event_draft", "email_draft_reply"]
    );
}

#[test]
fn accept_replays_idempotently_after_status_changes() {
    let pool = PersistencePool::open_in_memory().expect("db");
    let mut conn = pool.get().expect("conn");
    let overlay = overlay();
    let source = service::resolve_enabled_source(&overlay, "demo_selected_transcripts").unwrap();
    let input = service::input_from_stage(&stage_request(), source, 2_000_000).unwrap();
    store::insert_input(
        conn.connection(),
        "client",
        "operator",
        ActorKindDto::Operator,
        &input,
        "stage-call-1",
    )
    .expect("input staged");
    let staged = store::get_input(conn.connection_ref(), "client", &input.call_input_id)
        .unwrap()
        .expect("call input");

    let first = store::accept_input(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-call-1",
            now_ms: 2_000_100,
        },
        &input.call_input_id,
        &service::resolve_packet_kinds(&[], &overlay.routing, |_| true).expect("kinds"),
    )
    .expect("accepted");
    assert!(matches!(first, MutationOutcome::Applied { .. }));

    let replay = store::accept_input(
        conn.connection(),
        MutationContext {
            client_id: "client",
            actor_id: "operator",
            expected_revision: Some(staged.revision),
            idempotency_key: "accept-call-1",
            now_ms: 2_000_200,
        },
        &input.call_input_id,
        &service::resolve_packet_kinds(&[], &overlay.routing, |_| true).expect("kinds"),
    )
    .expect("replayed");
    assert!(matches!(replay, MutationOutcome::ReplayedIdempotent { .. }));
}

#[test]
fn source_view_feeds_existing_produce_pipeline() {
    let overlay = overlay();
    let source = service::resolve_enabled_source(&overlay, "demo_selected_transcripts").unwrap();
    let input = service::input_from_stage(&stage_request(), source, 2_000_000).unwrap();
    let view = service::source_view(&input, Some(source));
    assert_eq!(view.message_id, input.call_input_id);
    assert_eq!(view.resolved_category, service::CATEGORY_ID);
    assert_eq!(view.internal_date_ms, Some(1_650_000));
    assert!(view.body_full.contains("Transcript:"));
    assert!(view.body_full.contains("Dana: Can you follow up"));
    assert!(view.body_full.contains("Provenance:"));
}

#[test]
fn stage_canonicalizes_evidence_source_to_configured_call_source() {
    let overlay = overlay();
    let source = service::resolve_enabled_source(&overlay, "demo_selected_transcripts").unwrap();
    let mut request = stage_request();
    request.recording_ref.source.source_id = "unapproved_source".to_string();
    request.recording_ref.source.kind = SourceKind::Web;
    request.recording_ref.source.display_name = "Unapproved source".to_string();

    let input = service::input_from_stage(&request, source, 2_000_000).unwrap();

    assert_eq!(input.recording_ref.source.source_id, source.source_id);
    assert_eq!(input.recording_ref.source.kind, SourceKind::Call);
    assert_eq!(input.recording_ref.source.display_name, source.display_name);
}

#[test]
fn transcription_worker_is_inert_when_gate_is_off() {
    let mut state = crate::http::test_support::test_state();
    state.call_inputs_overlay = Arc::new(recording_overlay());
    let temp = temp_dir("gate-off");
    let intake = temp.join("intake");
    std::fs::create_dir_all(&intake).expect("intake");
    std::fs::write(intake.join("call.wav"), b"audio").expect("audio");
    let config = transcription_config(false, &temp, fake_whisper(&temp, "ok", 0, 0));

    let summary = worker::run_intake_cycle(&state, &config, 1_000).expect("cycle");

    assert_eq!(summary.files_seen, 0);
    assert!(intake.join("call.wav").exists());
}

#[test]
fn transcription_worker_stages_transcript_with_fake_executable() {
    let mut state = crate::http::test_support::test_state();
    state.call_inputs_overlay = Arc::new(recording_overlay());
    let temp = temp_dir("success");
    let intake = temp.join("intake");
    std::fs::create_dir_all(&intake).expect("intake");
    std::fs::write(intake.join("call.wav"), b"fake audio").expect("audio");
    let config = transcription_config(
        true,
        &temp,
        fake_whisper(&temp, "Caller asked for primer follow-up.", 0, 0),
    );

    let summary = worker::run_intake_cycle(&state, &config, 2_000).expect("cycle");

    assert_eq!(summary.staged, 1);
    assert_eq!(summary.failed, 0);
    assert!(intake.join("processed").exists());
    let persistence = state.persistence.lock();
    let rows = store::list_inputs(
        persistence.connection_ref(),
        &state.client_id,
        Some(CallInputStatus::Staged),
        10,
    )
    .expect("inputs");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input.input_kind, CallInputKind::Recording);
    assert!(rows[0].input.transcript_text.contains("primer follow-up"));
    let meta = rows[0].input.transcription_meta.as_ref().expect("meta");
    assert_eq!(meta.engine, "whisper.cpp");
    assert_eq!(meta.exit_status, "exit:0");
    assert!(meta
        .source_content_hash
        .as_deref()
        .unwrap_or("")
        .starts_with("sha256:"));
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        &state.client_id,
        store::CALL_INPUT_ENTITY_KIND,
        &rows[0].input.call_input_id,
        10,
    )
    .expect("receipts");
    assert_eq!(receipts[0].actor_kind, ActorKindDto::System);
}

#[test]
fn transcription_failure_receipted_and_audio_moved_aside() {
    let mut state = crate::http::test_support::test_state();
    state.call_inputs_overlay = Arc::new(recording_overlay());
    let temp = temp_dir("failure");
    let intake = temp.join("intake");
    std::fs::create_dir_all(&intake).expect("intake");
    let audio_path = intake.join("call.wav");
    std::fs::write(&audio_path, b"fake audio").expect("audio");
    let config = transcription_config(true, &temp, fake_whisper(&temp, "boom", 7, 0));

    let summary = worker::run_intake_cycle(&state, &config, 3_000).expect("cycle");

    assert_eq!(summary.failed, 1);
    assert!(intake.join("failed").exists());
    assert!(!audio_path.exists());
    let persistence = state.persistence.lock();
    let inputs = store::list_inputs(
        persistence.connection_ref(),
        &state.client_id,
        Some(CallInputStatus::Staged),
        10,
    )
    .expect("inputs");
    assert!(inputs.is_empty());
    let receipts = crate::store_core::receipts_for_entity(
        persistence.connection_ref(),
        &state.client_id,
        store::CALL_INPUT_ENTITY_KIND,
        &format!("transcription:{}", audio_path.display()),
        10,
    )
    .expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].change_kind, "transcription_failed");
}

#[test]
fn transcription_worker_drains_large_stdout_without_pipe_deadlock() {
    let temp = temp_dir("large-stdout");
    let audio = temp.join("call.wav");
    std::fs::write(&audio, b"fake audio").expect("audio");
    let config = transcription_config(
        true,
        &temp,
        fake_whisper(&temp, &"transcript ".repeat(40_000), 0, 0),
    );

    let transcript = worker::run_whisper(&config, &audio, "sha256:test", 10).expect("transcript");

    assert!(transcript.text.starts_with("transcript transcript"));
}

#[test]
fn transcription_worker_bounds_version_probe() {
    let temp = temp_dir("version-timeout");
    let audio = temp.join("call.wav");
    std::fs::write(&audio, b"fake audio").expect("audio");
    let mut config = transcription_config(
        true,
        &temp,
        fake_whisper_with_version_sleep(&temp, "short transcript", 5),
    );
    config.timeout = std::time::Duration::from_millis(500);

    let transcript = worker::run_whisper(&config, &audio, "sha256:test", 10).expect("transcript");

    assert_eq!(transcript.text, "short transcript");
    assert_eq!(transcript.meta.executable_version, "unknown");
}

#[test]
fn call_input_packet_kinds_must_be_catalog_owned_and_enabled() {
    let overlay = overlay();
    let unknown =
        service::resolve_packet_kinds(&["not_a_packet".to_string()], &overlay.routing, |_| true)
            .expect_err("unknown refused");
    assert!(unknown
        .to_string()
        .contains("work_queue_packet_kind_unknown:not_a_packet"));

    let disabled = service::resolve_packet_kinds(
        &["calendar_event_draft".to_string()],
        &overlay.routing,
        |slice| slice != crate::slices::calendar_drafts::SLICE.id,
    )
    .expect_err("disabled refused");
    assert!(disabled
        .to_string()
        .contains("work_queue_packet_kind_disabled:calendar_event_draft"));
}

#[test]
fn drive_intake_skips_existing_files_without_consuming_processing_cap() {
    let mut state = crate::http::test_support::test_state();
    state.call_inputs_overlay = Arc::new(drive_recording_overlay());
    let temp = temp_dir("drive-skip-cap");
    let config = transcription_config(
        true,
        &temp,
        fake_whisper(&temp, "Drive caller asked for a follow-up.", 0, 0),
    );
    let source = state.call_inputs_overlay.sources.first().expect("source");
    let drive = FakeDriveRead::new(vec![
        drive_meta("drive-a", "001-call.wav", "1"),
        drive_meta("drive-b", "002-call.wav", "1"),
    ]);

    let first = worker::run_drive_intake_cycle_with_client(
        &state,
        &config,
        source,
        10_000,
        &drive,
        "token",
        "folder-audio",
    )
    .expect("first cycle");
    assert_eq!(first.staged, 1);
    assert_eq!(first.skipped, 0);

    let second = worker::run_drive_intake_cycle_with_client(
        &state,
        &config,
        source,
        20_000,
        &drive,
        "token",
        "folder-audio",
    )
    .expect("second cycle");
    assert_eq!(second.staged, 1);
    assert_eq!(second.skipped, 1);

    let persistence = state.persistence.lock();
    let rows = store::list_inputs(
        persistence.connection_ref(),
        &state.client_id,
        Some(CallInputStatus::Staged),
        10,
    )
    .expect("inputs");
    assert_eq!(rows.len(), 2);
}

fn transcription_config(
    enabled: bool,
    temp: &std::path::Path,
    whisper_bin: std::path::PathBuf,
) -> worker::TranscriptionPumpConfig {
    worker::TranscriptionPumpConfig {
        enabled,
        interval: std::time::Duration::from_secs(60),
        intake_dir: Some(temp.join("intake")),
        tmp_dir: temp.join("tmp"),
        whisper_bin: Some(whisper_bin),
        whisper_model: Some("base.en".to_string()),
        timeout: std::time::Duration::from_millis(2_000),
        max_audio_bytes: 10_000_000,
        max_concurrency: 1,
    }
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bos-call-inputs-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("temp");
    path
}

fn fake_whisper(
    temp: &std::path::Path,
    output: &str,
    exit_code: i32,
    sleep_secs: u64,
) -> std::path::PathBuf {
    let path = temp.join(format!("fake-whisper-{exit_code}-{sleep_secs}.sh"));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo fake-whisper-v1; exit 0; fi\nsleep {sleep_secs}\necho {quoted}\nexit {exit_code}\n",
        quoted = shell_quote(output),
    );
    std::fs::write(&path, script).expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
    }
    path
}

fn fake_whisper_with_version_sleep(
    temp: &std::path::Path,
    output: &str,
    version_sleep_secs: u64,
) -> std::path::PathBuf {
    let path = temp.join(format!(
        "fake-whisper-version-sleep-{version_sleep_secs}.sh"
    ));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then sleep {version_sleep_secs}; echo fake-whisper-v1; exit 0; fi\necho {quoted}\nexit 0\n",
        quoted = shell_quote(output),
    );
    std::fs::write(&path, script).expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
    }
    path
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

#[test]
fn drive_settings_replace_is_receipted_and_revision_checked() {
    let state = crate::http::test_support::test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    let request = CallInputsDriveSettingsUpdateRequest {
        expected_revision: None,
        idempotency_key: "call-drive-settings-1".to_string(),
        actor_id: None,
        drive_folder_id: Some("folder-audio".to_string()),
        drive_folder_name: Some("BOS Call Audio".to_string()),
        ingestion_enabled: true,
        interval_secs: Some(300),
    };
    let outcome = store::replace_drive_settings(
        conn,
        &state.client_id,
        "operator",
        Some("operator"),
        &request,
        10_000,
    )
    .expect("save");
    assert!(matches!(
        outcome,
        MutationOutcome::Applied { revision: 1, .. }
    ));
    let stored = store::get_drive_settings(conn, &state.client_id)
        .expect("load")
        .expect("settings");
    assert_eq!(stored.drive_folder_id.as_deref(), Some("folder-audio"));
    assert_eq!(stored.credential_user_id.as_deref(), Some("operator"));
    assert_eq!(stored.drive_folder_name.as_deref(), Some("BOS Call Audio"));
    assert!(stored.ingestion_enabled);
    assert_eq!(stored.interval_secs, Some(300));
    assert_eq!(stored.revision, Some(1));

    let false_flag = CallInputsDriveSettingsUpdateRequest {
        expected_revision: Some(1),
        idempotency_key: "call-drive-settings-implied".to_string(),
        actor_id: None,
        drive_folder_id: Some("folder-audio-2".to_string()),
        drive_folder_name: Some("BOS Call Audio 2".to_string()),
        ingestion_enabled: false,
        interval_secs: Some(300),
    };
    store::replace_drive_settings(
        conn,
        &state.client_id,
        "operator",
        Some("operator"),
        &false_flag,
        10_050,
    )
    .expect("save implied");
    let stored = store::get_drive_settings(conn, &state.client_id)
        .expect("load implied")
        .expect("settings");
    assert_eq!(stored.drive_folder_id.as_deref(), Some("folder-audio-2"));
    assert!(stored.ingestion_enabled);
    assert_eq!(stored.revision, Some(2));

    let stale = CallInputsDriveSettingsUpdateRequest {
        expected_revision: Some(1),
        idempotency_key: "call-drive-settings-stale".to_string(),
        actor_id: None,
        drive_folder_id: Some("other-folder".to_string()),
        drive_folder_name: Some("Other".to_string()),
        ingestion_enabled: true,
        interval_secs: Some(600),
    };
    let conflict = store::replace_drive_settings(
        conn,
        &state.client_id,
        "operator",
        Some("operator"),
        &stale,
        10_100,
    )
    .expect("conflict");
    assert!(matches!(
        conflict,
        MutationOutcome::RevisionConflict {
            current_revision: Some(2),
            ..
        }
    ));
}

struct FakeDriveRead {
    files: Vec<DriveFileMeta>,
    downloads: HashMap<String, Vec<u8>>,
}

impl FakeDriveRead {
    fn new(files: Vec<DriveFileMeta>) -> Self {
        let downloads = files
            .iter()
            .map(|file| {
                (
                    file.file_id.clone(),
                    format!("audio:{}", file.file_id).into_bytes(),
                )
            })
            .collect();
        Self { files, downloads }
    }
}

impl DriveReadClient for FakeDriveRead {
    fn fetch_start_page_token(&self, _access_token: &str) -> Result<String, DriveError> {
        Ok("start".to_string())
    }

    fn fetch_changes(
        &self,
        _access_token: &str,
        _page_token: &str,
    ) -> Result<DriveChangesPage, DriveError> {
        Ok(DriveChangesPage {
            changes: Vec::<DriveChange>::new(),
            next_page_token: None,
            new_start_page_token: Some("start".to_string()),
        })
    }

    fn list_folder_files(
        &self,
        _access_token: &str,
        _folder_id: &str,
        page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError> {
        let index = match page_token {
            Some("page-2") => 1,
            Some(_) => self.files.len(),
            None => 0,
        };
        Ok(DriveFilePage {
            files: self.files.get(index).cloned().into_iter().collect(),
            next_page_token: (index + 1 < self.files.len()).then(|| "page-2".to_string()),
        })
    }

    fn list_folders(
        &self,
        _access_token: &str,
        _query: Option<&str>,
        _page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError> {
        Ok(DriveFilePage {
            files: Vec::new(),
            next_page_token: None,
        })
    }

    fn fetch_file(
        &self,
        _access_token: &str,
        file_id: &str,
    ) -> Result<Option<DriveFileMeta>, DriveError> {
        Ok(self
            .files
            .iter()
            .find(|file| file.file_id == file_id)
            .cloned())
    }

    fn read_text(
        &self,
        _access_token: &str,
        _file: &DriveFileMeta,
    ) -> Result<Option<String>, DriveError> {
        Ok(None)
    }

    fn download_file(
        &self,
        _access_token: &str,
        file: &DriveFileMeta,
        _max_bytes: u64,
    ) -> Result<Vec<u8>, DriveError> {
        Ok(self
            .downloads
            .get(&file.file_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn drive_meta(file_id: &str, name: &str, version: &str) -> DriveFileMeta {
    DriveFileMeta {
        file_id: file_id.to_string(),
        name: name.to_string(),
        mime_type: "audio/wav".to_string(),
        modified_time: "2026-06-27T00:00:00Z".to_string(),
        version: Some(version.to_string()),
        parent_folder_ids: vec!["folder-audio".to_string()],
        web_view_link: Some(format!("https://drive.example/{file_id}")),
        trashed: false,
    }
}
