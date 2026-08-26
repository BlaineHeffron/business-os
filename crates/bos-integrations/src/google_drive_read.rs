//! Google Drive read-only connector: changes-API incremental listing, folder
//! walks, and text extraction for the drive_corpus index. Config-driven — the
//! caller passes the access token per call (resolved from the per-user OAuth
//! credential by the slice); this module never reads env vars.
//!
//! Scope posture: everything here works under drive.readonly. Google Docs
//! export as text/markdown (headings survive, which the heading-aware chunker
//! depends on); plain text/markdown files download via alt=media; any other
//! mime returns `None` from [`DriveReadClient::read_text`] and the caller
//! records the document as skipped.
//!
//! Corpus filter harvested from agent-monitor-rust's
//! GoogleDriveRagCorpusPointer: a file belongs to the corpus when a direct
//! parent is a configured folder (or its id is explicitly included), minus
//! exclusions by id, name pattern, and mime allowlist.

use crate::google_api_errors;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;

pub const GOOGLE_DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";

const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const DRIVE_CHANGES_URL: &str = "https://www.googleapis.com/drive/v3/changes";
pub const GOOGLE_DOC_MIME: &str = "application/vnd.google-apps.document";
pub const GOOGLE_FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Markdown keeps heading structure; text/plain export flattens it.
const GOOGLE_DOC_EXPORT_MIME: &str = "text/markdown";
pub const DRIVE_MAX_PAGE_SIZE: u32 = 100;
/// Refuse to pull pathological documents into memory/the index.
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

const DRIVE_HTTP_TIMEOUT_SECS: u64 = 30;
const DRIVE_HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

