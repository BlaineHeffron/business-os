use std::collections::BTreeSet;

use bos_contracts::crm_cache::{CrmContactSnapshot, CrmDealSnapshot};
use bos_contracts::shopify_sales::{ShopifyCustomerSnapshotRow, ShopifyOrderSnapshotRow};

use super::types::{OrderSummary, PartyCandidate};

pub(super) fn dedupe_party_candidates(candidates: Vec<PartyCandidate>) -> Vec<PartyCandidate> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let key = (
            candidate
                .email
                .as_deref()
                .and_then(normalized_email)
                .unwrap_or_default(),
            candidate
                .company_name
                .as_deref()
                .or(candidate.display_name.as_deref())
                .map(normalized_name)
                .unwrap_or_default(),
        );
        if seen.insert(key) {
            out.push(candidate);
        }
    }
    out
}

pub(super) fn dedupe_crm_contacts(contacts: Vec<CrmContactSnapshot>) -> Vec<CrmContactSnapshot> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for contact in contacts {
        let key = (
            contact.provider_contact_id.clone(),
            contact
                .email
                .as_deref()
                .and_then(normalized_email)
                .unwrap_or_default(),
        );
        if seen.insert(key) {
            out.push(contact);
        }
    }
    out
}

pub(super) fn dedupe_crm_deals(deals: Vec<CrmDealSnapshot>) -> Vec<CrmDealSnapshot> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for deal in deals {
        if seen.insert(deal.provider_deal_id.clone()) {
            out.push(deal);
        }
    }
    out
}

pub(super) fn dedupe_shopify_orders(
    orders: Vec<ShopifyOrderSnapshotRow>,
) -> Vec<ShopifyOrderSnapshotRow> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for order in orders {
        if seen.insert(order.order_id.clone()) {
            out.push(order);
        }
    }
    out
}

pub(super) fn dedupe_shopify_customers(
    customers: Vec<ShopifyCustomerSnapshotRow>,
) -> Vec<ShopifyCustomerSnapshotRow> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for customer in customers {
        if seen.insert(customer.customer_id.clone()) {
            out.push(customer);
        }
    }
    out
}

pub(super) fn party_names(party: &PartyCandidate) -> BTreeSet<String> {
    [party.display_name.as_deref(), party.company_name.as_deref()]
        .into_iter()
        .flatten()
        .map(normalized_name)
        .filter(|value| !value.is_empty())
        .collect()
}

pub(super) fn availability_signal(
    quantity: f64,
    warning_threshold: Option<f64>,
    critical_threshold: Option<f64>,
) -> String {
    if quantity <= 0.0 {
        return "out_of_stock".to_string();
    }
    if critical_threshold.is_some_and(|threshold| quantity <= threshold) {
        return "critical".to_string();
    }
    if warning_threshold.is_some_and(|threshold| quantity <= threshold) {
        return "low".to_string();
    }
    "available".to_string()
}

pub(super) fn order_summary(
    order: crate::slices::inventory::store::OrderSnapshotRow,
) -> OrderSummary {
    OrderSummary {
        source: "inventory".to_string(),
        order_id: order.order_id,
        order_number: order.order_number,
        customer_name: order.customer_name,
        customer_email: order.customer_email,
        total_amount_cents: Some(order.total_amount_cents),
        board_status: Some(order.board_status),
        financial_status: None,
        fulfillment_status: None,
        carrier: order.carrier,
        tracking_number: order.tracking_number,
        tracking_url: None,
        ship_date: order.ship_date,
        created_at: order.order_date,
        line_items_summary: None,
    }
}

pub(super) fn shopify_order_summary(order: ShopifyOrderSnapshotRow) -> OrderSummary {
    OrderSummary {
        source: "shopify".to_string(),
        order_id: order.order_id,
        order_number: order.order_number,
        customer_name: order.customer_name,
        customer_email: order.customer_email,
        total_amount_cents: order.total_cents,
        board_status: None,
        financial_status: order.financial_status,
        fulfillment_status: order.fulfillment_status,
        carrier: order.carrier,
        tracking_number: order.tracking_number,
        tracking_url: None,
        ship_date: None,
        created_at: order.created_at,
        line_items_summary: Some(order.line_items_summary),
    }
}

pub(super) fn normalized_email(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.contains('@') {
        Some(trimmed)
    } else {
        None
    }
}

pub(super) fn normalized_name(raw: &str) -> String {
    raw.chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() || ch == '-' || ch == '_' {
                Some(' ')
            } else {
                None
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn normalize_identifier(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

pub(super) fn capped_chars(raw: &str, limit: usize) -> String {
    let mut out: String = raw.trim().chars().take(limit).collect();
    if raw.trim().chars().count() > limit {
        out.push_str("...");
    }
    out
}

pub(super) fn cents(cents: i64) -> f64 {
    cents as f64 / 100.0
}

pub(super) fn render_money(cents_value: i64, currency: Option<&str>) -> String {
    match currency {
        Some(currency) if !currency.trim().is_empty() => {
            format!("{} ${:.2}", currency.trim(), cents(cents_value))
        }
        _ => format!("${:.2}", cents(cents_value)),
    }
}

pub(super) fn utc_civil_date(now_ms: u64) -> String {
    crate::produce::epoch_ms_to_rfc3339_utc(now_ms)
        .get(..10)
        .unwrap_or("9999-99-99")
        .to_string()
}

pub(super) fn is_past_civil_date(raw: &str, today: &str) -> bool {
    raw < today
}
