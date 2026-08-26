//! Shared read-only grounding toolbox for cached business data.
//!
//! The functions in this module are the single read layer used by deterministic
//! produce context and agentic tool registries. They are side-effect-free except
//! for explicit `append_grounding_evidence` audit rows, which are record-only and
//! never feed back into draft staging.
//!
//! Scope policy is per source:
//! - inventory product/order reads and party identity references are
//!   client-global operational data and are allowed for all operator scopes;
//! - accounting invoice/balance reads require the existing accounting cached
//!   financial visibility helper;
//! - call transcript reads are sensitive and require all scope;
//! - email history reads use the existing source-user OperatorScope filter.

mod lookup;
mod render;
pub mod store;
#[cfg(test)]
mod tests;
mod tools;
mod types;
mod util;

pub use lookup::{
    call_transcript_lookup, crm_contact_lookup, customer_invoice_history, order_status_lookup,
    prior_conversation_lookup, product_lookup, resolve_party, shopify_order_grounding,
};
pub use render::{
    render_call_transcripts, render_crm_contact, render_invoice_history, render_orders,
    render_prior_conversation, render_products, render_shopify_order_grounding,
};
pub use store::{append_grounding_evidence, grounding_evidence_for_item};
pub use store::{GroundingEvidenceRow, NewGroundingEvidence};
pub use tools::{
    call_transcript_tool_payload, crm_contact_tool_payload, customer_invoice_history_tool_payload,
    denied_tool_result, email_thread_tool_payload, grounding_tool_definitions,
    grounding_tool_definitions_for, order_status_tool_payload, prior_conversation_tool_payload,
    product_tool_payload,
};
pub use types::{
    CallSummary, CallTranscriptLookup, ConversationSummary, CrmContactLookup, InvoiceHistory,
    InvoiceSummary, OrderStatusLookup, OrderSummary, PartyCandidate, PriorConversationLookup,
    ProductLookup, ProductSummary, ResolvedParty, ShopifyOrderGrounding,
};

pub const GROUNDING_ACTOR: &str = "grounding";
pub const TOOL_EMAIL_THREAD_LOOKUP: &str = "email_thread_lookup";
pub const TOOL_RESOLVE_PARTY: &str = "resolve_party";
pub const TOOL_CUSTOMER_INVOICE_HISTORY: &str = "customer_invoice_history";
pub const TOOL_PRODUCT_LOOKUP: &str = "product_lookup";
pub const TOOL_ORDER_STATUS_LOOKUP: &str = "order_status_lookup";
pub const TOOL_CRM_CONTACT_LOOKUP: &str = "crm_contact_lookup";
pub const TOOL_PRIOR_CONVERSATION_LOOKUP: &str = "prior_conversation_lookup";
pub const TOOL_CALL_TRANSCRIPT_LOOKUP: &str = "call_transcript_lookup";

const MAX_PARTY_CANDIDATES: usize = 8;
const MAX_INVOICES: usize = 8;
const MAX_PRODUCTS: usize = 8;
const MAX_ORDERS: usize = 8;
const MAX_CRM_CONTACTS: usize = 8;
const MAX_CRM_DEALS: usize = 8;
const MAX_SHOPIFY_CUSTOMERS: usize = 4;
const MAX_EMAIL_RECORDS: usize = 8;
const MAX_CALL_RECORDS: usize = 5;
pub(super) const MAX_EXCERPT_CHARS: usize = 2_000;
