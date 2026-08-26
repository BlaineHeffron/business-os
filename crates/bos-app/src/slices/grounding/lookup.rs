use rusqlite::Connection;

use crate::http::OperatorScope;
use crate::overlay::AccountingVisibilityPolicy;
use crate::store_core::StoreError;

use super::types::{
    CallSummary, CallTranscriptLookup, ConversationSummary, CrmContactLookup, InvoiceHistory,
    InvoiceSummary, OrderStatusLookup, PartyCandidate, PriorConversationLookup, ProductLookup,
    ProductSummary, ResolvedParty, ShopifyOrderGrounding,
};
use super::util::{
    availability_signal, capped_chars, dedupe_crm_contacts, dedupe_crm_deals,
    dedupe_party_candidates, dedupe_shopify_customers, dedupe_shopify_orders, is_past_civil_date,
    normalize_identifier, normalized_email, normalized_name, order_summary, party_names,
    shopify_order_summary, utc_civil_date,
};
use super::{
    MAX_CALL_RECORDS, MAX_CRM_CONTACTS, MAX_CRM_DEALS, MAX_EMAIL_RECORDS, MAX_INVOICES, MAX_ORDERS,
    MAX_PARTY_CANDIDATES, MAX_PRODUCTS, MAX_SHOPIFY_CUSTOMERS,
};

