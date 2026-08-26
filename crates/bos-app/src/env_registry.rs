//! The ONLY module in the workspace allowed to call `std::env::var`.
//! `just code-shape` enforces this.
//!
//! Adding a config knob = adding a typed `EnvVar` const here and listing it in
//! `ALL`. `REPO_MAP.md` and `--print-env` are generated from `ALL`, so an
//! unregistered variable is invisible to operators and reviewers — register it.

use std::fmt;

#[derive(Debug, Clone, Copy)]
pub struct EnvVar {
    pub name: &'static str,
    pub description: &'static str,
    pub group: EnvVarGroup,
    pub secret: bool,
    /// Default used when unset. `None` means required-when-feature-active.
    pub default: Option<&'static str>,
    /// Legacy/foreign names consulted when the primary is unset (e.g. fly.io
    /// secrets carried over from the predecessor deployment). BOS_* stays
    /// canonical; aliases just keep existing secret stores working.
    pub aliases: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvVarGroup {
    AgentMcp,
    AiDrafting,
    CallsTranscription,
    ConnectorsSync,
    ContentPublishing,
    Crm,
    CustomerTier,
    DataRetention,
    DriveCorpus,
    EmailTriage,
    InfraServer,
    InventoryClaims,
    InvoicingAccounting,
    LlmBackend,
    Reporting,
    SecurityWebhooks,
    WebEnrichmentSearch,
}

impl EnvVarGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentMcp => "Agent / MCP",
            Self::AiDrafting => "AI usage and drafting",
            Self::CallsTranscription => "Calls and transcription",
            Self::ConnectorsSync => "Connectors and sync",
            Self::ContentPublishing => "Content publishing",
            Self::Crm => "CRM",
            Self::CustomerTier => "Customer tier",
            Self::DataRetention => "Data retention",
            Self::DriveCorpus => "Drive corpus",
            Self::EmailTriage => "Email and triage",
            Self::InfraServer => "Infra / server",
            Self::InventoryClaims => "Inventory and claims",
            Self::InvoicingAccounting => "Invoicing and accounting",
            Self::LlmBackend => "LLM backend",
            Self::Reporting => "Reporting",
            Self::SecurityWebhooks => "Security and webhooks",
            Self::WebEnrichmentSearch => "Web enrichment and search",
        }
    }
}

impl fmt::Display for EnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

pub const BOS_AGENT_LAUNCH_ENABLED: EnvVar = EnvVar {
    name: "BOS_AGENT_LAUNCH_ENABLED",
    description: "Enable launching a Agent Monitor agent session from a work item (with the item's context plus optional operator notes). Reuses BOS_DEBUG_AGENT_MONITOR_URL/_TOKEN. Off by default; intended for the operator's own dashboard, not client instances.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AGENT_MCP_ENABLED: EnvVar = EnvVar {
    name: "BOS_AGENT_MCP_ENABLED",
    description: "Enable the optional BusinessOS MCP endpoint for explicitly BOS-contexted AgentMonitor/Fleet agents. Off by default; tools remain operator-authenticated and cannot approve drafts, send email, or write to providers.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS",
    description: "Maximum model spend budget for one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("0"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_ENABLED: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_ENABLED",
    description:
        "Enable the optional bounded agentic web research enrichment tier. Off by default.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS",
    description: "Maximum concurrent bounded agentic web research runs.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES",
    description: "Maximum pages fetched by one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("4"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS",
    description: "Maximum model output tokens for one bounded agentic web research action.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("4096"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES",
    description: "Maximum bytes read from one page during bounded agentic web research.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("524288"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS",
    description: "Maximum search results considered by one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("10"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES",
    description: "Maximum search actions in one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("2"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS",
    description: "Maximum model/action steps in one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("8"),
    aliases: &[],
};

pub const BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS: EnvVar = EnvVar {
    name: "BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS",
    description: "Wall-clock timeout for one bounded agentic web research run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("90000"),
    aliases: &[],
};

pub const BOS_AGENT_EVIDENCE_CLEANUP_ENABLED: EnvVar = EnvVar {
    name: "BOS_AGENT_EVIDENCE_CLEANUP_ENABLED",
    description: "Enable periodic cleanup of expired per-session agent evidence files staged from provider attachments.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS",
    description: "Interval between expired agent evidence file cleanup passes.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: Some("3600"),
    aliases: &[],
};

pub const BOS_AGENT_EVIDENCE_MAX_BYTES: EnvVar = EnvVar {
    name: "BOS_AGENT_EVIDENCE_MAX_BYTES",
    description: "Maximum bytes BusinessOS will stage for one email attachment evidence file.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: Some("10485760"),
    aliases: &[],
};

pub const BOS_AGENT_EVIDENCE_RETENTION_DAYS: EnvVar = EnvVar {
    name: "BOS_AGENT_EVIDENCE_RETENTION_DAYS",
    description: "Default retention window for staged agent evidence files.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: Some("30"),
    aliases: &[],
};

pub const BOS_AGENT_EVIDENCE_ROOT_DIR: EnvVar = EnvVar {
    name: "BOS_AGENT_EVIDENCE_ROOT_DIR",
    description:
        "Filesystem root for per-session agent evidence files staged from provider attachments.",
    group: EnvVarGroup::AgentMcp,
    secret: false,
    default: Some("var/agent-evidence"),
    aliases: &[],
};

pub const BOS_BUFFER_ACCESS_TOKEN: EnvVar = EnvVar {
    name: "BOS_BUFFER_ACCESS_TOKEN",
    description: "Buffer API key used only by the approval-gated outbox delivery adapter. Never included in proposal, model, receipt, or outbox payloads.",
    group: EnvVarGroup::ContentPublishing,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_BUFFER_API_URL: EnvVar = EnvVar {
    name: "BOS_BUFFER_API_URL",
    description: "Buffer GraphQL API endpoint for approved social-post delivery.",
    group: EnvVarGroup::ContentPublishing,
    secret: false,
    default: Some("https://api.buffer.com"),
    aliases: &[],
};

pub const BOS_BUFFER_CHANNELS_JSON: EnvVar = EnvVar {
    name: "BOS_BUFFER_CHANNELS_JSON",
    description: "Configured Buffer targets as JSON array entries {channel_id,name,platform}. Supported platform keys: facebook, googlebusiness, instagram, linkedin, twitter. Every staged proposal must cover the exact configured channel set.",
    group: EnvVarGroup::ContentPublishing,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_BUFFER_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_BUFFER_WRITE_ENABLED",
    description: "Enable approved social proposals to create Buffer posts. Off by default; closed-gate channel jobs dry-run independently.",
    group: EnvVarGroup::ContentPublishing,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AI_TRIAGE_ENABLED: EnvVar = EnvVar {
    name: "BOS_AI_TRIAGE_ENABLED",
    description: "Enable the tier-2 AI triage pass over fallback (rule-less) mail. Off by default.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE",
    description: "Max LLM triage calls per ingest cycle (cost bound).",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("5"),
    aliases: &[],
};

pub const BOS_AI_TRIAGE_MIN_CONFIDENCE: EnvVar = EnvVar {
    name: "BOS_AI_TRIAGE_MIN_CONFIDENCE",
    description: "Minimum confidence (high|medium|low) before an AI suggestion becomes a work item; below it the message stays quiet.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("high"),
    aliases: &[],
};

pub const BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED: EnvVar = EnvVar {
    name: "BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED",
    description: "When enabled, the tier-2 AI triage pass uses the unified packet proposal call to suggest and stage drafts in one LLM response. Off by default.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS: EnvVar = EnvVar {
    name: "BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS",
    description: "Milliseconds after which a still-running Smart draft proposal run is treated as stale on the next read. Default is one hour.",
    group: EnvVarGroup::AiDrafting,
    secret: false,
    default: Some("3600000"),
    aliases: &[],
};

pub const BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED: EnvVar = EnvVar {
    name: "BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED",
    description: "Enable Smart draft's backend-only recorded tool-loop proposal mode. Off by default; no operator-facing trigger is wired in v1.",
    group: EnvVarGroup::AiDrafting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AUTO_PRODUCE_ENABLED: EnvVar = EnvVar {
    name: "BOS_AUTO_PRODUCE_ENABLED",
    description: "Run the auto-produce pump: accepted items in categories whose policy has auto_produce on get their drafts produced automatically (LLM cost per accept). Off by default.",
    group: EnvVarGroup::AiDrafting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_AUTO_PRODUCE_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_AUTO_PRODUCE_INTERVAL_SECS",
    description: "Seconds between auto-produce pump cycles.",
    group: EnvVarGroup::AiDrafting,
    secret: false,
    default: Some("30"),
    aliases: &[],
};

pub const BOS_AUTO_PRODUCE_MAX_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_AUTO_PRODUCE_MAX_PER_CYCLE",
    description: "Max LLM produce calls per auto-produce cycle (cost bound).",
    group: EnvVarGroup::AiDrafting,
    secret: false,
    default: Some("3"),
    aliases: &[],
};

pub const BOS_BUILD_SHA: EnvVar = EnvVar {
    name: "BOS_BUILD_SHA",
    description: "Git sha of the deployed build, stamped into the image by CI (publish-image.yml). Surfaced in /api/diagnostics/health so the support hub can verify which build a client runs. Unset = local/unstamped build.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED",
    description: "Enable the local call-input audio transcription pump. Off by default; also requires a configured source with consent_basis, intake dir, Whisper binary, and model.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CALL_INPUTS_MAX_AUDIO_BYTES: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_MAX_AUDIO_BYTES",
    description: "Maximum raw audio file size accepted by the local call-input transcription pump.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: Some("52428800"),
    aliases: &[],
};

pub const BOS_CALL_INPUTS_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_SYNC_ENABLED",
    description: "Enable the call-input source sync pump. Off by default; raw-audio transcription also requires BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CALL_INPUTS_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_SYNC_INTERVAL_SECS",
    description: "Seconds between call-input source sync pump cycles.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: Some("300"),
    aliases: &[],
};

pub const BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR",
    description: "Local directory watched by the call-input transcription pump for approved raw audio files. Files are staged through the configured call_inputs source; raw audio is not archived by BusinessOS.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY",
    description: "Maximum concurrent local call-input transcription jobs. Defaults to 1 for small single-CPU deployments.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR",
    description: "Temporary directory root for per-job local Whisper transcription work. Per-job temp dirs are cleaned on every exit path.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: Some("var/call-inputs-transcription"),
    aliases: &[],
};

