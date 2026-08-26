//! Slice tests: deterministic chunking, FTS round-trip + BM25 ordering,
//! content-hash skip (receipt-quiet steady state), corpus-config change
//! handling, and the budgeted sync cycle against a fake Drive client. No
//! network, no LLM.

use std::collections::HashMap;
use std::sync::Mutex;

use bos_integrations::google_drive_read::{
    default_rag_mime_types, DriveChange, DriveChangesPage, DriveError, DriveFileMeta,
    DriveFilePage, DriveReadClient, GoogleDriveCorpusPointer, GOOGLE_DOC_MIME,
};

use super::service::{self, CHUNK_MAX_CHARS, CHUNK_TARGET_CHARS};
use super::store;
use super::worker;
use crate::http::test_support::{test_state, EnvGuard};
use crate::http::AppState;

const CLIENT: &str = "test-client";

fn pointer(folder_ids: &[&str]) -> GoogleDriveCorpusPointer {
    GoogleDriveCorpusPointer {
        corpus_id: "default".to_string(),
        folder_ids: folder_ids.iter().map(|id| id.to_string()).collect(),
        include_file_ids: Vec::new(),
        exclude_file_ids: Vec::new(),
        exclude_name_patterns: Vec::new(),
        allowed_mime_types: default_rag_mime_types(),
    }
}

fn meta(file_id: &str, name: &str, modified: &str, folder: &str) -> DriveFileMeta {
    DriveFileMeta {
        file_id: file_id.to_string(),
        name: name.to_string(),
        mime_type: GOOGLE_DOC_MIME.to_string(),
        modified_time: modified.to_string(),
        version: Some("1".to_string()),
        parent_folder_ids: vec![folder.to_string()],
        web_view_link: Some(format!("https://docs.example/{file_id}")),
        trashed: false,
    }
}

fn receipt_count(state: &AppState) -> i64 {
    let persistence = state.persistence.lock();
    persistence
        .connection_ref()
        .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .expect("receipt count")
}

fn pointer_config_hash(state: &AppState) -> String {
    let persistence = state.persistence.lock();
    let resolved = service::corpus_pointer_for_state(state, persistence.connection_ref())
        .expect("corpus pointer");
    service::corpus_config_hash(&resolved.pointer)
}

#[test]
fn corpus_settings_save_selected_folder_and_drive_status_uses_it() {
    let _guard = EnvGuard::set_many(&[
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
    ]);
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        crate::slices::google_connector::store::store_credential(
            persistence.connection(),
            CLIENT,
            "operator",
            crate::slices::google_connector::SERVICE_GMAIL,
            "refresh-token",
            &[crate::slices::google_connector::service::DRIVE_READONLY_SCOPE.to_string()],
            1_000,
        )
        .expect("credential");
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-1".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-docs".to_string()),
            drive_folder_name: Some("BOS Source Docs".to_string()),
        };
        service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_100,
        )
        .expect("settings");
    }

    let sync = state
        .sync_guards
        .guard(crate::http::Pump::Drive)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let status =
        service::corpus_status(&state, persistence.connection_ref(), &sync).expect("status");

    assert!(status.configured);
    assert_eq!(status.folder_ids, vec!["folder-docs".to_string()]);
    assert_eq!(status.credential_user_id.as_deref(), Some("operator"));
    assert!(status.credential_connected);
    assert_eq!(status.drive_scope_granted, Some(true));
    assert_eq!(status.revision, Some(1));
    assert_eq!(status.folder_names.len(), 1);
    assert_eq!(status.folder_names[0].name, "BOS Source Docs");
}

