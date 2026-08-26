use bos_integrations::llm_api::{DirectLlmToolCall, DirectLlmToolDefinition, DirectLlmToolResult};
use rusqlite::Connection;
use serde_json::json;

use crate::http::OperatorScope;
use crate::overlay::AccountingVisibilityPolicy;
use crate::store_core::StoreError;

use super::lookup::{
    call_transcript_lookup, crm_contact_lookup, customer_invoice_history, order_status_lookup,
    prior_conversation_lookup, product_lookup, resolve_party,
};
use super::render::{
    render_call_transcripts, render_crm_contact, render_invoice_history, render_orders,
    render_prior_conversation, render_products,
};
use super::util::{capped_chars, normalize_identifier};
use super::{
    MAX_EXCERPT_CHARS, TOOL_CALL_TRANSCRIPT_LOOKUP, TOOL_CRM_CONTACT_LOOKUP,
    TOOL_CUSTOMER_INVOICE_HISTORY, TOOL_EMAIL_THREAD_LOOKUP, TOOL_ORDER_STATUS_LOOKUP,
    TOOL_PRIOR_CONVERSATION_LOOKUP, TOOL_PRODUCT_LOOKUP,
};

pub fn grounding_tool_definitions() -> Vec<DirectLlmToolDefinition> {
    grounding_tool_definitions_for(&[TOOL_EMAIL_THREAD_LOOKUP])
}

pub fn grounding_tool_definitions_for(names: &[&str]) -> Vec<DirectLlmToolDefinition> {
    names
        .iter()
        .filter_map(|name| grounding_tool_definition(name))
        .collect()
}

fn grounding_tool_definition(name: &str) -> Option<DirectLlmToolDefinition> {
    match name {
        TOOL_EMAIL_THREAD_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_EMAIL_THREAD_LOOKUP.to_string(),
            description:
                "Read local metadata and bounded excerpts for the current inbound email or its thread."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["source", "thread"],
                        "description": "source reads only the selected message; thread reads the local thread when available."
                    },
                    "source_ref": {
                        "type": "string",
                        "description": "Optional source_ref to confirm the requested message."
                    }
                },
                "required": ["scope"]
            }),
        }),
        TOOL_CRM_CONTACT_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_CRM_CONTACT_LOOKUP.to_string(),
            description:
                "Read cached CRM contact and deal context for an email address or exact company name."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "email": {
                        "type": "string",
                        "description": "Customer/contact email address to look up."
                    },
                    "company": {
                        "type": "string",
                        "description": "Exact company name to look up when no email is available."
                    }
                }
            }),
        }),
        TOOL_ORDER_STATUS_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_ORDER_STATUS_LOOKUP.to_string(),
            description:
                "Read cached order status, fulfillment, carrier, and tracking facts across inventory and Shopify snapshots."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Order number, tracking number, or provider order id."
                    }
                },
                "required": ["query"]
            }),
        }),
        TOOL_PRODUCT_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_PRODUCT_LOOKUP.to_string(),
            description:
                "Read cached inventory product availability, quantity, lead-time, and vendor context."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Product name, SKU, or material identifier fragment."
                    }
                },
                "required": ["query"]
            }),
        }),
        TOOL_PRIOR_CONVERSATION_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_PRIOR_CONVERSATION_LOOKUP.to_string(),
            description:
                "Read bounded excerpts from prior cached inbound email conversations with a sender."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sender_email": {
                        "type": "string",
                        "description": "Sender email address whose prior local conversations should be searched."
                    }
                },
                "required": ["sender_email"]
            }),
        }),
        TOOL_CUSTOMER_INVOICE_HISTORY => Some(DirectLlmToolDefinition {
            name: TOOL_CUSTOMER_INVOICE_HISTORY.to_string(),
            description:
                "Read cached accounting invoice history for a customer resolved from an email address or name. Visibility gates may deny financial details."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "email": {
                        "type": "string",
                        "description": "Customer email address to resolve exactly."
                    },
                    "name": {
                        "type": "string",
                        "description": "Customer or company name to search when no email is available."
                    }
                }
            }),
        }),
        TOOL_CALL_TRANSCRIPT_LOOKUP => Some(DirectLlmToolDefinition {
            name: TOOL_CALL_TRANSCRIPT_LOOKUP.to_string(),
            description:
                "Read bounded excerpts from cached consented call transcripts. Non-All operator scopes receive a denied result."
                    .to_string(),
            parameters_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Caller name, email, or topic to search."
                    }
                },
                "required": ["query"]
            }),
        }),
        _ => None,
    }
}