pub const BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS",
    description: "Wall-clock timeout for one local Whisper call-input transcription job.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: Some("300000"),
    aliases: &[],
};

pub const BOS_CALL_INPUTS_WHISPER_BIN: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_WHISPER_BIN",
    description:
        "Path to the local whisper.cpp executable used for call-input raw-audio transcription.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CALL_INPUTS_WHISPER_MODEL: EnvVar = EnvVar {
    name: "BOS_CALL_INPUTS_WHISPER_MODEL",
    description: "Path or model id for the local whisper.cpp model used for call-input raw-audio transcription; base.en is the default deployment target for 1 CPU / 1 GB.",
    group: EnvVarGroup::CallsTranscription,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CLAIM_DRAFT_TO_ADDR: EnvVar = EnvVar {
    name: "BOS_CLAIM_DRAFT_TO_ADDR",
    description: "Recipient of approved shipping-damage packet Gmail drafts (the mailbox that handles carrier/platform filing — e.g. the owner's own address). Unset = claim approval refuses with claim_draft_to_addr_unset.",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on Stockforge API requests per claims sync cycle (damage list + pack-photo fetches).",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: Some("5"),
    aliases: &[],
};

pub const BOS_CLAIMS_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_CLAIMS_SYNC_ENABLED",
    description: "Run the shipping-damage claims pump (polls Stockforge OPEN damage events into the work queue). Off by default.",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CLAIMS_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_CLAIMS_SYNC_INTERVAL_SECS",
    description:
        "Seconds between claims pump cycles (min 300 — damage reports are not minute-urgent).",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_CLIENT_ID: EnvVar = EnvVar {
    name: "BOS_CLIENT_ID",
    description: "Client identifier stamped on every receipt and row. Default for local dev only.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: Some("dev"),
    aliases: &[],
};