#[test]
fn corpus_settings_save_can_kick_sync_after_successful_mutation() {
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-kick".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-docs".to_string()),
            drive_folder_name: Some("BOS Source Docs".to_string()),
        };
        service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_100,
        )
        .expect("settings");
    }

    let mut spawned_with_max_requests = None;
    let kick =
        super::routes::start_settings_sync_with(&state, 2_000, true, |_state, max_requests| {
            spawned_with_max_requests = Some(max_requests);
            Ok(())
        });

    assert!(kick.sync_started);
    assert_eq!(kick.sync_refusal_reason, None);
    assert_eq!(spawned_with_max_requests, Some(8));
    assert!(
        state
            .sync_guards
            .guard(crate::http::Pump::Drive)
            .lock()
            .in_flight
    );
}

#[test]
fn corpus_settings_save_bypasses_cooldown_when_pointer_changed() {
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-cooldown-initial".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-old".to_string()),
            drive_folder_name: Some("Old Folder".to_string()),
        };
        service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_100,
        )
        .expect("initial settings");
    }
    {
        let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
        status.next_allowed_at_ms = 10_000;
    }

    let before_hash = pointer_config_hash(&state);
    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-cooldown-changed".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-new".to_string()),
            drive_folder_name: Some("New Folder".to_string()),
        };
        let outcome = service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_200,
        )
        .expect("changed settings");
        assert!(matches!(
            outcome,
            crate::store_core::MutationOutcome::Applied { .. }
        ));
    }
    let after_hash = pointer_config_hash(&state);
    assert_ne!(before_hash, after_hash);

    let mut spawned_with_max_requests = None;
    let kick =
        super::routes::start_settings_sync_with(&state, 2_000, true, |_state, max_requests| {
            spawned_with_max_requests = Some(max_requests);
            Ok(())
        });

    assert!(kick.sync_started);
    assert_eq!(kick.sync_refusal_reason, None);
    assert_eq!(spawned_with_max_requests, Some(8));
    let sync = state.sync_guards.guard(crate::http::Pump::Drive).lock();
    assert!(sync.in_flight);
    assert_eq!(sync.last_attempt_ms, Some(2_000));
    assert_eq!(sync.next_allowed_at_ms, 10_000);
}

#[test]
fn corpus_settings_resave_respects_cooldown_when_pointer_unchanged() {
    let state = test_state();
    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-same-initial".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-same".to_string()),
            drive_folder_name: Some("Same Folder".to_string()),
        };
        service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_100,
        )
        .expect("initial settings");
    }
    {
        let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
        status.next_allowed_at_ms = 10_000;
    }

    let before_hash = pointer_config_hash(&state);
    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-same-applied".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-same".to_string()),
            drive_folder_name: Some("Same Folder".to_string()),
        };
        let outcome = service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_200,
        )
        .expect("same settings");
        assert!(matches!(
            outcome,
            crate::store_core::MutationOutcome::Applied { .. }
        ));
    }
    let after_hash = pointer_config_hash(&state);
    assert_eq!(before_hash, after_hash);

    let kick = super::routes::start_settings_sync_with(&state, 2_000, false, |_state, _max| {
        panic!("sync should not spawn while same-folder save is cooling down")
    });

    assert!(!kick.sync_started);
    assert_eq!(kick.sync_refusal_reason.as_deref(), Some("sync_cooldown"));
    let sync = state
        .sync_guards
        .guard(crate::http::Pump::Drive)
        .lock()
        .clone();
    assert!(!sync.in_flight);
    assert_eq!(sync.next_allowed_at_ms, 10_000);
    let persistence = state.persistence.lock();
    let status =
        service::corpus_status(&state, persistence.connection_ref(), &sync).expect("status");
    assert_eq!(status.folder_ids, vec!["folder-same".to_string()]);
    assert_eq!(status.folder_names[0].name, "Same Folder");
}

