//! Provider read integrations (Gmail read path, ported from agent-monitor-rust).
//!
//! HARD RULE: no `std::env::var` anywhere in this crate. All configuration
//! arrives as explicit config structs ([`google_oauth::GoogleOAuthConfig`],
//! [`gmail_inbox_read::GmailReadConfig`]) constructed by the caller (bos-app
//! `env_registry` / client overlay). Gmail writes are draft creation
//! ([`gmail_draft_write`], gated; never send) plus explicit, separately gated
//! message trashing ([`gmail_trash_write`]).
//! [`google_calendar`] adds the approval-gated event-create write client and
//! [`hubspot`]/[`espocrm`] the gated CRM note-create clients — all dry-run
//! unless their write config explicitly enables execution.
//!
//! Also hosts the typed-LLM transport (ported from agent-monitor-rust):
//! [`llm_typed_tasks`] (bounded typed-task contract + input credential scrub),
//! [`llm_api`] (direct API clients: anthropic/openai/openrouter), and
//! [`llm_harness`] (local Claude CLI via tmux, billed to the subscription).
//! Routing/config lives in bos-app `llm.rs`.

pub mod accounting_read;
pub mod buffer;
pub mod crm_read;
pub mod espocrm;
pub mod evidence;
pub mod gmail_draft_write;
pub mod gmail_http;
pub mod gmail_inbox_read;
pub mod gmail_mime;
pub mod gmail_trash_write;
pub mod gmail_triage_rules;
pub mod google_analytics_data;
mod google_api_errors;
pub mod google_calendar;
pub mod google_drive_read;
pub mod google_oauth;
pub mod google_search_console;
pub mod hubspot;
pub mod invoice_ninja;
pub mod llm_api;
pub mod llm_harness;
pub mod llm_typed_tasks;
pub mod qbo_common;
pub mod qbo_oauth;
pub mod qbo_payment_write;
pub mod qbo_read;
pub mod shopify;
pub mod shopify_oauth;
pub mod shopify_sales_read;
pub mod stockforge_read;
pub mod stripe;
pub mod web_page_read;
pub mod web_search_enrichment;

pub use gmail_http::{GmailHttp, ReqwestGmailHttpClient};
pub use gmail_inbox_read::{
    GmailInboxReadAdapter, GmailInboxReadClient, GmailInboxReadContext, GmailInboxReadRequest,
    GmailReadConfig, LiveGmailInboxReadClient,
};
pub use google_calendar::{
    google_calendar_execution_client, DryRunGoogleCalendarClient, GoogleCalendarExecutionClient,
    GoogleCalendarWriteConfig, LiveGoogleCalendarClient,
};
pub use google_oauth::GoogleOAuthConfig;