pub fn resolve_party(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    email: Option<&str>,
    name: Option<&str>,
) -> Result<ResolvedParty, StoreError> {
    let email = email.and_then(normalized_email);
    let name = name.map(normalized_name).filter(|value| !value.is_empty());
    let mut candidates = Vec::new();
    for customer in crate::slices::accounting::store::list_customers(conn, client_id)? {
        if email.as_deref().is_some_and(|needle| {
            customer
                .email
                .as_deref()
                .and_then(normalized_email)
                .as_deref()
                == Some(needle)
        }) || name.as_deref().is_some_and(|needle| {
            normalized_name(&customer.display_name) == needle
                || customer
                    .company_name
                    .as_deref()
                    .is_some_and(|value| normalized_name(value) == needle)
        }) {
            candidates.push(PartyCandidate {
                source: "accounting_customer".to_string(),
                source_id: customer.customer_id,
                display_name: Some(customer.display_name),
                company_name: customer.company_name,
                email: customer.email,
            });
        }
    }
    for order in crate::slices::inventory::store::list_orders(conn, client_id)? {
        if email.as_deref().is_some_and(|needle| {
            order
                .customer_email
                .as_deref()
                .and_then(normalized_email)
                .as_deref()
                == Some(needle)
        }) || name.as_deref().is_some_and(|needle| {
            order
                .customer_name
                .as_deref()
                .is_some_and(|value| normalized_name(value) == needle)
        }) {
            candidates.push(PartyCandidate {
                source: "inventory_order_customer".to_string(),
                source_id: order.order_id,
                display_name: order.customer_name.clone(),
                company_name: order.customer_name,
                email: order.customer_email,
            });
        }
    }
    candidates = dedupe_party_candidates(candidates);
    let exact_email_candidates = if email.is_some() {
        candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .email
                    .as_deref()
                    .and_then(normalized_email)
                    .as_deref()
                    == email.as_deref()
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if email.is_some() && exact_email_candidates.len() == 1 {
        return Ok(ResolvedParty {
            selected: exact_email_candidates.first().cloned(),
            confidence: "high".to_string(),
            reason: "exact_email".to_string(),
            candidates: exact_email_candidates,
        });
    }
    let reason = if email.is_some() && exact_email_candidates.len() > 1 {
        "ambiguous_email"
    } else if candidates.is_empty() {
        "no_match"
    } else if email.is_none() && name.is_some() {
        "name_candidates_only"
    } else {
        "candidates_only"
    };
    Ok(ResolvedParty {
        selected: None,
        confidence: if candidates.is_empty() {
            "none"
        } else {
            "ambiguous"
        }
        .to_string(),
        reason: reason.to_string(),
        candidates: candidates.into_iter().take(MAX_PARTY_CANDIDATES).collect(),
    })
}

/// Reads cached invoice history for the resolved party. V1 can only associate
/// invoices by normalized customer_name; tighten this to customer_id/email once
/// the accounting store exposes a keyed lookup.
pub fn customer_invoice_history(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    policy: AccountingVisibilityPolicy,
    party: Option<&PartyCandidate>,
    now_ms: u64,
) -> Result<InvoiceHistory, StoreError> {
    if !crate::slices::accounting::service::cached_financial_visibility_allowed(
        conn, client_id, scope, policy,
    )? {
        tracing::debug!("grounding denied: accounting visibility");
        return Ok(InvoiceHistory {
            allowed: false,
            denied_reason: Some("accounting_visibility_denied".to_string()),
            party: party.cloned(),
            invoices: Vec::new(),
            open_balance_cents: 0,
            overdue_balance_cents: 0,
        });
    }
    let Some(party) = party else {
        return Ok(InvoiceHistory {
            allowed: true,
            denied_reason: None,
            party: None,
            invoices: Vec::new(),
            open_balance_cents: 0,
            overdue_balance_cents: 0,
        });
    };
    let names = party_names(party);
    let matching_invoices = crate::slices::accounting::store::list_invoices(conn, client_id, 200)?
        .into_iter()
        .filter(|invoice| {
            invoice
                .customer_name
                .as_deref()
                .is_some_and(|value| names.contains(&normalized_name(value)))
        })
        .map(|invoice| InvoiceSummary {
            invoice_id: invoice.invoice_id,
            doc_number: invoice.doc_number,
            customer_name: invoice.customer_name,
            txn_date: invoice.txn_date,
            due_date: invoice.due_date,
            total_amt_cents: invoice.total_amt_cents,
            balance_cents: invoice.balance_cents,
            voided: invoice.voided,
        })
        .collect::<Vec<_>>();
    let open_balance_cents = matching_invoices
        .iter()
        .filter(|invoice| !invoice.voided)
        .map(|invoice| invoice.balance_cents)
        .sum();
    let today = utc_civil_date(now_ms);
    let overdue_balance_cents = matching_invoices
        .iter()
        .filter(|invoice| !invoice.voided && invoice.balance_cents > 0)
        .filter(|invoice| {
            invoice
                .due_date
                .as_deref()
                .is_some_and(|due_date| is_past_civil_date(due_date, &today))
        })
        .map(|invoice| invoice.balance_cents)
        .sum();
    let invoices = matching_invoices.into_iter().take(MAX_INVOICES).collect();
    Ok(InvoiceHistory {
        allowed: true,
        denied_reason: None,
        party: Some(party.clone()),
        invoices,
        open_balance_cents,
        overdue_balance_cents,
    })
}

pub fn product_lookup(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    query: &str,
) -> Result<ProductLookup, StoreError> {
    let needle = normalized_name(query);
    if needle.is_empty() {
        return Ok(ProductLookup {
            query: query.to_string(),
            products: Vec::new(),
        });
    }
    let products = crate::slices::inventory::store::list_materials(conn, client_id)?
        .into_iter()
        .filter(|material| {
            normalized_name(&material.name).contains(&needle)
                || material
                    .sku
                    .as_deref()
                    .is_some_and(|sku| normalized_name(sku).contains(&needle))
        })
        .take(MAX_PRODUCTS)
        .map(|material| {
            let availability_signal = availability_signal(
                material.quantity,
                material.warning_threshold,
                material.critical_threshold,
            );
            ProductSummary {
                material_id: material.material_id,
                name: material.name,
                sku: material.sku,
                quantity: material.quantity,
                unit: material.unit,
                warning_threshold: material.warning_threshold,
                critical_threshold: material.critical_threshold,
                unit_cost_cents: material.unit_cost_cents,
                lead_time_days: material.lead_time_days,
                vendor_name: material.vendor_name,
                availability_signal,
            }
        })
        .collect();
    Ok(ProductLookup {
        query: query.to_string(),
        products,
    })
}

pub fn order_status_lookup(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: &str,
) -> Result<OrderStatusLookup, StoreError> {
    let needle = normalize_identifier(query);
    if needle.is_empty() {
        return Ok(OrderStatusLookup {
            query: query.to_string(),
            orders: Vec::new(),
        });
    }
    let inventory_orders = crate::slices::inventory::store::list_orders(conn, client_id)?
        .into_iter()
        .filter(|order| {
            normalize_identifier(&order.order_number).contains(&needle)
                || order
                    .tracking_number
                    .as_deref()
                    .is_some_and(|value| normalize_identifier(value).contains(&needle))
                || order
                    .external_order_id
                    .as_deref()
                    .is_some_and(|value| normalize_identifier(value).contains(&needle))
        })
        .map(order_summary)
        .collect::<Vec<_>>();
    let shopify = shopify_order_grounding(conn, client_id, scope, Some(query), None)?;
    // One order-status grounding surface covers both Stockforge inventory and
    // Shopify sales snapshots. The caller asks one "where is order X?" question;
    // source labels keep provider facts distinct without creating duplicate
    // tools that invite the model to choose between near-identical actions.
    let orders = inventory_orders
        .into_iter()
        .chain(shopify.orders.into_iter().map(shopify_order_summary))
        .take(MAX_ORDERS)
        .collect();
    Ok(OrderStatusLookup {
        query: query.to_string(),
        orders,
    })
}

pub fn crm_contact_lookup(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    email: Option<&str>,
    company: Option<&str>,
) -> Result<CrmContactLookup, StoreError> {
    let normalized_email = email.and_then(normalized_email);
    let company = company
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut contacts = Vec::new();
    if let Some(email) = normalized_email.as_deref() {
        contacts.extend(crate::slices::crm_cache::service::contacts_by_email(
            conn, client_id, scope, email,
        )?);
    }
    if let Some(company) = company.as_deref() {
        contacts.extend(crate::slices::crm_cache::service::contact_by_company(
            conn, client_id, scope, company,
        )?);
    }
    contacts = dedupe_crm_contacts(contacts)
        .into_iter()
        .take(MAX_CRM_CONTACTS)
        .collect();
    let mut deals = Vec::new();
    for contact_email in contacts
        .iter()
        .filter_map(|contact| contact.email.as_deref())
    {
        deals.extend(crate::slices::crm_cache::service::deals_by_contact(
            conn,
            client_id,
            scope,
            contact_email,
        )?);
        if deals.len() >= MAX_CRM_DEALS {
            break;
        }
    }
    deals = dedupe_crm_deals(deals)
        .into_iter()
        .take(MAX_CRM_DEALS)
        .collect();
    Ok(CrmContactLookup {
        email: normalized_email,
        company,
        contacts,
        deals,
    })
}

pub fn shopify_order_grounding(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: Option<&str>,
    email: Option<&str>,
) -> Result<ShopifyOrderGrounding, StoreError> {
    let normalized_email = email
        .and_then(normalized_email)
        .or_else(|| query.and_then(normalized_email));
    let mut orders = Vec::new();
    let mut customers = Vec::new();
    if let Some(email) = normalized_email.as_deref() {
        orders.extend(crate::slices::shopify_sales::service::orders_for_customer(
            conn, client_id, scope, email, MAX_ORDERS,
        )?);
        customers.extend(crate::slices::shopify_sales::service::customers_for_email(
            conn,
            client_id,
            scope,
            email,
            MAX_SHOPIFY_CUSTOMERS,
        )?);
    }
    if let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) {
        let needle = normalize_identifier(query);
        if !needle.is_empty() {
            let recent = crate::slices::shopify_sales::service::recent_orders(
                conn,
                client_id,
                scope,
                MAX_ORDERS * 4,
            )?;
            orders.extend(recent.into_iter().filter(|order| {
                normalize_identifier(&order.order_number).contains(&needle)
                    || order
                        .tracking_number
                        .as_deref()
                        .is_some_and(|value| normalize_identifier(value).contains(&needle))
                    || normalize_identifier(&order.order_id).contains(&needle)
            }));
        }
    }
    let orders = dedupe_shopify_orders(orders)
        .into_iter()
        .take(MAX_ORDERS)
        .collect();
    let customers = dedupe_shopify_customers(customers)
        .into_iter()
        .take(MAX_SHOPIFY_CUSTOMERS)
        .collect();
    Ok(ShopifyOrderGrounding {
        query: query.map(str::to_string),
        email: normalized_email,
        orders,
        customers,
    })
}

