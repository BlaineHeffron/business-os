//! Read-only Gmail inbox projection (metadata-only, redacted) plus the
//! full-body lanes (full-read ingestion query + operator-display thread
//! reader). Config-driven: all credentials/labels arrive via [`GmailReadConfig`]
//! — this module never reads env vars.

#[cfg(test)]
pub(crate) use crate::gmail_http::FakeGmailHttp;
pub use crate::gmail_http::{GmailHttp, ReqwestGmailHttpClient};
use crate::gmail_triage_rules;
use crate::google_oauth::{fetch_access_token, has_scope, GoogleOAuthConfig};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bos_kernel::{AppError, AppResult, CorrelationId, ErrorCode};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const GMAIL_INBOX_READ_PROVIDER: &str = "gmail";
pub const GMAIL_INBOX_READ_ONLY_OPERATION: &str = "gmail_shared_mailbox_inbox_read_context";

/// Everything the Gmail read path needs, supplied by the caller (bos-app
/// `env_registry` / client overlay). Replaces the predecessor's env-driven
/// adapter (`GMAIL_OAUTH_*`, `GMAIL_SYNC_LABEL_IDS`, `GMAIL_INBOX_READ_*`).
#[derive(Debug, Clone)]
pub struct GmailReadConfig {
    /// OAuth material; `None` = no credentials configured (degraded).
    pub oauth: Option<GoogleOAuthConfig>,
    /// Gmail label ids to project (e.g. `["INBOX"]`).
    pub label_ids: Vec<String>,
    /// `true` (the default) serves the deterministic fixture and never touches
    /// the network; live reads are an explicit opt-in.
    pub dry_run: bool,
}