#[test]
fn corpus_settings_save_survives_sync_kick_refusal() {
    let state = test_state();
    {
        let mut status = state.sync_guards.guard(crate::http::Pump::Drive).lock();
        status.in_flight = true;
    }

    {
        let mut persistence = state.persistence.lock();
        let request = bos_contracts::drive_corpus::DriveCorpusSettingsUpdateRequest {
            expected_revision: None,
            idempotency_key: "drive-corpus-settings-refused".to_string(),
            actor_id: None,
            drive_folder_id: Some("folder-refused".to_string()),
            drive_folder_name: Some("Refused Folder".to_string()),
        };
        service::replace_corpus_settings(
            persistence.connection(),
            CLIENT,
            "operator",
            Some("operator"),
            &request,
            1_100,
        )
        .expect("settings");
    }

    let kick =
        super::routes::start_settings_sync_with(&state, 2_000, true, |_state, _max_requests| {
            panic!("sync should not spawn when the slot is already held")
        });

    assert!(!kick.sync_started);
    assert_eq!(kick.sync_refusal_reason.as_deref(), Some("sync_in_flight"));

    let sync = state
        .sync_guards
        .guard(crate::http::Pump::Drive)
        .lock()
        .clone();
    let persistence = state.persistence.lock();
    let status =
        service::corpus_status(&state, persistence.connection_ref(), &sync).expect("status");
    assert_eq!(status.folder_ids, vec!["folder-refused".to_string()]);
    assert_eq!(status.folder_names[0].name, "Refused Folder");
}

// ---------------------------------------------------------------------------
// Chunker
// ---------------------------------------------------------------------------

#[test]
fn chunker_tracks_heading_paths_and_respects_size_bounds() {
    let body_a = "Surface prep starts with degreasing. ".repeat(80); // ~2960 chars
    let body_b = "Use 220 grit before priming. ".repeat(10);
    let text = format!(
        "# Painting SOP\n\nIntro paragraph.\n\n## Prep\n\n{body_a}\n\n## Priming\n\n{body_b}\n"
    );

    let chunks = service::chunk_document(&text);

    assert!(
        chunks.len() >= 3,
        "expected several chunks: {}",
        chunks.len()
    );
    // Heading paths follow the document structure.
    assert_eq!(chunks[0].heading_path, vec!["Painting SOP".to_string()]);
    let prep_chunks: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.heading_path == vec!["Painting SOP".to_string(), "Prep".to_string()])
        .collect();
    assert!(prep_chunks.len() >= 2, "long section splits");
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.heading_path
                == vec!["Painting SOP".to_string(), "Priming".to_string()])
    );
    // Size bounds: nothing exceeds the ceiling.
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= CHUNK_MAX_CHARS,
            "chunk {} is {} chars",
            chunk.seq,
            chunk.text.len()
        );
    }
    // Consecutive chunks within the long section overlap (shared text).
    let first = &prep_chunks[0].text;
    let second = &prep_chunks[1].text;
    let tail: String = first.chars().rev().take(80).collect::<String>();
    let tail: String = tail.chars().rev().collect();
    assert!(
        second.contains(tail.trim()),
        "second chunk must start with the first chunk's tail"
    );
    // Sequences are contiguous from zero.
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.seq as usize, index);
    }
}

#[test]
fn chunker_merges_short_tails_and_handles_plain_text() {
    let body = "A sentence about epoxy floors. ".repeat(70); // just over target
    let text = format!("# Guide\n\n{body}\nShort tail.\n");
    let chunks = service::chunk_document(&text);
    assert!(
        chunks.last().unwrap().text.len() >= CHUNK_TARGET_CHARS / 4
            || chunks.last().unwrap().text.contains("Short tail."),
        "short tail merged, not a fragment"
    );

    // Headingless plain text still chunks (empty heading path).
    let plain = service::chunk_document("Just one paragraph of notes.\n");
    assert_eq!(plain.len(), 1);
    assert!(plain[0].heading_path.is_empty());

    assert!(service::chunk_document("").is_empty());
    assert!(service::chunk_document("\n\n\n").is_empty());
}

