//! Typed-LLM spine: config + routing + the execute entrypoint for bounded
//! typed LLM transforms.
//!
//! Three backends (transport machinery lives in bos-integrations):
//! - **Api** — direct API calls ([`bos_integrations::llm_api`]:
//!   anthropic | openai | openrouter), billed per token.
//! - **Harness** — local Claude CLI in a tmux session
//!   ([`bos_integrations::llm_harness`]), billed to the operator subscription.
//! - **Local** — loopback-only OpenAI-compatible inference (Ollama/LM Studio).
//!
//! Routing: `BOS_LLM_DEFAULT_BACKEND=api|harness|local` chooses the default backend.
//! `BOS_LLM_DEFAULT_MODEL` supplies the default model, per-backend model keys
//! refine it, and `BOS_LLM_ROUTE_OVERRIDES` (comma list
//! `purpose=api|harness|local[:model]`) wins per purpose. Selecting harness in any
//! route makes the local harness available.
//!
//! Execution is blocking; callers run it off the async path (worker threads).

use bos_integrations::llm_api::anthropic::{AnthropicDirectLlmClient, AnthropicDirectLlmConfig};
use bos_integrations::llm_api::{
    DirectLlmClient, OpenAiCompatibleDirectLlmClient, OpenAiCompatibleDirectLlmConfig,
};
use bos_integrations::llm_harness::{
    execute_harness_typed_task, HarnessRuntimeConfig, JsonObjectOutputValidator,
    TmuxCliSessionBackend, TmuxHarnessTypedTaskRunner, PROVIDER_ID_CLAUDE,
};
use bos_integrations::llm_typed_tasks::{
    sanitize_typed_task_request, TypedLlmExecutionRoute, TypedLlmTaskOutputEnvelope,
    TypedLlmTaskRequest,
};
use bos_kernel::{AiCallUsageSink, AppError, AppResult, CorrelationId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::env_registry;

const ANTHROPIC_DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
const OPENROUTER_DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const LOCAL_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434/v1/chat/completions";
const HARNESS_RUN_SUBDIR: &str = "llm-harness";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmBackend {
    Api,
    Harness,
    Local,
}

impl LlmBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Harness => "harness",
            Self::Local => "local",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmApiProvider {
    Anthropic,
    OpenAi,
    OpenRouter,
}

impl LlmApiProvider {
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Anthropic => ANTHROPIC_DEFAULT_ENDPOINT,
            Self::OpenAi => OPENAI_DEFAULT_ENDPOINT,
            Self::OpenRouter => OPENROUTER_DEFAULT_ENDPOINT,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRouteOverride {
    pub backend: LlmBackend,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLlmRoute {
    pub backend: LlmBackend,
    pub model: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LlmRuntimeConfig {
    pub api_provider: LlmApiProvider,
    pub api_key: Option<String>,
    pub api_model: Option<String>,
    /// Override base URL; `None` = the provider's default endpoint.
    pub api_endpoint: Option<String>,
    pub local_api_key: Option<String>,
    pub local_endpoint: String,
    pub local_model: Option<String>,
    pub default_backend: LlmBackend,
    pub default_model: Option<String>,
    pub harness_enabled: bool,
    pub harness_program: String,
    pub harness_model: Option<String>,
    pub harness_thinking_level: Option<String>,
    /// Default max output tokens applied when a request leaves spec.max_tokens at 0.
    pub max_tokens: u32,
    /// Default per-task timeout applied when a request leaves spec.timeout_ms at 0.
    pub timeout_ms: u64,
    /// Per-purpose backend/model pins; win over the configured defaults.
    pub route_overrides: BTreeMap<String, LlmRouteOverride>,
    /// Root for the harness's ephemeral per-task run dirs.
    pub harness_result_root: PathBuf,
}

impl std::fmt::Debug for LlmRuntimeConfig {
    // Hand-written so a stray `{:?}` cannot dump the api_key.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmRuntimeConfig")
            .field("api_provider", &self.api_provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("api_model", &self.api_model)
            .field("api_endpoint", &self.api_endpoint)
            .field(
                "local_api_key",
                &self.local_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("local_endpoint", &self.local_endpoint)
            .field("local_model", &self.local_model)
            .field("default_backend", &self.default_backend)
            .field("default_model", &self.default_model)
            .field("harness_enabled", &self.harness_enabled)
            .field("harness_program", &self.harness_program)
            .field("harness_model", &self.harness_model)
            .field("harness_thinking_level", &self.harness_thinking_level)
            .field("max_tokens", &self.max_tokens)
            .field("timeout_ms", &self.timeout_ms)
            .field("route_overrides", &self.route_overrides)
            .field("harness_result_root", &self.harness_result_root)
            .finish()
    }
}

/// Build the runtime config from the registered `BOS_LLM_*` env vars (plus
/// `BOS_STATE_DIR` for the harness run-dir root). Unparseable values degrade
/// to defaults with a warning — this function cannot fail; misconfiguration
/// surfaces as a fail-closed error at execute time instead.
pub fn config_from_env() -> LlmRuntimeConfig {
    config_from_lookup(env_registry::string)
}

fn config_from_lookup(
    lookup: impl Fn(&env_registry::EnvVar) -> Option<String>,
) -> LlmRuntimeConfig {
    let api_provider = match lookup(&env_registry::BOS_LLM_API_PROVIDER)
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("anthropic") => LlmApiProvider::Anthropic,
        Some("openai") | Some("openai_compatible") | Some("openai-compatible") => {
            LlmApiProvider::OpenAi
        }
        Some("openrouter") | Some("open-router") => LlmApiProvider::OpenRouter,
        Some(other) => {
            tracing::warn!(
                provider = other,
                "unknown BOS_LLM_API_PROVIDER; defaulting to anthropic"
            );
            LlmApiProvider::Anthropic
        }
    };
    let default_backend = lookup(&env_registry::BOS_LLM_DEFAULT_BACKEND)
        .and_then(|raw| parse_backend(&raw, env_registry::BOS_LLM_DEFAULT_BACKEND.name, "default"))
        .unwrap_or(LlmBackend::Api);
    let max_tokens = parse_or_default(
        lookup(&env_registry::BOS_LLM_MAX_TOKENS),
        4_096,
        env_registry::BOS_LLM_MAX_TOKENS.name,
    );
    let timeout_ms = parse_or_default(
        lookup(&env_registry::BOS_LLM_TIMEOUT_MS),
        120_000,
        env_registry::BOS_LLM_TIMEOUT_MS.name,
    );
    let route_overrides = lookup(&env_registry::BOS_LLM_ROUTE_OVERRIDES)
        .map(|value| parse_route_overrides(&value))
        .unwrap_or_default();
    let harness_enabled = default_backend == LlmBackend::Harness
        || route_overrides
            .values()
            .any(|override_config| override_config.backend == LlmBackend::Harness);
    let harness_result_root = lookup(&env_registry::BOS_STATE_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./state"))
        .join(HARNESS_RUN_SUBDIR);
    LlmRuntimeConfig {
        api_provider,
        api_key: non_empty(lookup(&env_registry::BOS_LLM_API_KEY)),
        api_model: non_empty(lookup(&env_registry::BOS_LLM_API_MODEL)),
        api_endpoint: non_empty(lookup(&env_registry::BOS_LLM_API_ENDPOINT)),
        local_api_key: non_empty(lookup(&env_registry::BOS_LLM_LOCAL_API_KEY)),
        local_endpoint: non_empty(lookup(&env_registry::BOS_LLM_LOCAL_ENDPOINT))
            .unwrap_or_else(|| LOCAL_DEFAULT_ENDPOINT.to_string()),
        local_model: non_empty(lookup(&env_registry::BOS_LLM_LOCAL_MODEL)),
        default_backend,
        default_model: non_empty(lookup(&env_registry::BOS_LLM_DEFAULT_MODEL)),
        harness_enabled,
        harness_program: non_empty(lookup(&env_registry::BOS_LLM_HARNESS_PROGRAM))
            .unwrap_or_else(|| PROVIDER_ID_CLAUDE.to_string()),
        harness_model: non_empty(lookup(&env_registry::BOS_LLM_HARNESS_MODEL)),
        harness_thinking_level: non_empty(lookup(&env_registry::BOS_LLM_HARNESS_THINKING_LEVEL)),
        max_tokens,
        timeout_ms,
        route_overrides,
        harness_result_root,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_or_default<T: std::str::FromStr + Copy>(
    value: Option<String>,
    default: T,
    name: &str,
) -> T {
    match value.as_deref().map(str::trim) {
        None | Some("") => default,
        Some(raw) => raw.parse().unwrap_or_else(|_| {
            tracing::warn!(
                name,
                value = raw,
                "unparseable numeric env var; using default"
            );
            default
        }),
    }
}

fn parse_backend(raw: &str, env_name: &str, purpose: &str) -> Option<LlmBackend> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "api" => Some(LlmBackend::Api),
        "harness" => Some(LlmBackend::Harness),
        "local" => Some(LlmBackend::Local),
        other => {
            tracing::warn!(
                env_name,
                purpose,
                backend = other,
                "LLM backend must be api|harness|local; skipped"
            );
            None
        }
    }
}

pub fn parse_backend_choice(raw: &str) -> Option<LlmBackend> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "api" => Some(LlmBackend::Api),
        "harness" => Some(LlmBackend::Harness),
        "local" => Some(LlmBackend::Local),
        _ => None,
    }
}

/// Parse `purpose=api|harness|local[:model]` entries (comma/semicolon separated).
/// Invalid entries are skipped with a warning so one typo cannot disable the
/// spine. The older `purpose=api` form remains valid.
fn parse_route_overrides(value: &str) -> BTreeMap<String, LlmRouteOverride> {
    let mut overrides = BTreeMap::new();
    for raw_entry in value.split([',', ';']) {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((raw_key, raw_backend)) = entry.split_once('=') else {
            tracing::warn!(
                entry,
                "BOS_LLM_ROUTE_OVERRIDES entry is not purpose=backend; skipped"
            );
            continue;
        };
        let key = raw_key.trim();
        let (raw_backend, raw_model) = raw_backend
            .split_once(':')
            .map(|(backend, model)| (backend, Some(model)))
            .unwrap_or((raw_backend, None));
        let Some(backend) = parse_backend(raw_backend, "BOS_LLM_ROUTE_OVERRIDES", key) else {
            continue;
        };
        if key.is_empty() {
            tracing::warn!(entry, "BOS_LLM_ROUTE_OVERRIDES purpose is empty; skipped");
            continue;
        }
        overrides.insert(
            key.to_string(),
            LlmRouteOverride {
                backend,
                model: non_empty(raw_model.map(str::to_string)),
            },
        );
    }
    overrides
}

/// Harness-enabled → harness default, otherwise api; per-purpose overrides win.
pub fn route_for_purpose(config: &LlmRuntimeConfig, purpose: &str) -> LlmBackend {
    route_config_for_purpose(config, purpose).backend
}

pub fn route_config_for_purpose(config: &LlmRuntimeConfig, purpose: &str) -> ResolvedLlmRoute {
    let override_config = config.route_overrides.get(purpose);
    let backend = override_config
        .map(|override_config| override_config.backend)
        .unwrap_or(config.default_backend);
    let model = override_config
        .and_then(|override_config| override_config.model.clone())
        .or_else(|| match backend {
            LlmBackend::Api => config.api_model.clone(),
            LlmBackend::Harness => config.harness_model.clone(),
            LlmBackend::Local => config.local_model.clone(),
        })
        .or_else(|| config.default_model.clone());
    ResolvedLlmRoute { backend, model }
}

/// Execute one bounded typed LLM transform on the backend routed for
/// `purpose`. Blocking — callers run it off the async path. Credential scrub
/// happens at the egress seam of each backend (input-only for the API; whole
/// request for the harness, whose prompt serializes every field).
pub fn execute_typed_task(
    config: &LlmRuntimeConfig,
    purpose: &str,
    request: &TypedLlmTaskRequest,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    execute_typed_task_with_usage_sink(config, purpose, request, None)
}

/// [`execute_typed_task`] with an optional per-attempt usage sink (harness
/// attempts and errors are recorded; API usage rides on the output envelope).
pub fn execute_typed_task_with_usage_sink(
    config: &LlmRuntimeConfig,
    purpose: &str,
    request: &TypedLlmTaskRequest,
    usage_sink: Option<&dyn AiCallUsageSink>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let route = route_config_for_purpose(config, purpose);
    let backend = route.backend;
    // Align the request with the routed backend: the transport clients guard
    // on default_route, and routing here is config-driven, not caller-driven.
    let mut request = request.clone();
    request.execution_policy.default_route = match backend {
        LlmBackend::Api => TypedLlmExecutionRoute::DirectApi,
        LlmBackend::Harness => TypedLlmExecutionRoute::Harness,
        LlmBackend::Local => TypedLlmExecutionRoute::DirectApi,
    };
    if request.spec.max_tokens == 0 {
        request.spec.max_tokens = config.max_tokens;
    }
    if request.spec.timeout_ms == 0 {
        request.spec.timeout_ms = config.timeout_ms;
    }
    if let Some(model) = route.model.as_deref() {
        request.provider_policy.preferred_model = model.to_string();
    }
    let envelope = match backend {
        LlmBackend::Api => execute_api_typed_task(config, &request, route.model.as_deref()),
        LlmBackend::Harness => {
            execute_harness_backend_typed_task(config, &request, route.model, usage_sink)
        }
        LlmBackend::Local => execute_local_typed_task(config, &request, route.model.as_deref()),
    }?;
    // Output validation beyond json-object (port #4): registry + caps +
    // redaction check run for EVERY typed transform, on every backend.
    validate_typed_task_output(&request.spec.schema_ref, &envelope.response_json)?;
    Ok(envelope)
}

fn execute_local_typed_task(
    config: &LlmRuntimeConfig,
    request: &TypedLlmTaskRequest,
    routed_model: Option<&str>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let Some(model) = routed_model.filter(|model| !model.trim().is_empty()) else {
        return Err(AppError::invalid_input(
            "llm_local_model_not_configured",
            "Local LLM routing requires BOS_LLM_LOCAL_MODEL or a per-purpose local model override",
            CorrelationId::generate(),
        ));
    };
    validate_local_endpoint(&config.local_endpoint)?;
    let request = sanitize_typed_task_request(request);
    let client = OpenAiCompatibleDirectLlmClient::new(OpenAiCompatibleDirectLlmConfig {
        provider_id: "local_openai_compatible".to_string(),
        api_key: config
            .local_api_key
            .clone()
            .unwrap_or_else(|| "local-no-auth".to_string()),
        model: model.to_string(),
        endpoint: config.local_endpoint.clone(),
        timeout_ms: config.timeout_ms,
    })?;
    client.complete_typed_task(&request)
}

fn validate_local_endpoint(raw: &str) -> AppResult<()> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| {
        AppError::invalid_input(
            "llm_local_endpoint_invalid",
            "Local LLM endpoint must be a loopback HTTP(S) URL",
            CorrelationId::generate(),
        )
    })?;
    let valid_scheme = matches!(parsed.scheme(), "http" | "https");
    let loopback = match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !valid_scheme || !loopback || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::invalid_input(
            "llm_local_endpoint_not_loopback",
            "Local LLM endpoint must stay on loopback and cannot contain credentials",
            CorrelationId::generate(),
        ));
    }
    Ok(())
}

