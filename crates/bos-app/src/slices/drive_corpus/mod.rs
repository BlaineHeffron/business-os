//! Drive RAG corpus (port #5, slice 1): a local, queryable lexical index
//! over the configured Google Drive folders. A request-budgeted incremental
//! sync (changes-API cursor, content-hash skip) keeps document snapshots
//! fresh; deterministic heading-aware chunking writes chunk rows + an FTS5
//! (BM25) index through store_core in one transaction. Index-time spends
//! ZERO LLM tokens; the content_drafts vertical queries this index per
//! draft. The browser never touches Drive.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SLICE: SliceSpec = SliceSpec {
    id: "drive_corpus",
    title: "Drive RAG corpus",
    summary: "Incremental Google Drive readonly sync (request-budgeted, env-gated, changes-API cursor, content-hash skip) into a local FTS5 (BM25) chunk index with deterministic heading-aware chunking. Corpus folders resolve from env pin > operator settings > overlay defaults. Serves corpus status and lexical search; the content_drafts vertical retrieves evidence here.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/drive-corpus/status",
            summary: "Corpus config, credential/scope state, sync freshness, and index counts",
        },
        RouteSpec {
            method: "POST",
            path: "/api/drive-corpus/settings",
            summary: "Replace the operator-selected Drive folder feeding the RAG corpus",
        },
        RouteSpec {
            method: "POST",
            path: "/api/drive-corpus/sync",
            summary: "Kick one sync cycle (202; 409 while syncing/cooling down/unconfigured)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/drive-corpus/search",
            summary: "BM25 search over the local chunk index (?q=&limit=)",
        },
    ],
    tables: &[
        "drive_doc_snapshots",
        "drive_chunks",
        "drive_chunks_fts",
        "drive_sync_cursors",
        "drive_corpus_settings",
    ],
    env_vars: &[
        &env_registry::BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS,
        &env_registry::BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS,
        &env_registry::BOS_DRIVE_CORPUS_FOLDER_IDS,
        &env_registry::BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS,
        &env_registry::BOS_DRIVE_CORPUS_USER_ID,
        &env_registry::BOS_DRIVE_MAX_REQUESTS_PER_CYCLE,
        &env_registry::BOS_DRIVE_SYNC_ENABLED,
        &env_registry::BOS_DRIVE_SYNC_INTERVAL_SECS,
    ],
    read_models: &["drive_corpus_status", "drive_corpus_search"],
};
