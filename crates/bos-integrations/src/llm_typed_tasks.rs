//! Typed LLM task contract: bounded typed transforms (typed input → one narrow
//! transform → typed output) plus the credential scrub applied at the
//! external-LLM input boundary. Ported from agent-monitor-rust
//! (`dm-agents::typed_tasks` + `dm-business::llm_input_scrubber` +
//! `dm-app::typed_task_input_scrub`), with `dm_kernel` → `bos_kernel`.
//!
//! No env reads here; backends receive these structs from bos-app (`llm.rs`).

use bos_kernel::{AppError, AppResult, CorrelationId};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskRequest {
    pub task_id: String,
    pub correlation_id: String,
    pub idempotency_key: String,
    pub tenant_or_project_scope: String,
    pub source_entity: Option<TypedLlmSourceEntity>,
    pub spec: TypedLlmTaskSpec,
    pub input: TypedLlmTaskInput,
    pub execution_policy: TypedLlmExecutionPolicy,
    pub provider_policy: TypedLlmProviderPolicy,
    pub safety_policy: TypedLlmSafetyPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmSourceEntity {
    pub entity_kind: String,
    pub entity_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskSpec {
    pub task_class: TypedLlmTaskClass,
    pub prompt_template_id: String,
    pub prompt_template_version: String,
    pub prompt_template_hash: String,
    pub schema_ref: String,
    pub response_format: TypedLlmResponseFormat,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_tokens: u32,
    pub timeout_ms: u64,
    pub capabilities: TypedLlmTaskCapabilities,
    pub authority: TypedLlmAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskInput {
    pub json: Value,
    #[serde(default)]
    pub text_blocks: Vec<TypedLlmTextBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTextBlock {
    pub block_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmTaskClass {
    Classify,
    Extract,
    Summarize,
    Draft,
    Rewrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmExecutionRoute {
    DirectApi,
    Harness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmResponseFormat {
    JsonObject,
    JsonSchema,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskCapabilities {
    pub filesystem: bool,
    pub tools: bool,
    pub browser: bool,
    pub rag: bool,
    pub multi_step: bool,
    pub provider_write_proposal: bool,
    pub network_fetch: bool,
}

impl TypedLlmTaskCapabilities {
    pub fn pure_transformation() -> Self {
        Self {
            filesystem: false,
            tools: false,
            browser: false,
            rag: false,
            multi_step: false,
            provider_write_proposal: false,
            network_fetch: false,
        }
    }

    pub fn requires_harness(&self) -> bool {
        self.filesystem
            || self.tools
            || self.browser
            || self.rag
            || self.multi_step
            || self.provider_write_proposal
            || self.network_fetch
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmAuthority {
    pub side_effects_forbidden: bool,
    pub provider_writes_enabled: bool,
}

impl TypedLlmAuthority {
    pub fn no_side_effects() -> Self {
        Self {
            side_effects_forbidden: true,
            provider_writes_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmExecutionPolicy {
    pub default_route: TypedLlmExecutionRoute,
    pub fallback_policy: TypedLlmFallbackPolicy,
    pub retry_policy: TypedLlmRetryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmFallbackPolicy {
    NoFallback,
    DirectProviderFallback,
    HarnessFallback,
    FailClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmRetryPolicy {
    pub max_attempts: u8,
    pub backoff_ms: u64,
    pub max_elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmProviderPolicy {
    pub preferred_provider: String,
    pub preferred_model: String,
    pub fallback_provider: Option<String>,
    pub fallback_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmRedactionPolicy {
    PreSubmit,
    PostResponse,
    PreAndPost,
    StrictAllowlist,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmSafetyPolicy {
    pub redaction_policy: TypedLlmRedactionPolicy,
    pub raw_output_retention: TypedLlmRawOutputRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedLlmRawOutputRetention {
    None,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskOutputEnvelope {
    pub task_id: String,
    pub execution_route: TypedLlmExecutionRoute,
    pub provider_id: String,
    pub model: String,
    pub schema_ref: String,
    pub raw_response_hash: String,
    pub response_json: Value,
    pub usage: Option<TypedLlmUsage>,
    pub finish_reason: Option<String>,
    pub latency_ms: u64,
    pub retry_count: u8,
    pub provider_request_id: Option<String>,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedLlmTaskRequestFingerprint {
    pub request_fingerprint_sha256: String,
    pub input_fingerprint_sha256: String,
}

pub fn typed_llm_task_input_fingerprint_sha256(input: &TypedLlmTaskInput) -> String {
    let payload = FingerprintInputPayload {
        json: normalize_json_value(&input.json),
        text_blocks: &input.text_blocks,
    };
    sha256_hex(canonical_json_bytes(payload))
}

pub fn typed_llm_task_request_fingerprint_sha256(request: &TypedLlmTaskRequest) -> String {
    let canonical_bytes = canonical_json_bytes(FingerprintRequestPayload {
        idempotency_key: &request.idempotency_key,
        tenant_or_project_scope: &request.tenant_or_project_scope,
        source_entity: &request.source_entity,
        spec: &request.spec,
        input: FingerprintInputPayload {
            json: normalize_json_value(&request.input.json),
            text_blocks: &request.input.text_blocks,
        },
        execution_policy: &request.execution_policy,
        provider_policy: &request.provider_policy,
        safety_policy: &request.safety_policy,
    });
    sha256_hex(canonical_bytes)
}

pub fn typed_llm_task_request_fingerprints(
    request: &TypedLlmTaskRequest,
) -> TypedLlmTaskRequestFingerprint {
    TypedLlmTaskRequestFingerprint {
        request_fingerprint_sha256: typed_llm_task_request_fingerprint_sha256(request),
        input_fingerprint_sha256: typed_llm_task_input_fingerprint_sha256(&request.input),
    }
}

fn canonical_json_bytes<T: Serialize>(value: T) -> Vec<u8> {
    let json = serde_json::to_value(value).unwrap_or(Value::Null);
    let normalized = normalize_json_value(&json);
    serde_json::to_vec(&normalized).unwrap_or_else(|_| b"null".to_vec())
}

#[derive(Serialize)]
struct FingerprintInputPayload<'a> {
    json: Value,
    text_blocks: &'a [TypedLlmTextBlock],
}

#[derive(Serialize)]
struct FingerprintRequestPayload<'a> {
    idempotency_key: &'a str,
    tenant_or_project_scope: &'a str,
    source_entity: &'a Option<TypedLlmSourceEntity>,
    spec: &'a TypedLlmTaskSpec,
    input: FingerprintInputPayload<'a>,
    execution_policy: &'a TypedLlmExecutionPolicy,
    provider_policy: &'a TypedLlmProviderPolicy,
    safety_policy: &'a TypedLlmSafetyPolicy,
}

fn normalize_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut normalized = serde_json::Map::with_capacity(keys.len());
            for key in keys {
                if let Some(child) = map.get(key) {
                    normalized.insert(key.clone(), normalize_json_value(child));
                }
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_json_value).collect()),
        _ => value.clone(),
    }
}

fn sha256_hex(bytes: Vec<u8>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Credential scrub at the external-LLM input boundary.
//
// Posture: CREDENTIAL-ONLY. Redacts credential-shaped tokens (API keys,
// bearer/OAuth tokens, private keys, `Authorization:` headers, DSNs with
// embedded `user:pass@`, key-name-gated `secret=...` assignments). Ordinary
// PII (emails, phones, names) passes through — triage and contact extraction
// need it, and it carries no credential risk.
// ---------------------------------------------------------------------------

/// Count of redactions performed, grouped by credential kind. Never carries
/// the redacted values themselves — safe to log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubReport {
    pub total: usize,
    pub by_kind: BTreeMap<&'static str, usize>,
}

impl ScrubReport {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Stable, value-free summary for logs, e.g. `credential=2,private_key=1`.
    pub fn summary(&self) -> String {
        self.by_kind
            .iter()
            .map(|(kind, count)| format!("{kind}={count}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// A single detection rule: a credential kind, the pattern that matches it,
/// and a replacement template. `$1`/`$2` preserve captured context (e.g. the
/// scheme of a DSN, or the key name of an assignment) while redacting only the
/// credential value. `{M}` is substituted with the kinded marker text.
struct Rule {
    kind: &'static str,
    re: &'static Regex,
    template: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // Multi-line private key blocks first (greedy to END), then a
            // truncated BEGIN with no END (e.g. body got bounded mid-key).
            Rule {
                kind: "private_key",
                re: re(r"(?s)-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----.*?-----END (?:[A-Z]+ )*PRIVATE KEY-----"),
                template: "{M}",
            },
            Rule {
                kind: "private_key",
                re: re(r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----[A-Za-z0-9+/=\s]*"),
                template: "{M}",
            },
            // Whole Authorization / Proxy-Authorization header value.
            Rule {
                kind: "authorization_header",
                re: re(r"(?i)\b(?:proxy-)?authorization\s*:\s*[^\r\n]+"),
                template: "{M}",
            },
            // JWTs (header.payload.signature).
            Rule {
                kind: "jwt",
                re: re(r"\beyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"),
                template: "{M}",
            },
            // DSN / connection string with embedded user:pass@ — keep the scheme.
            Rule {
                kind: "connection_string_credential",
                re: re(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s:/@]+:[^\s:/@]+@"),
                template: "${1}{M}@",
            },
            // Stripe live/test/restricted keys.
            Rule {
                kind: "credential",
                re: re(r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{8,}"),
                template: "{M}",
            },
            // OpenAI / Anthropic / OpenRouter style keys (sk-, sk-ant-, sk-or-).
            Rule {
                kind: "credential",
                re: re(r"\bsk-(?:ant-|or-)?[A-Za-z0-9_-]{16,}"),
                template: "{M}",
            },
            // GitHub tokens / PATs.
            Rule {
                kind: "credential",
                re: re(r"\b(?:gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,})"),
                template: "{M}",
            },
            // Slack tokens.
            Rule {
                kind: "credential",
                re: re(r"\bxox[bpars]-[A-Za-z0-9-]{8,}"),
                template: "{M}",
            },
            // AWS access key IDs.
            Rule {
                kind: "aws_access_key",
                re: re(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
                template: "{M}",
            },
            // Google API keys.
            Rule {
                kind: "credential",
                re: re(r"\bAIza[A-Za-z0-9_-]{10,}"),
                template: "{M}",
            },
            // Google OAuth access tokens.
            Rule {
                kind: "credential",
                re: re(r"\bya29\.[A-Za-z0-9_-]{10,}"),
                template: "{M}",
            },
            // Bearer / Basic credentials (when not already inside an Authorization header).
            Rule {
                kind: "credential",
                re: re(r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{6,}"),
                template: "{M}",
            },
            // Azure SAS signature query parameter (e.g. ...&sig=base64...).
            Rule {
                kind: "credential",
                re: re(r"(?i)([?&]sig=)[A-Za-z0-9%/+=_-]{16,}"),
                template: "${1}{M}",
            },
            // Key-name-gated assignment: only fires when the KEY is credential-y,
            // so `order_id=AB12CD34` is untouched but `api_key=AB12CD34` is. The
            // value class excludes brackets so it never re-redacts an existing
            // marker. Keeps the key name for context, redacts the value.
            Rule {
                kind: "credential_assignment",
                re: re(
                    r#"(?i)\b(password|passwd|pwd|secret|api[_-]?key|client[_-]?secret|access[_-]?token|refresh[_-]?token|auth[_-]?token|aws_secret_access_key|account[_-]?key)(\s*[:=]\s*)["']?([^\s"',;\[\]]{6,})"#,
                ),
                template: "${1}${2}{M}",
            },
        ]
    })
}

fn re(pattern: &str) -> &'static Regex {
    // Patterns are a small fixed set compiled once inside `rules()`; leak each
    // compiled Regex so the &'static reference lives for the process.
    Box::leak(Box::new(
        Regex::new(pattern).expect("llm input scrubber regex compiles"),
    ))
}

/// Scrub for the external-LLM input boundary: kinded `[REDACTED:<kind>]`
/// markers + a [`ScrubReport`].
pub fn scrub_llm_input(value: &str) -> (String, ScrubReport) {
    let mut current = value.to_string();
    let mut report = ScrubReport::default();
    for rule in rules() {
        let hits = rule.re.find_iter(&current).count();
        if hits == 0 {
            continue;
        }
        let replacement = rule
            .template
            .replace("{M}", &format!("[REDACTED:{}]", rule.kind));
        current = rule
            .re
            .replace_all(&current, replacement.as_str())
            .into_owned();
        *report.by_kind.entry(rule.kind).or_insert(0) += hits;
        report.total += hits;
    }
    (current, report)
}

/// Return a sanitized clone of `request` with credential-shaped content
/// redacted from every INPUT surface (`input.json` recursively + each
/// `text_blocks` entry). Logs a value-free [`ScrubReport`] summary when
/// anything was redacted. Used by the direct-API egress, where only `input`
/// reaches the provider.
pub fn sanitize_typed_task_request(request: &TypedLlmTaskRequest) -> TypedLlmTaskRequest {
    let (sanitized, report) = scrub_request(request);
    if !report.is_empty() {
        // Log only value-free fields: task_id/correlation_id/tenant are
        // caller-controlled and can themselves carry credential-shaped content.
        tracing::warn!(
            schema_ref = %request.spec.schema_ref,
            route = ?request.execution_policy.default_route,
            redacted_total = report.total,
            redacted_kinds = %report.summary(),
            "redacted credential-shaped content from typed LLM input before provider egress"
        );
    }
    sanitized
}

/// Core input-scrub transform: returns the sanitized clone and the report.
pub fn scrub_request(request: &TypedLlmTaskRequest) -> (TypedLlmTaskRequest, ScrubReport) {
    let mut sanitized = request.clone();
    let mut report = ScrubReport::default();
    scrub_value(&mut sanitized.input.json, &mut report);
    for block in &mut sanitized.input.text_blocks {
        let (scrubbed, block_report) = scrub_llm_input(&block.text);
        block.text = scrubbed;
        merge(&mut report, block_report);
    }
    (sanitized, report)
}

/// Return a FULLY sanitized clone for the HARNESS egress, where the tmux
/// runner serializes the WHOLE request into the prompt, not just `input`.
/// Every string leaf of the serialized request is scrubbed (metadata included:
/// task_id, tenant_or_project_scope, source_entity, idempotency_key, ...).
/// Fail-closed: a serialize/deserialize error returns Err so the harness call
/// is dropped rather than sending unsanitized bytes.
///
/// Receipts/idempotency are computed by callers from the ORIGINAL request, so
/// scrubbing identifiers here only affects the prompt projection, not dedup.
pub fn sanitize_typed_task_request_full(
    request: &TypedLlmTaskRequest,
) -> AppResult<(TypedLlmTaskRequest, ScrubReport)> {
    let mut value = serde_json::to_value(request).map_err(|error| {
        AppError::unexpected(
            "direct_llm_input_scrub_failed",
            format!("failed to encode request for scrubbing: {error}"),
            CorrelationId::generate(),
        )
    })?;
    let report = scrub_json_in_place(&mut value);
    let sanitized = serde_json::from_value(value).map_err(|error| {
        AppError::unexpected(
            "direct_llm_input_scrub_failed",
            format!("failed to decode scrubbed request: {error}"),
            CorrelationId::generate(),
        )
    })?;
    Ok((sanitized, report))
}

/// Scrub a JSON value in place (recursing into all string leaves), returning
/// the report. Also used for tool-turn history (tool arguments + outputs)
/// that is echoed back to the provider on subsequent turns, which does
/// not pass through `request.input`.
pub fn scrub_json_in_place(value: &mut Value) -> ScrubReport {
    let mut report = ScrubReport::default();
    scrub_value(value, &mut report);
    report
}

fn scrub_value(value: &mut Value, report: &mut ScrubReport) {
    match value {
        Value::String(text) => {
            let (scrubbed, r) = scrub_llm_input(text);
            *text = scrubbed;
            merge(report, r);
        }
        Value::Array(items) => {
            for item in items {
                scrub_value(item, report);
            }
        }
        Value::Object(map) => {
            for (_key, val) in map.iter_mut() {
                scrub_value(val, report);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn merge(into: &mut ScrubReport, from: ScrubReport) {
    into.total += from.total;
    for (kind, count) in from.by_kind {
        *into.by_kind.entry(kind).or_insert(0) += count;
    }
}

#[cfg(test)]
pub(crate) fn sample_typed_task_request() -> TypedLlmTaskRequest {
    use serde_json::json;

    TypedLlmTaskRequest {
        task_id: "task-1".to_string(),
        correlation_id: "corr-1".to_string(),
        idempotency_key: "idem-1".to_string(),
        tenant_or_project_scope: "tenant-a".to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: "email_thread".to_string(),
            entity_id: "thread-1".to_string(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Classify,
            prompt_template_id: "email_triage_direct.v1".to_string(),
            prompt_template_version: "v1".to_string(),
            prompt_template_hash: "0123456789abcdef".to_string(),
            schema_ref: "email.triage_result.v1".to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 8192,
            max_output_bytes: 8192,
            max_tokens: 512,
            timeout_ms: 30_000,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({"subject": "Hello", "priority": "high"}),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "body".to_string(),
                text: "hello".to_string(),
            }],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::DirectApi,
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
                max_elapsed_ms: 0,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: "openrouter".to_string(),
            preferred_model: "gpt-4o-mini".to_string(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreAndPost,
            raw_output_retention: TypedLlmRawOutputRetention::LocalOnly,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pure_transformation_capabilities_do_not_require_harness() {
        assert!(!TypedLlmTaskCapabilities::pure_transformation().requires_harness());
    }

    #[test]
    fn tool_or_browser_capability_requires_harness() {
        let mut capabilities = TypedLlmTaskCapabilities::pure_transformation();
        capabilities.browser = true;

        assert!(capabilities.requires_harness());
    }

    #[test]
    fn provider_write_proposal_capability_requires_harness() {
        let mut capabilities = TypedLlmTaskCapabilities::pure_transformation();
        capabilities.provider_write_proposal = true;

        assert!(capabilities.requires_harness());
    }

    #[test]
    fn no_side_effect_authority_disables_provider_writes() {
        let authority = TypedLlmAuthority::no_side_effects();

        assert!(authority.side_effects_forbidden);
        assert!(!authority.provider_writes_enabled);
    }

    #[test]
    fn input_fingerprint_is_stable_for_equivalent_json_key_order() {
        let input_a = TypedLlmTaskInput {
            json: json!({"b": 2, "a": {"d": 4, "c": 3}}),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "main".to_string(),
                text: "hello".to_string(),
            }],
        };
        let input_b = TypedLlmTaskInput {
            json: json!({"a": {"c": 3, "d": 4}, "b": 2}),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "main".to_string(),
                text: "hello".to_string(),
            }],
        };

        assert_eq!(
            typed_llm_task_input_fingerprint_sha256(&input_a),
            typed_llm_task_input_fingerprint_sha256(&input_b)
        );
    }

    #[test]
    fn request_fingerprint_changes_when_scope_or_idempotency_changes() {
        let mut request = sample_typed_task_request();
        let baseline = typed_llm_task_request_fingerprint_sha256(&request);

        request.tenant_or_project_scope = "tenant-b".to_string();
        let scope_changed = typed_llm_task_request_fingerprint_sha256(&request);
        assert_ne!(baseline, scope_changed);

        request = sample_typed_task_request();
        request.idempotency_key = "new-idempotency".to_string();
        let idempotency_changed = typed_llm_task_request_fingerprint_sha256(&request);
        assert_ne!(baseline, idempotency_changed);
    }

    #[test]
    fn request_fingerprint_ignores_volatile_run_identifiers() {
        let request = sample_typed_task_request();
        let mut retried = sample_typed_task_request();
        retried.task_id = "task-retry-2".to_string();
        retried.correlation_id = "corr-retry-2".to_string();

        assert_eq!(
            typed_llm_task_request_fingerprint_sha256(&request),
            typed_llm_task_request_fingerprint_sha256(&retried)
        );
    }

    #[test]
    fn input_fingerprint_changes_when_raw_text_changes() {
        let mut request = sample_typed_task_request();
        let baseline = typed_llm_task_input_fingerprint_sha256(&request.input);

        request.input.text_blocks[0].text = "hello world".to_string();
        let changed = typed_llm_task_input_fingerprint_sha256(&request.input);

        assert_ne!(baseline, changed);
    }

    #[test]
    fn fingerprints_pair_input_and_request_hashes() {
        let request = sample_typed_task_request();
        let pair = typed_llm_task_request_fingerprints(&request);

        assert_eq!(
            pair.request_fingerprint_sha256,
            typed_llm_task_request_fingerprint_sha256(&request)
        );
        assert_eq!(
            pair.input_fingerprint_sha256,
            typed_llm_task_input_fingerprint_sha256(&request.input)
        );
    }

    fn assert_clean(scrubbed: &str, forbidden: &[&str]) {
        for needle in forbidden {
            assert!(
                !scrubbed.contains(needle),
                "leaked {needle:?} in {scrubbed:?}"
            );
        }
    }

    #[test]
    fn redacts_known_secret_shapes_kinded() {
        let raw = "Authorization: Bearer AbCdEfGhIjKlMnOpQrStUvWxYz123456\n\
            stripe sk_live_1234567890abcdef\n\
            aws AKIAIOSFODNN7EXAMPLE\n\
            github ghp_1234567890abcdef1234567890abcdef1234\n\
            -----BEGIN PRIVATE KEY-----\nabcdef\n-----END PRIVATE KEY-----";
        let (scrubbed, report) = scrub_llm_input(raw);

        assert!(scrubbed.contains("[REDACTED:"));
        assert_clean(
            &scrubbed,
            &[
                "AbCdEfGhIjKlMnOpQrStUvWxYz123456",
                "sk_live_1234567890abcdef",
                "AKIAIOSFODNN7EXAMPLE",
                "ghp_1234567890abcdef1234567890abcdef1234",
                "BEGIN PRIVATE KEY",
            ],
        );
        assert!(report.total >= 5, "report={report:?}");
        assert_eq!(report.by_kind.get("private_key"), Some(&1));
        assert_eq!(report.by_kind.get("aws_access_key"), Some(&1));
        assert_eq!(report.by_kind.get("authorization_header"), Some(&1));
    }

    #[test]
    fn redacts_jwt_and_dsn_and_assignment() {
        let raw = "token eyJhbGciOi.eyJzdWIiOi.SflKxwRJ \
            db postgres://user:s3cretP@db.example.com/app \
            cfg password=Tr0ub4dor3x api_key: ABCDEF123456";
        let (scrubbed, report) = scrub_llm_input(raw);

        assert_clean(
            &scrubbed,
            &[
                "eyJhbGciOi.eyJzdWIiOi.SflKxwRJ",
                "s3cretP",
                "Tr0ub4dor3x",
                "ABCDEF123456",
            ],
        );
        // DSN scheme + host preserved, only creds gone.
        assert!(scrubbed.contains("postgres://"));
        assert!(scrubbed.contains("@db.example.com"));
        // Key names preserved for context.
        assert!(scrubbed.contains("password="));
        assert!(scrubbed.contains("api_key:"));
        assert!(report.by_kind.contains_key("jwt"));
        assert!(report.by_kind.contains_key("connection_string_credential"));
        assert!(report.by_kind.contains_key("credential_assignment"));
    }

    #[test]
    fn allows_pii_and_benign_lookalikes() {
        // Emails, phones, names, order ids, tracking numbers must survive.
        let raw = "From: Jane Smith <jane.smith@business-1194228da8.test> phone +1-555-123-4567. \
            order_id=AB12CD34EF tracking 1Z999AA10123456784 invoice #INV-2026-0042";
        let (scrubbed, report) = scrub_llm_input(raw);

        assert_eq!(
            scrubbed, raw,
            "PII/benign content must pass through unchanged"
        );
        assert!(report.is_empty(), "report={report:?}");
    }

    #[test]
    fn idempotent_no_marker_recursion() {
        let raw = "api_key=AKIAIOSFODNN7EXAMPLE";
        let (once, _) = scrub_llm_input(raw);
        let (twice, report2) = scrub_llm_input(&once);
        assert_eq!(once, twice, "second pass must be a no-op");
        assert!(
            report2.is_empty(),
            "second pass redacts nothing: {report2:?}"
        );
    }

    #[test]
    fn scrubs_json_and_text_blocks() {
        let mut request = sample_typed_task_request();
        request.input.json = json!({
            "subject": "Bearer AbCdEfGhIjKlMnOpQr123456",
            "body": "key sk_live_1234567890abcdef end",
        });
        request.input.text_blocks[0].text =
            "prompt with sk_live_1234567890abcdef inside".to_string();

        let (sanitized, report) = scrub_request(&request);

        let json_str = serde_json::to_string(&sanitized.input.json).unwrap();
        assert!(!json_str.contains("sk_live_1234567890abcdef"), "{json_str}");
        assert!(!json_str.contains("AbCdEfGhIjKlMnOpQr123456"), "{json_str}");
        let block = &sanitized.input.text_blocks[0].text;
        assert!(!block.contains("sk_live_1234567890abcdef"), "{block}");
        assert!(block.contains("[REDACTED:"));
        assert!(report.total >= 2, "report={report:?}");
    }

    #[test]
    fn full_scrub_covers_request_metadata_not_just_input() {
        let mut request = sample_typed_task_request();
        request.tenant_or_project_scope =
            "tenant with token sk_live_1234567890abcdef inside".to_string();

        let (sanitized, report) =
            sanitize_typed_task_request_full(&request).expect("full scrub succeeds");

        assert!(
            !sanitized
                .tenant_or_project_scope
                .contains("sk_live_1234567890abcdef"),
            "{}",
            sanitized.tenant_or_project_scope
        );
        assert!(!report.is_empty());
    }

    #[test]
    fn full_scrub_is_noop_for_clean_request() {
        let request = sample_typed_task_request();

        let (sanitized, report) =
            sanitize_typed_task_request_full(&request).expect("full scrub succeeds");

        assert_eq!(sanitized, request);
        assert!(report.is_empty(), "report={report:?}");
    }
}