fn execute_api_typed_task(
    config: &LlmRuntimeConfig,
    request: &TypedLlmTaskRequest,
    routed_model: Option<&str>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    let Some(api_key) = config.api_key.as_deref() else {
        return Err(AppError::invalid_input(
            "llm_api_not_configured",
            "LLM API backend requires BOS_LLM_API_KEY",
            CorrelationId::generate(),
        ));
    };
    let Some(model) = routed_model.filter(|model| !model.trim().is_empty()) else {
        return Err(AppError::invalid_input(
            "llm_api_model_not_configured",
            "LLM API backend requires BOS_LLM_DEFAULT_MODEL, BOS_LLM_API_MODEL, or a per-purpose API model override",
            CorrelationId::generate(),
        ));
    };
    let endpoint = config
        .api_endpoint
        .clone()
        .unwrap_or_else(|| config.api_provider.default_endpoint().to_string());
    // Credential scrub at the API egress: only `input` reaches the provider.
    let request = sanitize_typed_task_request(request);
    match config.api_provider {
        LlmApiProvider::Anthropic => {
            let client = AnthropicDirectLlmClient::new(AnthropicDirectLlmConfig {
                provider_id: "anthropic".to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                endpoint,
                timeout_ms: config.timeout_ms,
            })?;
            client.complete_typed_task(&request)
        }
        LlmApiProvider::OpenAi | LlmApiProvider::OpenRouter => {
            let provider_id = match config.api_provider {
                LlmApiProvider::OpenAi => "openai",
                _ => "openrouter",
            };
            let client = OpenAiCompatibleDirectLlmClient::new(OpenAiCompatibleDirectLlmConfig {
                provider_id: provider_id.to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                endpoint,
                timeout_ms: config.timeout_ms,
            })?;
            client.complete_typed_task(&request)
        }
    }
}

