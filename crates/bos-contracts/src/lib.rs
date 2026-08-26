//! Browser-safe wire contracts. Depends on nothing in the workspace.
//!
//! Rules:
//! - Types here are serde round-trip stable; breaking a field is a contract change.
//! - No transport, runtime, or provider code.
//! - Slices add their wire types in a submodule named after the slice.

pub mod accounting;
pub mod admin_settings;
pub mod ai_usage;
pub mod calendar_drafts;
pub mod call_inputs;
pub mod claim_drafts;
pub mod client_profile;
pub mod content_drafts;
pub mod content_plans;
pub mod crm_cache;
pub mod crm_drafts;
pub mod crm_record_drafts;
pub mod crm_sales_intent;
pub mod customer_tier_sync;
pub mod data_retention;
pub mod debug;
pub mod drive_corpus;
pub mod email_drafts;
pub mod email_identity;
pub mod email_triage;
pub mod enrichment;
pub mod follow_up_tasks;
pub mod google_connector;
pub mod home_dashboard;
pub mod instance_diagnostics;
pub mod inventory;
pub mod invoice_drafts;
pub mod lead_discovery;
pub mod ledger_drafts;
pub mod llm_settings;
pub mod mutation;
pub mod operator_notes;
pub mod operator_users;
pub mod outbox;
pub mod owner_reports;
pub mod packet_proposals;
pub mod produce;
pub mod quote_workflows;
pub mod receipt;
pub mod release_notes;
pub mod search_console;
pub mod shopify_sales;
pub mod social_publishing;
pub mod source;
pub mod work_queue;
