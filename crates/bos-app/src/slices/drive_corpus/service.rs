//! Corpus configuration + the deterministic indexing pipeline: pointer
//! resolution (env pin > operator settings > overlay defaults for folders),
//! heading-aware chunking (zero LLM tokens at index time — the design's
//! whole point), content hashing, and safe FTS5 query assembly.

use bos_contracts::drive_corpus::{
    DriveCorpusFolderName, DriveCorpusSettingsUpdateRequest, DriveCorpusStatus,
};
use bos_integrations::google_drive_read::{
    default_rag_mime_types, GoogleDriveCorpusPointer, GOOGLE_DRIVE_READONLY_SCOPE,
};
use bos_integrations::google_oauth;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::store;
use crate::env_registry;
use crate::http::{AppState, SyncGuard};
use crate::overlay::DriveCorpusOverlay;
use crate::store_core::StoreError;

/// Chunk geometry, in characters (≈4 chars/token): target ≈475 tokens,
/// ceiling ≈575, overlap ≈15% — the empirical RAG defaults from the design
/// doc (400–512 tokens, 10–20% overlap).
pub const CHUNK_TARGET_CHARS: usize = 1_900;
pub const CHUNK_MAX_CHARS: usize = 2_300;
pub const CHUNK_OVERLAP_CHARS: usize = 280;
/// Tails smaller than this merge into the previous chunk of the section.
const CHUNK_MIN_TAIL_CHARS: usize = 200;

pub struct ResolvedCorpusPointer {
    pub pointer: GoogleDriveCorpusPointer,
    pub revision: Option<u64>,
    pub credential_user_id: Option<String>,
    pub folder_names: Vec<DriveCorpusFolderName>,
    pub folder_selection_pinned: bool,
}

/// Resolve the corpus pointer. Folder ids use BOS_DRIVE_CORPUS_FOLDER_IDS as
/// a deployment pin, then operator-selected settings, then overlay defaults.
/// Other BOS_DRIVE_CORPUS_* fields still override their overlay defaults.
pub fn corpus_pointer(overlay: Option<&DriveCorpusOverlay>) -> GoogleDriveCorpusPointer {
    corpus_pointer_with_settings(overlay, None)
}

fn corpus_pointer_with_settings(
    overlay: Option<&DriveCorpusOverlay>,
    settings: Option<&store::StoredCorpusSettings>,
) -> GoogleDriveCorpusPointer {
    let ids = |var: &env_registry::EnvVar, fallback: &[String]| -> Vec<String> {
        env_registry::string(var)
            .map(|raw| google_oauth::parse_scope_list(&raw))
            .unwrap_or_else(|| fallback.to_vec())
    };
    let empty: &[String] = &[];
    let stored_folders = settings
        .filter(|settings| !settings.folder_ids.is_empty())
        .map(|settings| settings.folder_ids.as_slice());
    let folder_fallback = stored_folders
        .or_else(|| overlay.map(|o| o.folder_ids.as_slice()))
        .unwrap_or(empty);
    let include_fallback = overlay
        .map(|o| o.include_file_ids.as_slice())
        .unwrap_or(empty);
    let exclude_fallback = overlay
        .map(|o| o.exclude_file_ids.as_slice())
        .unwrap_or(empty);
    // Patterns may contain spaces — comma-separated only.
    let exclude_name_patterns =
        env_registry::string(&env_registry::BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS)
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|pattern| !pattern.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| {
                overlay
                    .map(|o| o.exclude_name_patterns.clone())
                    .unwrap_or_default()
            });
    GoogleDriveCorpusPointer {
        corpus_id: store::DEFAULT_CORPUS_ID.to_string(),
        folder_ids: ids(&env_registry::BOS_DRIVE_CORPUS_FOLDER_IDS, folder_fallback),
        include_file_ids: ids(
            &env_registry::BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS,
            include_fallback,
        ),
        exclude_file_ids: ids(
            &env_registry::BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS,
            exclude_fallback,
        ),
        exclude_name_patterns,
        allowed_mime_types: default_rag_mime_types(),
    }
}