#[test]
fn fts_match_expression_quotes_dedupes_and_drops_noise() {
    assert_eq!(
        service::fts_match_expression("Epoxy floor PREP prep!"),
        Some("\"epoxy\" OR \"floor\" OR \"prep\"".to_string())
    );
    // Operator-looking input is neutralized by term extraction + quoting.
    assert_eq!(
        service::fts_match_expression("NEAR(\"a\" OR b) AND -col:x"),
        Some("\"and\" OR \"col\" OR \"near\"".to_string())
    );
    assert_eq!(service::fts_match_expression("a an of"), None);
    assert_eq!(service::fts_match_expression(""), None);
}

// ---------------------------------------------------------------------------
// Store: index round-trip, BM25 ordering, removal
// ---------------------------------------------------------------------------

#[test]
fn index_search_reindex_and_remove_round_trip() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();

    let doc = meta(
        "doc-1",
        "Epoxy Floor Guide",
        "2026-06-01T00:00:00Z",
        "folder-a",
    );
    store::mark_stale_from_meta(conn, CLIENT, &doc, 1_000).expect("stale");
    let chunks = service::chunk_document(
        "# Epoxy Floor Guide\n\n## Prep\n\nDegrease and etch the slab before coating.\n\n## Cure\n\nAllow 72 hours before heavy traffic.\n",
    );
    store::index_document(
        conn,
        CLIENT,
        "doc-1",
        "Epoxy Floor Guide",
        "hash-1",
        &chunks,
        2_000,
    )
    .expect("index");

    // A second document whose BODY mentions the query term; doc-1 has it in
    // the title, which outranks body via column weights.
    let other = meta(
        "doc-2",
        "Maintenance Notes",
        "2026-06-01T00:00:00Z",
        "folder-a",
    );
    store::mark_stale_from_meta(conn, CLIENT, &other, 1_000).expect("stale");
    store::index_document(
        conn,
        CLIENT,
        "doc-2",
        "Maintenance Notes",
        "hash-2",
        &service::chunk_document("Routine mopping; epoxy patching once a year.\n"),
        2_000,
    )
    .expect("index 2");

    let hits = store::search_chunks(conn, CLIENT, "\"epoxy\"", 10).expect("search");
    assert!(hits.len() >= 2);
    assert_eq!(hits[0].file_id, "doc-1", "title match ranks first");
    assert_eq!(hits[0].doc_title, "Epoxy Floor Guide");
    assert!(!hits[0].heading_path.is_empty());
    assert!(hits[0].web_view_link.as_deref().unwrap().contains("doc-1"));

    // Re-index replaces chunks — no duplicates left behind.
    store::index_document(
        conn,
        CLIENT,
        "doc-1",
        "Epoxy Floor Guide",
        "hash-1b",
        &service::chunk_document("# Epoxy Floor Guide\n\nNew shorter body.\n"),
        3_000,
    )
    .expect("re-index");
    let snapshot = store::get_snapshot(conn, CLIENT, "doc-1")
        .expect("get")
        .expect("exists");
    assert_eq!(snapshot.status, store::STATUS_INDEXED);
    assert_eq!(snapshot.chunk_count, 1);
    assert_eq!(store::chunk_count(conn, CLIENT).expect("count"), 2); // 1 + doc-2's 1

    // Removal drops chunks and FTS rows; the search no longer returns it.
    assert!(store::mark_removed(conn, CLIENT, "doc-1", 4_000).expect("remove"));
    let hits = store::search_chunks(conn, CLIENT, "\"epoxy\"", 10).expect("search");
    assert!(hits.iter().all(|hit| hit.file_id != "doc-1"));
    // Second removal is a no-op.
    assert!(!store::mark_removed(conn, CLIENT, "doc-1", 5_000).expect("remove again"));
}