fn execute_harness_backend_typed_task(
    config: &LlmRuntimeConfig,
    request: &TypedLlmTaskRequest,
    routed_model: Option<String>,
    usage_sink: Option<&dyn AiCallUsageSink>,
) -> AppResult<TypedLlmTaskOutputEnvelope> {
    if routed_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .is_none()
    {
        return Err(AppError::invalid_input(
            "llm_harness_model_not_configured",
            "LLM harness backend requires BOS_LLM_DEFAULT_MODEL, BOS_LLM_HARNESS_MODEL, or a per-purpose harness model override so BusinessOS does not inherit the Claude CLI global model",
            CorrelationId::generate(),
        ));
    }
    let harness_program = resolve_harness_program(&config.harness_program)?;
    let harness_config = HarnessRuntimeConfig {
        provider_id: PROVIDER_ID_CLAUDE.to_string(),
        program: harness_program,
        model: routed_model,
        thinking_level: config.harness_thinking_level.clone(),
        result_root: config.harness_result_root.clone(),
        schema_lookup: None,
    };
    let backend = TmuxCliSessionBackend;
    let runner = TmuxHarnessTypedTaskRunner::new(&backend);
    // Full-request scrub happens inside execute_harness_typed_task (the tmux
    // prompt serializes the whole request, metadata included).
    execute_harness_typed_task(
        request,
        &runner,
        &harness_config,
        &JsonObjectOutputValidator,
        usage_sink,
    )
}