const FILE_FIELDS: &str = "id,name,mimeType,modifiedTime,version,parents,webViewLink,trashed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveError {
    /// 429: honor Retry-After; the caller stands the whole cycle down.
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    /// 401/403: token invalid or scope missing — an operator must reconnect.
    AuthRejected {
        message: String,
    },
    Permanent {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthRejected { message } => write!(formatter, "auth_rejected: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFileMeta {
    pub file_id: String,
    pub name: String,
    pub mime_type: String,
    pub modified_time: String,
    pub version: Option<String>,
    pub parent_folder_ids: Vec<String>,
    pub web_view_link: Option<String>,
    pub trashed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFilePage {
    pub files: Vec<DriveFileMeta>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveChange {
    pub file_id: String,
    pub removed: bool,
    pub file: Option<DriveFileMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveChangesPage {
    pub changes: Vec<DriveChange>,
    pub next_page_token: Option<String>,
    pub new_start_page_token: Option<String>,
}

/// Which Drive documents belong to the corpus. Values come from the client
/// overlay / env registry on the bos-app side; this is pure filter data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleDriveCorpusPointer {
    pub corpus_id: String,
    /// Direct-parent allowlist: a file is in scope when ANY parent is listed.
    pub folder_ids: Vec<String>,
    pub include_file_ids: Vec<String>,
    pub exclude_file_ids: Vec<String>,
    /// Case-insensitive name patterns; `*` matches any run of characters.
    pub exclude_name_patterns: Vec<String>,
    /// Empty = allow every mime the reader supports.
    pub allowed_mime_types: Vec<String>,
}

impl GoogleDriveCorpusPointer {
    pub fn is_configured(&self) -> bool {
        !self.folder_ids.is_empty() || !self.include_file_ids.is_empty()
    }
}

/// Mimes the corpus indexes by default — the set `read_text` can actually
/// turn into heading-aware text. PDFs and spreadsheets are deliberately out
/// until a deterministic text path exists for them.
pub fn default_rag_mime_types() -> Vec<String> {
    vec![
        GOOGLE_DOC_MIME.to_string(),
        "text/plain".to_string(),
        "text/markdown".to_string(),
    ]
}

/// Harvested allow/exclude rules: exclusions first (id, name pattern, mime),
/// then explicit include ids, then direct-parent folder membership.
pub fn document_allowed_for_corpus(
    file: &DriveFileMeta,
    corpus: &GoogleDriveCorpusPointer,
) -> bool {
    if file.trashed {
        return false;
    }
    if corpus
        .exclude_file_ids
        .iter()
        .any(|file_id| file_id == &file.file_id)
    {
        return false;
    }
    if corpus
        .exclude_name_patterns
        .iter()
        .any(|pattern| name_pattern_matches(pattern, &file.name))
    {
        return false;
    }
    if !corpus.allowed_mime_types.is_empty()
        && !corpus
            .allowed_mime_types
            .iter()
            .any(|mime| mime == &file.mime_type)
    {
        return false;
    }
    if corpus
        .include_file_ids
        .iter()
        .any(|file_id| file_id == &file.file_id)
    {
        return true;
    }
    let allowed: BTreeSet<&String> = corpus.folder_ids.iter().collect();
    file.parent_folder_ids
        .iter()
        .any(|parent| allowed.contains(parent))
}

/// Case-insensitive match with `*` as "any run of characters". A pattern
/// without `*` must match the whole name.
fn name_pattern_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.len() == 1 {
        return pattern == name;
    }
    let mut position = 0usize;
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        match name[position..].find(segment) {
            Some(found) => {
                // An anchored first segment must match at the start.
                if index == 0 && found != 0 {
                    return false;
                }
                position += found + segment.len();
            }
            None => return false,
        }
    }
    // An anchored last segment must match at the end.
    segments
        .last()
        .map(|last| last.is_empty() || name.ends_with(last))
        .unwrap_or(true)
}

/// Minimal HTTP surface for Drive calls; mocked in tests.
pub trait DriveHttp: Send + Sync {
    fn get_json(&self, url: &str, access_token: &str) -> Result<Value, DriveError>;
    fn get_text(&self, url: &str, access_token: &str) -> Result<String, DriveError>;
    fn get_bytes(&self, url: &str, access_token: &str) -> Result<Vec<u8>, DriveError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestDriveHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestDriveHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(DRIVE_HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(DRIVE_HTTP_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl ReqwestDriveHttpClient {
    fn send(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<reqwest::blocking::Response, DriveError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .map_err(|err| DriveError::Permanent {
                code: "drive_http_send_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        if status >= 400 {
            let retry_after_ms = google_api_errors::retry_after_ms(response.headers());
            let body = response.json::<Value>().unwrap_or(Value::Null);
            if (status == 403 && google_api_errors::has_retryable_quota_reason(&body))
                || status == 429
            {
                return Err(DriveError::RateLimited {
                    retry_after_ms,
                    message: drive_status_message(status, &body),
                });
            }
            if status == 401 || status == 403 {
                return Err(DriveError::AuthRejected {
                    message: drive_status_message(status, &body),
                });
            }
            return Err(DriveError::Permanent {
                code: format!("drive_http_status_{status}"),
                message: format!("{} for {url}", drive_status_message(status, &body)),
            });
        }
        Ok(response)
    }
}

fn drive_status_message(status: u16, body: &Value) -> String {
    let reason = google_api_errors::first_error_reason(body)
        .map(|reason| format!(" reason={reason}"))
        .unwrap_or_default();
    let message = google_api_errors::error_message(body)
        .map(|message| format!(" message={message}"))
        .unwrap_or_default();
    format!("drive returned {status}{reason}{message}")
}

impl DriveHttp for ReqwestDriveHttpClient {
    fn get_json(&self, url: &str, access_token: &str) -> Result<Value, DriveError> {
        self.send(url, access_token)?
            .json::<Value>()
            .map_err(|err| DriveError::Permanent {
                code: "drive_http_parse_failed".to_string(),
                message: err.to_string(),
            })
    }

    fn get_text(&self, url: &str, access_token: &str) -> Result<String, DriveError> {
        self.send(url, access_token)?
            .text()
            .map_err(|err| DriveError::Permanent {
                code: "drive_http_body_failed".to_string(),
                message: err.to_string(),
            })
    }

    fn get_bytes(&self, url: &str, access_token: &str) -> Result<Vec<u8>, DriveError> {
        self.send(url, access_token)?
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|err| DriveError::Permanent {
                code: "drive_http_body_failed".to_string(),
                message: err.to_string(),
            })
    }
}

/// What the drive_corpus sync pump consumes; the fake in slice tests
/// implements this directly.
pub trait DriveReadClient: Send + Sync {
    fn fetch_start_page_token(&self, access_token: &str) -> Result<String, DriveError>;
    fn fetch_changes(
        &self,
        access_token: &str,
        page_token: &str,
    ) -> Result<DriveChangesPage, DriveError>;
    fn list_folder_files(
        &self,
        access_token: &str,
        folder_id: &str,
        page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError>;
    fn list_folders(
        &self,
        access_token: &str,
        query: Option<&str>,
        page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError>;
    /// `None` = the file is gone (404) — callers mark it removed.
    fn fetch_file(
        &self,
        access_token: &str,
        file_id: &str,
    ) -> Result<Option<DriveFileMeta>, DriveError>;
    /// `None` = no supported text representation for the file's mime.
    fn read_text(
        &self,
        access_token: &str,
        file: &DriveFileMeta,
    ) -> Result<Option<String>, DriveError>;
    fn download_file(
        &self,
        access_token: &str,
        file: &DriveFileMeta,
        max_bytes: u64,
    ) -> Result<Vec<u8>, DriveError>;
}

pub struct LiveDriveReadClient<H: DriveHttp> {
    http: H,
}

impl<H: DriveHttp> LiveDriveReadClient<H> {
    pub fn new(http: H) -> Self {
        Self { http }
    }
}

fn encode(raw: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    utf8_percent_encode(raw, NON_ALPHANUMERIC).to_string()
}

impl<H: DriveHttp> DriveReadClient for LiveDriveReadClient<H> {
    fn fetch_start_page_token(&self, access_token: &str) -> Result<String, DriveError> {
        let url = format!("{DRIVE_CHANGES_URL}/startPageToken?supportsAllDrives=true");
        let body = self.http.get_json(&url, access_token)?;
        body.get("startPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| DriveError::Permanent {
                code: "drive_start_page_token_missing".to_string(),
                message: "startPageToken absent in response".to_string(),
            })
    }

    fn fetch_changes(
        &self,
        access_token: &str,
        page_token: &str,
    ) -> Result<DriveChangesPage, DriveError> {
        let url = format!(
            "{DRIVE_CHANGES_URL}?pageToken={}&pageSize={DRIVE_MAX_PAGE_SIZE}\
             &includeRemoved=true&supportsAllDrives=true&includeItemsFromAllDrives=true\
             &fields={}",
            encode(page_token),
            encode(&format!(
                "nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))"
            )),
        );
        let body = self.http.get_json(&url, access_token)?;
        Ok(parse_changes_page(&body))
    }

    fn list_folder_files(
        &self,
        access_token: &str,
        folder_id: &str,
        page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError> {
        let query = format!("'{folder_id}' in parents and trashed = false");
        let mut url = format!(
            "{DRIVE_FILES_URL}?q={}&pageSize={DRIVE_MAX_PAGE_SIZE}\
             &supportsAllDrives=true&includeItemsFromAllDrives=true&fields={}",
            encode(&query),
            encode(&format!("nextPageToken,files({FILE_FIELDS})")),
        );
        if let Some(token) = page_token {
            url.push_str(&format!("&pageToken={}", encode(token)));
        }
        let body = self.http.get_json(&url, access_token)?;
        let files = body
            .get("files")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_file_meta).collect())
            .unwrap_or_default();
        Ok(DriveFilePage {
            files,
            next_page_token: non_empty_str(&body, "nextPageToken"),
        })
    }

    fn list_folders(
        &self,
        access_token: &str,
        query: Option<&str>,
        page_token: Option<&str>,
    ) -> Result<DriveFilePage, DriveError> {
        let escaped = query
            .unwrap_or("")
            .trim()
            .replace('\\', "\\\\")
            .replace('\'', "\\'");
        let mut clauses = vec![
            format!("mimeType = '{GOOGLE_FOLDER_MIME}'"),
            "trashed = false".to_string(),
        ];
        if !escaped.is_empty() {
            clauses.push(format!("name contains '{escaped}'"));
        }
        let query = clauses.join(" and ");
        let mut url = format!(
            "{DRIVE_FILES_URL}?q={}&pageSize=50\
             &supportsAllDrives=true&includeItemsFromAllDrives=true&fields={}",
            encode(&query),
            encode(&format!("nextPageToken,files({FILE_FIELDS})")),
        );
        if let Some(token) = page_token {
            url.push_str(&format!("&pageToken={}", encode(token)));
        }
        let body = self.http.get_json(&url, access_token)?;
        let files = body
            .get("files")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_file_meta).collect())
            .unwrap_or_default();
        Ok(DriveFilePage {
            files,
            next_page_token: non_empty_str(&body, "nextPageToken"),
        })
    }

    fn fetch_file(
        &self,
        access_token: &str,
        file_id: &str,
    ) -> Result<Option<DriveFileMeta>, DriveError> {
        let url = format!(
            "{DRIVE_FILES_URL}/{}?supportsAllDrives=true&fields={}",
            encode(file_id),
            encode(FILE_FIELDS),
        );
        match self.http.get_json(&url, access_token) {
            Ok(body) => Ok(parse_file_meta(&body)),
            Err(DriveError::Permanent { code, .. }) if code == "drive_http_status_404" => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn read_text(
        &self,
        access_token: &str,
        file: &DriveFileMeta,
    ) -> Result<Option<String>, DriveError> {
        let url = match file.mime_type.as_str() {
            GOOGLE_DOC_MIME => format!(
                "{DRIVE_FILES_URL}/{}/export?mimeType={}",
                encode(&file.file_id),
                encode(GOOGLE_DOC_EXPORT_MIME),
            ),
            "text/plain" | "text/markdown" => format!(
                "{DRIVE_FILES_URL}/{}?alt=media&supportsAllDrives=true",
                encode(&file.file_id),
            ),
            _ => return Ok(None),
        };
        let text = self.http.get_text(&url, access_token)?;
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(DriveError::Permanent {
                code: "drive_document_too_large".to_string(),
                message: format!(
                    "{} is {} bytes (cap {MAX_DOCUMENT_BYTES})",
                    file.file_id,
                    text.len()
                ),
            });
        }
        Ok(Some(text))
    }

    fn download_file(
        &self,
        access_token: &str,
        file: &DriveFileMeta,
        max_bytes: u64,
    ) -> Result<Vec<u8>, DriveError> {
        let url = format!(
            "{DRIVE_FILES_URL}/{}?alt=media&supportsAllDrives=true",
            encode(&file.file_id),
        );
        let bytes = self.http.get_bytes(&url, access_token)?;
        if bytes.len() as u64 > max_bytes {
            return Err(DriveError::Permanent {
                code: "drive_file_too_large".to_string(),
                message: format!(
                    "{} is {} bytes (cap {max_bytes})",
                    file.file_id,
                    bytes.len()
                ),
            });
        }
        Ok(bytes)
    }
}

fn parse_changes_page(body: &Value) -> DriveChangesPage {
    let changes = body
        .get("changes")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let file = entry.get("file").and_then(parse_file_meta_opt);
                    let file_id = non_empty_str(entry, "fileId")
                        .or_else(|| file.as_ref().map(|meta| meta.file_id.clone()))?;
                    Some(DriveChange {
                        file_id,
                        removed: entry
                            .get("removed")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        file,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    DriveChangesPage {
        changes,
        next_page_token: non_empty_str(body, "nextPageToken"),
        new_start_page_token: non_empty_str(body, "newStartPageToken"),
    }
}

fn parse_file_meta_opt(value: &Value) -> Option<DriveFileMeta> {
    parse_file_meta(value)
}

fn parse_file_meta(value: &Value) -> Option<DriveFileMeta> {
    let file_id = non_empty_str(value, "id")?;
    Some(DriveFileMeta {
        file_id,
        name: non_empty_str(value, "name").unwrap_or_else(|| "(unnamed)".to_string()),
        mime_type: non_empty_str(value, "mimeType").unwrap_or_default(),
        modified_time: non_empty_str(value, "modifiedTime").unwrap_or_default(),
        version: non_empty_str(value, "version"),
        parent_folder_ids: value
            .get("parents")
            .and_then(Value::as_array)
            .map(|parents| {
                parents
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        web_view_link: non_empty_str(value, "webViewLink"),
        trashed: value
            .get("trashed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn non_empty_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn corpus(folder_ids: &[&str]) -> GoogleDriveCorpusPointer {
        GoogleDriveCorpusPointer {
            corpus_id: "default".to_string(),
            folder_ids: folder_ids.iter().map(|id| id.to_string()).collect(),
            include_file_ids: Vec::new(),
            exclude_file_ids: Vec::new(),
            exclude_name_patterns: Vec::new(),
            allowed_mime_types: default_rag_mime_types(),
        }
    }

    fn file(file_id: &str, name: &str, mime: &str, parents: &[&str]) -> DriveFileMeta {
        DriveFileMeta {
            file_id: file_id.to_string(),
            name: name.to_string(),
            mime_type: mime.to_string(),
            modified_time: "2026-06-10T00:00:00Z".to_string(),
            version: Some("3".to_string()),
            parent_folder_ids: parents.iter().map(|id| id.to_string()).collect(),
            web_view_link: None,
            trashed: false,
        }
    }

    #[test]
    fn corpus_filter_applies_parent_include_exclude_and_mime_rules() {
        let mut pointer = corpus(&["folder-a"]);
        pointer.include_file_ids.push("solo-file".to_string());
        pointer.exclude_file_ids.push("banned".to_string());
        pointer.exclude_name_patterns.push("*draft*".to_string());

        let in_folder = file("f1", "SOP Painting", GOOGLE_DOC_MIME, &["folder-a"]);
        assert!(document_allowed_for_corpus(&in_folder, &pointer));

        let outside = file("f2", "SOP", GOOGLE_DOC_MIME, &["folder-z"]);
        assert!(!document_allowed_for_corpus(&outside, &pointer));

        let included_anywhere = file("solo-file", "Notes", "text/plain", &["folder-z"]);
        assert!(document_allowed_for_corpus(&included_anywhere, &pointer));

        let excluded_id = file("banned", "SOP", GOOGLE_DOC_MIME, &["folder-a"]);
        assert!(!document_allowed_for_corpus(&excluded_id, &pointer));

        let excluded_name = file("f3", "My Draft Doc", GOOGLE_DOC_MIME, &["folder-a"]);
        assert!(!document_allowed_for_corpus(&excluded_name, &pointer));

        let wrong_mime = file("f4", "Sheet", "application/pdf", &["folder-a"]);
        assert!(!document_allowed_for_corpus(&wrong_mime, &pointer));

        let mut trashed = file("f5", "SOP", GOOGLE_DOC_MIME, &["folder-a"]);
        trashed.trashed = true;
        assert!(!document_allowed_for_corpus(&trashed, &pointer));
    }

    #[test]
    fn name_patterns_anchor_without_stars_and_float_with_them() {
        assert!(name_pattern_matches("*draft*", "Q3 DRAFT plan"));
        assert!(name_pattern_matches("archive*", "Archive 2024"));
        assert!(!name_pattern_matches("archive*", "old archive"));
        assert!(name_pattern_matches("*.tmp", "scratch.TMP"));
        assert!(!name_pattern_matches("*.tmp", "scratch.tmp.txt"));
        assert!(name_pattern_matches("readme", "README"));
        assert!(!name_pattern_matches("readme", "readme.md"));
    }

    struct FakeHttp {
        responses: Mutex<Vec<(String, Result<Value, DriveError>)>>,
    }

    impl DriveHttp for FakeHttp {
        fn get_json(&self, url: &str, _access_token: &str) -> Result<Value, DriveError> {
            let mut responses = self.responses.lock().expect("responses");
            let (expected, response) = responses.remove(0);
            assert!(
                url.contains(&expected),
                "url {url} missing expected fragment {expected}"
            );
            response
        }

        fn get_text(&self, _url: &str, _access_token: &str) -> Result<String, DriveError> {
            unreachable!("json-only fake")
        }

        fn get_bytes(&self, _url: &str, _access_token: &str) -> Result<Vec<u8>, DriveError> {
            unreachable!("json-only fake")
        }
    }

    #[test]
    fn changes_page_parses_files_removals_and_tokens() {
        let http = FakeHttp {
            responses: Mutex::new(vec![(
                "changes?pageToken=tok%2D1".to_string(),
                Ok(json!({
                    "changes": [
                        {"fileId": "gone", "removed": true},
                        {"fileId": "doc-1", "removed": false, "file": {
                            "id": "doc-1", "name": "SOP", "mimeType": GOOGLE_DOC_MIME,
                            "modifiedTime": "2026-06-09T12:00:00Z", "version": "7",
                            "parents": ["folder-a"], "webViewLink": "https://docs.example/doc-1",
                            "trashed": false
                        }}
                    ],
                    "newStartPageToken": "tok-2"
                })),
            )]),
        };
        let client = LiveDriveReadClient::new(http);

        let page = client.fetch_changes("token", "tok-1").expect("changes");

        assert_eq!(page.changes.len(), 2);
        assert!(page.changes[0].removed);
        assert_eq!(
            page.changes[1].file.as_ref().unwrap().version.as_deref(),
            Some("7")
        );
        assert_eq!(page.new_start_page_token.as_deref(), Some("tok-2"));
        assert_eq!(page.next_page_token, None);
    }

    #[test]
    fn folder_listing_builds_query_and_parses_files() {
        let http = FakeHttp {
            responses: Mutex::new(vec![(
                encode("'folder-a' in parents and trashed = false"),
                Ok(json!({
                    "files": [
                        {"id": "doc-1", "name": "SOP", "mimeType": "text/plain",
                         "modifiedTime": "2026-06-09T12:00:00Z", "parents": ["folder-a"]}
                    ],
                    "nextPageToken": "page-2"
                })),
            )]),
        };
        let client = LiveDriveReadClient::new(http);

        let page = client
            .list_folder_files("token", "folder-a", None)
            .expect("list");

        assert_eq!(page.files.len(), 1);
        assert_eq!(page.files[0].file_id, "doc-1");
        assert_eq!(page.next_page_token.as_deref(), Some("page-2"));
    }

    fn one_response_server(status: u16, headers: &[(&str, &str)], body: &str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let headers = headers
            .iter()
            .map(|(key, value)| format!("{key}: {value}\r\n"))
            .collect::<String>();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/drive")
    }

    #[test]
    fn quota_403_maps_to_rate_limited() {
        let url = one_response_server(
            403,
            &[("Retry-After", "11")],
            r#"{"error":{"errors":[{"domain":"usageLimits","reason":"quotaExceeded","message":"Quota Exceeded"}],"code":403,"message":"Quota Exceeded"}}"#,
        );
        let http = ReqwestDriveHttpClient::default();
        let err = http.get_json(&url, "tok").expect_err("quota 403");

        match err {
            DriveError::RateLimited {
                retry_after_ms,
                message,
            } => {
                assert_eq!(retry_after_ms, Some(11_000));
                assert!(message.contains("reason=quotaExceeded"));
            }
            other => panic!("expected rate limited, got {other:?}"),
        }
    }

    #[test]
    fn non_quota_403_stays_auth_rejected() {
        let url = one_response_server(
            403,
            &[],
            r#"{"error":{"errors":[{"domain":"global","reason":"appNotAuthorizedToFile","message":"not authorized"}],"code":403,"message":"not authorized"}}"#,
        );
        let http = ReqwestDriveHttpClient::default();
        let err = http.get_json(&url, "tok").expect_err("auth 403");

        match err {
            DriveError::AuthRejected { message } => {
                assert!(message.contains("reason=appNotAuthorizedToFile"));
            }
            other => panic!("expected auth rejected, got {other:?}"),
        }
    }
}