pub const BOS_CLIENT_OVERLAY_DIR: EnvVar = EnvVar {
    name: "BOS_CLIENT_OVERLAY_DIR",
    description: "Client overlay directory (client.toml etc). Unset = built-in dev profile.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CONTENT_WEB_FACTS_ENABLED: EnvVar = EnvVar {
    name: "BOS_CONTENT_WEB_FACTS_ENABLED",
    description: "Opt-in (off by default) for content-draft web-fact enrichment on briefs that literally name a domain. Nested under the BOS_WEB_ENRICHMENT_ENABLED kill-switch — both must be on: this flag enables the feature, the global switch must also permit the web read.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CONTENT_PUBLISH_ADAPTER_URL: EnvVar = EnvVar {
    name: "BOS_CONTENT_PUBLISH_ADAPTER_URL",
    description: "Client-specific HTTP adapter that accepts approved content publish jobs. Unset means direct publishing is unavailable.",
    group: EnvVarGroup::ContentPublishing,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CONTENT_PUBLISH_ADAPTER_TOKEN: EnvVar = EnvVar {
    name: "BOS_CONTENT_PUBLISH_ADAPTER_TOKEN",
    description: "Bearer token used only for authenticated calls to the client-specific content publisher adapter.",
    group: EnvVarGroup::ContentPublishing,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_CONTENT_PUBLISH_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_CONTENT_PUBLISH_WRITE_ENABLED",
    description: "Enable approved content drafts to be written through the configured publisher adapter. Off by default; closed-gate jobs dry-run.",
    group: EnvVarGroup::ContentPublishing,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH: EnvVar = EnvVar {
    name: "BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH",
    description:
        "Maximum stored inbound messages the email enrichment backfill will reprocess per cycle.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("200"),
    aliases: &[],
};

pub const BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED: EnvVar = EnvVar {
    name: "BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED",
    description: "Enable the bounded runtime backfill that re-runs configured inbound email parsers over stored mail. Off by default.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS: EnvVar = EnvVar {
    name: "BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS",
    description: "Freshness window for cached email-triage provider facts. Positive CRM facts use this TTL; negative CRM facts use the smaller of this value and 1800 seconds.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("21600"),
    aliases: &[],
};

pub const BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE: EnvVar = EnvVar {
    name: "BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE",
    description: "Maximum live CRM fact lookups the email-triage resolver may spend for one newly ingested message. Reclassify stays cache-only.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("2"),
    aliases: &[],
};

pub const BOS_CRM_DEAL_VISIBILITY_POLICY: EnvVar = EnvVar {
    name: "BOS_CRM_DEAL_VISIBILITY_POLICY",
    description: "Controls visibility of cached CRM deal amounts. Allowed modes: shared, admin_only, or authorizer_only; empty/unset defaults to authorizer_only.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("authorizer_only"),
    aliases: &[],
};

pub const BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS: EnvVar = EnvVar {
    name: "BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS",
    description: "Comma-separated domain roots whose automated/platform sender addresses must not be treated as CRM contacts for inbox context or email-triage CRM facts. Subdomains match on dot boundaries.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("amazonses.com,hubspotemail.net,intuit.com,mailchimp.com,mandrillapp.com,myshopify.com,paypal.com,quickbooks.intuit.com,sendgrid.net,shopify.com,shopifyemail.com,squareup.com,stripe.com,zendesk.com"),
    aliases: &[],
};

pub const BOS_CRM_PROVIDER: EnvVar = EnvVar {
    name: "BOS_CRM_PROVIDER",
    description: "CRM provider receiving approved crm_activity notes: hubspot | espocrm.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("hubspot"),
    aliases: &[],
};

pub const BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on CRM API requests per cache sync cycle.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("8"),
    aliases: &[],
};

pub const BOS_CRM_READ_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_CRM_READ_SYNC_ENABLED",
    description: "Run the CRM cache sync. Off by default; manual sync still works.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_CRM_READ_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_CRM_READ_SYNC_INTERVAL_SECS",
    description: "Seconds between CRM cache sync cycles (minimum 300).",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS: EnvVar = EnvVar {
    name: "BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS",
    description: "Comma/space-separated Drive file ids excluded from the RAG corpus. Overrides the overlay [drive_corpus] exclude_file_ids when set.",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS: EnvVar = EnvVar {
    name: "BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS",
    description: "Comma-separated case-insensitive file-name patterns (`*` wildcard) excluded from the RAG corpus. Overrides the overlay value when set.",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_CORPUS_FOLDER_IDS: EnvVar = EnvVar {
    name: "BOS_DRIVE_CORPUS_FOLDER_IDS",
    description: "Comma/space-separated Google Drive folder ids whose DIRECT children form the RAG corpus. Overrides the overlay [drive_corpus] folder_ids when set; both unset = corpus unconfigured and the sync pump waits quietly.",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS: EnvVar = EnvVar {
    name: "BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS",
    description: "Comma/space-separated Drive file ids included in the RAG corpus regardless of folder. Overrides the overlay value when set.",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_CORPUS_USER_ID: EnvVar = EnvVar {
    name: "BOS_DRIVE_CORPUS_USER_ID",
    description: "Operator user whose Google credential the Drive corpus sync reads with (needs drive.readonly — reconnect Google after the scope joined the consent list). Unset = the only stored credential (single-account mode).",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_DRIVE_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on Google Drive API requests per sync cycle (listing pages and document reads each cost one).",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: Some("8"),
    aliases: &[],
};

pub const BOS_DRIVE_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_DRIVE_SYNC_ENABLED",
    description: "Run the Drive corpus sync pump (incremental, request-budgeted). Off by default; the manual Sync-now route works regardless.",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DRIVE_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_DRIVE_SYNC_INTERVAL_SECS",
    description: "Seconds between Drive corpus sync pump cycles (min 300 — reference docs rarely need to be fresher).",
    group: EnvVarGroup::DriveCorpus,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_ENRICHMENT_FRESHNESS_ENABLED: EnvVar = EnvVar {
    name: "BOS_ENRICHMENT_FRESHNESS_ENABLED",
    description:
        "Run the enrichment freshness pump for stale critical staged-draft fields. Off by default.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS",
    description: "Seconds between enrichment freshness pump cycles (min 300).",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE",
    description: "Hard cap on enrichment freshness engine runs per cycle; each run keeps the normal web/search budgets.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("3"),
    aliases: &[],
};

pub const BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS: EnvVar = EnvVar {
    name: "BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS",
    description:
        "Age in seconds after which accepted critical enrichment proposals are considered stale.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("2592000"),
    aliases: &[],
};

pub const BOS_ESPOCRM_API_KEY: EnvVar = EnvVar {
    name: "BOS_ESPOCRM_API_KEY",
    description: "EspoCRM API key (Administration → API Users; role must grant Note create).",
    group: EnvVarGroup::Crm,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_ESPOCRM_BASE_URL: EnvVar = EnvVar {
    name: "BOS_ESPOCRM_BASE_URL",
    description: "EspoCRM instance base URL (e.g. http://localhost:4580).",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ESPOCRM_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_ESPOCRM_WRITE_ENABLED",
    description: "Provider write gate for EspoCRM. Off (default) = approved CRM drafts deliver through the dry-run client and no note is created. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_GMAIL_INGEST_ENABLED: EnvVar = EnvVar {
    name: "BOS_GMAIL_INGEST_ENABLED",
    description: "Enable the Gmail ingestion pump (1/true/yes). Off by default.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_GMAIL_INGEST_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_GMAIL_INGEST_INTERVAL_SECS",
    description: "Seconds between Gmail ingestion polls.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("120"),
    aliases: &[],
};

pub const BOS_GMAIL_INGEST_QUERY: EnvVar = EnvVar {
    name: "BOS_GMAIL_INGEST_QUERY",
    description: "Gmail search query selecting messages to ingest.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: Some("in:inbox newer_than:14d"),
    aliases: &[],
};

pub const BOS_GMAIL_OAUTH_CLIENT_ID: EnvVar = EnvVar {
    name: "BOS_GMAIL_OAUTH_CLIENT_ID",
    description: "Google OAuth client id for the Gmail read connector.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &["GOOGLE_OAUTH_CLIENT_ID"],
};

pub const BOS_GMAIL_OAUTH_CLIENT_SECRET: EnvVar = EnvVar {
    name: "BOS_GMAIL_OAUTH_CLIENT_SECRET",
    description: "Google OAuth client secret for the Gmail read connector.",
    group: EnvVarGroup::EmailTriage,
    secret: true,
    default: None,
    aliases: &["GOOGLE_OAUTH_CLIENT_SECRET"],
};

pub const BOS_GMAIL_OAUTH_REFRESH_TOKEN: EnvVar = EnvVar {
    name: "BOS_GMAIL_OAUTH_REFRESH_TOKEN",
    description: "Google OAuth refresh token for the Gmail read connector.",
    group: EnvVarGroup::EmailTriage,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_GMAIL_OAUTH_SCOPES: EnvVar = EnvVar {
    name: "BOS_GMAIL_OAUTH_SCOPES",
    description: "Space/comma-separated OAuth scopes. Unset = unknown, scope check skipped.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_GMAIL_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_GMAIL_WRITE_ENABLED",
    description: "Provider write gate for Gmail DRAFT creation (never send). Off (default) = approved reply drafts deliver through the dry-run client and no Gmail draft is created. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_GMAIL_TRASH_ENABLED: EnvVar = EnvVar {
    name: "BOS_GMAIL_TRASH_ENABLED",
    description: "Provider write gate for explicitly moving Gmail messages to Trash. Off (default) = requests are audited and dry-run without changing Gmail. Requires gmail.modify and is independent from Gmail draft creation.",
    group: EnvVarGroup::EmailTriage,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_GOOGLE_CALENDAR_ID: EnvVar = EnvVar {
    name: "BOS_GOOGLE_CALENDAR_ID",
    description: "Calendar approved event drafts write to: \"primary\" or a specific calendar id (Google Calendar settings → calendar → Integrate). The connected account needs write access to it.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("primary"),
    aliases: &[],
};

pub const BOS_GOOGLE_CALENDAR_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_GOOGLE_CALENDAR_WRITE_ENABLED",
    description: "Provider write gate for Google Calendar. Off (default) = approved drafts deliver through the dry-run client and no event is created. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_ACCESS_TOKEN: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_ACCESS_TOKEN",
    description: "HubSpot private-app access token for CRM reads and the gated CRM write path.",
    group: EnvVarGroup::Crm,
    secret: true,
    default: None,
    aliases: &["HUBSPOT_PRIVATE_APP_TOKEN"],
};

pub const BOS_HUBSPOT_PORTAL_ID: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_PORTAL_ID",
    description: "HubSpot portal/account id used to build operator deep links to cached CRM contacts and deals.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY",
    description: "HubSpot deal property used as the closed date for close-rate/contact-to-close reporting (for example closedate or a client-specific close field).",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("closedate"),
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_LOST_STAGE_IDS: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_LOST_STAGE_IDS",
    description: "Comma-separated HubSpot deal stage ids that count as lost for close-rate reporting. Client/pipeline specific; no default.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS",
    description: "Comma-separated HubSpot deal stage ids that count as open for deal reporting. Optional today, retained so the pipeline mapping is complete and client-specific.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_PIPELINE_ID: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_PIPELINE_ID",
    description: "HubSpot deal pipeline id used for owner-report close-rate/contact-to-close metrics. Client specific; no default.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES",
    description: "Comma-separated HubSpot deal properties to retain as configured segment cuts in close-rate reporting (for example dealtype, territory, owner field). Optional.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY",
    description: "HubSpot deal property used as the started/contact date for contact-to-close reporting. Defaults to createdate but should be set per client when their pipeline uses a better field.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: Some("createdate"),
    aliases: &[],
};

pub const BOS_HUBSPOT_DEALS_WON_STAGE_IDS: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_DEALS_WON_STAGE_IDS",
    description: "Comma-separated HubSpot deal stage ids that count as won for close-rate reporting. Client/pipeline specific; no default.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_HUBSPOT_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_HUBSPOT_WRITE_ENABLED",
    description: "Provider write gate for HubSpot. Off (default) = approved CRM drafts deliver through the dry-run client and no note is created. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::Crm,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_INVOICE_NINJA_API_TOKEN: EnvVar = EnvVar {
    name: "BOS_INVOICE_NINJA_API_TOKEN",
    description:
        "Invoice Ninja API token (Settings → Account Management → Integrations → API tokens).",
    group: EnvVarGroup::InvoicingAccounting,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_INVOICE_NINJA_BASE_URL: EnvVar = EnvVar {
    name: "BOS_INVOICE_NINJA_BASE_URL",
    description: "Self-hosted Invoice Ninja base URL (e.g. http://localhost:8003).",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_INVOICE_NINJA_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_INVOICE_NINJA_WRITE_ENABLED",
    description: "Provider write gate for Invoice Ninja. Off (default) = approved ledger entries deliver through the dry-run client and nothing is recorded. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED: EnvVar = EnvVar {
    name: "BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED",
    description: "Run the approved-source lead discovery feed poller. Off by default.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS",
    description:
        "Seconds between approved-source lead discovery feed polling cycles (minimum 300).",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE",
    description: "Maximum new lead findings staged by one approved-source feed polling cycle.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("10"),
    aliases: &[],
};

pub const BOS_QBO_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_QBO_WRITE_ENABLED",
    description: "Provider write gate for QuickBooks Online (record-payment only). Off (default) = approved ledger entries deliver through the dry-run client and nothing is recorded. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_STRIPE_SECRET_KEY: EnvVar = EnvVar {
    name: "BOS_STRIPE_SECRET_KEY",
    description: "Stripe secret (or restricted) API key for BOS_ACCOUNTING_PROVIDER=stripe — invoice/customer reads, and (behind BOS_STRIPE_WRITE_ENABLED) draft-invoice creation. Prefer a restricted key scoped to Customers, Invoices, and Invoice Items read+write.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_STRIPE_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_STRIPE_WRITE_ENABLED",
    description: "Provider write gate for Stripe (create-invoice-DRAFT only; the invoice is never finalized or sent by BusinessOS). Off (default) = approved invoice drafts deliver through the dry-run client and nothing reaches Stripe. Flipping it is an attended, operator decision.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_ENABLED: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_ENABLED",
    description: "Run the owner-digest pump (generates reports when missing or stale, and evaluates scheduled delivery). Off by default; Generate-now in the Reports view always works.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_INTERVAL_SECS",
    description: "Seconds between owner-digest pump cycles (min 600 — a digest is at most daily-fresh; each stale period costs one LLM narration call).",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: Some("21600"),
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_DELIVERY_ENABLED: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_DELIVERY_ENABLED",
    description: "Explicit gate for scheduled owner-report Gmail draft delivery. Requires BOS_REPORT_DIGEST_ENABLED plus recipients and a due schedule. Off by default; manual Email-to-owners still works.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_TO_ADDR: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_TO_ADDR",
    description: "Recipient(s) of owner-digest Gmail drafts. Accepts comma/semicolon/space separated addresses and overrides overlay [owner_reports].recipients. Unset with no overlay recipients = the email-digest action refuses with owner_report_to_addr_unset.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_WEEKLY_WEEKDAY: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_WEEKLY_WEEKDAY",
    description: "Weekday for scheduled weekly owner-report delivery (monday..sunday). Overrides overlay [owner_reports].weekly_weekday. Unset disables weekly scheduled delivery.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_MTD_DAY: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_MTD_DAY",
    description: "Day of month for scheduled month-to-date owner-report delivery (1-31). Overrides overlay [owner_reports].mtd_day. Unset disables MTD scheduled delivery.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_METRICS: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_METRICS",
    description: "Ordered owner-report email metric ids, comma/space separated. Known ids: sales, calls, follow_ups, inventory, orders, damage_claims, site_traffic, close_rate. Overrides overlay [owner_reports].metrics; unknown ids are ignored.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR",
    description: "Recipient address list for owner-report Gmail drafts that must omit financial metrics and narration that may contain dollar figures. Accepts comma/semicolon/space separated addresses and augments overlay [owner_reports].recipient_profiles without sales metrics.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_REPORT_DIGEST_SUBJECT_PREFIX: EnvVar = EnvVar {
    name: "BOS_REPORT_DIGEST_SUBJECT_PREFIX",
    description: "Subject prefix for owner-report Gmail drafts. Overrides overlay [owner_reports].subject_prefix. Default: Owner digest.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS",
    description: "Comma-separated case-insensitive branded query patterns for Search Console cuts (supports `*` wildcard). Overrides overlay [search_console] branded_query_patterns.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS",
    description: "Comma/space-separated referrer-spam domains excluded from GA4 reporting views in addition to the vendored community list. Overrides overlay [search_console].analytics_excluded_referrer_domains. Raw GA4 snapshots are unchanged.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID",
    description: "Numeric GA4 property id for behavior/acquisition/conversion reporting. Overrides overlay [search_console] ga4_property_id. Unset renders GA4 metrics as pending setup.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on Google Search Console API requests per sync cycle.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("8"),
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_PROPERTY_URL: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_PROPERTY_URL",
    description: "Google Search Console property id/url to sync, e.g. sc-domain:example.com or https://www.example.com/. Overrides overlay [search_console] property_url.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_SYNC_DAYS: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_SYNC_DAYS",
    description: "Recent finalized Search Console days refreshed each sync cycle.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("90"),
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_SYNC_ENABLED",
    description: "Run the Search Console sync pump (read-only, request-budgeted). Off by default; manual Sync-now works regardless.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS",
    description: "Seconds between Search Console sync pump cycles (min 300).",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_SEARCH_CONSOLE_USER_ID: EnvVar = EnvVar {
    name: "BOS_SEARCH_CONSOLE_USER_ID",
    description: "Operator user whose Google credential reads the configured Search Console property. Unset = acting/only credential fallback.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_API_ENDPOINT: EnvVar = EnvVar {
    name: "BOS_LLM_API_ENDPOINT",
    description: "Override base URL for the LLM API backend. Unset = provider default.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_API_KEY: EnvVar = EnvVar {
    name: "BOS_LLM_API_KEY",
    description: "API key for the LLM API backend.",
    group: EnvVarGroup::LlmBackend,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_API_MODEL: EnvVar = EnvVar {
    name: "BOS_LLM_API_MODEL",
    description: "Model id for the LLM API backend.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_API_PROVIDER: EnvVar = EnvVar {
    name: "BOS_LLM_API_PROVIDER",
    description: "LLM API backend provider: anthropic | openai | openrouter.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: Some("anthropic"),
    aliases: &[],
};

pub const BOS_LLM_DEFAULT_BACKEND: EnvVar = EnvVar {
    name: "BOS_LLM_DEFAULT_BACKEND",
    description: "Typed-LLM backend route: api | harness | local. Unset defaults to api.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_DEFAULT_MODEL: EnvVar = EnvVar {
    name: "BOS_LLM_DEFAULT_MODEL",
    description: "Default model id/alias for the selected typed-LLM backend. Per-backend and per-purpose model settings override it.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_HARNESS_MODEL: EnvVar = EnvVar {
    name: "BOS_LLM_HARNESS_MODEL",
    description: "Model the harness session should use. Unset = harness default.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_HARNESS_PROGRAM: EnvVar = EnvVar {
    name: "BOS_LLM_HARNESS_PROGRAM",
    description: "Executable path/name for the local typed-LLM harness CLI.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: Some("claude"),
    aliases: &[],
};

pub const BOS_LLM_HARNESS_THINKING_LEVEL: EnvVar = EnvVar {
    name: "BOS_LLM_HARNESS_THINKING_LEVEL",
    description: "Thinking/effort level for harness sessions.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_LOCAL_API_KEY: EnvVar = EnvVar {
    name: "BOS_LLM_LOCAL_API_KEY",
    description: "Optional API key for the loopback-only OpenAI-compatible local LLM backend.",
    group: EnvVarGroup::LlmBackend,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_LOCAL_ENDPOINT: EnvVar = EnvVar {
    name: "BOS_LLM_LOCAL_ENDPOINT",
    description: "Loopback OpenAI-compatible endpoint for local inference (Ollama/LM Studio). Non-loopback endpoints are refused.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: Some("http://127.0.0.1:11434/v1/chat/completions"),
    aliases: &[],
};

pub const BOS_LLM_LOCAL_MODEL: EnvVar = EnvVar {
    name: "BOS_LLM_LOCAL_MODEL",
    description: "Default model id for the loopback-only local LLM backend.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_MAX_TOKENS: EnvVar = EnvVar {
    name: "BOS_LLM_MAX_TOKENS",
    description: "Max output tokens for API backend calls.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: Some("4096"),
    aliases: &[],
};

pub const BOS_LLM_ROUTE_OVERRIDES: EnvVar = EnvVar {
    name: "BOS_LLM_ROUTE_OVERRIDES",
    description: "Per-purpose typed-LLM routing overrides, comma list of purpose=api|harness|local optionally followed by :model (e.g. social_post_draft=local:qwen3). Local uses the loopback-only OpenAI-compatible profile and never falls back remotely.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LLM_TIMEOUT_MS: EnvVar = EnvVar {
    name: "BOS_LLM_TIMEOUT_MS",
    description: "Timeout for one typed LLM task execution.",
    group: EnvVarGroup::LlmBackend,
    secret: false,
    default: Some("120000"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_BATCH_SIZE: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_BATCH_SIZE",
    description: "Maximum email or receipt rows compacted in one receipted transaction.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("200"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_EMAIL_BODY_DAYS: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_EMAIL_BODY_DAYS",
    description: "Days to retain full plain-text and HTML email bodies; excerpts and metadata remain permanent.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("90"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_ENABLED: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_ENABLED",
    description: "Enable automatic bounded SQLite retention and storage maintenance.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES",
    description: "Maximum freelist pages requested from incremental_vacuum after each retention cycle; zero disables it.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("256"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_INTERVAL_SECS",
    description:
        "Seconds between automatic retention cycles; runtime clamps this to at least 900 seconds.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("21600"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE",
    description: "Maximum total email and receipt rows compacted in one retention cycle.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("5000"),
    aliases: &[],
};

pub const BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS: EnvVar = EnvVar {
    name: "BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS",
    description: "Days to retain before/after JSON on applied receipts for the explicit provider-mirror allowlist; receipt rows and idempotency fields remain permanent.",
    group: EnvVarGroup::DataRetention,
    secret: false,
    default: Some("90"),
    aliases: &[],
};

pub const BOS_DEBUG_ENABLED: EnvVar = EnvVar {
    name: "BOS_DEBUG_ENABLED",
    description: "Enable the operator Debug surface. Default off for production overlays; dev/all-slices can still enable via this flag.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_DEBUG_AGENT_MONITOR_TOKEN: EnvVar = EnvVar {
    name: "BOS_DEBUG_AGENT_MONITOR_TOKEN",
    description: "Bearer token used when posting to the local Agent Monitor /api/agents/sessions endpoint (the Debug spawn-agent action and the work-item launch-agent action share it). Unset = no Authorization header.",
    group: EnvVarGroup::InfraServer,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_DEBUG_AGENT_MONITOR_URL: EnvVar = EnvVar {
    name: "BOS_DEBUG_AGENT_MONITOR_URL",
    description: "Base URL for a local Agent Monitor instance. When set, the Debug surface (with BOS_DEBUG_ENABLED) can spawn a Codex agent with diagnostic context, and the work-item launch-agent action (with BOS_AGENT_LAUNCH_ENABLED) can spawn one with work-item context.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_LOG_LEVEL: EnvVar = EnvVar {
    name: "BOS_LOG_LEVEL",
    description: "Tracing filter (e.g. info, bos_app=debug).",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: Some("info"),
    aliases: &[],
};

pub const BOS_OUTBOX_DELIVERY_ENABLED: EnvVar = EnvVar {
    name: "BOS_OUTBOX_DELIVERY_ENABLED",
    description:
        "Run the outbox delivery worker (on by default; set 0 to pause all provider deliveries).",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_OUTBOX_DELIVERY_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_OUTBOX_DELIVERY_INTERVAL_SECS",
    description: "Seconds between outbox delivery polls.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("15"),
    aliases: &[],
};

pub const BOS_PUBLIC_BASE_URL: EnvVar = EnvVar {
    name: "BOS_PUBLIC_BASE_URL",
    description: "Externally reachable base URL (OAuth redirect URIs are derived from it).",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("http://127.0.0.1:4400"),
    aliases: &[],
};

pub const BOS_QBO_CLIENT_ID: EnvVar = EnvVar {
    name: "BOS_QBO_CLIENT_ID",
    description: "Intuit OAuth app client id for the QuickBooks Online read connector.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &["QUICKBOOKS_CLIENT_ID"],
};

pub const BOS_QBO_CLIENT_SECRET: EnvVar = EnvVar {
    name: "BOS_QBO_CLIENT_SECRET",
    description: "Intuit OAuth app client secret for the QuickBooks Online read connector.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: true,
    default: None,
    aliases: &["QUICKBOOKS_CLIENT_SECRET"],
};

pub const BOS_QBO_ENVIRONMENT: EnvVar = EnvVar {
    name: "BOS_QBO_ENVIRONMENT",
    description: "QuickBooks environment: sandbox | production. Selects the API base URL; the connected realm must match.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: Some("sandbox"),
    aliases: &["QUICKBOOKS_ENVIRONMENT"],
};

pub const BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on accounting-provider API requests per sync cycle (rate-limit budget; QBO allows ~500/min per realm, we stay far below).",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: Some("8"),
    aliases: &["BOS_QBO_MAX_REQUESTS_PER_CYCLE"],
};

pub const BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS",
    description: "Imported current-period freight deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS",
    description: "Imported current-period insurance deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS",
    description: "Imported current-period tax deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_METRIC_BASELINE_CENTS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_BASELINE_CENTS",
    description: "Imported monthly baseline value, in cents, for the configured accounting management metric. Used when automated provider baseline extraction is unavailable.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_METRIC_BASIS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_BASIS",
    description: "Accounting management metric basis: gross_margin | adjusted_gross_sales | invoice_totals. Overrides overlay [accounting.metric_basis].basis.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_METRIC_LABEL: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_METRIC_LABEL",
    description: "Operator-facing label for the configured accounting management metric. Overrides overlay [accounting.metric_basis].label.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_ACCOUNTING_PROVIDER: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_PROVIDER",
    description: "Accounting provider behind the Accounting views: qbo | invoice_ninja | stripe.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: Some("qbo"),
    aliases: &[],
};

pub const BOS_ACCOUNTING_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_SYNC_ENABLED",
    description: "Run the accounting sync pump (incremental, request-budgeted). Off by default; the manual Sync-now route works regardless.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &["BOS_QBO_SYNC_ENABLED"],
};

pub const BOS_ACCOUNTING_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_SYNC_INTERVAL_SECS",
    description: "Seconds between accounting sync pump cycles (min 300 — accounting data rarely needs to be fresher).",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: Some("1800"),
    aliases: &["BOS_QBO_SYNC_INTERVAL_SECS"],
};

pub const BOS_ACCOUNTING_VISIBILITY_POLICY: EnvVar = EnvVar {
    name: "BOS_ACCOUNTING_VISIBILITY_POLICY",
    description: "Internal BusinessOS accounting visibility policy. Allowed modes: shared, admin_only, or authorizer_only; empty/unset uses the overlay [accounting].visibility_policy, whose default is authorizer_only. QBO OAuth scopes remain provider-wide; this controls only who can see BusinessOS accounting views.",
    group: EnvVarGroup::InvoicingAccounting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OPERATOR_TOKEN: EnvVar = EnvVar {
    name: "BOS_OPERATOR_TOKEN",
    description: "Bearer token required on operator routes. Unset = open (local dev only).",
    group: EnvVarGroup::InfraServer,
    secret: true,
    default: None,
    aliases: &["DM_OPERATOR_TOKEN"],
};

pub const BOS_RELEASE_NOTES_WEBHOOK_SECRET: EnvVar = EnvVar {
    name: "BOS_RELEASE_NOTES_WEBHOOK_SECRET",
    description:
        "Bearer token required on /api/webhooks/release-notes. Unset = the webhook route 404s.",
    group: EnvVarGroup::SecurityWebhooks,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS",
    description: "Comma/space-separated operator user ids allowed to view, generate, and email owner reports. Overrides overlay [owner_reports].allowed_operator_user_ids. Empty = any authenticated operator.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID",
    description: "Email triage category whose inbound messages count as the owner-report call volume metric. Overrides overlay [owner_reports.call_volume].category_id. Unset with no overlay config renders the metric as pending data.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL",
    description: "Gmail label name used by the deployment for the call-summary source. Overrides overlay [owner_reports.call_volume].gmail_label; used for pending-data honesty, not as a second ingestion path.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY",
    description: "Gmail query/source selector that should include the call-summary emails counted by owner reports. Overrides overlay [owner_reports.call_volume].gmail_query; used for pending-data honesty, not as a second ingestion path.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_CALL_VOLUME_LABEL: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_CALL_VOLUME_LABEL",
    description: "Operator-facing label for the owner-report call volume KPI. Overrides overlay [owner_reports.call_volume].label.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL: EnvVar = EnvVar {
    name: "BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL",
    description: "Coverage/source wording for the owner-report call volume KPI, e.g. whether answering-service summaries represent all calls or only summarized calls. Overrides overlay [owner_reports.call_volume].source_label.",
    group: EnvVarGroup::Reporting,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SERVER_BIND: EnvVar = EnvVar {
    name: "BOS_SERVER_BIND",
    description: "Listen address for the HTTP server.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: Some("127.0.0.1:4400"),
    aliases: &[],
};

pub const BOS_SHOPIFY_ACCESS_TOKEN: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_ACCESS_TOKEN",
    description: "Shopify Admin API access token used by the read-only sales sync and approved customer-tier writes.",
    group: EnvVarGroup::CustomerTier,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_SHOPIFY_API_VERSION: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_API_VERSION",
    description:
        "Shopify Admin GraphQL API version used by Shopify sales sync and customer-tier writes.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: Some("2026-01"),
    aliases: &[],
};

pub const BOS_SHOPIFY_CLIENT_ID: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_CLIENT_ID",
    description: "Shopify custom app client id used to fetch Admin API access tokens when BOS_SHOPIFY_ACCESS_TOKEN is not set.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: None,
    aliases: &["SHOPIFY_CLIENT_ID", "SHOPIFY_API_KEY"],
};

pub const BOS_SHOPIFY_CLIENT_SECRET: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_CLIENT_SECRET",
    description: "Shopify custom app client secret used to fetch Admin API access tokens when BOS_SHOPIFY_ACCESS_TOKEN is not set.",
    group: EnvVarGroup::CustomerTier,
    secret: true,
    default: None,
    aliases: &["SHOPIFY_CLIENT_SECRET", "SHOPIFY_API_SECRET_KEY"],
};

pub const BOS_SHOPIFY_READ_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_READ_SYNC_ENABLED",
    description: "Enable the background Shopify sales cache sync. Off by default; manual sync remains available.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS",
    description:
        "Seconds between background Shopify sales cache sync cycles. Values below 300 are clamped.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("1800"),
    aliases: &[],
};

pub const BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE",
    description: "Maximum recent Shopify orders fetched per sync cycle. Values are clamped to Shopify's page cap.",
    group: EnvVarGroup::ConnectorsSync,
    secret: false,
    default: Some("250"),
    aliases: &[],
};

pub const BOS_SHOPIFY_SALES_VISIBILITY_POLICY: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_SALES_VISIBILITY_POLICY",
    description: "Controls Shopify sales dollar visibility. Allowed modes: shared, admin_only, or authorizer_only; empty/unset defaults to authorizer_only. Client overlays may set this to shared while CRM deal dollars remain authorizer_only.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: Some("authorizer_only"),
    aliases: &[],
};

pub const BOS_SHOPIFY_SHOP_DOMAIN: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_SHOP_DOMAIN",
    description:
        "Shopify shop domain (for example example.myshopify.com) for sales sync and approved customer-tier writes.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SHOPIFY_TIER_MAPPING_JSON: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_TIER_MAPPING_JSON",
    description: "Optional JSON object mapping QBO tier names to explicit Shopify targets. When set, it overrides overlay copy-through tier configuration. Example: {\"Wholesale\":{\"tag\":\"Wholesale\",\"metafield_namespace\":\"customer\",\"metafield_key\":\"tier\",\"metafield_value\":\"Wholesale\",\"segment_query\":\"customer_tags CONTAINS 'Wholesale'\"}}.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_SHOPIFY_WRITE_ENABLED: EnvVar = EnvVar {
    name: "BOS_SHOPIFY_WRITE_ENABLED",
    description: "Provider write gate for Shopify customer-tier sync. Off (default) = approved sync runs deliver through the dry-run client and no Shopify customer is changed. Opening it is an attended operator decision.",
    group: EnvVarGroup::CustomerTier,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_STATE_DIR: EnvVar = EnvVar {
    name: "BOS_STATE_DIR",
    description: "Directory holding the sqlite database and runtime state.",
    group: EnvVarGroup::InfraServer,
    secret: false,
    default: Some("./state"),
    aliases: &[],
};

pub const BOS_STOCKFORGE_API_KEY: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_API_KEY",
    description: "Stockforge org API key (sfk_live_…) for the read-only inventory connector — an org ADMIN creates a VIEWER-role key in Stockforge Settings → API Keys (shown once).",
    group: EnvVarGroup::InventoryClaims,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_STOCKFORGE_BASE_URL: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_BASE_URL",
    description: "Stockforge API base URL for the read-only inventory connector (e.g. https://api.stockforge.ai).",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_STOCKFORGE_APP_URL: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_APP_URL",
    description: "Stockforge user-facing app base URL for dashboard deep links (e.g. https://app.stockforge.ai). If unset, known api.stockforge.ai URLs are mapped to app.stockforge.ai.",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE",
    description: "Hard cap on Stockforge API requests per sync cycle (a full cycle needs ~5).",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: Some("10"),
    aliases: &[],
};

pub const BOS_STOCKFORGE_SYNC_ENABLED: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_SYNC_ENABLED",
    description: "Run the Stockforge sync pump (request-budgeted). Off by default; the manual Sync-now route works regardless.",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_STOCKFORGE_SYNC_INTERVAL_SECS: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_SYNC_INTERVAL_SECS",
    description: "Seconds between Stockforge sync pump cycles (min 120 — the order board likes to be fresh). Webhooks make this a fallback cadence.",
    group: EnvVarGroup::InventoryClaims,
    secret: false,
    default: Some("900"),
    aliases: &[],
};