fn resolve_harness_program(program: &str) -> AppResult<String> {
    let program = program.trim();
    if program.is_empty() {
        return harness_program_not_found(program);
    }
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = Path::new(program);
        if !is_executable_file(path) {
            return harness_program_not_found(program);
        }
        let absolute = path.canonicalize().map_err(|error| {
            AppError::invalid_input(
                "llm_harness_program_not_found",
                format!(
                    "LLM harness backend requires BOS_LLM_HARNESS_PROGRAM to resolve to an executable; configured value was {program:?}: {error}"
                ),
                CorrelationId::generate(),
            )
        })?;
        return Ok(absolute.display().to_string());
    }
    if harness_program_available_on_path(program) {
        return Ok(program.to_string());
    }
    harness_program_not_found(program)
}

fn harness_program_not_found<T>(program: &str) -> AppResult<T> {
    Err(AppError::invalid_input(
        "llm_harness_program_not_found",
        format!(
            "LLM harness backend requires BOS_LLM_HARNESS_PROGRAM to resolve to an executable; configured value was {program:?}"
        ),
        CorrelationId::generate(),
    ))
}

fn harness_program_available_on_path(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v -- \"$1\" >/dev/null 2>&1")
        .arg("bos-harness-program-check")
        .arg(program)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Output validation (port #4). Pattern harvested from agent-monitor-rust's
// artifact_validators.rs: a versioned schema-ref registry, per-artifact
// structure caps (depth + element counts), and a post-validation redaction
// check, applied centrally at the execute seam so no vertical can skip it.
// The registry checks STRUCTURE (registered ref, required top-level fields);
// semantic validation (grounded amounts, date formats, catalogs) stays in
// each slice's parse function — one registration point, no copy-paste of
// domain rules.
//
// Divergence from agent_monitor: the redaction check matches credential-SHAPED
// strings (PEM headers, provider token prefixes, JWT pairs) instead of bare
// words like "secret" — drafts summarize real customer email, where "secret"
// is legitimate prose.
// ---------------------------------------------------------------------------

struct OutputSchemaSpec {
    schema_ref: &'static str,
    /// Top-level fields whose ABSENCE the owning slice already rejects —
    /// presence-only here, so the central check never refuses an output the
    /// slice would accept.
    required_fields: &'static [&'static str],
    max_elements: usize,
}

