//! CRM cache read assembly and visibility policy. Provider access lives in
//! worker.rs; service reads only local snapshots.

use bos_contracts::crm_cache::{
    CrmCacheSyncInfo, CrmContactSnapshot, CrmContextResponse, CrmDealSnapshot,
};
use rusqlite::Connection;
use url::Url;

use super::store::{self, ContactSnapshotRow, DealSnapshotRow};
use crate::env_registry;
use crate::http::{OperatorScope, SyncGuard};
use crate::store_core::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrmDealVisibilityPolicy {
    Shared,
    AdminOnly,
    AuthorizerOnly,
}

impl CrmDealVisibilityPolicy {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shared" => Self::Shared,
            "admin_only" => Self::AdminOnly,
            "authorizer_only" | "" => Self::AuthorizerOnly,
            _ => Self::AuthorizerOnly,
        }
    }
}

pub fn configured_crm_provider() -> Result<&'static str, String> {
    crate::slices::crm_drafts::service::configured_crm_provider()
}

pub fn configured_deal_visibility_policy(
    conn: &Connection,
    client_id: &str,
) -> Result<CrmDealVisibilityPolicy, StoreError> {
    Ok(crate::slices::admin_settings::service::value(
        conn,
        client_id,
        &env_registry::BOS_CRM_DEAL_VISIBILITY_POLICY,
    )?
    .as_deref()
    .map(CrmDealVisibilityPolicy::parse)
    .unwrap_or(CrmDealVisibilityPolicy::AuthorizerOnly))
}

pub fn deal_amount_visible(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    policy: CrmDealVisibilityPolicy,
) -> Result<bool, StoreError> {
    match policy {
        CrmDealVisibilityPolicy::Shared => Ok(true),
        CrmDealVisibilityPolicy::AdminOnly => Ok(matches!(scope, OperatorScope::All)),
        CrmDealVisibilityPolicy::AuthorizerOnly => {
            if matches!(scope, OperatorScope::All) {
                return Ok(true);
            }
            Ok(store::snapshot_counts(conn, client_id)?.deals == 0)
        }
    }
}

pub fn sync_info(
    conn: &Connection,
    client_id: &str,
    status: &SyncGuard,
) -> Result<CrmCacheSyncInfo, StoreError> {
    let counts = store::snapshot_counts(conn, client_id)?;
    let contact_cursor = store::get_cursor(conn, client_id, store::ENTITY_CONTACT)?;
    let deal_cursor = store::get_cursor(conn, client_id, store::ENTITY_DEAL)?;
    let successful_cursor_sync = [&contact_cursor, &deal_cursor]
        .into_iter()
        .filter(|cursor| cursor.backfill_complete && cursor.last_error.is_none())
        .filter_map(|cursor| cursor.last_advanced_at_ms)
        .max();
    let last_error = contact_cursor
        .last_error
        .clone()
        .or(deal_cursor.last_error.clone());
    Ok(CrmCacheSyncInfo {
        provider: match configured_crm_provider() {
            Ok(provider) => provider.to_string(),
            Err(err) => err,
        },
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &env_registry::BOS_CRM_READ_SYNC_ENABLED,
        )?,
        in_flight: status.in_flight,
        contact_count: counts.contacts,
        deal_count: counts.deals,
        last_synced_at_ms: counts
            .last_synced_at_ms
            .into_iter()
            .chain(successful_cursor_sync)
            .max(),
        last_requests_used: status.units_used,
        next_sync_allowed_at_ms: status.next_allowed_at_ms,
        last_error: last_error.or_else(|| {
            status
                .last_outcome
                .clone()
                .filter(|outcome| outcome.starts_with("error:"))
        }),
    })
}

pub fn contacts_by_email(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    email: &str,
) -> Result<Vec<CrmContactSnapshot>, StoreError> {
    store::contacts_by_email(conn, client_id, email).map(contact_rows)
}

pub fn contact_by_company(
    conn: &Connection,
    client_id: &str,
    _scope: &OperatorScope,
    company: &str,
) -> Result<Vec<CrmContactSnapshot>, StoreError> {
    store::contact_by_company(conn, client_id, company).map(contact_rows)
}

pub fn deals_by_contact(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    contact_email: &str,
) -> Result<Vec<CrmDealSnapshot>, StoreError> {
    let visible = deal_amount_visible(
        conn,
        client_id,
        scope,
        configured_deal_visibility_policy(conn, client_id)?,
    )?;
    store::deals_by_contact(conn, client_id, contact_email).map(|rows| deal_rows(rows, visible))
}