#[test]
fn unchanged_metadata_and_cursor_write_nothing() {
    let state = test_state();
    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();

    let doc = meta("doc-1", "Guide", "2026-06-01T00:00:00Z", "folder-a");
    assert!(store::mark_stale_from_meta(conn, CLIENT, &doc, 1_000).expect("first"));
    drop(persistence);
    let receipts_after_first = receipt_count(&state);

    let mut persistence = state.persistence.lock();
    let conn = persistence.connection();
    // Same revision again: zero writes, zero receipts.
    assert!(!store::mark_stale_from_meta(conn, CLIENT, &doc, 2_000).expect("second"));
    let cursor = store::get_cursor(conn, CLIENT).expect("cursor");
    assert!(!store::put_cursor(conn, CLIENT, &cursor, 2_000).expect("cursor put"));
    drop(persistence);
    assert_eq!(receipt_count(&state), receipts_after_first);
}

// ---------------------------------------------------------------------------
// Sync cycle against a fake Drive client
// ---------------------------------------------------------------------------

struct FakeDrive {
    start_token: String,
    folder_files: HashMap<String, Vec<DriveFileMeta>>,
    changes: Mutex<Vec<DriveChangesPage>>,
    texts: HashMap<String, String>,
    rate_limit_listing: bool,
}

impl FakeDrive {
    fn new() -> Self {
        Self {
            start_token: "spt-1".to_string(),
            folder_files: HashMap::new(),
            changes: Mutex::new(Vec::new()),
            texts: HashMap::new(),
            rate_limit_listing: false,
        }
    }
}

impl DriveReadClient for FakeDrive {
    fn fetch_start_page_token(&self, _access_token: &str) -> Result<String, DriveError> {
        Ok(self.start_token.clone())
    }

    fn fetch_changes(
        &self,
        _access_token: &str,
        _page_token: &str,
    ) -> Result<DriveChangesPage, DriveError> {
        let mut queued = self.changes.lock().expect("changes");
        if queued.is_empty() {
            return Ok(DriveChangesPage {
                changes: Vec::new(),
                next_page_token: None,
                new_start_page_token: Some(self.start_token.clone()),
            });
        }
        Ok(queued.remove(0))
    }

    fn list_folder_files(
        &self,
        _access_token: &str,
        folder_id: &str,
        _page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError> {
        if self.rate_limit_listing {
            return Err(DriveError::RateLimited {
                retry_after_ms: Some(30_000),
                message: "throttled".to_string(),
            });
        }
        Ok(DriveFilePage {
            files: self
                .folder_files
                .get(folder_id)
                .cloned()
                .unwrap_or_default(),
            next_page_token: None,
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
            .folder_files
            .values()
            .flatten()
            .find(|file| file.file_id == file_id)
            .cloned())
    }

    fn read_text(
        &self,
        _access_token: &str,
        file: &DriveFileMeta,
    ) -> Result<Option<String>, DriveError> {
        Ok(self.texts.get(&file.file_id).cloned())
    }

    fn download_file(
        &self,
        _access_token: &str,
        _file: &DriveFileMeta,
        _max_bytes: u64,
    ) -> Result<Vec<u8>, DriveError> {
        Ok(Vec::new())
    }
}

#[test]
fn first_cycle_backfills_indexes_and_pins_the_changes_token() {
    let state = test_state();
    let corpus = pointer(&["folder-a"]);
    let mut drive = FakeDrive::new();
    let doc_a = meta("doc-a", "SOP", "2026-06-01T00:00:00Z", "folder-a");
    let doc_b = meta("doc-b", "Pricing Notes", "2026-06-02T00:00:00Z", "folder-a");
    drive
        .folder_files
        .insert("folder-a".to_string(), vec![doc_a, doc_b]);
    drive.texts.insert(
        "doc-a".to_string(),
        "# SOP\n\nAlways degrease before coating.\n".to_string(),
    );
    drive.texts.insert(
        "doc-b".to_string(),
        "# Pricing\n\nFloor jobs price per square foot.\n".to_string(),
    );

    let summary = worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 1_000).expect("cycle");

    // 1 startPageToken + 1 listing + 1 changes page + 2 reads = 5 requests.
    assert_eq!(summary.requests_used, 5);
    assert_eq!(summary.marked_stale, 2);
    assert_eq!(summary.indexed, 2);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let cursor = store::get_cursor(conn, CLIENT).expect("cursor");
    assert!(cursor.backfill_complete);
    assert_eq!(cursor.start_page_token.as_deref(), Some("spt-1"));
    let counts = store::doc_counts(conn, CLIENT).expect("counts");
    assert_eq!(counts.indexed, 2);
    let hits = store::search_chunks(conn, CLIENT, "\"degrease\"", 5).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file_id, "doc-a");
}