pub fn corpus_pointer_for_state(
    state: &AppState,
    conn: &Connection,
) -> Result<ResolvedCorpusPointer, StoreError> {
    let settings = store::get_corpus_settings(conn, &state.client_id)?;
    let pointer = corpus_pointer_with_settings(
        state.drive_corpus_overlay.as_ref().as_ref(),
        settings.as_ref(),
    );
    let folder_selection_pinned =
        env_registry::string(&env_registry::BOS_DRIVE_CORPUS_FOLDER_IDS).is_some();
    let folder_names = pointer
        .folder_ids
        .iter()
        .filter_map(|folder_id| {
            settings
                .as_ref()
                .and_then(|settings| settings.folder_names.get(folder_id))
                .map(|name| DriveCorpusFolderName {
                    folder_id: folder_id.clone(),
                    name: name.clone(),
                })
        })
        .collect();
    let credential_user_id =
        env_registry::string(&env_registry::BOS_DRIVE_CORPUS_USER_ID).or_else(|| {
            settings
                .as_ref()
                .and_then(|settings| settings.credential_user_id.clone())
        });
    Ok(ResolvedCorpusPointer {
        pointer,
        revision: settings.as_ref().and_then(|settings| settings.revision),
        credential_user_id,
        folder_names,
        folder_selection_pinned,
    })
}

pub fn replace_corpus_settings(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    credential_user_id: Option<&str>,
    request: &DriveCorpusSettingsUpdateRequest,
    now_ms: u64,
) -> Result<crate::store_core::MutationOutcome, StoreError> {
    store::replace_corpus_settings(
        conn,
        client_id,
        actor_id,
        credential_user_id,
        request,
        now_ms,
    )
}