pub const BOS_STOCKFORGE_WEBHOOK_SECRET: EnvVar = EnvVar {
    name: "BOS_STOCKFORGE_WEBHOOK_SECRET",
    description: "Per-endpoint secret from registering this server's /api/webhooks/stockforge URL as a Stockforge webhook (ADMIN, shown once). Unset = the webhook route 404s; verification is HMAC-SHA256.",
    group: EnvVarGroup::InventoryClaims,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_ENRICHMENT_ENABLED: EnvVar = EnvVar {
    name: "BOS_WEB_ENRICHMENT_ENABLED",
    description: "Kill-switch for guarded website enrichment (read-only fetch of an operator-authored domain that prefills eligible draft fields). Default ON — this is a read, gated only by the same operator note that named the domain; set to 0/false to disable the crawl entirely.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_API_KEY: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_API_KEY",
    description: "Bearer token for the optional web-search enrichment endpoint. Used only when BOS_WEB_SEARCH_ENRICHMENT_ENABLED is on.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: true,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS",
    description: "Per-enrichment paid-search budget in micros. Zero refuses search even when the feature gate is on.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("100000"),
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_ENABLED: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_ENABLED",
    description: "Enable external web-search enrichment for eligible draft fields. Off by default; LLMs receive only curated, cited search evidence, never arbitrary browser access.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT",
    description: "JSON web-search endpoint template for the generic/SearXNG provider, e.g. a self-hosted SearXNG https://searxng.local/search?q={query}&format=json. Common result JSON shapes are parsed (incl. SearXNG `content`). This is the recommended keyless default.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT",
    description: "Keyless fallback web-search endpoint queried (as a Generic provider, no API key) when the primary provider errors or rate-limits — e.g. a self-hosted SearXNG URL https://searxng.local/search?q={query}&format=json. Unset = no fallback.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES",
    description:
        "Max public result pages fetched through the guarded crawler per search enrichment run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("2"),
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES",
    description: "Max search queries per draft enrichment run.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("1"),
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS",
    description: "Max search results retained per query for draft enrichment diagnostics.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("3"),
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_PROVIDER: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_PROVIDER",
    description: "Web-search enrichment provider. Supported values: searxng (alias of generic — the recommended keyless self-hosted default, set BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT to its URL) | generic | tavily (needs an API key). Unset = generic endpoint path.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: None,
    aliases: &[],
};

pub const BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS: EnvVar = EnvVar {
    name: "BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS",
    description: "Timeout for one search API call during draft enrichment.",
    group: EnvVarGroup::WebEnrichmentSearch,
    secret: false,
    default: Some("10000"),
    aliases: &[],
};

/// Every registered variable. Keep sorted by name; the registry test asserts it.
pub const ALL: &[&EnvVar] = &[
    &BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE,
    &BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS,
    &BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS,
    &BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS,
    &BOS_ACCOUNTING_METRIC_BASELINE_CENTS,
    &BOS_ACCOUNTING_METRIC_BASIS,
    &BOS_ACCOUNTING_METRIC_LABEL,
    &BOS_ACCOUNTING_PROVIDER,
    &BOS_ACCOUNTING_SYNC_ENABLED,
    &BOS_ACCOUNTING_SYNC_INTERVAL_SECS,
    &BOS_ACCOUNTING_VISIBILITY_POLICY,
    &BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS,
    &BOS_AGENTIC_WEB_RESEARCH_ENABLED,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES,
    &BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS,
    &BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS,
    &BOS_AGENT_EVIDENCE_CLEANUP_ENABLED,
    &BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS,
    &BOS_AGENT_EVIDENCE_MAX_BYTES,
    &BOS_AGENT_EVIDENCE_RETENTION_DAYS,
    &BOS_AGENT_EVIDENCE_ROOT_DIR,
    &BOS_AGENT_LAUNCH_ENABLED,
    &BOS_AGENT_MCP_ENABLED,
    &BOS_AI_TRIAGE_ENABLED,
    &BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE,
    &BOS_AI_TRIAGE_MIN_CONFIDENCE,
    &BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED,
    &BOS_AUTO_PRODUCE_ENABLED,
    &BOS_AUTO_PRODUCE_INTERVAL_SECS,
    &BOS_AUTO_PRODUCE_MAX_PER_CYCLE,
    &BOS_BUFFER_ACCESS_TOKEN,
    &BOS_BUFFER_API_URL,
    &BOS_BUFFER_CHANNELS_JSON,
    &BOS_BUFFER_WRITE_ENABLED,
    &BOS_BUILD_SHA,
    &BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED,
    &BOS_CALL_INPUTS_MAX_AUDIO_BYTES,
    &BOS_CALL_INPUTS_SYNC_ENABLED,
    &BOS_CALL_INPUTS_SYNC_INTERVAL_SECS,
    &BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR,
    &BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY,
    &BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS,
    &BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR,
    &BOS_CALL_INPUTS_WHISPER_BIN,
    &BOS_CALL_INPUTS_WHISPER_MODEL,
    &BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE,
    &BOS_CLAIMS_SYNC_ENABLED,
    &BOS_CLAIMS_SYNC_INTERVAL_SECS,
    &BOS_CLAIM_DRAFT_TO_ADDR,
    &BOS_CLIENT_ID,
    &BOS_CLIENT_OVERLAY_DIR,
    &BOS_CONTENT_PUBLISH_ADAPTER_TOKEN,
    &BOS_CONTENT_PUBLISH_ADAPTER_URL,
    &BOS_CONTENT_PUBLISH_WRITE_ENABLED,
    &BOS_CONTENT_WEB_FACTS_ENABLED,
    &BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS,
    &BOS_CRM_DEAL_VISIBILITY_POLICY,
    &BOS_CRM_PROVIDER,
    &BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE,
    &BOS_CRM_READ_SYNC_ENABLED,
    &BOS_CRM_READ_SYNC_INTERVAL_SECS,
    &BOS_DATA_RETENTION_BATCH_SIZE,
    &BOS_DATA_RETENTION_EMAIL_BODY_DAYS,
    &BOS_DATA_RETENTION_ENABLED,
    &BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES,
    &BOS_DATA_RETENTION_INTERVAL_SECS,
    &BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE,
    &BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS,
    &BOS_DEBUG_AGENT_MONITOR_TOKEN,
    &BOS_DEBUG_AGENT_MONITOR_URL,
    &BOS_DEBUG_ENABLED,
    &BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS,
    &BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS,
    &BOS_DRIVE_CORPUS_FOLDER_IDS,
    &BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS,
    &BOS_DRIVE_CORPUS_USER_ID,
    &BOS_DRIVE_MAX_REQUESTS_PER_CYCLE,
    &BOS_DRIVE_SYNC_ENABLED,
    &BOS_DRIVE_SYNC_INTERVAL_SECS,
    &BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH,
    &BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED,
    &BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS,
    &BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE,
    &BOS_ENRICHMENT_FRESHNESS_ENABLED,
    &BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS,
    &BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE,
    &BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS,
    &BOS_ESPOCRM_API_KEY,
    &BOS_ESPOCRM_BASE_URL,
    &BOS_ESPOCRM_WRITE_ENABLED,
    &BOS_GMAIL_INGEST_ENABLED,
    &BOS_GMAIL_INGEST_INTERVAL_SECS,
    &BOS_GMAIL_INGEST_QUERY,
    &BOS_GMAIL_OAUTH_CLIENT_ID,
    &BOS_GMAIL_OAUTH_CLIENT_SECRET,
    &BOS_GMAIL_OAUTH_REFRESH_TOKEN,
    &BOS_GMAIL_OAUTH_SCOPES,
    &BOS_GMAIL_TRASH_ENABLED,
    &BOS_GMAIL_WRITE_ENABLED,
    &BOS_GOOGLE_CALENDAR_ID,
    &BOS_GOOGLE_CALENDAR_WRITE_ENABLED,
    &BOS_HUBSPOT_ACCESS_TOKEN,
    &BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY,
    &BOS_HUBSPOT_DEALS_LOST_STAGE_IDS,
    &BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS,
    &BOS_HUBSPOT_DEALS_PIPELINE_ID,
    &BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES,
    &BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY,
    &BOS_HUBSPOT_DEALS_WON_STAGE_IDS,
    &BOS_HUBSPOT_PORTAL_ID,
    &BOS_HUBSPOT_WRITE_ENABLED,
    &BOS_INVOICE_NINJA_API_TOKEN,
    &BOS_INVOICE_NINJA_BASE_URL,
    &BOS_INVOICE_NINJA_WRITE_ENABLED,
    &BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED,
    &BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS,
    &BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE,
    &BOS_LLM_API_ENDPOINT,
    &BOS_LLM_API_KEY,
    &BOS_LLM_API_MODEL,
    &BOS_LLM_API_PROVIDER,
    &BOS_LLM_DEFAULT_BACKEND,
    &BOS_LLM_DEFAULT_MODEL,
    &BOS_LLM_HARNESS_MODEL,
    &BOS_LLM_HARNESS_PROGRAM,
    &BOS_LLM_HARNESS_THINKING_LEVEL,
    &BOS_LLM_LOCAL_API_KEY,
    &BOS_LLM_LOCAL_ENDPOINT,
    &BOS_LLM_LOCAL_MODEL,
    &BOS_LLM_MAX_TOKENS,
    &BOS_LLM_ROUTE_OVERRIDES,
    &BOS_LLM_TIMEOUT_MS,
    &BOS_LOG_LEVEL,
    &BOS_OPERATOR_TOKEN,
    &BOS_OUTBOX_DELIVERY_ENABLED,
    &BOS_OUTBOX_DELIVERY_INTERVAL_SECS,
    &BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS,
    &BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID,
    &BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL,
    &BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY,
    &BOS_OWNER_REPORT_CALL_VOLUME_LABEL,
    &BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL,
    &BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS,
    &BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED,
    &BOS_PUBLIC_BASE_URL,
    &BOS_QBO_CLIENT_ID,
    &BOS_QBO_CLIENT_SECRET,
    &BOS_QBO_ENVIRONMENT,
    &BOS_QBO_WRITE_ENABLED,
    &BOS_RELEASE_NOTES_WEBHOOK_SECRET,
    &BOS_REPORT_DIGEST_DELIVERY_ENABLED,
    &BOS_REPORT_DIGEST_ENABLED,
    &BOS_REPORT_DIGEST_INTERVAL_SECS,
    &BOS_REPORT_DIGEST_METRICS,
    &BOS_REPORT_DIGEST_MTD_DAY,
    &BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR,
    &BOS_REPORT_DIGEST_SUBJECT_PREFIX,
    &BOS_REPORT_DIGEST_TO_ADDR,
    &BOS_REPORT_DIGEST_WEEKLY_WEEKDAY,
    &BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS,
    &BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS,
    &BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID,
    &BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE,
    &BOS_SEARCH_CONSOLE_PROPERTY_URL,
    &BOS_SEARCH_CONSOLE_SYNC_DAYS,
    &BOS_SEARCH_CONSOLE_SYNC_ENABLED,
    &BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS,
    &BOS_SEARCH_CONSOLE_USER_ID,
    &BOS_SERVER_BIND,
    &BOS_SHOPIFY_ACCESS_TOKEN,
    &BOS_SHOPIFY_API_VERSION,
    &BOS_SHOPIFY_CLIENT_ID,
    &BOS_SHOPIFY_CLIENT_SECRET,
    &BOS_SHOPIFY_READ_SYNC_ENABLED,
    &BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS,
    &BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE,
    &BOS_SHOPIFY_SALES_VISIBILITY_POLICY,
    &BOS_SHOPIFY_SHOP_DOMAIN,
    &BOS_SHOPIFY_TIER_MAPPING_JSON,
    &BOS_SHOPIFY_WRITE_ENABLED,
    &BOS_STATE_DIR,
    &BOS_STOCKFORGE_API_KEY,
    &BOS_STOCKFORGE_APP_URL,
    &BOS_STOCKFORGE_BASE_URL,
    &BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE,
    &BOS_STOCKFORGE_SYNC_ENABLED,
    &BOS_STOCKFORGE_SYNC_INTERVAL_SECS,
    &BOS_STOCKFORGE_WEBHOOK_SECRET,
    &BOS_STRIPE_SECRET_KEY,
    &BOS_STRIPE_WRITE_ENABLED,
    &BOS_WEB_ENRICHMENT_ENABLED,
    &BOS_WEB_SEARCH_ENRICHMENT_API_KEY,
    &BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS,
    &BOS_WEB_SEARCH_ENRICHMENT_ENABLED,
    &BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT,
    &BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT,
    &BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES,
    &BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES,
    &BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS,
    &BOS_WEB_SEARCH_ENRICHMENT_PROVIDER,
    &BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS,
];

/// Read a registered variable: primary name, then aliases (legacy secret
/// names from the predecessor deployment), then the default.
pub fn string(var: &EnvVar) -> Option<String> {
    for name in std::iter::once(var.name).chain(var.aliases.iter().copied()) {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    var.default.map(str::to_string)
}

/// Read a registered boolean variable ("1"/"true"/"yes" = true).
pub fn flag(var: &EnvVar) -> bool {
    string(var)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn usize_or(var: &EnvVar, default: usize) -> usize {
    string(var)
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn u64_or(var: &EnvVar, default: u64) -> u64 {
    string(var)
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgenticWebResearchConfig {
    pub enabled: bool,
    pub max_steps: usize,
    pub max_searches: usize,
    pub max_results: usize,
    pub max_fetched_pages: usize,
    pub max_page_bytes: usize,
    pub timeout_ms: u64,
    pub cost_budget_micros: u64,
    pub max_output_tokens: u64,
    pub max_concurrent_runs: usize,
}

pub fn agentic_web_research_config() -> AgenticWebResearchConfig {
    AgenticWebResearchConfig {
        enabled: flag(&BOS_AGENTIC_WEB_RESEARCH_ENABLED),
        max_steps: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS, 8),
        max_searches: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES, 2),
        max_results: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS, 10),
        max_fetched_pages: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES, 4),
        max_page_bytes: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES, 512 * 1024),
        timeout_ms: u64_or(&BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS, 90_000),
        cost_budget_micros: u64_or(&BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS, 0),
        max_output_tokens: u64_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS, 4_096),
        max_concurrent_runs: usize_or(&BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS, 1),
    }
}