pub fn context_for_source(
    conn: &Connection,
    client_id: &str,
    scope: &OperatorScope,
    source_key: &str,
) -> Result<CrmContextResponse, StoreError> {
    let message = crate::slices::email_triage::store::inbound_by_source_keys(
        conn,
        client_id,
        &[source_key.to_string()],
        scope,
    )?
    .into_iter()
    .next();
    let Some(message) = message else {
        return Ok(empty_context(Some("source_not_found".to_string())));
    };
    let policy = crate::slices::email_triage::service::crm_sender_policy(conn, client_id);
    let lookup_email =
        match crate::slices::email_triage::service::crm_identity_email_for_inbound_record(
            &message, &policy,
        ) {
            Ok(email) => email,
            Err(reason) => return Ok(empty_context(Some(reason.to_string()))),
        };
    let contacts = contacts_by_email(conn, client_id, scope, &lookup_email)?;
    let deals = deals_by_contact(conn, client_id, scope, &lookup_email)?;
    Ok(CrmContextResponse {
        contacts,
        deals,
        lookup_email: Some(lookup_email),
        skipped_reason: None,
        hubspot_links_configured: hubspot_links_configured(),
    })
}

fn empty_context(skipped_reason: Option<String>) -> CrmContextResponse {
    CrmContextResponse {
        contacts: Vec::new(),
        deals: Vec::new(),
        lookup_email: None,
        skipped_reason,
        hubspot_links_configured: hubspot_links_configured(),
    }
}

fn configured_provider_name() -> String {
    configured_crm_provider()
        .map(str::to_string)
        .unwrap_or_else(|err| err)
}

fn hubspot_portal_id() -> Option<String> {
    env_registry::string(&env_registry::BOS_HUBSPOT_PORTAL_ID)
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

fn hubspot_links_configured() -> bool {
    match configured_crm_provider() {
        Ok(crate::slices::crm_drafts::service::PROVIDER_HUBSPOT) => hubspot_portal_id().is_some(),
        _ => true,
    }
}

fn espocrm_base_url() -> Option<String> {
    let raw = env_registry::string(&env_registry::BOS_ESPOCRM_BASE_URL)?;
    let mut parsed = Url::parse(raw.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);
    parsed.set_query(None);
    Some(parsed.as_str().trim_end_matches('/').to_string())
}

pub fn contact_url(provider: &str, provider_contact_id: &str) -> Option<String> {
    match provider {
        crate::slices::crm_drafts::service::PROVIDER_HUBSPOT => hubspot_portal_id().map(|portal| {
            format!("https://app.hubspot.com/contacts/{portal}/record/0-1/{provider_contact_id}")
        }),
        crate::slices::crm_drafts::service::PROVIDER_ESPOCRM => {
            espocrm_base_url().map(|base| format!("{base}/#Contact/view/{provider_contact_id}"))
        }
        _ => None,
    }
}

pub fn deal_url(provider: &str, provider_deal_id: &str) -> Option<String> {
    match provider {
        crate::slices::crm_drafts::service::PROVIDER_HUBSPOT => hubspot_portal_id().map(|portal| {
            format!("https://app.hubspot.com/contacts/{portal}/record/0-3/{provider_deal_id}")
        }),
        _ => None,
    }
}

fn contact_rows(rows: Vec<ContactSnapshotRow>) -> Vec<CrmContactSnapshot> {
    let provider = configured_provider_name();
    rows.into_iter()
        .map(|row| CrmContactSnapshot {
            provider: provider.clone(),
            contact_url: contact_url(&provider, &row.provider_contact_id),
            provider_contact_id: row.provider_contact_id,
            email: row.email,
            name: row.full_name,
            company: row.company,
            phone: row.phone,
            lifecycle_stage: row.lifecycle_stage,
            owner: row.owner,
            last_activity_at: row.last_activity_at,
        })
        .collect()
}

fn deal_rows(rows: Vec<DealSnapshotRow>, amount_visible: bool) -> Vec<CrmDealSnapshot> {
    let provider = configured_provider_name();
    rows.into_iter()
        .map(|row| CrmDealSnapshot {
            provider: provider.clone(),
            deal_url: deal_url(&provider, &row.provider_deal_id),
            provider_deal_id: row.provider_deal_id,
            name: row.deal_name,
            stage: row.stage,
            amount_cents: amount_visible.then_some(row.amount_cents).flatten(),
            currency: amount_visible.then_some(row.currency).flatten(),
            amount_visible,
            pipeline: row.pipeline,
            close_date: row.close_date,
            associated_contact_email: row.associated_contact_email,
            associated_contact_company: row.associated_company,
        })
        .collect()
}