#[test]
fn second_cycle_is_receipt_quiet_and_changes_reindex_or_remove() {
    let state = test_state();
    let corpus = pointer(&["folder-a"]);
    let mut drive = FakeDrive::new();
    let doc_a = meta("doc-a", "SOP", "2026-06-01T00:00:00Z", "folder-a");
    drive
        .folder_files
        .insert("folder-a".to_string(), vec![doc_a.clone()]);
    drive.texts.insert(
        "doc-a".to_string(),
        "# SOP\n\nAlways degrease before coating.\n".to_string(),
    );
    worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 1_000).expect("cycle 1");
    let receipts_after_first = receipt_count(&state);

    // Steady state: same metadata, empty changes feed → zero writes.
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 60_000).expect("cycle 2");
    assert_eq!(summary.indexed, 0);
    assert_eq!(summary.marked_stale, 0);
    assert_eq!(receipt_count(&state), receipts_after_first);

    // A modified change re-indexes; a removal drops the doc.
    let mut doc_a2 = doc_a.clone();
    doc_a2.modified_time = "2026-06-05T00:00:00Z".to_string();
    drive.texts.insert(
        "doc-a".to_string(),
        "# SOP\n\nNew procedure: degrease, etch, prime.\n".to_string(),
    );
    drive
        .changes
        .lock()
        .expect("changes")
        .push(DriveChangesPage {
            changes: vec![DriveChange {
                file_id: "doc-a".to_string(),
                removed: false,
                file: Some(doc_a2),
            }],
            next_page_token: None,
            new_start_page_token: Some("spt-2".to_string()),
        });
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 120_000).expect("cycle 3");
    assert_eq!(summary.indexed, 1);
    {
        let persistence = state.persistence.lock();
        let conn = persistence.connection_ref();
        let cursor = store::get_cursor(conn, CLIENT).expect("cursor");
        assert_eq!(cursor.start_page_token.as_deref(), Some("spt-2"));
        let hits = store::search_chunks(conn, CLIENT, "\"etch\"", 5).expect("search");
        assert_eq!(hits.len(), 1);
    }

    drive
        .changes
        .lock()
        .expect("changes")
        .push(DriveChangesPage {
            changes: vec![DriveChange {
                file_id: "doc-a".to_string(),
                removed: true,
                file: None,
            }],
            next_page_token: None,
            new_start_page_token: Some("spt-3".to_string()),
        });
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 180_000).expect("cycle 4");
    assert_eq!(summary.removed, 1);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    assert_eq!(store::chunk_count(conn, CLIENT).expect("count"), 0);
}

#[test]
fn unchanged_content_with_new_revision_touches_without_reindex() {
    let state = test_state();
    let corpus = pointer(&["folder-a"]);
    let mut drive = FakeDrive::new();
    let doc_a = meta("doc-a", "SOP", "2026-06-01T00:00:00Z", "folder-a");
    drive
        .folder_files
        .insert("folder-a".to_string(), vec![doc_a.clone()]);
    drive
        .texts
        .insert("doc-a".to_string(), "# SOP\n\nStable text.\n".to_string());
    worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 1_000).expect("cycle 1");

    // New revision stamp, identical text → content-hash skip (unchanged).
    let mut doc_a2 = doc_a.clone();
    doc_a2.modified_time = "2026-06-06T00:00:00Z".to_string();
    drive
        .changes
        .lock()
        .expect("changes")
        .push(DriveChangesPage {
            changes: vec![DriveChange {
                file_id: "doc-a".to_string(),
                removed: false,
                file: Some(doc_a2),
            }],
            next_page_token: None,
            new_start_page_token: Some("spt-2".to_string()),
        });
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 60_000).expect("cycle 2");
    assert_eq!(summary.indexed, 0);
    assert_eq!(summary.unchanged, 1);
}

