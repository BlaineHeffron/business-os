use super::types::{
    CallTranscriptLookup, CrmContactLookup, InvoiceHistory, OrderStatusLookup,
    PriorConversationLookup, ProductLookup, ShopifyOrderGrounding,
};
use super::util::{capped_chars, cents, render_money};
use super::MAX_EXCERPT_CHARS;

pub fn render_invoice_history(history: &InvoiceHistory) -> Option<String> {
    if !history.allowed || history.invoices.is_empty() {
        return None;
    }
    let mut out = String::from("Cached customer invoice history:\n");
    if let Some(party) = &history.party {
        out.push_str(&format!(
            "- Party: {} <{}>\n",
            party
                .company_name
                .as_deref()
                .or(party.display_name.as_deref())
                .unwrap_or("(unknown)"),
            party.email.as_deref().unwrap_or("unknown")
        ));
    }
    out.push_str(&format!(
        "- Open balance: ${:.2}; overdue balance: ${:.2}\n",
        cents(history.open_balance_cents),
        cents(history.overdue_balance_cents)
    ));
    for invoice in &history.invoices {
        out.push_str(&format!(
            "- {}: customer={}, txn={}, due={}, total=${:.2}, balance=${:.2}, voided={}\n",
            invoice.doc_number.as_deref().unwrap_or(&invoice.invoice_id),
            invoice.customer_name.as_deref().unwrap_or("(unknown)"),
            invoice.txn_date.as_deref().unwrap_or("unknown"),
            invoice.due_date.as_deref().unwrap_or("unknown"),
            cents(invoice.total_amt_cents),
            cents(invoice.balance_cents),
            invoice.voided
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_prior_conversation(prior: &PriorConversationLookup) -> Option<String> {
    if prior.records.is_empty() {
        return None;
    }
    let mut out = format!("Prior cached conversations with {}:\n", prior.sender_email);
    for record in &prior.records {
        out.push_str(&format!(
            "- {} | {} | {}\n{}\n",
            record.internal_date_ms.unwrap_or_default(),
            record.subject.as_deref().unwrap_or("(no subject)"),
            record.category,
            record.excerpt
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_products(products: &ProductLookup) -> Option<String> {
    if products.products.is_empty() {
        return None;
    }
    let mut out = format!("Cached product lookup for '{}':\n", products.query);
    for product in &products.products {
        out.push_str(&format!(
            "- {}{}: qty={} {}, availability={}, lead_time_days={:?}, vendor={}\n",
            product.name,
            product
                .sku
                .as_deref()
                .map(|sku| format!(" ({sku})"))
                .unwrap_or_default(),
            product.quantity,
            product.unit.as_deref().unwrap_or("units"),
            product.availability_signal,
            product.lead_time_days,
            product.vendor_name.as_deref().unwrap_or("unknown")
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_orders(orders: &OrderStatusLookup) -> Option<String> {
    if orders.orders.is_empty() {
        return None;
    }
    let mut out = format!("Cached order lookup for '{}':\n", orders.query);
    for order in &orders.orders {
        let shopify_total = if order.source == "shopify" {
            order
                .total_amount_cents
                .map(|total| format!(", order_total={}", render_money(total, None)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- {} [{}]: status={}, customer={}, carrier={}, tracking={}, ship_date={}, created_at={}, items={}{}\n",
            order.order_number,
            order.source,
            order
                .board_status
                .as_deref()
                .or(order.fulfillment_status.as_deref())
                .or(order.financial_status.as_deref())
                .unwrap_or("unknown"),
            order.customer_name.as_deref().unwrap_or("unknown"),
            order.carrier.as_deref().unwrap_or("unknown"),
            order.tracking_number.as_deref().unwrap_or("unknown"),
            order.ship_date.as_deref().unwrap_or("unknown"),
            order.created_at.as_deref().unwrap_or("unknown"),
            order.line_items_summary.as_deref().unwrap_or("unknown"),
            shopify_total
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_crm_contact(lookup: &CrmContactLookup) -> Option<String> {
    if lookup.contacts.is_empty() && lookup.deals.is_empty() {
        return None;
    }
    let mut out = String::from("Cached CRM contact context:\n");
    for contact in &lookup.contacts {
        out.push_str(&format!(
            "- Contact: {} <{}>, company={}, stage={}, owner={}, last_activity={}\n",
            contact.name.as_deref().unwrap_or("(unknown)"),
            contact.email.as_deref().unwrap_or("unknown"),
            contact.company.as_deref().unwrap_or("unknown"),
            contact.lifecycle_stage.as_deref().unwrap_or("unknown"),
            contact.owner.as_deref().unwrap_or("unknown"),
            contact.last_activity_at.as_deref().unwrap_or("unknown")
        ));
    }
    for deal in &lookup.deals {
        let amount = match (
            deal.amount_visible,
            deal.amount_cents,
            deal.currency.as_deref(),
        ) {
            (true, Some(cents_value), Some(currency)) => {
                format!("{currency} {:.2}", cents(cents_value))
            }
            (true, Some(cents_value), None) => format!("{:.2}", cents(cents_value)),
            _ => "redacted".to_string(),
        };
        out.push_str(&format!(
            "- Deal: {} | stage={} | amount={} | close_date={} | pipeline={}\n",
            deal.name.as_deref().unwrap_or("(unknown)"),
            deal.stage.as_deref().unwrap_or("unknown"),
            amount,
            deal.close_date.as_deref().unwrap_or("unknown"),
            deal.pipeline.as_deref().unwrap_or("unknown")
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_shopify_order_grounding(lookup: &ShopifyOrderGrounding) -> Option<String> {
    if lookup.orders.is_empty() && lookup.customers.is_empty() {
        return None;
    }
    let mut out = String::from("Cached Shopify order context:\n");
    for customer in &lookup.customers {
        let total_spent = customer
            .total_spent_cents
            .map(|total| format!(", total_spent={}", render_money(total, None)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- Customer: {} <{}>, orders_count={}, tier={}{}\n",
            customer.name.as_deref().unwrap_or("(unknown)"),
            customer.email.as_deref().unwrap_or("unknown"),
            customer.orders_count,
            customer.tier.as_deref().unwrap_or("unknown"),
            total_spent
        ));
    }
    for order in &lookup.orders {
        let order_total = order
            .total_cents
            .map(|total| {
                format!(
                    ", order_total={}",
                    render_money(total, order.currency.as_deref())
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "- {} [shopify]: financial_status={}, fulfillment_status={}, customer={}, carrier={}, tracking={}, created_at={}, items={}{}\n",
            order.order_number,
            order.financial_status.as_deref().unwrap_or("unknown"),
            order.fulfillment_status.as_deref().unwrap_or("unknown"),
            order.customer_name.as_deref().unwrap_or("unknown"),
            order.carrier.as_deref().unwrap_or("unknown"),
            order.tracking_number.as_deref().unwrap_or("unknown"),
            order.created_at.as_deref().unwrap_or("unknown"),
            order.line_items_summary,
            order_total
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}

pub fn render_call_transcripts(lookup: &CallTranscriptLookup) -> Option<String> {
    if !lookup.allowed || lookup.calls.is_empty() {
        return None;
    }
    let mut out = format!("Cached call transcript lookup for '{}':\n", lookup.query);
    for call in &lookup.calls {
        out.push_str(&format!(
            "- {} | caller={} <{}> | occurred_at_ms={}\n{}\n",
            call.title,
            call.caller_name.as_deref().unwrap_or("unknown"),
            call.caller_email.as_deref().unwrap_or("unknown"),
            call.occurred_at_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            call.excerpt
        ));
    }
    Some(capped_chars(&out, MAX_EXCERPT_CHARS))
}