pub fn web_search_enrichment_config() -> bos_integrations::web_search_enrichment::WebSearchConfig {
    use bos_integrations::web_search_enrichment::WebSearchProvider;

    let provider_raw = string(&BOS_WEB_SEARCH_ENRICHMENT_PROVIDER);
    let provider = WebSearchProvider::from_name(provider_raw.as_deref());

    bos_integrations::web_search_enrichment::WebSearchConfig {
        // Explicit gate only: naming a provider selects the backend, it does
        // not turn on paid search. BOS_WEB_SEARCH_ENRICHMENT_ENABLED is the
        // switch.
        enabled: flag(&BOS_WEB_SEARCH_ENRICHMENT_ENABLED),
        provider,
        endpoint_url: string(&BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT),
        api_key: string(&BOS_WEB_SEARCH_ENRICHMENT_API_KEY),
        fallback_endpoint_url: string(&BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT),
        max_queries: usize_or(&BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES, 1),
        max_results_per_query: usize_or(&BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS, 3),
        max_fetched_pages: usize_or(&BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES, 2),
        timeout_ms: u64_or(&BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS, 10_000),
        cost_budget_micros: u64_or(&BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS, 100_000),
    }
}

/// Markdown table of all registered variables, for REPO_MAP generation.
pub fn markdown_table() -> String {
    let mut out = String::from("| Variable | Default | Description |\n| --- | --- | --- |\n");
    for var in ALL {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            var.name,
            var.default
                .map(|d| format!("`{d}`"))
                .unwrap_or_else(|| "—".to_string()),
            var.description
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_and_unique() {
        let names: Vec<&str> = ALL.iter().map(|v| v.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            names, sorted,
            "ALL must be sorted by name with no duplicates"
        );
    }

    #[test]
    fn defaults_apply_when_unset() {
        assert_eq!(string(&BOS_LOG_LEVEL).as_deref(), Some("info"));
    }
}
