//! Home dashboard preference persistence through store_core.

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};
use bos_contracts::home_dashboard::{
    HomeDashboardPreference, HomeDashboardPreferencesUpdateRequest, HomeDashboardWidgetKind,
    HomeDashboardWidgetPreference, HubSpotDealMappedStatus, HubSpotDealPipelineMapping,
    HubSpotDealPipelineMappingResponse, HubSpotDealPipelineMappingSaveRequest,
};
use bos_contracts::receipt::ActorKindDto;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeSet;

pub const PREFERENCE_ENTITY_KIND: &str = "home_dashboard_preference";
pub const HUBSPOT_DEAL_MAPPING_ENTITY_KIND: &str = "home_dashboard_hubspot_deal_mapping";
const HUBSPOT_DEAL_MAPPING_ENTITY_ID: &str = "hubspot_deals";

pub fn default_widgets() -> Vec<HomeDashboardWidgetPreference> {
    use HomeDashboardWidgetKind::*;
    [
        BusinessSummary,
        SalesPipeline,
        OpenTasks,
        ImportantEmails,
        WorkQueueEvents,
        RecentOrders,
        FinancialOverview,
        InventoryAlerts,
        SystemHealth,
        HelpShortcuts,
        SystemDiagnostics,
    ]
    .into_iter()
    .map(|kind| HomeDashboardWidgetPreference {
        kind,
        enabled: true,
    })
    .collect()
}

pub fn load_hubspot_deal_mapping(
    conn: &Connection,
    client_id: &str,
) -> Result<HubSpotDealPipelineMappingResponse, StoreError> {
    let mapping_json: Option<String> = conn
        .query_row(
            "SELECT mapping_json FROM home_dashboard_hubspot_deal_mapping WHERE client_id = ?1",
            params![client_id],
            |row| row.get(0),
        )
        .optional()?;
    let mapping = match mapping_json {
        Some(raw) => Some(
            serde_json::from_str(&raw)
                .map_err(|err| StoreError::Domain(format!("parse hubspot deal mapping: {err}")))?,
        ),
        None => None,
    };
    let revision = store_core::current_revision(
        conn,
        client_id,
        HUBSPOT_DEAL_MAPPING_ENTITY_KIND,
        HUBSPOT_DEAL_MAPPING_ENTITY_ID,
    )?;
    Ok(HubSpotDealPipelineMappingResponse { mapping, revision })
}