impl Default for GmailReadConfig {
    fn default() -> Self {
        Self {
            oauth: None,
            label_ids: Vec::new(),
            dry_run: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GmailInboxReadMode {
    DryRun,
    Live,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GmailInboxReadStatusCode {
    Ready,
    DegradedMissingCredentials,
    LabelConfigMissing,
    AuthFailed,
    ScopeMismatch,
    ProviderUnavailable,
    SchemaMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GmailInboxReadObjectKind {
    ThreadRef,
    MessageRef,
    LabelCategory,
}

impl GmailInboxReadObjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThreadRef => "thread_ref",
            Self::MessageRef => "message_ref",
            Self::LabelCategory => "label_category",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxReadRequest {
    #[serde(default)]
    pub mailbox_ref: Option<String>,
    pub object_kinds: Vec<GmailInboxReadObjectKind>,
    pub label_ids: Vec<String>,
    pub category_ids: Vec<String>,
    pub max_threads: u32,
    pub cursor: Option<String>,
}

impl Default for GmailInboxReadRequest {
    fn default() -> Self {
        Self {
            mailbox_ref: None,
            object_kinds: vec![
                GmailInboxReadObjectKind::ThreadRef,
                GmailInboxReadObjectKind::MessageRef,
                GmailInboxReadObjectKind::LabelCategory,
            ],
            label_ids: vec!["INBOX".to_string()],
            category_ids: vec![
                "CATEGORY_PERSONAL".to_string(),
                "CATEGORY_UPDATES".to_string(),
            ],
            max_threads: 25,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxReadStatus {
    pub provider: String,
    pub ready: bool,
    pub credential_configured: bool,
    pub labels_configured: bool,
    pub mode: GmailInboxReadMode,
    pub status: GmailInboxReadStatusCode,
    pub message: String,
    pub expected_read_scopes: Vec<String>,
    pub provider_payload_redacted: bool,
    pub provider_write_disabled: bool,
    pub gmail_send_disabled: bool,
    pub gmail_draft_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxReadCounts {
    pub mailbox_count: u32,
    pub thread_count: u32,
    pub message_ref_count: u32,
    pub label_count: u32,
    pub category_count: u32,
}

impl GmailInboxReadCounts {
    pub const fn empty() -> Self {
        Self {
            mailbox_count: 0,
            thread_count: 0,
            message_ref_count: 0,
            label_count: 0,
            category_count: 0,
        }
    }

    pub fn observed_records(&self) -> u32 {
        self.mailbox_count
            .saturating_add(self.thread_count)
            .saturating_add(self.message_ref_count)
            .saturating_add(self.label_count)
            .saturating_add(self.category_count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxThreadRef {
    pub thread_ref: String,
    pub mailbox_ref: String,
    pub latest_message_ref: Option<String>,
    pub label_refs: Vec<String>,
    pub category_refs: Vec<String>,
    pub sender_domain_hint: Option<String>,
    pub received_bucket: String,
    pub triage_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxMessageRef {
    pub message_ref: String,
    pub thread_ref: String,
    pub label_refs: Vec<String>,
    pub category_refs: Vec<String>,
    pub attachment_count: u32,
    pub body_redacted: bool,
    pub header_payload_redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GmailInboxReadContext {
    pub status: GmailInboxReadStatus,
    pub request: GmailInboxReadRequest,
    pub counts: GmailInboxReadCounts,
    pub threads: Vec<GmailInboxThreadRef>,
    pub messages: Vec<GmailInboxMessageRef>,
    pub next_cursor: Option<String>,
    pub cursor_hash: Option<String>,
    pub evidence_refs: Vec<String>,
    pub network_calls_planned: bool,
    pub provider_payload_redacted: bool,
    pub provider_write_disabled: bool,
    pub gmail_send_disabled: bool,
    pub gmail_draft_disabled: bool,
    pub gmail_label_mutation_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailFullReadRequest {
    pub query: String,
    pub max_messages: u32,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailFullReadPage {
    pub messages: Vec<GmailFullMessage>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailFullMessage {
    pub message_id: String,
    pub thread_id: Option<String>,
    pub label_ids: Vec<String>,
    pub internal_date_epoch_ms: Option<i64>,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub headers: Vec<(String, String)>,
    pub plain_text_body: String,
    pub html_body: Option<String>,
    pub attachments: Vec<GmailAttachmentMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailAttachmentMeta {
    pub attachment_id: String,
    pub part_id: Option<String>,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub inline: bool,
    pub content_id: Option<String>,
}

pub trait GmailInboxReadClient: Send + Sync {
    fn read_context(&self, request: &GmailInboxReadRequest) -> AppResult<GmailInboxReadContext>;
}

#[derive(Debug, Clone)]
pub struct DryRunGmailInboxReadClient {
    context: GmailInboxReadContext,
}

impl DryRunGmailInboxReadClient {
    pub fn new(context: GmailInboxReadContext) -> Self {
        Self { context }
    }

    pub fn sample() -> Self {
        Self::new(sample_gmail_inbox_read_context())
    }
}

impl Default for DryRunGmailInboxReadClient {
    fn default() -> Self {
        Self::sample()
    }
}

impl GmailInboxReadClient for DryRunGmailInboxReadClient {
    fn read_context(&self, request: &GmailInboxReadRequest) -> AppResult<GmailInboxReadContext> {
        let mut context = self.context.clone();
        context.request = request.clone();
        context.counts = counts_for(&context);
        context.cursor_hash = Some(stable_hash(&cursor_material(&context)));
        Ok(context)
    }
}

#[derive(Debug, Clone)]
pub struct GmailInboxReadAdapter<C = DryRunGmailInboxReadClient> {
    client: C,
    credential: Option<String>,
    labels: Vec<String>,
    dry_run: bool,
}

impl<C> GmailInboxReadAdapter<C>
where
    C: GmailInboxReadClient,
{
    pub fn new(client: C, credential: Option<String>, labels: Vec<String>, dry_run: bool) -> Self {
        Self {
            client,
            credential: credential
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            labels: labels
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect(),
            dry_run,
        }
    }

    pub fn status(&self) -> GmailInboxReadStatus {
        let status = if self.credential.is_none() {
            GmailInboxReadStatusCode::DegradedMissingCredentials
        } else if self.labels.is_empty() {
            GmailInboxReadStatusCode::LabelConfigMissing
        } else {
            GmailInboxReadStatusCode::Ready
        };
        gmail_inbox_read_status(
            self.credential.is_some(),
            !self.labels.is_empty(),
            if status != GmailInboxReadStatusCode::Ready {
                GmailInboxReadMode::Degraded
            } else if self.dry_run {
                GmailInboxReadMode::DryRun
            } else {
                GmailInboxReadMode::Live
            },
            status,
        )
    }

    pub fn read_context(
        &self,
        mut request: GmailInboxReadRequest,
    ) -> AppResult<GmailInboxReadContext> {
        if request.label_ids.is_empty() {
            request.label_ids = self.labels.clone();
        }
        let status = self.status();
        if status.status != GmailInboxReadStatusCode::Ready {
            return Ok(degraded_context(request, status.status));
        }

        let mut context = self.client.read_context(&request)?;
        context.status = status;
        context.network_calls_planned = !self.dry_run;
        context.provider_payload_redacted = true;
        context.provider_write_disabled = true;
        context.gmail_send_disabled = true;
        context.gmail_draft_disabled = true;
        context.gmail_label_mutation_disabled = true;
        context.counts = counts_for(&context);
        context.cursor_hash = Some(stable_hash(&cursor_material(&context)));
        Ok(context)
    }
}

pub fn sample_gmail_inbox_read_context() -> GmailInboxReadContext {
    let request = GmailInboxReadRequest::default();
    let threads = vec![
        GmailInboxThreadRef {
            thread_ref: "gmail:thread:fake-thread-001".to_string(),
            mailbox_ref: "gmail:mailbox:shared-inbox".to_string(),
            latest_message_ref: Some("gmail:message:fake-message-001".to_string()),
            label_refs: vec![
                "gmail:label:INBOX".to_string(),
                "bos:label:needs-review".to_string(),
            ],
            category_refs: vec!["gmail:category:CATEGORY_PERSONAL".to_string()],
            sender_domain_hint: Some("customer.example.test".to_string()),
            received_bucket: "recent_24h".to_string(),
            triage_hint: "human_reply_needed".to_string(),
        },
        GmailInboxThreadRef {
            thread_ref: "gmail:thread:fake-thread-002".to_string(),
            mailbox_ref: "gmail:mailbox:shared-inbox".to_string(),
            latest_message_ref: Some("gmail:message:fake-message-002".to_string()),
            label_refs: vec!["gmail:label:INBOX".to_string()],
            category_refs: vec!["gmail:category:CATEGORY_UPDATES".to_string()],
            sender_domain_hint: Some("system.example.test".to_string()),
            received_bucket: "recent_7d".to_string(),
            triage_hint: "system_update_review".to_string(),
        },
    ];
    let messages = threads
        .iter()
        .filter_map(|thread| {
            thread
                .latest_message_ref
                .as_ref()
                .map(|message_ref| GmailInboxMessageRef {
                    message_ref: message_ref.clone(),
                    thread_ref: thread.thread_ref.clone(),
                    label_refs: thread.label_refs.clone(),
                    category_refs: thread.category_refs.clone(),
                    attachment_count: u32::from(thread.thread_ref.ends_with("001")),
                    body_redacted: true,
                    header_payload_redacted: true,
                })
        })
        .collect::<Vec<_>>();
    let mut context = GmailInboxReadContext {
        status: gmail_inbox_read_status(
            true,
            true,
            GmailInboxReadMode::DryRun,
            GmailInboxReadStatusCode::Ready,
        ),
        request,
        counts: GmailInboxReadCounts::empty(),
        threads,
        messages,
        next_cursor: None,
        cursor_hash: None,
        evidence_refs: vec!["external_projection:gmail_inbox:inbox_intake".to_string()],
        network_calls_planned: false,
        provider_payload_redacted: true,
        provider_write_disabled: true,
        gmail_send_disabled: true,
        gmail_draft_disabled: true,
        gmail_label_mutation_disabled: true,
    };
    context.counts = counts_for(&context);
    context.cursor_hash = Some(stable_hash(&cursor_material(&context)));
    context
}

pub fn degraded_context(
    request: GmailInboxReadRequest,
    status: GmailInboxReadStatusCode,
) -> GmailInboxReadContext {
    let labels_configured = status != GmailInboxReadStatusCode::LabelConfigMissing;
    GmailInboxReadContext {
        status: gmail_inbox_read_status(
            false,
            labels_configured,
            GmailInboxReadMode::Degraded,
            status,
        ),
        request,
        counts: GmailInboxReadCounts::empty(),
        threads: Vec::new(),
        messages: Vec::new(),
        next_cursor: None,
        cursor_hash: None,
        evidence_refs: vec![
            "external_projection:gmail_inbox:inbox_intake".to_string(),
            format!(
                "external_projection:gmail_inbox:blocker:{}",
                status_code(status)
            ),
        ],
        network_calls_planned: false,
        provider_payload_redacted: true,
        provider_write_disabled: true,
        gmail_send_disabled: true,
        gmail_draft_disabled: true,
        gmail_label_mutation_disabled: true,
    }
}

fn gmail_inbox_read_status(
    credential_configured: bool,
    labels_configured: bool,
    mode: GmailInboxReadMode,
    status: GmailInboxReadStatusCode,
) -> GmailInboxReadStatus {
    GmailInboxReadStatus {
        provider: GMAIL_INBOX_READ_PROVIDER.to_string(),
        ready: status == GmailInboxReadStatusCode::Ready,
        credential_configured,
        labels_configured,
        mode,
        status,
        message: match status {
            GmailInboxReadStatusCode::Ready => "Gmail inbox read context available",
            GmailInboxReadStatusCode::DegradedMissingCredentials => {
                "Gmail OAuth material missing for inbox read context"
            }
            GmailInboxReadStatusCode::LabelConfigMissing => {
                "Gmail inbox labels or categories missing for read context"
            }
            GmailInboxReadStatusCode::AuthFailed => "Gmail inbox read auth failed",
            GmailInboxReadStatusCode::ScopeMismatch => "Gmail inbox read scopes missing",
            GmailInboxReadStatusCode::ProviderUnavailable => "Gmail inbox provider unavailable",
            GmailInboxReadStatusCode::SchemaMismatch => "Gmail inbox context failed safety checks",
        }
        .to_string(),
        expected_read_scopes: vec!["https://www.googleapis.com/auth/gmail.readonly".to_string()],
        provider_payload_redacted: true,
        provider_write_disabled: true,
        gmail_send_disabled: true,
        gmail_draft_disabled: true,
    }
}

fn counts_for(context: &GmailInboxReadContext) -> GmailInboxReadCounts {
    GmailInboxReadCounts {
        mailbox_count: u32::from(!context.threads.is_empty() || !context.messages.is_empty()),
        thread_count: context.threads.len() as u32,
        message_ref_count: context.messages.len() as u32,
        label_count: unique_ref_count(context.threads.iter().flat_map(|thread| &thread.label_refs)),
        category_count: unique_ref_count(
            context
                .threads
                .iter()
                .flat_map(|thread| &thread.category_refs),
        ),
    }
}

fn unique_ref_count<'a>(values: impl Iterator<Item = &'a String>) -> u32 {
    let mut unique = values.collect::<Vec<_>>();
    unique.sort();
    unique.dedup();
    unique.len() as u32
}

fn cursor_material(context: &GmailInboxReadContext) -> String {
    let mut material = BTreeMap::new();
    material.insert("provider", context.status.provider.clone());
    material.insert("status", status_code(context.status.status).to_string());
    material.insert("mailboxes", context.counts.mailbox_count.to_string());
    material.insert("threads", context.counts.thread_count.to_string());
    material.insert("messages", context.counts.message_ref_count.to_string());
    material.insert("labels", context.counts.label_count.to_string());
    material.insert("categories", context.counts.category_count.to_string());
    material.insert("evidence_refs", context.evidence_refs.join(","));
    material
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("|")
}

fn stable_hash(input: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn status_code(status: GmailInboxReadStatusCode) -> &'static str {
    match status {
        GmailInboxReadStatusCode::Ready => "ready",
        GmailInboxReadStatusCode::DegradedMissingCredentials => "credentials_missing",
        GmailInboxReadStatusCode::LabelConfigMissing => "label_config_missing",
        GmailInboxReadStatusCode::AuthFailed => "auth_failed",
        GmailInboxReadStatusCode::ScopeMismatch => "scope_mismatch",
        GmailInboxReadStatusCode::ProviderUnavailable => "provider_unavailable",
        GmailInboxReadStatusCode::SchemaMismatch => "schema_mismatch",
    }
}

pub fn gmail_inbox_read_error(code: &'static str, message: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::ExternalDependency,
        code,
        message.to_string(),
        CorrelationId::new("corr_gmail_inbox_read"),
    )
}

const GMAIL_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
const GMAIL_MODIFY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";
const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const GMAIL_MESSAGES_PAGE_SIZE: u32 = 500;
const URL_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Live, read-only Gmail inbox projection. Metadata-only; subject is read for the
/// rule-map then dropped. Never performs writes.
pub struct LiveGmailInboxReadClient {
    http: Arc<dyn GmailHttp>,
    access_token: String,
}

impl LiveGmailInboxReadClient {
    pub fn from_access_token(http: Arc<dyn GmailHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }

    /// Exchanges the refresh token for an access token (one network call to the
    /// token endpoint). Fails without network when a populated scope list lacks
    /// a Gmail read-capable scope.
    pub fn from_credentials(
        http: Arc<dyn GmailHttp>,
        creds: &GoogleOAuthConfig,
    ) -> AppResult<Self> {
        // When resolved credentials don't enumerate scopes (empty list), we cannot verify and proceed.
        // A populated scope list must include a Gmail scope that permits message reads.
        if !creds.scopes.is_empty() && !has_gmail_read_scope(creds) {
            return Err(gmail_inbox_read_error(
                "gmail_scope_missing",
                "gmail.readonly or gmail.modify scope absent from resolved credentials",
            ));
        }
        let access_token = fetch_access_token(creds)?;
        Ok(Self { http, access_token })
    }

    #[cfg(test)]
    pub fn for_test(http: Arc<dyn GmailHttp>, access_token: String) -> Self {
        Self { http, access_token }
    }

    fn list_threads(&self, request: &GmailInboxReadRequest) -> AppResult<Value> {
        let labels = if request.label_ids.is_empty() {
            vec!["INBOX".to_string()]
        } else {
            request.label_ids.clone()
        };
        let mut query = vec![format!("maxResults={}", request.max_threads)];
        query.extend(
            labels
                .iter()
                .map(|label| format!("labelIds={}", encode_query_component(label))),
        );
        if let Some(cursor) = request.cursor.as_ref() {
            query.push(format!("pageToken={}", encode_query_component(cursor)));
        }
        self.http.get_json(
            &format!("{GMAIL_API_BASE}/threads?{}", query.join("&")),
            &self.access_token,
        )
    }

    fn get_thread_metadata(&self, thread_id: &str) -> AppResult<Value> {
        let thread_id = encode_path_segment(thread_id);
        let url = format!(
            "{GMAIL_API_BASE}/threads/{thread_id}?format=metadata&metadataHeaders=From&metadataHeaders=Date&metadataHeaders=Subject"
        );
        self.http.get_json(&url, &self.access_token)
    }

    /// Label id → display name (one GET users/me/labels). User labels carry
    /// opaque ids like "Label_13" on messages; system labels (INBOX, …) are
    /// their own names. Callers resolve message label ids through this map so
    /// stored/displayed labels are the names the operator knows.
    pub fn list_label_names(&self) -> AppResult<std::collections::HashMap<String, String>> {
        let value = self
            .http
            .get_json(&format!("{GMAIL_API_BASE}/labels"), &self.access_token)?;
        let mut map = std::collections::HashMap::new();
        if let Some(labels) = value.get("labels").and_then(Value::as_array) {
            for label in labels {
                if let (Some(id), Some(name)) = (
                    label.get("id").and_then(Value::as_str),
                    label.get("name").and_then(Value::as_str),
                ) {
                    map.insert(id.to_string(), name.to_string());
                }
            }
        }
        Ok(map)
    }

    /// Read full message bodies for the configured ingestion query.
    ///
    /// This intentionally differs from the shared-mailbox triage projection,
    /// which must remain metadata-only and redacted. The only caller should be
    /// the configured full-read source connector using gmail.readonly.
    pub fn read_full_messages_page(
        &self,
        request: &GmailFullReadRequest,
    ) -> AppResult<GmailFullReadPage> {
        let max_messages = request.max_messages.clamp(1, GMAIL_MESSAGES_PAGE_SIZE);
        let list = self.list_messages_by_query(
            &request.query,
            max_messages,
            request.page_token.as_deref(),
        )?;
        let message_ids = list
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|message| {
                message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .take(max_messages as usize);
        let next_page_token = list
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|token| !token.is_empty());
        if next_page_token.is_some() && next_page_token == request.page_token {
            return Err(gmail_inbox_read_error(
                "gmail_messages_page_token_repeated",
                "Gmail repeated a messages.list page token",
            ));
        }

        let mut messages = Vec::new();
        for message_id in message_ids {
            let full = self.get_message_full(&message_id)?;
            if let Some(message) = full_message_from_value(&full) {
                messages.push(message);
            }
        }
        Ok(GmailFullReadPage {
            messages,
            next_page_token,
        })
    }

    fn list_messages_by_query(
        &self,
        query: &str,
        max_results: u32,
        page_token: Option<&str>,
    ) -> AppResult<Value> {
        let mut url = format!(
            "{GMAIL_API_BASE}/messages?q={}&maxResults={}",
            encode_query_component(query),
            max_results
        );
        if let Some(page_token) = page_token {
            url.push_str("&pageToken=");
            url.push_str(&encode_query_component(page_token));
        }
        self.http.get_json(&url, &self.access_token)
    }

    fn get_message_full(&self, message_id: &str) -> AppResult<Value> {
        let message_id = encode_path_segment(message_id);
        let url = format!("{GMAIL_API_BASE}/messages/{message_id}?format=full");
        self.http.get_json(&url, &self.access_token)
    }

    pub fn read_attachment(&self, message_id: &str, attachment_id: &str) -> AppResult<Vec<u8>> {
        let message_id = encode_path_segment(message_id);
        let attachment_id = encode_path_segment(attachment_id);
        let url = format!("{GMAIL_API_BASE}/messages/{message_id}/attachments/{attachment_id}");
        let value = self.http.get_json(&url, &self.access_token)?;
        let data = value.get("data").and_then(Value::as_str).ok_or_else(|| {
            gmail_inbox_read_error("schema_mismatch", "gmail attachment missing data")
        })?;
        decode_gmail_body_bytes(data).ok_or_else(|| {
            gmail_inbox_read_error("schema_mismatch", "gmail attachment decode failed")
        })
    }

    fn get_thread_full(&self, thread_id: &str) -> AppResult<Value> {
        let thread_id = encode_path_segment(thread_id);
        let url = format!("{GMAIL_API_BASE}/threads/{thread_id}?format=full");
        self.http.get_json(&url, &self.access_token)
    }

    /// Read every message of a single thread with full bodies, for the
    /// OPERATOR-DISPLAY thread reader (raw content, no scrub).
    ///
    /// The caller MUST enforce operator auth + mailbox visibility before
    /// returning this, and MUST NEVER route it into an LLM-input path — this is
    /// the raw-display lane, kept separate from the redacted triage projection.
    pub fn read_thread_messages(&self, thread_id: &str) -> AppResult<Vec<GmailFullMessage>> {
        let thread = self.get_thread_full(thread_id)?;
        let messages = thread
            .get("messages")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(full_message_from_value).collect())
            .unwrap_or_default();
        Ok(messages)
    }
}

fn has_gmail_read_scope(creds: &GoogleOAuthConfig) -> bool {
    has_scope(creds, GMAIL_READONLY_SCOPE) || has_scope(creds, GMAIL_MODIFY_SCOPE)
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, URL_COMPONENT_ENCODE_SET).to_string()
}

fn encode_query_component(value: &str) -> String {
    utf8_percent_encode(value, URL_COMPONENT_ENCODE_SET).to_string()
}

fn header_value<'a>(message: &'a Value, name: &str) -> Option<&'a str> {
    message
        .get("payload")?
        .get("headers")?
        .as_array()?
        .iter()
        .find(|h| h.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|h| h.get("value"))
        .and_then(Value::as_str)
}

fn full_message_from_value(message: &Value) -> Option<GmailFullMessage> {
    let message_id = message.get("id")?.as_str()?.trim();
    if message_id.is_empty() {
        return None;
    }
    let internal_date_epoch_ms = message
        .get("internalDate")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i64>().ok());
    let subject = header_value(message, "Subject")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let from = header_value(message, "From")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let to = header_value(message, "To")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let headers = message
        .get("payload")
        .and_then(|payload| payload.get("headers"))
        .and_then(Value::as_array)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|header| {
                    let name = header.get("name").and_then(Value::as_str)?.trim();
                    let value = header.get("value").and_then(Value::as_str)?.trim();
                    if name.is_empty() || value.is_empty() {
                        return None;
                    }
                    Some((name.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let payload = message.get("payload");
    let plain_text_body = payload.and_then(plain_text_body_from_payload).or_else(|| {
        message
            .get("snippet")
            .and_then(Value::as_str)
            .map(str::to_string)
    })?;
    let plain_text_body = plain_text_body.trim().to_string();
    if plain_text_body.is_empty() {
        return None;
    }
    let thread_id = message
        .get("threadId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let attachments = message
        .get("payload")
        .map(attachment_metadata_from_payload)
        .unwrap_or_default();
    let html_body = payload
        .and_then(html_body_from_payload)
        .map(|body| body.trim().to_string())
        .filter(|body| !body.is_empty());
    Some(GmailFullMessage {
        message_id: message_id.to_string(),
        thread_id,
        label_ids: message
            .get("labelIds")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        internal_date_epoch_ms,
        subject,
        from,
        to,
        headers,
        plain_text_body,
        html_body,
        attachments,
    })
}

fn attachment_metadata_from_payload(payload: &Value) -> Vec<GmailAttachmentMeta> {
    let mut out = Vec::new();
    collect_attachment_metadata(payload, &mut out);
    out
}

fn collect_attachment_metadata(payload: &Value, out: &mut Vec<GmailAttachmentMeta>) {
    let attachment_id = payload
        .get("body")
        .and_then(|body| body.get("attachmentId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(attachment_id) = attachment_id {
        let headers = payload
            .get("headers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let content_disposition = part_header_value(&headers, "Content-Disposition");
        let inline = content_disposition
            .as_deref()
            .map(|value| value.to_ascii_lowercase().contains("inline"))
            .unwrap_or(false);
        let content_id = part_header_value(&headers, "Content-ID").map(|value| {
            value
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        });
        out.push(GmailAttachmentMeta {
            attachment_id: attachment_id.to_string(),
            part_id: payload
                .get("partId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            filename: payload
                .get("filename")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("attachment")
                .to_string(),
            mime_type: payload
                .get("mimeType")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            size_bytes: payload
                .get("body")
                .and_then(|body| body.get("size"))
                .and_then(Value::as_u64),
            inline,
            content_id,
        });
    }
    if let Some(parts) = payload.get("parts").and_then(Value::as_array) {
        for part in parts {
            collect_attachment_metadata(part, out);
        }
    }
}

fn part_header_value(headers: &[Value], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| {
            header
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        })
        .and_then(|header| header.get("value"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn plain_text_body_from_payload(payload: &Value) -> Option<String> {
    let mime_type = payload
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if mime_type.eq_ignore_ascii_case("text/plain") {
        if let Some(decoded) = payload
            .get("body")
            .and_then(|body| body.get("data"))
            .and_then(Value::as_str)
            .and_then(decode_gmail_body)
            .filter(|body| !body.trim().is_empty())
        {
            return Some(decoded);
        }
    }

    payload
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(plain_text_body_from_payload)
                .filter(|body| !body.trim().is_empty())
        })
}

fn html_body_from_payload(payload: &Value) -> Option<String> {
    let mime_type = payload
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if mime_type.eq_ignore_ascii_case("text/html") {
        if let Some(decoded) = payload
            .get("body")
            .and_then(|body| body.get("data"))
            .and_then(Value::as_str)
            .and_then(decode_gmail_body)
            .filter(|body| !body.trim().is_empty())
        {
            return Some(decoded);
        }
    }

    payload
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(html_body_from_payload)
                .filter(|body| !body.trim().is_empty())
        })
}

fn decode_gmail_body(data: &str) -> Option<String> {
    decode_gmail_body_bytes(data).and_then(|bytes| String::from_utf8(bytes).ok())
}

fn decode_gmail_body_bytes(data: &str) -> Option<Vec<u8>> {
    let normalized = data.trim().replace('-', "+").replace('_', "/");
    let mut padded = normalized;
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    URL_SAFE_NO_PAD
        .decode(data.trim().as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(padded.as_bytes()))
        .ok()
}

fn sender_domain(from: &str) -> Option<String> {
    let at = from.rfind('@')?;
    let tail = &from[at + 1..];
    let domain: String = tail
        .chars()
        .take_while(|c| !matches!(c, '>' | ' ' | '\t'))
        .collect();
    let domain = domain.trim().to_ascii_lowercase();
    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

fn received_bucket(_date: Option<&str>) -> String {
    // v1: always returns "recent" — live therefore diverges from the dry-run fixture's bucket
    // values (named hook); a date-based bucketing pass will replace this when a wall-clock
    // anchor is available in this layer.
    "recent".to_string()
}

impl GmailInboxReadClient for LiveGmailInboxReadClient {
    fn read_context(&self, request: &GmailInboxReadRequest) -> AppResult<GmailInboxReadContext> {
        let list = self.list_threads(request)?;
        let next_cursor = list
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        let thread_ids: Vec<String> = list
            .get("threads")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mailbox_ref = request
            .mailbox_ref
            .clone()
            .unwrap_or_else(|| "gmail:mailbox:shared-inbox".to_string());
        let mut threads = Vec::new();
        let mut messages = Vec::new();
        for thread_id in thread_ids {
            let thread_resp = self.get_thread_metadata(&thread_id)?;
            // Take the latest message = last element of the messages array; skip on empty.
            let latest_msg = match thread_resp
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|arr| arr.last())
            {
                Some(m) => m,
                None => continue,
            };
            let msg_id = match latest_msg.get("id").and_then(Value::as_str) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let from = header_value(latest_msg, "From");
            let subject = header_value(latest_msg, "Subject").unwrap_or("");
            let domain = from.and_then(sender_domain);
            let plain_labels: Vec<String> = latest_msg
                .get("labelIds")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let label_refs: Vec<String> = plain_labels
                .iter()
                .map(|l| format!("gmail:label:{l}"))
                .collect();
            // Subject consumed HERE then dropped (never stored).
            let triage_hint =
                gmail_triage_rules::classify(subject, domain.as_deref(), &plain_labels).to_string();
            let message_ref = format!("gmail:message:{msg_id}");
            messages.push(GmailInboxMessageRef {
                message_ref: message_ref.clone(),
                thread_ref: format!("gmail:thread:{thread_id}"),
                label_refs: label_refs.clone(),
                category_refs: Vec::new(),
                attachment_count: 0,
                body_redacted: true,
                header_payload_redacted: true,
            });
            threads.push(GmailInboxThreadRef {
                thread_ref: format!("gmail:thread:{thread_id}"),
                mailbox_ref: mailbox_ref.clone(),
                latest_message_ref: Some(message_ref),
                label_refs,
                category_refs: Vec::new(),
                sender_domain_hint: domain,
                received_bucket: received_bucket(header_value(latest_msg, "Date")),
                triage_hint,
            });
        }

        let mut context = GmailInboxReadContext {
            status: gmail_inbox_read_status(
                true,
                true,
                GmailInboxReadMode::Live,
                GmailInboxReadStatusCode::Ready,
            ),
            request: request.clone(),
            counts: GmailInboxReadCounts::empty(),
            threads,
            messages,
            next_cursor,
            cursor_hash: None,
            evidence_refs: vec!["external_projection:gmail_inbox:inbox_intake".to_string()],
            network_calls_planned: true,
            provider_payload_redacted: true,
            provider_write_disabled: true,
            gmail_send_disabled: true,
            gmail_draft_disabled: true,
            gmail_label_mutation_disabled: true,
        };
        context.counts = counts_for(&context);
        context.cursor_hash = Some(stable_hash(&cursor_material(&context)));
        Ok(context)
    }
}

impl GmailInboxReadClient for Box<dyn GmailInboxReadClient> {
    fn read_context(&self, request: &GmailInboxReadRequest) -> AppResult<GmailInboxReadContext> {
        (**self).read_context(request)
    }
}

impl GmailInboxReadAdapter<Box<dyn GmailInboxReadClient>> {
    /// Choose the live client when `config.dry_run` is false and credentials
    /// resolve; construction failures degrade empty instead of seeding fixture
    /// rows. Never hits the network in dry-run. Going live performs one token
    /// exchange during construction.
    pub fn from_config(config: &GmailReadConfig) -> Self {
        let labels = config.label_ids.clone();
        if !config.dry_run {
            if let Some(creds) = config.oauth.as_ref() {
                if let Ok(live) = LiveGmailInboxReadClient::from_credentials(
                    Arc::new(ReqwestGmailHttpClient::default()),
                    creds,
                ) {
                    return Self::new(
                        Box::new(live),
                        Some("resolved_google_oauth".to_string()),
                        labels,
                        false,
                    );
                }
            }
            return Self::new(
                Box::new(DryRunGmailInboxReadClient::sample()),
                None,
                labels,
                false,
            );
        }
        let credential = config.oauth.as_ref().map(|c| c.refresh_token.clone());
        Self::new(
            Box::new(DryRunGmailInboxReadClient::sample()),
            credential,
            labels,
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_oauth(scopes: Vec<String>) -> GoogleOAuthConfig {
        GoogleOAuthConfig {
            refresh_token: "local-refresh".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            scopes,
            token_url: None,
        }
    }

    #[test]
    fn gmail_modify_scope_permits_read_projection() {
        let creds = test_oauth(vec![GMAIL_MODIFY_SCOPE.to_string()]);

        assert!(has_gmail_read_scope(&creds));
    }

    #[test]
    fn dry_run_fixture_projects_safe_inbox_refs() -> Result<(), String> {
        let adapter = GmailInboxReadAdapter::new(
            DryRunGmailInboxReadClient::sample(),
            Some("local-test-credential".to_string()),
            vec!["INBOX".to_string()],
            true,
        );

        let context = adapter
            .read_context(GmailInboxReadRequest::default())
            .map_err(|error| error.to_string())?;

        assert_eq!(context.status.status, GmailInboxReadStatusCode::Ready);
        assert_eq!(context.counts.thread_count, 2);
        assert_eq!(context.counts.message_ref_count, 2);
        assert!(context.threads.iter().all(|thread| thread
            .sender_domain_hint
            .as_ref()
            .is_none_or(|domain| domain.ends_with(".test") && !domain.contains('@'))));
        assert!(!context.network_calls_planned);
        assert!(context.provider_payload_redacted);
        assert!(context.provider_write_disabled);
        assert!(context.gmail_send_disabled);
        assert!(context.gmail_draft_disabled);
        assert!(context.gmail_label_mutation_disabled);
        Ok(())
    }

    #[test]
    fn missing_credentials_degrade_without_calling_client_or_writes() -> Result<(), String> {
        let adapter = GmailInboxReadAdapter::new(
            DryRunGmailInboxReadClient::sample(),
            None,
            vec!["INBOX".to_string()],
            false,
        );

        let context = adapter
            .read_context(GmailInboxReadRequest::default())
            .map_err(|error| error.to_string())?;

        assert_eq!(
            context.status.status,
            GmailInboxReadStatusCode::DegradedMissingCredentials
        );
        assert!(!context.status.ready);
        assert_eq!(context.counts.observed_records(), 0);
        assert!(context.threads.is_empty());
        assert!(!context.network_calls_planned);
        assert!(context.provider_write_disabled);
        assert!(context.gmail_send_disabled);
        assert!(context.gmail_draft_disabled);
        Ok(())
    }

    #[test]
    fn missing_label_config_degrades_without_network() -> Result<(), String> {
        let adapter = GmailInboxReadAdapter::new(
            DryRunGmailInboxReadClient::sample(),
            Some("local-test-credential".to_string()),
            Vec::new(),
            false,
        );

        let context = adapter
            .read_context(GmailInboxReadRequest {
                label_ids: Vec::new(),
                ..GmailInboxReadRequest::default()
            })
            .map_err(|error| error.to_string())?;

        assert_eq!(
            context.status.status,
            GmailInboxReadStatusCode::LabelConfigMissing
        );
        assert!(!context.network_calls_planned);
        assert!(context.messages.is_empty());
        assert!(context.provider_write_disabled);
        Ok(())
    }

    // Predecessor's "configured OAuth defaults to dry-run until live opt-in":
    // GmailReadConfig::default() carries dry_run = true, so a config with
    // credentials but no explicit opt-out must never plan network calls.
    #[test]
    fn config_with_oauth_defaults_to_dry_run() -> Result<(), String> {
        let config = GmailReadConfig {
            oauth: Some(test_oauth(Vec::new())),
            label_ids: vec!["INBOX".to_string(), "CATEGORY_PERSONAL".to_string()],
            ..GmailReadConfig::default()
        };
        let adapter = GmailInboxReadAdapter::from_config(&config);

        let context = adapter
            .read_context(GmailInboxReadRequest::default())
            .map_err(|error| error.to_string())?;

        assert_eq!(context.status.mode, GmailInboxReadMode::DryRun);
        assert_eq!(context.status.status, GmailInboxReadStatusCode::Ready);
        assert!(!context.network_calls_planned);
        Ok(())
    }

    #[test]
    fn from_config_degrades_empty_when_live_construction_fails() -> Result<(), String> {
        // Scope mismatch makes live construction fail BEFORE any network call;
        // the adapter must degrade empty, not seed dry-run fixture rows.
        let config = GmailReadConfig {
            oauth: Some(test_oauth(vec![
                "https://www.googleapis.com/auth/gmail.send".to_string(),
            ])),
            label_ids: vec!["INBOX".to_string()],
            dry_run: false,
        };
        let adapter = GmailInboxReadAdapter::from_config(&config);

        let ctx = adapter
            .read_context(GmailInboxReadRequest {
                label_ids: Vec::new(),
                ..GmailInboxReadRequest::default()
            })
            .map_err(|e| e.to_string())?;

        assert_eq!(
            ctx.status.status,
            GmailInboxReadStatusCode::DegradedMissingCredentials
        );
        assert!(ctx.threads.is_empty(), "must not seed dry-run fixture rows");
        assert!(!ctx.network_calls_planned);
        Ok(())
    }

    #[test]
    fn from_config_without_oauth_in_live_mode_degrades_empty() -> Result<(), String> {
        let config = GmailReadConfig {
            oauth: None,
            label_ids: vec!["INBOX".to_string()],
            dry_run: false,
        };
        let adapter = GmailInboxReadAdapter::from_config(&config);

        let ctx = adapter
            .read_context(GmailInboxReadRequest::default())
            .map_err(|e| e.to_string())?;

        assert_eq!(
            ctx.status.status,
            GmailInboxReadStatusCode::DegradedMissingCredentials
        );
        assert!(ctx.threads.is_empty());
        assert!(!ctx.network_calls_planned);
        Ok(())
    }

    #[test]
    fn fake_gmail_http_returns_queued_responses() -> Result<(), String> {
        let http = FakeGmailHttp::new(vec![
            serde_json::json!({"threads":[{"id":"t1"}],"nextPageToken":"p2"}),
            serde_json::json!({"id":"m1","payload":{"headers":[
                {"name":"From","value":"Jane <jane@business-1194228da8.test>"},
                {"name":"Date","value":"Wed, 27 May 2026 10:00:00 -0400"},
                {"name":"Subject","value":"Invoice #5 ready"}
            ]}}),
        ]);
        let r1 = http
            .get_json("https://gmail.example/threads", "tok")
            .map_err(|e| e.to_string())?;
        assert_eq!(r1["threads"][0]["id"], "t1");
        let r2 = http
            .get_json("https://gmail.example/messages/m1", "tok")
            .map_err(|e| e.to_string())?;
        assert_eq!(r2["payload"]["headers"][0]["name"], "From");
        Ok(())
    }

    #[test]
    fn serialized_context_leaks_no_body_secret_or_write_path() -> Result<(), String> {
        let context = sample_gmail_inbox_read_context();
        let rendered = serde_json::to_string(&context).map_err(|error| error.to_string())?;
        for forbidden in [
            "local-test-credential",
            "refreshToken",
            "emailAddress",
            "rawEmailBody",
            "rawEmailHeaders",
            "providerOutbox",
            "sendDraft",
            "createDraft",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "forbidden Gmail inbox read context leak: {forbidden}"
            );
        }
        assert!(rendered.contains("providerWriteDisabled"));
        assert!(rendered.contains("gmailSendDisabled"));
        Ok(())
    }

    #[test]
    fn live_client_projects_redacted_refs_and_drops_subject() -> Result<(), String> {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({"threads":[{"id":"thread-aaa"}],"nextPageToken":"page-2"}),
            serde_json::json!({"id":"thread-aaa","messages":[{"id":"msg-aaa","threadId":"thread-aaa","labelIds":["INBOX"],"payload":{"headers":[
                {"name":"From","value":"Jane Roe <jane@business-1194228da8.test>"},
                {"name":"Date","value":"Wed, 27 May 2026 10:00:00 -0400"},
                {"name":"Subject","value":"Invoice #5 is ready"}
            ]}}]}),
        ]));
        let client =
            LiveGmailInboxReadClient::for_test(http.clone(), "test-access-token".to_string());
        let ctx = client
            .read_context(&GmailInboxReadRequest::default())
            .map_err(|e| e.to_string())?;

        assert_eq!(ctx.counts.thread_count, 1);
        assert_eq!(ctx.threads[0].thread_ref, "gmail:thread:thread-aaa");
        assert_eq!(ctx.threads[0].triage_hint, "billing");
        assert_eq!(
            ctx.threads[0].sender_domain_hint.as_deref(),
            Some("business-1194228da8.test")
        );
        assert_eq!(ctx.next_cursor.as_deref(), Some("page-2"));
        assert_eq!(
            ctx.threads[0].latest_message_ref.as_deref(),
            Some("gmail:message:msg-aaa")
        );
        assert!(ctx.threads[0].label_refs.iter().all(|l| !l.contains('@')));

        let rendered = serde_json::to_string(&ctx).map_err(|e| e.to_string())?;
        assert!(
            !rendered.contains("Invoice #5"),
            "subject leaked into projection"
        );
        assert!(
            !rendered.contains("jane@business-1194228da8.test"),
            "email address leaked"
        );
        Ok(())
    }

    #[test]
    fn live_client_uses_request_mailbox_ref() -> Result<(), String> {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({"threads":[{"id":"thread-aaa"}]}),
            serde_json::json!({"id":"thread-aaa","messages":[{"id":"msg-aaa","threadId":"thread-aaa","labelIds":["INBOX"],"payload":{"headers":[
                {"name":"From","value":"Jane Roe <jane@business-1194228da8.test>"},
                {"name":"Subject","value":"Hello"}
            ]}}]}),
        ]));
        let client = LiveGmailInboxReadClient::for_test(http, "test-access-token".to_string());
        let ctx = client
            .read_context(&GmailInboxReadRequest {
                mailbox_ref: Some("gmail:mailbox:user:jordan".to_string()),
                ..GmailInboxReadRequest::default()
            })
            .map_err(|e| e.to_string())?;

        assert_eq!(
            ctx.threads[0].mailbox_ref,
            "gmail:mailbox:user:jordan".to_string()
        );
        Ok(())
    }

    #[test]
    fn live_client_metadata_request_shape() -> Result<(), String> {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({"threads":[{"id":"thread-aaa"}]}),
            serde_json::json!({"id":"thread-aaa","messages":[{"id":"msg-aaa","threadId":"thread-aaa","labelIds":["INBOX"],"payload":{"headers":[{"name":"From","value":"j@business-1194228da8.test"},{"name":"Subject","value":"hi"}]}}]}),
        ]));
        let client = LiveGmailInboxReadClient::for_test(http.clone(), "tok".to_string());
        let _ = client
            .read_context(&GmailInboxReadRequest::default())
            .map_err(|e| e.to_string())?;
        let calls = http.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            calls.iter().any(|u| u.contains("format=metadata")),
            "must request metadata only"
        );
        assert!(
            calls
                .iter()
                .any(|u| u.contains("/threads/") && u.contains("metadataHeaders=From")),
            "threads.get must include metadataHeaders=From"
        );
        assert!(
            calls.iter().all(|u| !u.contains("format=full")),
            "must never request full bodies"
        );
        assert!(
            calls.iter().all(|u| !u.contains("/messages/")),
            "must use threads.get, not messages.get"
        );
        Ok(())
    }

    #[test]
    fn live_client_encodes_list_query_and_repeats_label_ids() -> Result<(), String> {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({"threads":[{"id":"thread/aaa"}]}),
            serde_json::json!({"id":"thread/aaa","messages":[{"id":"msg-aaa","threadId":"thread/aaa","labelIds":["INBOX"],"payload":{"headers":[{"name":"From","value":"j@business-1194228da8.test"},{"name":"Subject","value":"hi"}]}}]}),
        ]));
        let client = LiveGmailInboxReadClient::for_test(http.clone(), "tok".to_string());
        let _ = client
            .read_context(&GmailInboxReadRequest {
                label_ids: vec!["INBOX".into(), "Label With Space".into()],
                cursor: Some("token+/=".into()),
                ..GmailInboxReadRequest::default()
            })
            .map_err(|e| e.to_string())?;
        let calls = http.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let list_url = calls.first().ok_or("missing list request")?;
        assert!(list_url.contains("labelIds=INBOX"));
        assert!(list_url.contains("labelIds=Label%20With%20Space"));
        assert!(!list_url.contains("labelIds=INBOX,"));
        assert!(list_url.contains("pageToken=token%2B%2F%3D"));
        assert!(
            calls.iter().any(|u| u.contains("/threads/thread%2Faaa?")),
            "thread id path segment must be encoded"
        );
        Ok(())
    }

    #[test]
    fn live_client_full_read_read_uses_query_and_full_message_body() -> Result<(), String> {
        let body = URL_SAFE_NO_PAD.encode("Ruby summary: call Jamie back at noon.".as_bytes());
        let html = URL_SAFE_NO_PAD
            .encode("<html><body><p>Ruby summary: call Jamie back at noon.</p></body></html>");
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({"messages":[{"id":"msg-ruby-1"}]}),
            serde_json::json!({
                "id":"msg-ruby-1",
                "internalDate":"1780759900000",
                "snippet":"snippet fallback",
                "payload":{
                    "mimeType":"multipart/alternative",
                    "headers":[{"name":"Subject","value":"Ruby call summary"}],
                    "parts":[{
                        "mimeType":"text/plain",
                        "body":{"data": body}
                    },{
                        "mimeType":"text/html",
                        "body":{"data": html}
                    },{
                        "partId":"1",
                        "filename":"invoice.pdf",
                        "mimeType":"application/pdf",
                        "headers":[{"name":"Content-Disposition","value":"attachment; filename=\"invoice.pdf\""}],
                        "body":{"attachmentId":"att-1","size":1234}
                    }]
                }
            }),
        ]));
        let client = LiveGmailInboxReadClient::for_test(http.clone(), "tok".to_string());

        let page = client
            .read_full_messages_page(&GmailFullReadRequest {
                query: "label:\"Ruby Summary\" after:1780759000".to_string(),
                max_messages: 5,
                page_token: None,
            })
            .map_err(|e| e.to_string())?;
        let messages = page.messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, "msg-ruby-1");
        assert_eq!(messages[0].subject.as_deref(), Some("Ruby call summary"));
        assert_eq!(
            messages[0].plain_text_body,
            "Ruby summary: call Jamie back at noon."
        );
        assert_eq!(
            messages[0].html_body.as_deref(),
            Some("<html><body><p>Ruby summary: call Jamie back at noon.</p></body></html>")
        );
        assert_eq!(messages[0].attachments.len(), 1);
        assert_eq!(messages[0].attachments[0].attachment_id, "att-1");
        assert_eq!(messages[0].attachments[0].filename, "invoice.pdf");
        assert_eq!(
            messages[0].attachments[0].mime_type.as_deref(),
            Some("application/pdf")
        );
        assert_eq!(messages[0].attachments[0].size_bytes, Some(1234));
        assert_eq!(messages[0].internal_date_epoch_ms, Some(1_780_759_900_000));
        let calls = http.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            calls
                .iter()
                .any(|url| url.contains("/messages?") && url.contains("q=label%3A%22Ruby")),
            "messages.list must use the configured query"
        );
        assert!(
            calls
                .iter()
                .any(|url| url.contains("/messages/msg-ruby-1?format=full")),
            "Ruby summary read must fetch full body for the configured listener"
        );
        Ok(())
    }

    #[test]
    fn live_client_full_read_returns_and_accepts_message_page_tokens() -> Result<(), String> {
        let full_message = |id: &str| {
            serde_json::json!({
                "id": id,
                "snippet": format!("body for {id}"),
                "payload": {"headers": []}
            })
        };
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![
            serde_json::json!({
                "messages": [{"id":"msg-1"}, {"id":"msg-2"}],
                "nextPageToken":"page+/2"
            }),
            full_message("msg-1"),
            full_message("msg-2"),
            serde_json::json!({"messages":[{"id":"msg-3"}]}),
            full_message("msg-3"),
        ]));
        let client = LiveGmailInboxReadClient::for_test(http.clone(), "tok".to_string());

        let first = client
            .read_full_messages_page(&GmailFullReadRequest {
                query: "in:inbox newer_than:14d".to_string(),
                max_messages: 500,
                page_token: None,
            })
            .map_err(|error| error.to_string())?;
        assert_eq!(first.next_page_token.as_deref(), Some("page+/2"));
        let second = client
            .read_full_messages_page(&GmailFullReadRequest {
                query: "in:inbox newer_than:14d".to_string(),
                max_messages: 500,
                page_token: first.next_page_token,
            })
            .map_err(|error| error.to_string())?;

        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-1", "msg-2"]
        );
        assert_eq!(second.messages[0].message_id, "msg-3");
        assert_eq!(second.next_page_token, None);
        let calls = http.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(calls[0].contains("maxResults=500"));
        assert!(calls[3].contains("maxResults=500"));
        assert!(calls[3].contains("pageToken=page%2B%2F2"));
        Ok(())
    }

    #[test]
    fn live_client_full_read_rejects_a_repeated_page_token() {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![serde_json::json!({
            "messages": [], "nextPageToken": "same"
        })]));
        let client = LiveGmailInboxReadClient::for_test(http, "tok".to_string());

        let error = client
            .read_full_messages_page(&GmailFullReadRequest {
                query: "in:inbox newer_than:14d".to_string(),
                max_messages: 500,
                page_token: Some("same".to_string()),
            })
            .expect_err("repeated token must fail");

        assert_eq!(error.code(), "gmail_messages_page_token_repeated");
    }

    #[test]
    fn list_label_names_maps_ids_to_display_names() {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![serde_json::json!({
            "labels": [
                {"id": "INBOX", "name": "INBOX", "type": "system"},
                {"id": "Label_13", "name": "user@example.test", "type": "user"},
                {"id": "Label_7", "name": "Ruby Summary", "type": "user"}
            ]
        })]));
        let client = LiveGmailInboxReadClient::for_test(http.clone(), "tok".to_string());
        let map = client.list_label_names().expect("labels");
        assert_eq!(
            map.get("Label_13").map(String::as_str),
            Some("user@example.test")
        );
        assert_eq!(map.get("INBOX").map(String::as_str), Some("INBOX"));
        let calls = http.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(calls.iter().any(|url| url.ends_with("/labels")));
    }

    #[test]
    fn missing_scope_errors_without_network() {
        let http = std::sync::Arc::new(FakeGmailHttp::new(vec![]));
        let creds = test_oauth(vec!["https://www.googleapis.com/auth/gmail.send".into()]);
        let result = LiveGmailInboxReadClient::from_credentials(http.clone(), &creds);
        assert!(result.is_err());
        assert_eq!(
            http.call_count(),
            0,
            "must not hit network when scope is missing"
        );
    }
}