pub fn email_thread_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    source_ref: &str,
    thread_id: Option<&str>,
    tool_scope: &str,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let records = if tool_scope == "thread" {
        crate::slices::email_triage::store::inbound_by_thread_id(
            conn,
            client_id,
            thread_id.unwrap_or_default(),
            scope,
            10,
        )?
    } else {
        crate::slices::email_triage::store::inbound_by_source_keys(
            conn,
            client_id,
            &[source_ref.to_string()],
            scope,
        )?
    };
    let records_json = records
        .iter()
        .map(|record| {
            json!({
                "source_key": record.source_key,
                "message_id": record.message_id,
                "thread_id": record.thread_id,
                "internal_date_ms": record.internal_date_ms,
                "from_addr": record.from_addr,
                "to_addr": record.to_addr,
                "subject": record.subject,
                "resolved_category": record.resolved_category,
                "body_excerpt": capped_chars(&crate::slices::email_triage::service::body_for_ai(record), 700),
            })
        })
        .collect::<Vec<_>>();
    let excerpt = records
        .iter()
        .map(|record| {
            format!(
                "From: {}\nSubject: {}\n{}",
                record.from_addr.as_deref().unwrap_or("(unknown)"),
                record.subject.as_deref().unwrap_or("(no subject)"),
                capped_chars(
                    &crate::slices::email_triage::service::body_for_ai(record),
                    700
                )
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let excerpt = capped_chars(&excerpt, MAX_EXCERPT_CHARS);
    let result_ref = if tool_scope == "thread" {
        format!("email_thread:{}", thread_id.unwrap_or(source_ref))
    } else {
        format!("email_source:{source_ref}")
    };
    let payload = json!({
        "ok": true,
        "result_ref": result_ref,
        "scope": tool_scope,
        "records": records_json,
    });
    Ok((payload, result_ref, excerpt))
}

pub fn crm_contact_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    email: Option<&str>,
    company: Option<&str>,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let lookup = crm_contact_lookup(conn, client_id, scope, email, company)?;
    let result_ref = lookup
        .email
        .as_deref()
        .map(|email| format!("crm_contact:{email}"))
        .or_else(|| {
            lookup
                .company
                .as_deref()
                .map(|company| format!("crm_company:{company}"))
        })
        .unwrap_or_else(|| "crm_contact:empty_query".to_string());
    let excerpt =
        render_crm_contact(&lookup).unwrap_or_else(|| "No cached CRM contacts found.".to_string());
    let payload = json!({
        "ok": true,
        "result_ref": result_ref,
        "email": lookup.email,
        "company": lookup.company,
        "contacts": lookup.contacts,
        "deals": lookup.deals,
    });
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn order_status_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: &str,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let lookup = order_status_lookup(conn, client_id, scope, query)?;
    let result_ref = format!("order_status:{}", normalize_identifier(query));
    let excerpt =
        render_orders(&lookup).unwrap_or_else(|| "No cached matching orders found.".to_string());
    let payload = json!({
        "ok": true,
        "result_ref": result_ref,
        "query": lookup.query,
        "orders": lookup.orders,
    });
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn product_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: &str,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let lookup = product_lookup(conn, client_id, scope, query)?;
    let result_ref = format!("product_lookup:{}", normalize_identifier(query));
    let excerpt = render_products(&lookup)
        .unwrap_or_else(|| "No cached matching products found.".to_string());
    let payload = json!({
        "ok": true,
        "result_ref": result_ref,
        "query": lookup.query,
        "products": lookup.products,
    });
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn prior_conversation_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    sender_email: &str,
    current_source_key: Option<&str>,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let lookup =
        prior_conversation_lookup(conn, client_id, scope, sender_email, current_source_key)?;
    let result_ref = format!("prior_conversation:{}", lookup.sender_email);
    let excerpt = render_prior_conversation(&lookup)
        .unwrap_or_else(|| "No cached prior conversations found.".to_string());
    let payload = json!({
        "ok": true,
        "result_ref": result_ref,
        "sender_email": lookup.sender_email,
        "records": lookup.records,
    });
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn customer_invoice_history_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    policy: AccountingVisibilityPolicy,
    email: Option<&str>,
    name: Option<&str>,
    now_ms: u64,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let resolved = resolve_party(conn, client_id, scope, email, name)?;
    let history = customer_invoice_history(
        conn,
        client_id,
        scope,
        policy,
        resolved.selected.as_ref(),
        now_ms,
    )?;
    let result_ref = format!("customer_invoice_history:{}", resolved.reason);
    let excerpt = render_invoice_history(&history).unwrap_or_else(|| {
        if !history.allowed {
            history
                .denied_reason
                .clone()
                .unwrap_or_else(|| "invoice_history_denied".to_string())
        } else {
            format!(
                "No cached invoice history returned. resolve_party reason={}, candidates={}",
                resolved.reason,
                resolved.candidates.len()
            )
        }
    });
    let payload = if history.allowed {
        json!({
            "ok": true,
            "result_ref": result_ref,
            "resolve_party": resolved,
            "customer_invoice_history": history,
        })
    } else {
        json!({
            "ok": false,
            "result_ref": result_ref,
            "error_code": history.denied_reason.as_deref().unwrap_or("invoice_history_denied"),
            "resolve_party": resolved,
            "customer_invoice_history": history,
        })
    };
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn call_transcript_tool_payload(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    query: &str,
) -> Result<(serde_json::Value, String, String), StoreError> {
    let lookup = call_transcript_lookup(conn, client_id, scope, query)?;
    let result_ref = format!("call_transcript:{}", normalize_identifier(query));
    let excerpt = render_call_transcripts(&lookup).unwrap_or_else(|| {
        lookup
            .denied_reason
            .clone()
            .unwrap_or_else(|| "No cached matching call transcripts found.".to_string())
    });
    let payload = if lookup.allowed {
        json!({
            "ok": true,
            "result_ref": result_ref,
            "query": lookup.query,
            "allowed": lookup.allowed,
            "denied_reason": lookup.denied_reason,
            "calls": lookup.calls,
        })
    } else {
        json!({
            "ok": false,
            "result_ref": result_ref,
            "error_code": lookup.denied_reason.as_deref().unwrap_or("call_transcript_denied"),
            "query": lookup.query,
            "allowed": lookup.allowed,
            "denied_reason": lookup.denied_reason,
            "calls": lookup.calls,
        })
    };
    Ok((
        payload,
        result_ref,
        capped_chars(&excerpt, MAX_EXCERPT_CHARS),
    ))
}

pub fn denied_tool_result(
    call: &DirectLlmToolCall,
    result_ref: &str,
    error_code: &str,
) -> DirectLlmToolResult {
    DirectLlmToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
        result_json: json!({
            "result_ref": result_ref,
            "excerpt": error_code,
            "payload": {
                "ok": false,
                "error_code": error_code,
                "records": [],
            },
        }),
    }
}