/// Every schema a produce/classify transform may emit. Adding a vertical
/// means registering its schema ref here — an unregistered ref is an error,
/// not a pass-through.
const OUTPUT_SCHEMA_REGISTRY: &[OutputSchemaSpec] = &[
    OutputSchemaSpec {
        schema_ref: crate::slices::email_triage::service::AI_TRIAGE_SCHEMA_REF,
        required_fields: &["suggested_packet_kinds", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::packet_proposals::service::PROPOSAL_SCHEMA_REF,
        required_fields: &["confidence", "outcomes"],
        max_elements: 8_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::follow_up_tasks::service::FILL_SCHEMA_REF,
        required_fields: &["title", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::calendar_drafts::service::EXTRACT_SCHEMA_REF,
        // extractable=false responses legitimately omit every other field.
        required_fields: &["extractable"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::crm_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["note_body", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        // company/contact may legitimately be absent; only confidence is
        // always present (the grounded records ride in provenance + fields).
        schema_ref: crate::slices::crm_record_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        // Every record field is optional (the gap-filler fills only what was
        // missing and could be grounded); confidence is the one constant.
        schema_ref: crate::slices::crm_record_drafts::service::ENRICH_SCHEMA_REF,
        required_fields: &["confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::crm_sales_intent::service::FILL_SCHEMA_REF,
        required_fields: &[
            "lead_title",
            "intent_summary",
            "next_step_text",
            "confidence",
        ],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::email_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["body_text", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::ledger_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["payer_name", "amount_cents", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::content_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["title", "body_markdown", "claims", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::social_publishing::service::DRAFT_SCHEMA_REF,
        required_fields: &["targets", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::claim_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["damage_narrative", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::owner_reports::service::NARRATION_SCHEMA_REF,
        required_fields: &["headline", "narrative", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::invoice_drafts::service::FILL_SCHEMA_REF,
        required_fields: &["customer_name", "line_items", "confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::invoice_drafts::service::CUSTOMER_ENRICH_SCHEMA_REF,
        required_fields: &["confidence"],
        max_elements: 2_000,
    },
    OutputSchemaSpec {
        schema_ref: crate::slices::enrichment::service::RESEARCH_ACTION_SCHEMA_REF,
        required_fields: &["action"],
        max_elements: 2_000,
    },
];

const OUTPUT_MAX_JSON_DEPTH: usize = 64;

/// Validate a typed transform's output against the schema registry. Errors
/// surface as the execute error the caller already records (ai_usage row +
/// per-slice error status) — an invalid output never reaches a stage().
pub fn validate_typed_task_output(schema_ref: &str, response: &serde_json::Value) -> AppResult<()> {
    fn invalid(code: &'static str, message: String) -> AppError {
        AppError::invalid_input(code, message, CorrelationId::generate())
    }
    let Some(spec) = OUTPUT_SCHEMA_REGISTRY
        .iter()
        .find(|spec| spec.schema_ref == schema_ref)
    else {
        return Err(invalid(
            "llm_output_schema_unregistered",
            format!("schema_ref {schema_ref} is not in the output schema registry"),
        ));
    };
    let Some(object) = response.as_object() else {
        return Err(invalid(
            "llm_output_not_object",
            format!("{schema_ref}: output is not a JSON object"),
        ));
    };
    for field in spec.required_fields {
        if !object.contains_key(*field) {
            return Err(invalid(
                "llm_output_missing_field",
                format!("{schema_ref}: required field {field} is missing"),
            ));
        }
    }
    validate_output_caps(response, spec.max_elements).map_err(|message| {
        invalid(
            "llm_output_caps_exceeded",
            format!("{schema_ref}: {message}"),
        )
    })?;
    let serialized = response.to_string();
    if let Some(pattern) = credential_like_match(&serialized) {
        return Err(invalid(
            "llm_output_redaction_failed",
            format!("{schema_ref}: output contains credential-shaped text ({pattern})"),
        ));
    }
    Ok(())
}

/// Depth + element caps (harvested from agent_monitor's validate_json_caps): a
/// pathological output must fail loudly, never walk into a store.
fn validate_output_caps(value: &serde_json::Value, max_elements: usize) -> Result<(), String> {
    fn walk(value: &serde_json::Value, depth: usize, elements: &mut usize) -> Result<(), String> {
        if depth > OUTPUT_MAX_JSON_DEPTH {
            return Err("json depth cap exceeded".to_string());
        }
        match value {
            serde_json::Value::Array(items) => {
                *elements += items.len();
                for item in items {
                    walk(item, depth + 1, elements)?;
                }
            }
            serde_json::Value::Object(map) => {
                *elements += map.len();
                for item in map.values() {
                    walk(item, depth + 1, elements)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut elements = 1usize;
    walk(value, 1, &mut elements)?;
    if elements > max_elements {
        return Err("json element cap exceeded".to_string());
    }
    Ok(())
}

/// High-confidence credential shapes only — names the matched class.
fn credential_like_match(text: &str) -> Option<&'static str> {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"-----BEGIN ", "pem_block"),
            (r"\bsk-[A-Za-z0-9_-]{16,}", "provider_api_key"),
            (r"\b[sr]k_live_[A-Za-z0-9]{8,}", "stripe_live_key"),
            (r"\bxox[bp]-[A-Za-z0-9-]{8,}", "slack_token"),
            (r"\bghp_[A-Za-z0-9]{16,}", "github_token"),
            (r"\bgithub_pat_[A-Za-z0-9_]{16,}", "github_token"),
            (r"\bAKIA[0-9A-Z]{16}\b", "aws_access_key"),
            (r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ", "jwt"),
        ]
        .into_iter()
        .map(|(pattern, label)| (regex::Regex::new(pattern).expect("static regex"), label))
        .collect()
    });
    patterns
        .iter()
        .find(|(regex, _label)| regex.is_match(text))
        .map(|(_regex, label)| *label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&env_registry::EnvVar) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |var: &env_registry::EnvVar| {
            map.get(var.name)
                .cloned()
                .or_else(|| var.default.map(str::to_string))
        }
    }

    fn base_config() -> LlmRuntimeConfig {
        config_from_lookup(lookup_from(&[]))
    }

    #[test]
    fn defaults_route_to_api_with_anthropic_provider() {
        let config = base_config();

        assert_eq!(config.api_provider, LlmApiProvider::Anthropic);
        assert!(!config.harness_enabled);
        assert_eq!(config.default_backend, LlmBackend::Api);
        assert!(config.default_model.is_none());
        assert!(config.api_key.is_none());
        assert_eq!(config.harness_program, "claude");
        assert_eq!(config.max_tokens, 4_096);
        assert_eq!(config.timeout_ms, 120_000);
        assert!(config.route_overrides.is_empty());
        assert!(config.harness_result_root.ends_with("state/llm-harness"));
        assert_eq!(route_for_purpose(&config, "email_triage"), LlmBackend::Api);
    }

    #[test]
    fn default_backend_harness_enables_and_routes_to_harness() {
        let config = config_from_lookup(lookup_from(&[("BOS_LLM_DEFAULT_BACKEND", "harness")]));

        assert!(config.harness_enabled);
        assert_eq!(config.default_backend, LlmBackend::Harness);
        assert_eq!(
            route_for_purpose(&config, "email_triage"),
            LlmBackend::Harness
        );
    }

    #[test]
    fn explicit_default_backend_api_keeps_harness_disabled() {
        let config = config_from_lookup(lookup_from(&[("BOS_LLM_DEFAULT_BACKEND", "api")]));

        assert!(!config.harness_enabled);
        assert_eq!(config.default_backend, LlmBackend::Api);
        assert_eq!(route_for_purpose(&config, "email_triage"), LlmBackend::Api);
    }

    #[test]
    fn route_overrides_win_over_default_in_both_directions() {
        let harness_default = config_from_lookup(lookup_from(&[
            ("BOS_LLM_DEFAULT_BACKEND", "harness"),
            ("BOS_LLM_ROUTE_OVERRIDES", "email_triage=api"),
        ]));
        assert!(harness_default.harness_enabled);
        assert_eq!(
            route_for_purpose(&harness_default, "email_triage"),
            LlmBackend::Api
        );
        assert_eq!(
            route_for_purpose(&harness_default, "anything_else"),
            LlmBackend::Harness
        );

        let api_default = config_from_lookup(lookup_from(&[(
            "BOS_LLM_ROUTE_OVERRIDES",
            "blog_research=harness, web_lookup = harness",
        )]));
        assert!(api_default.harness_enabled);
        assert_eq!(
            route_for_purpose(&api_default, "blog_research"),
            LlmBackend::Harness
        );
        assert_eq!(
            route_for_purpose(&api_default, "web_lookup"),
            LlmBackend::Harness
        );
        assert_eq!(
            route_for_purpose(&api_default, "email_triage"),
            LlmBackend::Api
        );
    }

    #[test]
    fn invalid_override_entries_are_skipped_not_fatal() {
        let overrides = parse_route_overrides("good=api, bad-entry, worse=teleport, =api");

        assert_eq!(overrides.len(), 1);
        assert_eq!(
            overrides
                .get("good")
                .map(|override_config| override_config.backend),
            Some(LlmBackend::Api)
        );
    }

    #[test]
    fn route_overrides_can_pin_models_per_purpose() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_DEFAULT_MODEL", "global-model"),
            (
                "BOS_LLM_ROUTE_OVERRIDES",
                "invoice_fill=harness:claude-sonnet-4-6, email_ai_triage=api:gpt-4.1-mini",
            ),
        ]));

        assert_eq!(
            route_config_for_purpose(&config, "invoice_fill"),
            ResolvedLlmRoute {
                backend: LlmBackend::Harness,
                model: Some("claude-sonnet-4-6".to_string())
            }
        );
        assert_eq!(
            route_config_for_purpose(&config, "email_ai_triage"),
            ResolvedLlmRoute {
                backend: LlmBackend::Api,
                model: Some("gpt-4.1-mini".to_string())
            }
        );
        assert_eq!(
            route_config_for_purpose(&config, "crm_note_fill"),
            ResolvedLlmRoute {
                backend: LlmBackend::Api,
                model: Some("global-model".to_string())
            }
        );
    }

    #[test]
    fn route_override_to_harness_enables_harness_with_api_default() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_DEFAULT_BACKEND", "api"),
            ("BOS_LLM_ROUTE_OVERRIDES", "invoice_fill=harness"),
        ]));

        assert!(config.harness_enabled);
        assert_eq!(config.default_backend, LlmBackend::Api);
        assert_eq!(
            route_for_purpose(&config, "invoice_fill"),
            LlmBackend::Harness
        );
        assert_eq!(route_for_purpose(&config, "email_triage"), LlmBackend::Api);
    }

    #[test]
    fn local_route_uses_separate_loopback_profile_and_model() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_API_PROVIDER", "anthropic"),
            (
                "BOS_LLM_API_ENDPOINT",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "BOS_LLM_LOCAL_ENDPOINT",
                "http://localhost:1234/v1/chat/completions",
            ),
            (
                "BOS_LLM_ROUTE_OVERRIDES",
                "social_post_draft=local:qwen3:8b",
            ),
        ]));

        assert_eq!(
            route_config_for_purpose(&config, "social_post_draft"),
            ResolvedLlmRoute {
                backend: LlmBackend::Local,
                model: Some("qwen3:8b".to_string()),
            }
        );
        assert_eq!(
            route_for_purpose(&config, "content_grounded_draft"),
            LlmBackend::Api
        );
        assert_eq!(
            config.local_endpoint,
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn social_drafting_uses_the_configured_default_harness() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_DEFAULT_BACKEND", "harness"),
            ("BOS_LLM_HARNESS_MODEL", "claude-sonnet-4-6"),
        ]));

        assert_eq!(
            route_for_purpose(&config, "social_post_draft"),
            LlmBackend::Harness
        );
        assert_eq!(
            route_for_purpose(&config, "content_grounded_draft"),
            LlmBackend::Harness
        );
    }

    #[test]
    fn local_route_refuses_remote_endpoint_without_api_fallback() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_ROUTE_OVERRIDES", "email_triage=local:qwen3"),
            (
                "BOS_LLM_LOCAL_ENDPOINT",
                "https://models.example.com/v1/chat/completions",
            ),
            ("BOS_LLM_API_KEY", "cloud-key-must-not-be-used"),
            ("BOS_LLM_API_MODEL", "cloud-model-must-not-be-used"),
        ]));

        let error = execute_typed_task(&config, "email_triage", &sample_request())
            .expect_err("remote local profile must fail closed before network");
        assert_eq!(error.code(), "llm_local_endpoint_not_loopback");
    }

    #[test]
    fn harness_program_env_is_registered_and_trimmed() {
        let config = config_from_lookup(lookup_from(&[(
            "BOS_LLM_HARNESS_PROGRAM",
            " /opt/bos/bin/claude ",
        )]));

        assert_eq!(config.harness_program, "/opt/bos/bin/claude");
    }

    #[test]
    fn api_provider_aliases_parse() {
        for (raw, expected) in [
            ("anthropic", LlmApiProvider::Anthropic),
            ("openai", LlmApiProvider::OpenAi),
            ("openai-compatible", LlmApiProvider::OpenAi),
            ("openrouter", LlmApiProvider::OpenRouter),
            ("open-router", LlmApiProvider::OpenRouter),
            ("mystery", LlmApiProvider::Anthropic),
        ] {
            let config = config_from_lookup(lookup_from(&[("BOS_LLM_API_PROVIDER", raw)]));
            assert_eq!(config.api_provider, expected, "provider alias {raw}");
        }
    }

    #[test]
    fn provider_default_endpoints_are_wired() {
        assert!(LlmApiProvider::Anthropic
            .default_endpoint()
            .contains("api.anthropic.com"));
        assert!(LlmApiProvider::OpenAi
            .default_endpoint()
            .contains("api.openai.com"));
        assert!(LlmApiProvider::OpenRouter
            .default_endpoint()
            .contains("openrouter.ai"));
    }

    #[test]
    fn execute_api_fails_closed_when_key_or_model_missing() {
        let config = base_config();
        let request = sample_request();

        let error = execute_typed_task(&config, "email_triage", &request)
            .expect_err("unconfigured API backend must fail closed");

        assert_eq!(error.code(), "llm_api_not_configured");
    }

    #[test]
    fn harness_route_without_businessos_model_fails_before_cli_launch() {
        let config = config_from_lookup(lookup_from(&[("BOS_LLM_DEFAULT_BACKEND", "harness")]));
        let request = sample_request();

        let error = execute_typed_task(&config, "invoice_fill", &request)
            .expect_err("unconfigured harness model must fail closed");

        assert_eq!(error.code(), "llm_harness_model_not_configured");
    }

    #[test]
    fn harness_route_with_missing_program_fails_before_cli_launch() {
        let config = config_from_lookup(lookup_from(&[
            ("BOS_LLM_DEFAULT_BACKEND", "harness"),
            ("BOS_LLM_HARNESS_MODEL", "claude-sonnet-4-6"),
            (
                "BOS_LLM_HARNESS_PROGRAM",
                "/tmp/bos-definitely-missing-claude-cli",
            ),
        ]));
        let request = sample_request();

        let error = execute_typed_task(&config, "invoice_fill", &request)
            .expect_err("missing harness program must fail closed");

        assert_eq!(error.code(), "llm_harness_program_not_found");
    }

    #[cfg(unix)]
    #[test]
    fn relative_harness_program_paths_are_resolved_before_tmux_launch() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            PathBuf::from("target").join(format!("bos-llm-program-test-{}", std::process::id()));
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("mkdir");
        let program = bin_dir.join("claude");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write stub");
        let mut permissions = std::fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions).expect("chmod");

        let relative_program = root.join("bin/claude");
        let resolved =
            resolve_harness_program(&relative_program.display().to_string()).expect("resolve");
        let expected = program
            .canonicalize()
            .expect("canonical")
            .display()
            .to_string();
        std::fs::remove_dir_all(&root).expect("cleanup");

        assert_eq!(resolved, expected);
        assert!(Path::new(&resolved).is_absolute());
    }

    #[test]
    fn debug_redacts_api_key() {
        let mut config = base_config();
        config.api_key = Some("sk-ant-super-secret".to_string());

        let rendered = format!("{config:?}");
        assert!(!rendered.contains("sk-ant-super-secret"), "{rendered}");
        assert!(rendered.contains("[redacted]"));
    }

    fn sample_request() -> TypedLlmTaskRequest {
        use bos_integrations::llm_typed_tasks::*;
        use serde_json::json;

        TypedLlmTaskRequest {
            task_id: "task-1".to_string(),
            correlation_id: "corr-1".to_string(),
            idempotency_key: "idem-1".to_string(),
            tenant_or_project_scope: "tenant-1".to_string(),
            source_entity: None,
            spec: TypedLlmTaskSpec {
                task_class: TypedLlmTaskClass::Classify,
                prompt_template_id: "email_triage_direct.v1".to_string(),
                prompt_template_version: "1".to_string(),
                prompt_template_hash: "hash-1".to_string(),
                schema_ref: "email.triage_result.v1".to_string(),
                response_format: TypedLlmResponseFormat::JsonObject,
                max_input_bytes: 8192,
                max_output_bytes: 8192,
                max_tokens: 0,
                timeout_ms: 0,
                capabilities: TypedLlmTaskCapabilities::pure_transformation(),
                authority: TypedLlmAuthority::no_side_effects(),
            },
            input: TypedLlmTaskInput {
                json: json!({"subject": "Hello"}),
                text_blocks: Vec::new(),
            },
            execution_policy: TypedLlmExecutionPolicy {
                default_route: TypedLlmExecutionRoute::DirectApi,
                fallback_policy: TypedLlmFallbackPolicy::FailClosed,
                retry_policy: TypedLlmRetryPolicy {
                    max_attempts: 1,
                    backoff_ms: 0,
                    max_elapsed_ms: 0,
                },
            },
            provider_policy: TypedLlmProviderPolicy {
                preferred_provider: "anthropic".to_string(),
                preferred_model: "claude-sonnet-4-6".to_string(),
                fallback_provider: None,
                fallback_model: None,
            },
            safety_policy: TypedLlmSafetyPolicy {
                redaction_policy: TypedLlmRedactionPolicy::PreAndPost,
                raw_output_retention: TypedLlmRawOutputRetention::LocalOnly,
            },
        }
    }
}