pub fn save_hubspot_deal_mapping(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    request: &HubSpotDealPipelineMappingSaveRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    validate_hubspot_deal_mapping(&request.mapping)?;
    let before = load_hubspot_deal_mapping(conn, client_id)
        .ok()
        .and_then(|response| response.mapping)
        .and_then(|mapping| serde_json::to_string(&mapping).ok());
    let after = serde_json::to_string(&request.mapping)
        .map_err(|err| StoreError::Domain(format!("serialize hubspot deal mapping: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_mapping = after.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: HUBSPOT_DEAL_MAPPING_ENTITY_KIND,
            entity_id: HUBSPOT_DEAL_MAPPING_ENTITY_ID,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: before,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO home_dashboard_hubspot_deal_mapping \
                 (client_id, mapping_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT(client_id) DO UPDATE SET \
                   mapping_json = excluded.mapping_json, \
                   updated_at_ms = excluded.updated_at_ms",
                params![owned_client, owned_mapping, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

fn validate_hubspot_deal_mapping(mapping: &HubSpotDealPipelineMapping) -> Result<(), StoreError> {
    if mapping.pipeline_id.trim().is_empty() {
        return Err(StoreError::Domain(
            "hubspot_pipeline_id_required".to_string(),
        ));
    }
    if mapping.started_date_property.trim().is_empty() {
        return Err(StoreError::Domain(
            "hubspot_started_date_property_required".to_string(),
        ));
    }
    if mapping.closed_date_property.trim().is_empty() {
        return Err(StoreError::Domain(
            "hubspot_closed_date_property_required".to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut has_open = false;
    let mut has_won = false;
    let mut has_lost = false;
    for stage in &mapping.stage_mappings {
        let stage_id = stage.stage_id.trim();
        if stage_id.is_empty() {
            return Err(StoreError::Domain("hubspot_stage_id_required".to_string()));
        }
        if !seen.insert(stage_id.to_string()) {
            return Err(StoreError::Domain(
                "hubspot_stage_mapping_duplicate".to_string(),
            ));
        }
        match stage.status {
            HubSpotDealMappedStatus::Open => has_open = true,
            HubSpotDealMappedStatus::Won => has_won = true,
            HubSpotDealMappedStatus::Lost => has_lost = true,
        }
    }
    if !has_open || !has_won || !has_lost {
        return Err(StoreError::Domain(
            "hubspot_stage_mapping_incomplete".to_string(),
        ));
    }
    Ok(())
}

pub fn load_preference(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
) -> Result<HomeDashboardPreference, StoreError> {
    let widgets_json: Option<String> = conn
        .query_row(
            "SELECT widgets_json FROM home_dashboard_preferences \
             WHERE client_id = ?1 AND user_id = ?2",
            params![client_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    let revision = store_core::current_revision(conn, client_id, PREFERENCE_ENTITY_KIND, user_id)?;
    let widgets = match widgets_json {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| default_widgets()),
        None => default_widgets(),
    };
    let widgets = reconcile_widgets(&widgets);
    Ok(HomeDashboardPreference { widgets, revision })
}

pub fn replace_preference(
    conn: &mut Connection,
    client_id: &str,
    user_id: &str,
    actor_id: &str,
    request: &HomeDashboardPreferencesUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    let widgets = normalize_widgets(&request.widgets)?;
    let before = load_preference(conn, client_id, user_id)
        .ok()
        .and_then(|preference| serde_json::to_string(&preference.widgets).ok());
    let after = serde_json::to_string(&widgets)
        .map_err(|err| StoreError::Domain(format!("serialize dashboard preferences: {err}")))?;
    let owned_client = client_id.to_string();
    let owned_user = user_id.to_string();
    let owned_widgets = after.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: PREFERENCE_ENTITY_KIND,
            entity_id: user_id,
            change_kind: "replace",
            actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: request.expected_revision,
            idempotency_key: &request.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: before,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO home_dashboard_preferences \
                 (client_id, user_id, widgets_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?4) \
                 ON CONFLICT(client_id, user_id) DO UPDATE SET \
                   widgets_json = excluded.widgets_json, \
                   updated_at_ms = excluded.updated_at_ms",
                params![owned_client, owned_user, owned_widgets, now_ms as i64],
            )?;
            Ok(())
        },
    )
}

#[cfg(test)]
pub fn insert_preference_json_for_test(
    conn: &Connection,
    client_id: &str,
    user_id: &str,
    widgets_json: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO home_dashboard_preferences \
         (client_id, user_id, widgets_json, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![client_id, user_id, widgets_json, 1_000i64],
    )?;
    Ok(())
}

fn normalize_widgets(
    widgets: &[HomeDashboardWidgetPreference],
) -> Result<Vec<HomeDashboardWidgetPreference>, StoreError> {
    if widgets.is_empty() {
        return Ok(default_widgets());
    }
    Ok(reconcile_widgets(widgets))
}

fn reconcile_widgets(
    widgets: &[HomeDashboardWidgetPreference],
) -> Vec<HomeDashboardWidgetPreference> {
    let mut out: Vec<HomeDashboardWidgetPreference> = Vec::new();
    for widget in widgets {
        if let Some(existing) = out.iter_mut().find(|existing| existing.kind == widget.kind) {
            // Legacy saved preferences may deserialize both `financials` and
            // `outstanding_invoices` into the merged `financial_overview`
            // kind. Keep the merged card visible if either old card was
            // visible; two disabled old cards stay disabled.
            existing.enabled |= widget.enabled;
        } else {
            out.push(widget.clone());
        }
    }
    for default in default_widgets() {
        if !out.iter().any(|widget| widget.kind == default.kind) {
            out.push(default);
        }
    }
    out
}