/// Stable hash of the pointer config; a change resets the backfill walk.
pub fn corpus_config_hash(pointer: &GoogleDriveCorpusPointer) -> String {
    let mut hasher = Sha256::new();
    for list in [
        &pointer.folder_ids,
        &pointer.include_file_ids,
        &pointer.exclude_file_ids,
        &pointer.exclude_name_patterns,
        &pointer.allowed_mime_types,
    ] {
        for entry in list {
            hasher.update(entry.as_bytes());
            hasher.update([0u8]);
        }
        hasher.update([1u8]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Deterministic heading-aware chunker. Markdown headings (`#`–`######`)
/// maintain a heading-path stack; sections never share a chunk. Within a
/// section, lines accumulate to CHUNK_TARGET_CHARS and successive chunks
/// overlap by ~CHUNK_OVERLAP_CHARS of trailing lines; a short section tail
/// merges into the previous chunk instead of becoming a fragment.
pub fn chunk_document(text: &str) -> Vec<store::ChunkRow> {
    struct PendingLine {
        start: usize,
        end: usize,
        text: String,
    }

    let mut chunks: Vec<store::ChunkRow> = Vec::new();
    let mut heading_path: Vec<String> = Vec::new();
    let mut buffer: Vec<PendingLine> = Vec::new();
    let mut buffer_chars = 0usize;
    // Chunks already emitted for the CURRENT section (controls tail merging).
    let mut section_chunks = 0usize;
    // The buffer holds only overlap seed lines (already emitted text); a
    // section ending now must not re-emit them as a duplicate tail chunk.
    let mut buffer_is_overlap_only = false;
    let mut offset = 0usize;

    fn emit(
        chunks: &mut Vec<store::ChunkRow>,
        heading_path: &[String],
        buffer: &mut Vec<PendingLine>,
        buffer_chars: &mut usize,
        section_chunks: &mut usize,
        overlap: bool,
    ) {
        // Trim blank lines at both ends.
        while buffer.first().is_some_and(|line| line.text.is_empty()) {
            buffer.remove(0);
        }
        while buffer.last().is_some_and(|line| line.text.is_empty()) {
            buffer.pop();
        }
        if buffer.is_empty() {
            *buffer_chars = 0;
            return;
        }
        let body = buffer
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let start = buffer.first().map(|line| line.start).unwrap_or(0);
        let end = buffer.last().map(|line| line.end).unwrap_or(start);
        let is_tail = !overlap;
        if is_tail && *section_chunks > 0 && body.len() < CHUNK_MIN_TAIL_CHARS {
            // Merge the short tail into the section's previous chunk.
            if let Some(previous) = chunks.last_mut() {
                if previous.text.len() + 1 + body.len() <= CHUNK_MAX_CHARS {
                    previous.text.push('\n');
                    previous.text.push_str(&body);
                    previous.end_offset = end.min(u32::MAX as usize) as u32;
                    buffer.clear();
                    *buffer_chars = 0;
                    return;
                }
            }
        }
        chunks.push(store::ChunkRow {
            seq: chunks.len() as u32,
            heading_path: heading_path.to_vec(),
            start_offset: start.min(u32::MAX as usize) as u32,
            end_offset: end.min(u32::MAX as usize) as u32,
            text: body,
        });
        *section_chunks += 1;
        if overlap {
            // Seed the next chunk with the trailing lines (≤ OVERLAP chars).
            let mut kept: Vec<PendingLine> = Vec::new();
            let mut kept_chars = 0usize;
            while let Some(line) = buffer.pop() {
                if line.text.is_empty() {
                    continue;
                }
                if kept_chars + line.text.len() > CHUNK_OVERLAP_CHARS && !kept.is_empty() {
                    break;
                }
                kept_chars += line.text.len();
                kept.insert(0, line);
                if kept_chars >= CHUNK_OVERLAP_CHARS {
                    break;
                }
            }
            *buffer_chars = kept.iter().map(|line| line.text.len()).sum();
            *buffer = kept;
        } else {
            buffer.clear();
            *buffer_chars = 0;
        }
    }

    for block in text.split_inclusive('\n') {
        let line = block.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        let is_heading =
            (1..=6).contains(&hashes) && trimmed.chars().nth(hashes).is_none_or(|next| next == ' ');
        if is_heading {
            let title = trimmed[hashes..].trim();
            // Section boundary: flush whatever accumulated (tail, no overlap).
            if buffer_is_overlap_only {
                buffer.clear();
                buffer_chars = 0;
            }
            emit(
                &mut chunks,
                &heading_path,
                &mut buffer,
                &mut buffer_chars,
                &mut section_chunks,
                false,
            );
            heading_path.truncate(hashes - 1);
            if !title.is_empty() {
                while heading_path.len() < hashes - 1 {
                    // Skipped levels (## directly under document root) keep
                    // the path aligned with the heading depth.
                    heading_path.push(String::new());
                }
                heading_path.push(title.to_string());
            }
            section_chunks = 0;
            offset += block.len();
            continue;
        }
        // Exported Google Docs render a whole paragraph as ONE markdown
        // line — split long lines at word boundaries so a single paragraph
        // can never blow past the chunk ceiling.
        for (piece_start, piece) in split_long_line(trimmed) {
            if piece.is_empty() && buffer.is_empty() {
                continue;
            }
            if !piece.is_empty() {
                buffer_is_overlap_only = false;
            }
            buffer_chars += piece.len();
            buffer.push(PendingLine {
                start: offset + piece_start,
                end: offset + piece_start + piece.len(),
                text: piece.to_string(),
            });
            if buffer_chars >= CHUNK_TARGET_CHARS && !buffer_is_overlap_only {
                emit(
                    &mut chunks,
                    &heading_path,
                    &mut buffer,
                    &mut buffer_chars,
                    &mut section_chunks,
                    true,
                );
                buffer_is_overlap_only = true;
            }
        }
        offset += block.len();
    }
    if buffer_is_overlap_only {
        buffer.clear();
        buffer_chars = 0;
    }
    emit(
        &mut chunks,
        &heading_path,
        &mut buffer,
        &mut buffer_chars,
        &mut section_chunks,
        false,
    );
    // Overlap seeding can leave duplicate-ish sequences; re-number to be safe.
    for (index, chunk) in chunks.iter_mut().enumerate() {
        chunk.seq = index as u32;
    }
    chunks
}

/// Split one physical line into word-boundary pieces (sentence ends
/// preferred) small enough that the accumulator above can never overshoot:
/// the target check runs after each piece, so the worst chunk is
/// TARGET + PIECE_MAX (+ joins) < CHUNK_MAX_CHARS. Also bounds the overlap
/// seed, whose smallest unit is one piece. Short lines come back whole.
fn split_long_line(line: &str) -> Vec<(usize, &str)> {
    const PIECE_MAX: usize = 380;
    if line.len() <= PIECE_MAX {
        return vec![(0, line)];
    }
    let mut pieces = Vec::new();
    let mut start = 0usize;
    while line.len() - start > PIECE_MAX {
        let mut window_end = start + PIECE_MAX;
        while !line.is_char_boundary(window_end) {
            window_end -= 1;
        }
        let window = &line[start..window_end];
        let cut = window
            .rfind(". ")
            .map(|index| index + 1) // the period stays with the left piece
            .or_else(|| window.rfind(' '))
            .filter(|index| *index > 0)
            .map(|index| start + index)
            .unwrap_or(window_end);
        pieces.push((start, line[start..cut].trim_end()));
        start = cut;
        while line[start..].starts_with(' ') {
            start += 1;
        }
    }
    pieces.push((start, &line[start..]));
    pieces
}

/// Build a safe FTS5 MATCH expression from free text: alphanumeric terms
/// longer than 2 chars, deduped, each quoted, OR-joined (any-term match,
/// BM25 ranks). `None` = nothing searchable in the input.
pub fn fts_match_expression(query: &str) -> Option<String> {
    let mut terms: Vec<String> = query
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 2)
        .map(str::to_ascii_lowercase)
        .collect();
    terms.sort_unstable();
    terms.dedup();
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Status projection for the operator surface: config + credential + cursor
/// + index counts in one read.
pub fn corpus_status(
    state: &AppState,
    conn: &Connection,
    sync: &SyncGuard,
) -> Result<DriveCorpusStatus, StoreError> {
    let resolved = corpus_pointer_for_state(state, conn)?;
    let oauth = crate::slices::google_connector::service::resolve_google_oauth(
        conn,
        &state.client_id,
        resolved.credential_user_id.as_deref(),
    )?;
    let drive_scope_granted = oauth.as_ref().map(|config| {
        if config.scopes.is_empty() {
            None
        } else {
            Some(google_oauth::has_scope(config, GOOGLE_DRIVE_READONLY_SCOPE))
        }
    });
    let cursor = store::get_cursor(conn, &state.client_id)?;
    Ok(DriveCorpusStatus {
        configured: resolved.pointer.is_configured(),
        revision: resolved.revision,
        credential_user_id: resolved.credential_user_id,
        folder_ids: resolved.pointer.folder_ids,
        folder_names: resolved.folder_names,
        include_file_ids: resolved.pointer.include_file_ids,
        folder_selection_pinned: resolved.folder_selection_pinned,
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &env_registry::BOS_DRIVE_SYNC_ENABLED,
        )?,
        credential_connected: oauth.is_some(),
        drive_scope_granted: drive_scope_granted.flatten(),
        in_flight: sync.in_flight,
        backfill_complete: cursor.backfill_complete,
        doc_counts: store::doc_counts(conn, &state.client_id)?,
        chunk_count: store::chunk_count(conn, &state.client_id)?,
        last_attempt_ms: sync.last_attempt_ms,
        last_outcome: sync.last_outcome.clone(),
        last_error: cursor.last_error,
        next_sync_allowed_at_ms: sync.next_allowed_at_ms,
    })
}