#[cfg(test)]
mod output_validation_tests {
    use super::validate_typed_task_output;
    use serde_json::json;

    const LEDGER: &str = "bos.ledger_drafts.receipt_fill.v1";

    #[test]
    fn registered_minimal_outputs_pass() {
        let cases = [
            (
                "bos.email_triage.ai_triage.v1",
                json!({"suggested_packet_kinds": [], "confidence": "low"}),
            ),
            (
                "bos.follow_up_tasks.fill.v1",
                json!({"title": "Reply to Jane", "confidence": "high"}),
            ),
            // extractable=false responses legitimately omit everything else.
            (
                "bos.calendar_drafts.event_extract.v1",
                json!({"extractable": false, "reason": "no dated event"}),
            ),
            (
                "bos.crm_drafts.note_fill.v1",
                json!({"note_body": "Call logged.", "confidence": "high"}),
            ),
            (
                "bos.email_drafts.reply_fill.v1",
                json!({"body_text": "Thanks!", "confidence": "medium"}),
            ),
            (
                LEDGER,
                json!({"payer_name": "Acme", "amount_cents": 100, "confidence": "high"}),
            ),
            (
                crate::slices::enrichment::service::RESEARCH_ACTION_SCHEMA_REF,
                json!({"action": "finish"}),
            ),
        ];
        for (schema_ref, output) in cases {
            validate_typed_task_output(schema_ref, &output)
                .unwrap_or_else(|err| panic!("{schema_ref} should pass: {err:?}"));
        }
    }