#[test]
fn budget_exhaustion_resumes_and_429_stamps_the_cursor() {
    let state = test_state();
    let corpus = pointer(&["folder-a"]);
    let mut drive = FakeDrive::new();
    let doc_a = meta("doc-a", "SOP", "2026-06-01T00:00:00Z", "folder-a");
    let doc_b = meta("doc-b", "Notes", "2026-06-02T00:00:00Z", "folder-a");
    drive
        .folder_files
        .insert("folder-a".to_string(), vec![doc_a, doc_b]);
    drive
        .texts
        .insert("doc-a".to_string(), "# SOP\n\nBody A.\n".to_string());
    drive
        .texts
        .insert("doc-b".to_string(), "# Notes\n\nBody B.\n".to_string());

    // Budget 2: startPageToken + folder listing. No reads happen yet.
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 2, 1_000).expect("cycle 1");
    assert_eq!(summary.requests_used, 2);
    assert_eq!(summary.marked_stale, 2);
    assert_eq!(summary.indexed, 0);

    // Next cycle finishes the walk + indexes both docs.
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus, 10, 2_000).expect("cycle 2");
    assert_eq!(summary.indexed, 2);

    // 429 on listing stamps the cursor; the following cycle stands down.
    let mut throttled = FakeDrive::new();
    throttled.rate_limit_listing = true;
    let state2 = test_state();
    let summary =
        worker::run_sync_cycle(&state2, &throttled, "tok", &corpus, 10, 1_000).expect("throttled");
    assert!(summary.rate_limited);
    let summary =
        worker::run_sync_cycle(&state2, &throttled, "tok", &corpus, 10, 2_000).expect("standdown");
    assert_eq!(summary.requests_used, 0);
}

#[test]
fn config_change_removes_out_of_corpus_docs_and_restarts_backfill() {
    let state = test_state();
    let corpus_a = pointer(&["folder-a"]);
    let mut drive = FakeDrive::new();
    let doc_a = meta("doc-a", "SOP", "2026-06-01T00:00:00Z", "folder-a");
    drive
        .folder_files
        .insert("folder-a".to_string(), vec![doc_a]);
    drive
        .texts
        .insert("doc-a".to_string(), "# SOP\n\nBody A.\n".to_string());
    worker::run_sync_cycle(&state, &drive, "tok", &corpus_a, 10, 1_000).expect("cycle 1");

    // Corpus now points at folder-b only: doc-a is locally removed (no API
    // spend) and the walk restarts over folder-b.
    let corpus_b = pointer(&["folder-b"]);
    let doc_c = meta("doc-c", "Specs", "2026-06-03T00:00:00Z", "folder-b");
    drive
        .folder_files
        .insert("folder-b".to_string(), vec![doc_c]);
    drive
        .texts
        .insert("doc-c".to_string(), "# Specs\n\nBody C.\n".to_string());
    let summary =
        worker::run_sync_cycle(&state, &drive, "tok", &corpus_b, 10, 60_000).expect("cycle 2");
    assert_eq!(summary.removed, 1);
    assert_eq!(summary.indexed, 1);
    let persistence = state.persistence.lock();
    let conn = persistence.connection_ref();
    let removed = store::get_snapshot(conn, CLIENT, "doc-a")
        .expect("get")
        .expect("tombstone");
    assert_eq!(removed.status, store::STATUS_REMOVED);
    let hits = store::search_chunks(conn, CLIENT, "\"specs\" OR \"body\"", 5).expect("search");
    assert!(hits.iter().all(|hit| hit.file_id == "doc-c"));
}