pub fn prior_conversation_lookup(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    sender_email: &str,
    current_source_key: Option<&str>,
) -> Result<PriorConversationLookup, StoreError> {
    let Some(sender) = normalized_email(sender_email) else {
        return Ok(PriorConversationLookup {
            sender_email: sender_email.to_string(),
            records: Vec::new(),
        });
    };
    let records = crate::slices::email_triage::store::inbound_by_sender(
        conn,
        client_id,
        &sender,
        scope,
        MAX_EMAIL_RECORDS + 1,
    )?
    .into_iter()
    .filter(|record| current_source_key != Some(record.source_key.as_str()))
    .take(MAX_EMAIL_RECORDS)
    .map(|record| ConversationSummary {
        source_key: record.source_key,
        thread_id: record.thread_id,
        internal_date_ms: record.internal_date_ms,
        from_addr: record.from_addr,
        subject: record.subject,
        category: record.resolved_category,
        excerpt: capped_chars(
            &if record.body_full.trim().is_empty() {
                record.body_excerpt
            } else {
                record.body_full
            },
            600,
        ),
    })
    .collect();
    Ok(PriorConversationLookup {
        sender_email: sender,
        records,
    })
}

pub fn call_transcript_lookup(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: &str,
) -> Result<CallTranscriptLookup, StoreError> {
    if !matches!(scope, OperatorScope::All) {
        tracing::debug!("grounding denied: call transcript requires all scope");
        return Ok(CallTranscriptLookup {
            allowed: false,
            denied_reason: Some("call_transcript_scope_denied".to_string()),
            query: query.to_string(),
            calls: Vec::new(),
        });
    }
    let needle = normalized_name(query);
    let calls = crate::slices::call_inputs::store::list_inputs(conn, client_id, None, 100)?
        .into_iter()
        .filter(|entry| {
            let input = &entry.input;
            needle.is_empty()
                || normalized_name(&input.title).contains(&needle)
                || normalized_name(&input.summary).contains(&needle)
                || input
                    .caller_name
                    .as_deref()
                    .is_some_and(|value| normalized_name(value).contains(&needle))
                || input
                    .caller_email
                    .as_deref()
                    .and_then(normalized_email)
                    .as_deref()
                    == normalized_email(query).as_deref()
        })
        .take(MAX_CALL_RECORDS)
        .map(|entry| {
            let input = entry.input;
            CallSummary {
                call_input_id: input.call_input_id,
                title: input.title,
                summary: input.summary,
                caller_name: input.caller_name,
                caller_phone: input.caller_phone,
                caller_email: input.caller_email,
                occurred_at_ms: input.occurred_at_ms,
                excerpt: capped_chars(&input.transcript_text, 900),
            }
        })
        .collect();
    Ok(CallTranscriptLookup {
        allowed: true,
        denied_reason: None,
        query: query.to_string(),
        calls,
    })
}