    #[test]
    fn unregistered_schema_and_non_object_fail() {
        let err =
            validate_typed_task_output("bos.unknown.v1", &json!({})).expect_err("unregistered");
        assert_eq!(err.code(), "llm_output_schema_unregistered");

        let err = validate_typed_task_output(LEDGER, &json!(["array"])).expect_err("non-object");
        assert_eq!(err.code(), "llm_output_not_object");
    }

    #[test]
    fn missing_required_field_fails() {
        let err = validate_typed_task_output(
            LEDGER,
            &json!({"payer_name": "Acme", "confidence": "high"}),
        )
        .expect_err("amount missing");
        assert_eq!(err.code(), "llm_output_missing_field");

        let err = validate_typed_task_output(
            crate::slices::enrichment::service::RESEARCH_ACTION_SCHEMA_REF,
            &json!({"query": "example"}),
        )
        .expect_err("research action missing");
        assert_eq!(err.code(), "llm_output_missing_field");
    }

    #[test]
    fn element_cap_rejects_pathological_outputs() {
        let huge: Vec<u32> = (0..5_000).collect();
        let err = validate_typed_task_output(
            LEDGER,
            &json!({"payer_name": "Acme", "amount_cents": 100, "confidence": "high", "junk": huge}),
        )
        .expect_err("oversized");
        assert_eq!(err.code(), "llm_output_caps_exceeded");
    }

    #[test]
    fn credential_shaped_text_fails_but_ordinary_prose_passes() {
        let leak = json!({
            "payer_name": "Acme",
            "amount_cents": 100,
            "confidence": "high",
            "description": "key sk-abcdefghijklmnop1234 attached"
        });
        let err = validate_typed_task_output(LEDGER, &leak).expect_err("leak");
        assert_eq!(err.code(), "llm_output_redaction_failed");

        let pem = json!({
            "payer_name": "Acme",
            "amount_cents": 100,
            "confidence": "high",
            "description": "-----BEGIN PRIVATE KEY-----"
        });
        assert!(validate_typed_task_output(LEDGER, &pem).is_err());

        // Divergence from agent_monitor: bare words like "secret" are legitimate
        // prose in customer email and must NOT trip the check.
        let prose = json!({
            "payer_name": "Acme",
            "amount_cents": 100,
            "confidence": "high",
            "description": "Payment for the Secret Santa event; access token granted to the venue."
        });
        validate_typed_task_output(LEDGER, &prose).expect("prose passes");
    }
}
