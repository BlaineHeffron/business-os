use bos_contracts::crm_cache::{CrmContactSnapshot, CrmDealSnapshot};
use bos_contracts::shopify_sales::{ShopifyCustomerSnapshotRow, ShopifyOrderSnapshotRow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyCandidate {
    pub source: String,
    pub source_id: String,
    pub display_name: Option<String>,
    pub company_name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedParty {
    pub selected: Option<PartyCandidate>,
    pub confidence: String,
    pub reason: String,
    pub candidates: Vec<PartyCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceHistory {
    pub allowed: bool,
    pub denied_reason: Option<String>,
    pub party: Option<PartyCandidate>,
    pub invoices: Vec<InvoiceSummary>,
    pub open_balance_cents: i64,
    pub overdue_balance_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceSummary {
    pub invoice_id: String,
    pub doc_number: Option<String>,
    pub customer_name: Option<String>,
    pub txn_date: Option<String>,
    pub due_date: Option<String>,
    pub total_amt_cents: i64,
    pub balance_cents: i64,
    pub voided: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductLookup {
    pub query: String,
    pub products: Vec<ProductSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductSummary {
    pub material_id: String,
    pub name: String,
    pub sku: Option<String>,
    pub quantity: f64,
    pub unit: Option<String>,
    pub warning_threshold: Option<f64>,
    pub critical_threshold: Option<f64>,
    pub unit_cost_cents: i64,
    pub lead_time_days: Option<i64>,
    pub vendor_name: Option<String>,
    pub availability_signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderStatusLookup {
    pub query: String,
    pub orders: Vec<OrderSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderSummary {
    pub source: String,
    pub order_id: String,
    pub order_number: String,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub total_amount_cents: Option<i64>,
    pub board_status: Option<String>,
    pub financial_status: Option<String>,
    pub fulfillment_status: Option<String>,
    pub carrier: Option<String>,
    pub tracking_number: Option<String>,
    pub tracking_url: Option<String>,
    pub ship_date: Option<String>,
    pub created_at: Option<String>,
    pub line_items_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrmContactLookup {
    pub email: Option<String>,
    pub company: Option<String>,
    pub contacts: Vec<CrmContactSnapshot>,
    pub deals: Vec<CrmDealSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopifyOrderGrounding {
    pub query: Option<String>,
    pub email: Option<String>,
    pub orders: Vec<ShopifyOrderSnapshotRow>,
    pub customers: Vec<ShopifyCustomerSnapshotRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorConversationLookup {
    pub sender_email: String,
    pub records: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub source_key: String,
    pub thread_id: Option<String>,
    pub internal_date_ms: Option<i64>,
    pub from_addr: Option<String>,
    pub subject: Option<String>,
    pub category: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTranscriptLookup {
    pub allowed: bool,
    pub denied_reason: Option<String>,
    pub query: String,
    pub calls: Vec<CallSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSummary {
    pub call_input_id: String,
    pub title: String,
    pub summary: String,
    pub caller_name: Option<String>,
    pub caller_phone: Option<String>,
    pub caller_email: Option<String>,
    pub occurred_at_ms: Option<u64>,
    pub excerpt: String,
}
