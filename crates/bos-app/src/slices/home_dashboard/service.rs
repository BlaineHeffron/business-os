//! Server-side Home widget assembly. Authorization stays with the backend:
//! financial widgets only assemble after the existing accounting visibility
//! policy says this operator may see cached accounting values.

use bos_contracts::home_dashboard::{
    HomeDashboardAction, HomeDashboardMetric, HomeDashboardResponse, HomeDashboardTarget,
    HomeDashboardTargetView, HomeDashboardWidget, HomeDashboardWidgetChart,
    HomeDashboardWidgetChartPoint, HomeDashboardWidgetItem, HomeDashboardWidgetKind,
    HomeDashboardWidgetState, HubSpotDealDatePropertyOption, HubSpotDealDiscoveryResponse,
    HubSpotDealMappedStatus, HubSpotDealPipelineMapping, HubSpotDealPipelineOption,
    HubSpotDealStageOption,
};
use bos_contracts::work_queue::WorkItemStatus;

use crate::http::{now_ms, AppState, OperatorScope};
use crate::store_core::StoreError;

use super::store;

pub fn dashboard_response(
    state: &AppState,
    scope: &OperatorScope,
    user_id: &str,
) -> Result<HomeDashboardResponse, StoreError> {
    let persistence = state.persistence();
    let conn = persistence.connection_ref();
    let preferences = store::load_preference(conn, &state.client_id, user_id)?;
    let available_widgets = available_widgets(state, conn, scope)?;
    let mut widget_kinds = preferences
        .widgets
        .iter()
        .filter(|pref| pref.enabled && available_widgets.contains(&pref.kind))
        .map(|pref| pref.kind)
        .collect::<Vec<_>>();
    widget_kinds.sort_by_key(|kind| canonical_widget_position(*kind));
    let widgets = widget_kinds
        .into_iter()
        .map(|kind| assemble_widget(state, conn, scope, user_id, kind))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(HomeDashboardResponse {
        preferences,
        available_widgets,
        widgets,
    })
}

fn available_widgets(
    state: &AppState,
    conn: &rusqlite::Connection,
    scope: &OperatorScope,
) -> Result<Vec<HomeDashboardWidgetKind>, StoreError> {
    use HomeDashboardWidgetKind::*;
    let mut out = Vec::new();
    // BusinessSummary leads the canonical order: available when ANY of its
    // sub-metric sources is, so the KPI ribbon degrades gracefully instead of
    // hiding entirely when one slice is off.
    let accounting_visible = state.slice_enabled("accounting")
        && crate::slices::accounting::service::cached_financial_visibility_allowed(
            conn,
            &state.client_id,
            scope,
            state.accounting_visibility_policy,
        )?;
    if accounting_visible
        || state.slice_enabled("inventory")
        || state.slice_enabled("search_console")
    {
        out.push(BusinessSummary);
    }
    if state.slice_enabled("home_dashboard") {
        out.push(SalesPipeline);
    }
    if state.slice_enabled("follow_up_tasks") {
        out.push(OpenTasks);
    }
    if state.slice_enabled("email_triage") {
        out.push(ImportantEmails);
    }
    if state.slice_enabled("work_queue") {
        out.push(WorkQueueEvents);
    }
    if state.slice_enabled("inventory") {
        out.push(RecentOrders);
        out.push(InventoryAlerts);
    }

    if state.slice_enabled("accounting")
        && crate::slices::accounting::service::cached_financial_visibility_allowed(
            conn,
            &state.client_id,
            scope,
            state.accounting_visibility_policy,
        )?
    {
        out.push(FinancialOverview);
    }
    if has_system_health_source(state) {
        out.push(SystemHealth);
    }
    if state.slice_enabled("home_dashboard") {
        out.push(HelpShortcuts);
    }
    if state.slice_enabled("debug")
        && crate::env_registry::flag(&crate::env_registry::BOS_DEBUG_ENABLED)
    {
        out.push(SystemDiagnostics);
    }
    Ok(out)
}

fn assemble_widget(
    state: &AppState,
    conn: &rusqlite::Connection,
    scope: &OperatorScope,
    user_id: &str,
    kind: HomeDashboardWidgetKind,
) -> Result<HomeDashboardWidget, StoreError> {
    match kind {
        HomeDashboardWidgetKind::BusinessSummary => business_summary(state, conn, scope),
        HomeDashboardWidgetKind::SalesPipeline => sales_pipeline(state, conn),
        HomeDashboardWidgetKind::SystemHealth => system_health(state, conn, user_id),
        HomeDashboardWidgetKind::HelpShortcuts => help_shortcuts(),
        HomeDashboardWidgetKind::SystemDiagnostics => system_diagnostics(conn, &state.client_id),
        HomeDashboardWidgetKind::OpenTasks => open_tasks(conn, &state.client_id, scope),
        HomeDashboardWidgetKind::ImportantEmails => important_emails(conn, &state.client_id, scope),
        HomeDashboardWidgetKind::WorkQueueEvents => work_queue(conn, &state.client_id, scope),
        HomeDashboardWidgetKind::RecentOrders => recent_orders(conn, &state.client_id),
        HomeDashboardWidgetKind::FinancialOverview => financial_overview(state, conn, scope),
        HomeDashboardWidgetKind::InventoryAlerts => inventory_alerts(conn, &state.client_id),
    }
}

/// Top KPI ribbon: Revenue (P&L income, month to date), Active customers, and
/// Orders in pipeline. Reuses the existing accounting + inventory reads — no
/// new stores or duplicate queries — and includes only the metrics whose
/// source is available so the ribbon degrades gracefully.
fn business_summary(
    state: &AppState,
    conn: &rusqlite::Connection,
    scope: &OperatorScope,
) -> Result<HomeDashboardWidget, StoreError> {
    let mut metrics = Vec::new();

    let accounting_visible = state.slice_enabled("accounting")
        && crate::slices::accounting::service::cached_financial_visibility_allowed(
            conn,
            &state.client_id,
            scope,
            state.accounting_visibility_policy,
        )?;
    if accounting_visible {
        let today = crate::slices::accounting::service::today_string(now_ms());
        let sync = accounting_sync_info(conn, &state.client_id)?;
        let metric_config = crate::slices::accounting::service::metric_basis_config_from_sources(
            Some(&state.accounting_overlay.metric_basis),
        )?;
        let financials = crate::slices::accounting::service::financials_from_store(
            conn,
            &state.client_id,
            &today,
            sync,
            &metric_config,
        )?;
        metrics.push(metric_cents_target(
            "Revenue · month to date",
            financials.month_to_date_cents,
            accounting_target(None),
        ));

        let customers = crate::slices::accounting::store::list_customers(conn, &state.client_id)?;
        let active = customers.iter().filter(|customer| customer.active).count();
        metrics.push(metric_number_target(
            "Active customers",
            active,
            accounting_target(None),
        ));
    }

    if state.slice_enabled("inventory") {
        let snapshots = crate::slices::inventory::store::list_orders(conn, &state.client_id)?;
        let today = crate::slices::accounting::service::today_string(now_ms());
        let (pipeline, _controls, _rows) =
            crate::slices::inventory::service::compute_orders(&snapshots, &today);
        let in_pipeline = pipeline.new_count + pipeline.picking_count + pipeline.packed_count;
        metrics.push(metric_number_target(
            "Orders in pipeline",
            in_pipeline as usize,
            HomeDashboardTarget {
                view: Some(HomeDashboardTargetView::Inventory),
                focus_id: None,
                external_url: None,
            },
        ));
    }

    if state.slice_enabled("search_console") {
        let config = crate::slices::search_console::service::config(
            state.search_console_overlay.as_ref().as_ref(),
        );
        if let Some(property_id) = config
            .ga4_property_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let today = crate::slices::accounting::service::today_string(now_ms());
            let month_start = crate::slices::accounting::service::month_start_date(&today)
                .unwrap_or_else(|| today.clone());
            let analytics = crate::slices::search_console::store::sum_analytics_daily(
                conn,
                &state.client_id,
                property_id,
                &month_start,
                &today,
            )?;
            metrics.push(metric_number_target(
                "Website sessions · month to date",
                analytics.sessions as usize,
                reports_target(),
            ));
            metrics.push(metric_number_target(
                "Conversions · month to date",
                analytics.conversions as usize,
                reports_target(),
            ));
        }
    }

    if metrics.is_empty() {
        return Ok(unavailable(
            HomeDashboardWidgetKind::BusinessSummary,
            "Business summary",
            "business_summary_sources_unavailable",
        ));
    }

    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::BusinessSummary,
        title: "Business summary".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Key figures across accounting and inventory".to_string()),
        metrics,
        items: Vec::new(),
        action: None,
        chart: None,
        error_code: None,
    })
}

fn sales_pipeline(
    state: &AppState,
    conn: &rusqlite::Connection,
) -> Result<HomeDashboardWidget, StoreError> {
    let saved = store::load_hubspot_deal_mapping(conn, &state.client_id)?;
    let Some(mapping) = saved.mapping else {
        return Ok(HomeDashboardWidget {
            kind: HomeDashboardWidgetKind::SalesPipeline,
            title: "Sales pipeline".to_string(),
            state: HomeDashboardWidgetState::PendingSetup,
            summary: Some(
                "Choose the HubSpot Deals pipeline and stage mapping to populate this widget."
                    .to_string(),
            ),
            metrics: Vec::new(),
            items: vec![HomeDashboardWidgetItem {
                label: "HubSpot deal mapping pending".to_string(),
                detail: Some(
                    "Open setup to discover pipelines and map open, won, and lost stages."
                        .to_string(),
                ),
                tone: Some("warning".to_string()),
                target: None,
            }],
            action: Some(HomeDashboardAction {
                label: "Set up deals".to_string(),
                target: settings_target(Some("hubspot_deals".to_string())),
            }),
            chart: Some(HomeDashboardWidgetChart::Funnel { stages: Vec::new() }),
            error_code: Some("hubspot_deal_mapping_pending".to_string()),
        });
    };

    Ok(hubspot_sales_pipeline_from_mapping(&mapping))
}

fn hubspot_sales_pipeline_from_mapping(
    mapping: &HubSpotDealPipelineMapping,
) -> HomeDashboardWidget {
    let access_token = crate::env_registry::string(&crate::env_registry::BOS_HUBSPOT_ACCESS_TOKEN);
    let client = match bos_integrations::hubspot::hubspot_deal_discovery_client(access_token) {
        Ok(client) => client,
        Err(reason) => {
            return hubspot_unavailable_widget("hubspot_access_token_missing", &reason);
        }
    };
    let open_stage_ids = stage_ids_for(mapping, HubSpotDealMappedStatus::Open);
    let counts = match client.open_stage_counts(&mapping.pipeline_id, &open_stage_ids) {
        Ok(counts) => counts,
        Err(err) => {
            let message = hubspot_read_error_message(err);
            return hubspot_unavailable_widget("hubspot_deals_limited", &message);
        }
    };
    let samples = client
        .sample_open_deals(&mapping.pipeline_id, &open_stage_ids, 5)
        .unwrap_or_default();
    let stage_labels = stage_label_lookup(mapping);
    let stage_count_map = counts
        .into_iter()
        .map(|count| (count.stage_id, count.count))
        .collect::<std::collections::BTreeMap<_, _>>();
    let open_total: u32 = stage_count_map.values().sum();
    let stages = mapping
        .stage_mappings
        .iter()
        .filter(|stage| stage.status == HubSpotDealMappedStatus::Open)
        .map(|stage| {
            let label = stage_labels
                .get(&stage.stage_id)
                .cloned()
                .unwrap_or_else(|| stage.stage_id.clone());
            HomeDashboardWidgetChartPoint {
                label,
                value: *stage_count_map.get(&stage.stage_id).unwrap_or(&0),
                color: None,
                target: Some(hubspot_stage_target(&mapping.pipeline_id, &stage.stage_id)),
            }
        })
        .collect::<Vec<_>>();
    HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::SalesPipeline,
        title: "Sales pipeline".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Read-only from HubSpot Deals".to_string()),
        metrics: vec![
            metric_number_target(
                "Open deals",
                open_total as usize,
                hubspot_pipeline_target(&mapping.pipeline_id),
            ),
            metric_number("Open stages", stages.len()),
        ],
        items: samples
            .into_iter()
            .map(|deal| HomeDashboardWidgetItem {
                label: deal.name,
                detail: deal.amount_cents.map(format_cents).or_else(|| {
                    deal.stage_id
                        .and_then(|stage| stage_labels.get(&stage).cloned())
                }),
                tone: Some("info".to_string()),
                target: Some(hubspot_deal_target(&deal.deal_id)),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "Open HubSpot".to_string(),
            target: hubspot_pipeline_target(&mapping.pipeline_id),
        }),
        chart: Some(HomeDashboardWidgetChart::Funnel { stages }),
        error_code: None,
    }
}

pub fn discover_hubspot_deals() -> HubSpotDealDiscoveryResponse {
    let access_token = crate::env_registry::string(&crate::env_registry::BOS_HUBSPOT_ACCESS_TOKEN);
    let client = match bos_integrations::hubspot::hubspot_deal_discovery_client(access_token) {
        Ok(client) => client,
        Err(reason) => {
            return HubSpotDealDiscoveryResponse {
                configured: false,
                message: Some(reason),
                pipelines: Vec::new(),
                date_properties: Vec::new(),
            }
        }
    };
    let pipelines = match client.discover_pipelines() {
        Ok(pipelines) => pipelines
            .into_iter()
            .map(|pipeline| {
                let pipeline_id = pipeline.pipeline_id;
                HubSpotDealPipelineOption {
                    url: Some(hubspot_pipeline_url(&pipeline_id)),
                    pipeline_id: pipeline_id.clone(),
                    label: pipeline.label,
                    display_order: pipeline.display_order,
                    archived: pipeline.archived,
                    stages: pipeline
                        .stages
                        .into_iter()
                        .map(|stage| HubSpotDealStageOption {
                            url: Some(hubspot_stage_url(&pipeline_id, &stage.stage_id)),
                            stage_id: stage.stage_id,
                            label: stage.label,
                            display_order: stage.display_order,
                            probability: stage.probability,
                            archived: stage.archived,
                        })
                        .collect(),
                }
            })
            .collect(),
        Err(err) => {
            return HubSpotDealDiscoveryResponse {
                configured: true,
                message: Some(hubspot_read_error_message(err)),
                pipelines: Vec::new(),
                date_properties: Vec::new(),
            }
        }
    };
    let date_properties = match client.discover_date_properties() {
        Ok(properties) => properties
            .into_iter()
            .map(|property| HubSpotDealDatePropertyOption {
                name: property.name,
                label: property.label,
                field_type: property.field_type,
            })
            .collect(),
        Err(err) => {
            return HubSpotDealDiscoveryResponse {
                configured: true,
                message: Some(hubspot_read_error_message(err)),
                pipelines,
                date_properties: Vec::new(),
            }
        }
    };
    HubSpotDealDiscoveryResponse {
        configured: true,
        message: None,
        pipelines,
        date_properties,
    }
}

fn system_diagnostics(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<HomeDashboardWidget, StoreError> {
    let diagnostics = crate::slices::debug::store::list_recent(conn, client_id, 50)?;
    let errors = diagnostics
        .iter()
        .filter(|row| row.severity == "error")
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|row| row.severity == "warning")
        .count();
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::SystemDiagnostics,
        title: "System diagnostics".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some(format!(
            "{} recent diagnostic{}",
            diagnostics.len(),
            plural(diagnostics.len())
        )),
        metrics: vec![
            metric_number("Errors", errors),
            metric_number("Warnings", warnings),
        ],
        items: diagnostics
            .into_iter()
            .take(5)
            .map(|row| HomeDashboardWidgetItem {
                label: row.error_code,
                detail: row
                    .error_message
                    .or(row.operation)
                    .or(row.entity_kind)
                    .or(Some(row.category)),
                tone: match row.severity.as_str() {
                    "error" => Some("critical".to_string()),
                    "warning" => Some("warning".to_string()),
                    _ => Some("neutral".to_string()),
                },
                target: Some(debug_target(Some(row.diagnostic_id))),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "Open Debug".to_string(),
            target: debug_target(None),
        }),
        chart: None,
        error_code: None,
    })
}

fn system_health(
    state: &AppState,
    conn: &rusqlite::Connection,
    user_id: &str,
) -> Result<HomeDashboardWidget, StoreError> {
    let mut items = Vec::new();

    if state.slice_enabled("email_triage") {
        let requested_scopes =
            crate::slices::google_connector::service::requested_scopes_for_enabled_slices(
                |slice_id| state.slice_enabled(slice_id),
            );
        let status = crate::slices::google_connector::service::gmail_status(
            conn,
            &state.client_id,
            user_id,
            &requested_scopes,
        )?;
        let (detail, tone) = if !status.connected {
            ("Not connected", "critical")
        } else if !status.missing_scopes.is_empty() {
            ("Reconnect needed", "warning")
        } else {
            ("Connected", "ok")
        };
        // Problem rows deep-link to where the operator fixes them: the
        // provider's own view (which hosts the connect/sync action and is
        // gated by the same slice as the row, so no dead links). Healthy
        // ("ok") rows stay non-actionable, mirroring drive_corpus_health_item.
        let mut gmail = health_item("Gmail", detail, tone);
        if tone != "ok" {
            gmail.target = Some(inbox_target(None));
        }
        items.push(gmail);
    }

    if state.slice_enabled("accounting") {
        let connector =
            crate::slices::accounting::service::connector_status(conn, &state.client_id)?;
        let sync = {
            let status = state
                .sync_guards
                .guard(crate::http::Pump::Accounting)
                .lock()
                .clone();
            crate::slices::accounting::service::sync_info(conn, &state.client_id, &status)?
        };
        let mut accounting = if connector.reconnect_required {
            health_item("Accounting", "Reconnect needed", "warning")
        } else {
            provider_sync_health_item(
                "Accounting",
                connector.connected,
                sync.sync_enabled,
                sync.in_flight,
                sync.backfill_complete,
                sync.last_error.is_some(),
            )
        };
        if accounting.tone.as_deref() != Some("ok") {
            accounting.target = Some(accounting_target(None));
        }
        items.push(accounting);
    }

    if state.slice_enabled("inventory") {
        let (has_synced, sync_error, backfill_complete, in_flight) =
            inventory_sync_status(state, conn, &state.client_id)?;
        let connector = crate::slices::inventory::service::connector_status(has_synced);
        let mut stockforge = provider_sync_health_item(
            "Stockforge",
            connector.configured,
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_STOCKFORGE_SYNC_ENABLED,
            )?,
            in_flight,
            backfill_complete,
            sync_error,
        );
        if stockforge.tone.as_deref() != Some("ok") {
            stockforge.target = Some(inventory_target(None));
        }
        items.push(stockforge);
        // Order readiness is operational, not a connection: blocked orders
        // land on the Inventory orders list with the blocked filter applied.
        let mut readiness = inventory_readiness_item(conn, &state.client_id)?;
        if readiness.tone.as_deref() != Some("ok") {
            readiness.target = Some(inventory_target(Some("orders:blocked".to_string())));
        }
        items.push(readiness);
    }

    if state.slice_enabled("drive_corpus") {
        let sync_status = state
            .sync_guards
            .guard(crate::http::Pump::Drive)
            .lock()
            .clone();
        let status =
            crate::slices::drive_corpus::service::corpus_status(state, conn, &sync_status)?;
        items.push(drive_corpus_health_item(&status));
    }

    if state.slice_enabled("owner_reports") {
        let report_config = crate::slices::owner_reports::service::config_from_sources(
            state.owner_reports_overlay.as_ref().as_ref(),
        );
        let (detail, tone) =
            if crate::slices::owner_reports::service::recipients_line(&report_config).is_none() {
                ("Recipients missing", "critical")
            } else if !crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_REPORT_DIGEST_ENABLED,
            )? {
                ("Pump off; manual generate only", "warning")
            } else if !report_config.delivery_enabled {
                ("Scheduled delivery off", "warning")
            } else {
                ("Ready", "ok")
            };
        let mut digest = health_item("Owner digest", detail, tone);
        if tone != "ok" {
            digest.target = system_settings_target(state);
        }
        items.push(digest);
    }

    if let Some(mut write_gates) = write_gate_health_item(state, conn)? {
        // Write/send gates are runtime admin-settings flags, toggled on the
        // System settings panel.
        if write_gates.tone.as_deref() != Some("ok") {
            write_gates.target = system_settings_target(state);
        }
        items.push(write_gates);
    }

    if items.is_empty() {
        items.push(health_item(
            "Connections",
            "No provider slices enabled",
            "neutral",
        ));
    }

    let needs_attention = items
        .iter()
        .filter(|item| matches!(item.tone.as_deref(), Some("critical") | Some("warning")))
        .count();
    let connected = items
        .iter()
        .filter(|item| item.tone.as_deref() == Some("ok"))
        .count();
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::SystemHealth,
        title: "System health".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Connections and provider sync".to_string()),
        metrics: vec![
            metric_number("Connected", connected),
            metric_number("Needs attention", needs_attention),
        ],
        items,
        action: None,
        chart: None,
        error_code: None,
    })
}

fn help_shortcuts() -> Result<HomeDashboardWidget, StoreError> {
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::HelpShortcuts,
        title: "Help & shortcuts".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Fast paths for keyboard-first work".to_string()),
        metrics: Vec::new(),
        items: vec![
            HomeDashboardWidgetItem {
                label: "Command palette".to_string(),
                detail: Some("⌘K/Ctrl+K — commands · ? — shortcuts".to_string()),
                tone: Some("info".to_string()),
                target: None,
            },
            HomeDashboardWidgetItem {
                label: "Help".to_string(),
                detail: Some("Open the shortcut reference".to_string()),
                tone: Some("info".to_string()),
                target: None,
            },
            HomeDashboardWidgetItem {
                label: "Review work".to_string(),
                detail: Some("j/k moves focus · Enter opens".to_string()),
                tone: Some("neutral".to_string()),
                target: None,
            },
        ],
        action: None,
        chart: None,
        error_code: None,
    })
}

fn open_tasks(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<HomeDashboardWidget, StoreError> {
    let tasks = crate::slices::follow_up_tasks::store::list_tasks(
        conn,
        client_id,
        Some(bos_contracts::follow_up_tasks::TaskStatus::Open),
        100,
        scope,
    )?;
    let today = crate::slices::accounting::service::today_string(now_ms());
    let mut decorated = tasks;
    crate::slices::follow_up_tasks::service::decorate_task_escalations(&mut decorated, &today);
    let overdue = decorated
        .iter()
        .filter(|task| {
            matches!(
                task.escalation.as_ref().map(|e| e.lane),
                Some(bos_contracts::follow_up_tasks::TaskDueLane::Overdue)
            )
        })
        .count();
    let due_today = decorated
        .iter()
        .filter(|task| {
            matches!(
                task.escalation.as_ref().map(|e| e.lane),
                Some(bos_contracts::follow_up_tasks::TaskDueLane::DueToday)
            )
        })
        .count();
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::OpenTasks,
        title: "Tasks".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some(format!("{} open", decorated.len())),
        metrics: vec![
            metric_number("Open", decorated.len()),
            metric_number("Overdue", overdue),
            metric_number("Due today", due_today),
        ],
        items: decorated
            .into_iter()
            .take(5)
            .map(|entry| HomeDashboardWidgetItem {
                label: entry.task.title,
                detail: entry.task.due_date,
                tone: entry.escalation.and_then(|e| match e.lane {
                    bos_contracts::follow_up_tasks::TaskDueLane::Overdue => {
                        Some("critical".to_string())
                    }
                    bos_contracts::follow_up_tasks::TaskDueLane::DueToday => {
                        Some("warning".to_string())
                    }
                    _ => None,
                }),
                target: Some(tasks_target(Some(entry.task.task_id.clone()))),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "View tasks".to_string(),
            target: tasks_target(None),
        }),
        chart: None,
        error_code: None,
    })
}

fn important_emails(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<HomeDashboardWidget, StoreError> {
    let messages = crate::slices::email_triage::store::list_recent_inbound(
        conn,
        client_id,
        25,
        scope,
        &crate::slices::email_triage::store::InboxFilter::default(),
    )?;
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::ImportantEmails,
        title: "Inbox".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Configured Gmail tabs".to_string()),
        metrics: Vec::new(),
        items: messages
            .into_iter()
            .take(5)
            .map(|message| HomeDashboardWidgetItem {
                label: message
                    .subject
                    .unwrap_or_else(|| "(no subject)".to_string()),
                detail: message.from_addr,
                tone: Some("info".to_string()),
                target: Some(inbox_target(Some(message.source_key.clone()))),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "Open inbox".to_string(),
            target: inbox_target(None),
        }),
        chart: None,
        error_code: None,
    })
}

fn work_queue(
    conn: &rusqlite::Connection,
    client_id: &str,
    scope: &OperatorScope,
) -> Result<HomeDashboardWidget, StoreError> {
    let items = crate::slices::work_queue::store::list_items(
        conn,
        client_id,
        Some(WorkItemStatus::Open),
        50,
        scope,
    )?;
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::WorkQueueEvents,
        title: "Work queue".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some(format!(
            "{} item{} need review",
            items.len(),
            plural(items.len())
        )),
        metrics: vec![metric_number_target(
            "Needs you",
            items.len(),
            queue_target(None),
        )],
        items: items
            .into_iter()
            .take(5)
            .map(|entry| HomeDashboardWidgetItem {
                target: Some(queue_target(Some(entry.item.item_id.clone()))),
                label: entry.item.title,
                detail: Some(entry.item.summary),
                tone: if entry.item.ai_suggested {
                    Some("ai".to_string())
                } else {
                    Some("info".to_string())
                },
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "Review queue".to_string(),
            target: queue_target(None),
        }),
        chart: None,
        error_code: None,
    })
}

fn recent_orders(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<HomeDashboardWidget, StoreError> {
    let snapshots = crate::slices::inventory::store::list_orders(conn, client_id)?;
    let today = crate::slices::accounting::service::today_string(now_ms());
    let (pipeline, controls, rows) =
        crate::slices::inventory::service::compute_orders(&snapshots, &today);
    let active = pipeline.new_count + pipeline.picking_count + pipeline.packed_count;
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::RecentOrders,
        title: "Orders in production".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some(format!("{} active before shipment", active)),
        metrics: vec![
            metric_number("In production", active as usize),
            metric_number("Exceptions", pipeline.exception_count as usize),
            metric_number_target(
                "Blocked",
                controls.blocked_count as usize,
                inventory_target(Some("orders:blocked".to_string())),
            ),
        ],
        items: rows
            .into_iter()
            .take(5)
            .map(|row| HomeDashboardWidgetItem {
                label: row.order_number.clone(),
                detail: Some(order_detail(&row)),
                tone: if row.exception || row.blocked {
                    Some("critical".to_string())
                } else if row.age_days > controls.stale_after_days as i64 {
                    Some("warning".to_string())
                } else {
                    Some("neutral".to_string())
                },
                target: Some(inventory_target(Some(format!("order:{}", row.order_id)))),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "View orders".to_string(),
            target: inventory_target(None),
        }),
        chart: Some(HomeDashboardWidgetChart::Donut {
            segments: vec![
                chart_item("New", pipeline.new_count, "#38bdf8"),
                chart_item("Picking", pipeline.picking_count, "#f59e0b"),
                chart_item("Packed", pipeline.packed_count, "#22c55e"),
            ],
        }),
        error_code: None,
    })
}

fn financial_overview(
    state: &AppState,
    conn: &rusqlite::Connection,
    scope: &OperatorScope,
) -> Result<HomeDashboardWidget, StoreError> {
    if !crate::slices::accounting::service::cached_financial_visibility_allowed(
        conn,
        &state.client_id,
        scope,
        state.accounting_visibility_policy,
    )? {
        return Ok(unavailable(
            HomeDashboardWidgetKind::FinancialOverview,
            "Financial overview",
            "qbo_financial_scope_forbidden",
        ));
    }
    let today = crate::slices::accounting::service::today_string(now_ms());
    let sync = accounting_sync_info(conn, &state.client_id)?;
    let metric_config = crate::slices::accounting::service::metric_basis_config_from_sources(
        Some(&state.accounting_overlay.metric_basis),
    )?;
    let financials = crate::slices::accounting::service::financials_from_store(
        conn,
        &state.client_id,
        &today,
        sync,
        &metric_config,
    )?;
    let invoices = crate::slices::accounting::store::list_invoices(conn, &state.client_id, 500)?;
    let invoice_rows: Vec<_> = invoices
        .iter()
        .map(|snapshot| crate::slices::accounting::service::invoice_row(snapshot, &today))
        .filter(|row| row.status == "open" || row.status == "overdue")
        .collect();
    let accounts_receivable: i64 = invoice_rows.iter().map(|row| row.balance_cents).sum();
    let bills = crate::slices::accounting::store::list_bills(conn, &state.client_id, 500)?;
    let accounts_payable: i64 = bills
        .iter()
        .filter(|bill| !bill.voided && bill.balance_cents > 0)
        .map(|bill| bill.balance_cents)
        .sum();
    let bills_known = crate::slices::accounting::store::get_cursor(
        conn,
        &state.client_id,
        crate::slices::accounting::store::ENTITY_BILL,
    )?
    .backfill_complete;
    let cash_on_hand = crate::slices::accounting::store::get_latest_balance_sheet_snapshot(
        conn,
        &state.client_id,
    )?
    .map(|snapshot| snapshot.cash_on_hand_cents);
    let daily_revenue = crate::slices::accounting::service::daily_revenue_from_store(
        conn,
        &state.client_id,
        &today,
    )?;
    let points = daily_revenue
        .iter()
        .map(|row| HomeDashboardWidgetChartPoint {
            label: short_date_label(&row.period_start),
            value: cents_to_chart_units(row.total_income_cents),
            color: Some("#38bdf8".to_string()),
            target: None,
        })
        .collect::<Vec<_>>();
    let open_bill_count = bills
        .iter()
        .filter(|bill| !bill.voided && bill.balance_cents > 0)
        .count();
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::FinancialOverview,
        title: "Financial overview".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some("Daily revenue · last 7 days".to_string()),
        metrics: vec![
            metric_cents("Accounts receivable", accounts_receivable),
            if bills_known {
                metric_cents("Accounts payable", accounts_payable)
            } else {
                metric_unknown("Accounts payable")
            },
            match cash_on_hand {
                Some(cents) => metric_cents("Cash on hand", cents),
                None => metric_unknown("Cash on hand"),
            },
            metric_cents("Revenue · month to date", financials.month_to_date_cents),
        ],
        items: vec![
            HomeDashboardWidgetItem {
                label: "Open invoices".to_string(),
                detail: Some(format!(
                    "{} invoice{}",
                    invoice_rows.len(),
                    plural(invoice_rows.len())
                )),
                tone: Some(if invoice_rows.iter().any(|row| row.status == "overdue") {
                    "warning".to_string()
                } else {
                    "neutral".to_string()
                }),
                target: Some(accounting_target(Some("invoices".to_string()))),
            },
            HomeDashboardWidgetItem {
                label: "Open bills".to_string(),
                detail: Some(if bills_known {
                    format!("{} bill{}", open_bill_count, plural(open_bill_count))
                } else {
                    "Pending sync".to_string()
                }),
                tone: Some(if bills_known && accounts_payable > 0 {
                    "warning".to_string()
                } else {
                    "neutral".to_string()
                }),
                target: None,
            },
        ],
        action: Some(HomeDashboardAction {
            label: "Open accounting".to_string(),
            target: accounting_target(None),
        }),
        chart: Some(HomeDashboardWidgetChart::Sparkline { points }),
        error_code: None,
    })
}

fn inventory_alerts(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<HomeDashboardWidget, StoreError> {
    let alerts = crate::slices::inventory::store::list_alerts(conn, client_id)?;
    let reorder = crate::slices::inventory::store::list_reorder_suggestions(conn, client_id)?;
    let materials = crate::slices::inventory::store::list_materials(conn, client_id)?;
    let (_kpis, mut stock_rows) =
        crate::slices::inventory::service::compute_stock(&materials, &alerts);
    stock_rows.retain(|row| row.is_stocked);
    stock_rows.sort_by_key(|row| std::cmp::Reverse(row.stock_value_cents));
    let top_skus = stock_rows
        .iter()
        .filter(|row| row.stock_value_cents > 0)
        .take(5)
        .map(|row| HomeDashboardWidgetChartPoint {
            label: row.sku.clone().unwrap_or_else(|| row.name.clone()),
            value: cents_to_chart_units(row.stock_value_cents),
            color: Some("#38bdf8".to_string()),
            target: Some(inventory_target(Some(format!(
                "material:{}",
                row.material_id
            )))),
        })
        .collect::<Vec<_>>();
    let actionable_alerts = crate::slices::inventory::service::alert_rows(&alerts, &materials);
    let pending_reorders = crate::slices::inventory::service::reorder_rows(&reorder, &materials);
    let critical = actionable_alerts
        .iter()
        .filter(|alert| alert.severity.eq_ignore_ascii_case("critical"))
        .count();
    let out_of_stock = stock_rows
        .iter()
        .filter(|row| row.stock_status == "out")
        .count();
    Ok(HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::InventoryAlerts,
        title: "Inventory".to_string(),
        state: HomeDashboardWidgetState::Ready,
        summary: Some(format!(
            "Stocked report · {} alert{}, {} reorder suggestion{}",
            actionable_alerts.len(),
            plural(actionable_alerts.len()),
            pending_reorders.len(),
            plural(pending_reorders.len())
        )),
        metrics: vec![
            metric_number("Alerts", actionable_alerts.len()),
            metric_number_target(
                "Critical",
                critical,
                inventory_target(Some("alerts:critical".to_string())),
            ),
            metric_number_target(
                "Out of stock",
                out_of_stock,
                inventory_target(Some("stock:out".to_string())),
            ),
            metric_number_target(
                "Reorder",
                pending_reorders.len(),
                inventory_target(Some("reorder".to_string())),
            ),
        ],
        items: actionable_alerts
            .into_iter()
            .take(5)
            .map(|alert| HomeDashboardWidgetItem {
                label: alert
                    .material_name
                    .or(alert.material_sku)
                    .unwrap_or_else(|| alert.alert_id.clone()),
                detail: alert.message,
                tone: if alert.severity.eq_ignore_ascii_case("critical") {
                    Some("critical".to_string())
                } else {
                    Some("warning".to_string())
                },
                target: Some(inventory_identity_target(
                    Some(format!("alert:{}", alert.alert_id)),
                    alert.external_url,
                )),
            })
            .collect(),
        action: Some(HomeDashboardAction {
            label: "Open stocked report".to_string(),
            target: inventory_target(None),
        }),
        chart: Some(HomeDashboardWidgetChart::Bar { items: top_skus }),
        error_code: None,
    })
}

fn accounting_sync_info(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<bos_contracts::accounting::AccountingSyncInfo, StoreError> {
    let (invoice_count, customer_count) =
        crate::slices::accounting::store::snapshot_counts(conn, client_id)?;
    Ok(bos_contracts::accounting::AccountingSyncInfo {
        sync_enabled: crate::slices::admin_settings::service::flag(
            conn,
            client_id,
            &crate::env_registry::BOS_ACCOUNTING_SYNC_ENABLED,
        )?,
        in_flight: false,
        backfill_complete: true,
        last_synced_at_ms: None,
        invoice_count,
        customer_count,
        last_requests_used: 0,
        next_sync_allowed_at_ms: 0,
        last_error: None,
    })
}

fn unavailable(
    kind: HomeDashboardWidgetKind,
    title: &str,
    error_code: &str,
) -> HomeDashboardWidget {
    HomeDashboardWidget {
        kind,
        title: title.to_string(),
        state: HomeDashboardWidgetState::Unavailable,
        summary: None,
        metrics: Vec::new(),
        items: Vec::new(),
        action: None,
        chart: None,
        error_code: Some(error_code.to_string()),
    }
}

fn has_system_health_source(state: &AppState) -> bool {
    state.slice_enabled("email_triage")
        || state.slice_enabled("accounting")
        || state.slice_enabled("inventory")
        || state.slice_enabled("drive_corpus")
        || has_write_gate_source(state)
}

fn has_write_gate_source(state: &AppState) -> bool {
    has_gmail_write_gate_source(state)
        || state.slice_enabled("calendar_drafts")
        || state.slice_enabled("crm_drafts")
        || state.slice_enabled("crm_record_drafts")
        || state.slice_enabled("crm_sales_intent")
        || state.slice_enabled("invoice_drafts")
        || state.slice_enabled("ledger_drafts")
        || state.slice_enabled("customer_tier_sync")
}

fn has_gmail_write_gate_source(state: &AppState) -> bool {
    state.slice_enabled("email_drafts")
        || state.slice_enabled("claim_drafts")
        || state.slice_enabled("owner_reports")
}

fn canonical_widget_position(kind: HomeDashboardWidgetKind) -> usize {
    store::default_widgets()
        .into_iter()
        .position(|widget| widget.kind == kind)
        .unwrap_or(usize::MAX)
}

fn debug_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Debug),
        focus_id,
        external_url: None,
    }
}

fn inbox_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Inbox),
        focus_id,
        external_url: None,
    }
}

fn tasks_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Tasks),
        focus_id,
        external_url: None,
    }
}

fn queue_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Queue),
        focus_id,
        external_url: None,
    }
}

// Multi-entity focus grammar:
// Inventory: order:{order_id}, alert:{alert_id}, material:{material_id},
// plus metric-level orders:blocked, alerts:critical, stock:out, reorder.
// Accounting: invoices.
fn inventory_target(focus_id: Option<String>) -> HomeDashboardTarget {
    inventory_identity_target(focus_id, None)
}

fn inventory_identity_target(
    focus_id: Option<String>,
    external_url: Option<String>,
) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Inventory),
        focus_id,
        external_url,
    }
}

fn settings_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Settings),
        focus_id,
        external_url: None,
    }
}

fn system_settings_target(state: &AppState) -> Option<HomeDashboardTarget> {
    state
        .slice_enabled("admin_settings")
        .then(|| settings_target(Some("system".to_string())))
}

fn hubspot_pipeline_target(pipeline_id: &str) -> HomeDashboardTarget {
    external_target(hubspot_pipeline_url(pipeline_id))
}

fn hubspot_stage_target(pipeline_id: &str, stage_id: &str) -> HomeDashboardTarget {
    external_target(hubspot_stage_url(pipeline_id, stage_id))
}

fn hubspot_deal_target(deal_id: &str) -> HomeDashboardTarget {
    external_target(format!("https://app.hubspot.com/contacts/deal/{deal_id}"))
}

fn external_target(url: String) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: None,
        focus_id: None,
        external_url: Some(url),
    }
}

fn hubspot_pipeline_url(pipeline_id: &str) -> String {
    format!("https://app.hubspot.com/sales/deals/board/view/all/?pipelineId={pipeline_id}")
}

fn hubspot_stage_url(pipeline_id: &str, stage_id: &str) -> String {
    format!(
        "https://app.hubspot.com/sales/deals/board/view/all/?pipelineId={pipeline_id}&stageId={stage_id}"
    )
}

fn stage_ids_for(
    mapping: &HubSpotDealPipelineMapping,
    status: HubSpotDealMappedStatus,
) -> Vec<String> {
    mapping
        .stage_mappings
        .iter()
        .filter(|stage| stage.status == status)
        .map(|stage| stage.stage_id.clone())
        .collect()
}

fn stage_label_lookup(
    mapping: &HubSpotDealPipelineMapping,
) -> std::collections::BTreeMap<String, String> {
    mapping
        .stage_mappings
        .iter()
        .filter_map(|stage| {
            stage
                .label
                .as_ref()
                .map(|label| (stage.stage_id.clone(), label.clone()))
        })
        .collect()
}

fn hubspot_unavailable_widget(code: &str, message: &str) -> HomeDashboardWidget {
    HomeDashboardWidget {
        kind: HomeDashboardWidgetKind::SalesPipeline,
        title: "Sales pipeline".to_string(),
        state: HomeDashboardWidgetState::Unavailable,
        summary: Some(message.to_string()),
        metrics: Vec::new(),
        items: Vec::new(),
        action: Some(HomeDashboardAction {
            label: "Review setup".to_string(),
            target: settings_target(Some("hubspot_deals".to_string())),
        }),
        chart: Some(HomeDashboardWidgetChart::Funnel { stages: Vec::new() }),
        error_code: Some(code.to_string()),
    }
}

fn hubspot_read_error_message(err: bos_integrations::hubspot::HubSpotReadError) -> String {
    match err {
        bos_integrations::hubspot::HubSpotReadError::Limited { code, message }
        | bos_integrations::hubspot::HubSpotReadError::Retryable { code, message } => {
            format!("{code}: {message}")
        }
    }
}

fn health_item(label: &str, detail: &str, tone: &str) -> HomeDashboardWidgetItem {
    HomeDashboardWidgetItem {
        label: label.to_string(),
        detail: Some(detail.to_string()),
        tone: Some(tone.to_string()),
        target: None,
    }
}

pub(super) fn provider_sync_health_item(
    label: &str,
    connected: bool,
    sync_enabled: bool,
    in_flight: bool,
    backfill_complete: bool,
    has_error: bool,
) -> HomeDashboardWidgetItem {
    let (detail, tone) = if !connected {
        ("Not connected", "critical")
    } else if has_error {
        ("Sync needs attention", "critical")
    } else if in_flight {
        ("Sync running", "progress")
    } else if !sync_enabled {
        ("Pump off; manual sync only", "warning")
    } else if !backfill_complete {
        ("Initial sync pending", "warning")
    } else {
        ("Connected and synced", "ok")
    };
    health_item(label, detail, tone)
}

fn drive_corpus_health_item(
    status: &bos_contracts::drive_corpus::DriveCorpusStatus,
) -> HomeDashboardWidgetItem {
    let (detail, tone) = if !status.credential_connected {
        ("Not connected", "critical")
    } else if status.drive_scope_granted == Some(false) {
        ("Reconnect needed", "warning")
    } else if !status.configured {
        ("Not configured", "warning")
    } else if status.last_error.is_some() {
        ("Sync needs attention", "critical")
    } else if status.in_flight {
        ("Sync running", "progress")
    } else if !status.sync_enabled {
        ("Pump off; manual sync only", "warning")
    } else if !status.backfill_complete {
        ("Initial sync pending", "warning")
    } else {
        ("Connected and synced", "ok")
    };
    let target = if tone == "ok" {
        None
    } else {
        Some(settings_target(Some("content_generation".to_string())))
    };
    HomeDashboardWidgetItem {
        label: "Google Drive".to_string(),
        detail: Some(detail.to_string()),
        tone: Some(tone.to_string()),
        target,
    }
}

fn inventory_sync_status(
    state: &AppState,
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<(bool, bool, bool, bool), StoreError> {
    let mut has_synced = false;
    let mut sync_error = false;
    let mut backfill_complete = true;
    for entity in crate::slices::inventory::store::ALL_ENTITIES {
        let cursor = crate::slices::inventory::store::get_cursor(conn, client_id, entity)?;
        has_synced |= cursor.last_advanced_at_ms.is_some();
        sync_error |= cursor.last_error.is_some();
        backfill_complete &= cursor.backfill_complete;
    }
    let in_flight = state
        .sync_guards
        .guard(crate::http::Pump::Stockforge)
        .lock()
        .in_flight;
    Ok((has_synced, sync_error, backfill_complete, in_flight))
}

fn inventory_readiness_item(
    conn: &rusqlite::Connection,
    client_id: &str,
) -> Result<HomeDashboardWidgetItem, StoreError> {
    let snapshots = crate::slices::inventory::store::list_orders(conn, client_id)?;
    let today = crate::slices::accounting::service::today_string(now_ms());
    let (_pipeline, controls, _rows) =
        crate::slices::inventory::service::compute_orders(&snapshots, &today);
    if controls.deduction_failed_count > 0 {
        return Ok(health_item(
            "Order readiness",
            &format!(
                "{} failed deduction{}",
                controls.deduction_failed_count,
                plural(controls.deduction_failed_count as usize)
            ),
            "critical",
        ));
    }
    if controls.needs_mapping_count > 0 {
        return Ok(health_item(
            "Order readiness",
            &format!(
                "{} unmapped SKU{}",
                controls.needs_mapping_count,
                plural(controls.needs_mapping_count as usize)
            ),
            "critical",
        ));
    }
    if controls.blocked_count > 0 {
        return Ok(health_item(
            "Order readiness",
            &format!(
                "{} blocked order{}",
                controls.blocked_count,
                plural(controls.blocked_count as usize)
            ),
            "critical",
        ));
    }
    if controls.stale_count > 0 {
        return Ok(health_item(
            "Order readiness",
            &format!(
                "{} stale order{}",
                controls.stale_count,
                plural(controls.stale_count as usize)
            ),
            "warning",
        ));
    }
    Ok(health_item("Order readiness", "No blocked orders", "ok"))
}

fn write_gate_health_item(
    state: &AppState,
    conn: &rusqlite::Connection,
) -> Result<Option<HomeDashboardWidgetItem>, StoreError> {
    let mut gates = Vec::new();
    if has_gmail_write_gate_source(state) {
        gates.push(crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &crate::env_registry::BOS_GMAIL_WRITE_ENABLED,
        )?);
    }
    if state.slice_enabled("calendar_drafts") {
        gates.push(crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &crate::env_registry::BOS_GOOGLE_CALENDAR_WRITE_ENABLED,
        )?);
    }
    if state.slice_enabled("crm_drafts")
        || state.slice_enabled("crm_record_drafts")
        || state.slice_enabled("crm_sales_intent")
    {
        let provider = crate::env_registry::string(&crate::env_registry::BOS_CRM_PROVIDER)
            .unwrap_or_else(|| "hubspot".to_string());
        let enabled = if provider.eq_ignore_ascii_case("espocrm") {
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_ESPOCRM_WRITE_ENABLED,
            )?
        } else {
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_HUBSPOT_WRITE_ENABLED,
            )?
        };
        gates.push(enabled);
    }
    if state.slice_enabled("invoice_drafts") || state.slice_enabled("ledger_drafts") {
        let provider = crate::env_registry::string(&crate::env_registry::BOS_ACCOUNTING_PROVIDER)
            .unwrap_or_else(|| "qbo".to_string());
        let enabled = if provider.eq_ignore_ascii_case("invoice_ninja") {
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_INVOICE_NINJA_WRITE_ENABLED,
            )?
        } else if provider.eq_ignore_ascii_case("stripe") {
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_STRIPE_WRITE_ENABLED,
            )?
        } else {
            crate::slices::admin_settings::service::flag(
                conn,
                &state.client_id,
                &crate::env_registry::BOS_QBO_WRITE_ENABLED,
            )?
        };
        gates.push(enabled);
    }
    if state.slice_enabled("customer_tier_sync") {
        gates.push(crate::slices::admin_settings::service::flag(
            conn,
            &state.client_id,
            &crate::env_registry::BOS_SHOPIFY_WRITE_ENABLED,
        )?);
    }
    if gates.is_empty() {
        return Ok(None);
    }
    let live = gates.iter().filter(|enabled| **enabled).count();
    let dry_run = gates.len().saturating_sub(live);
    let (detail, tone) = if dry_run > 0 {
        (
            format!("{live} live, {dry_run} dry-run gate{}", plural(dry_run)),
            "warning",
        )
    } else {
        (format!("{live} live gate{}", plural(live)), "ok")
    };
    Ok(Some(health_item("Write gates", &detail, tone)))
}

fn chart_item(label: &str, value: u32, color: &str) -> HomeDashboardWidgetChartPoint {
    HomeDashboardWidgetChartPoint {
        label: label.to_string(),
        value,
        color: Some(color.to_string()),
        target: None,
    }
}

fn order_detail(row: &bos_contracts::inventory::InventoryOrderRow) -> String {
    let stage = match row.board_status.as_str() {
        "NEW" => "New",
        "PICKING" => "Picking",
        "PACKED" => "Packed",
        "SHIPPED" => "Shipped",
        "DELIVERED" => "Delivered",
        _ => "Needs review",
    };
    match row.customer_name.as_deref() {
        Some(customer) if !customer.trim().is_empty() => format!("{customer} · {stage}"),
        _ => stage.to_string(),
    }
}

fn short_date_label(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = date
        .get(5..7)
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|month| (1..=12).contains(month));
    let day = date
        .get(8..10)
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|day| (1..=31).contains(day));
    match (month, day) {
        (Some(month), Some(day)) => format!("{} {day}", MONTHS[month - 1]),
        _ => date.to_string(),
    }
}

fn cents_to_chart_units(cents: i64) -> u32 {
    let dollars = (cents.max(0) / 100).max(1);
    u32::try_from(dollars).unwrap_or(u32::MAX)
}

fn metric_number(label: &str, value: usize) -> HomeDashboardMetric {
    HomeDashboardMetric {
        label: label.to_string(),
        value: Some(value.to_string()),
        cents: None,
        target: None,
    }
}

fn metric_cents(label: &str, cents: i64) -> HomeDashboardMetric {
    HomeDashboardMetric {
        label: label.to_string(),
        value: Some(format_cents(cents)),
        cents: Some(cents),
        target: None,
    }
}

fn metric_unknown(label: &str) -> HomeDashboardMetric {
    HomeDashboardMetric {
        label: label.to_string(),
        value: Some("—".to_string()),
        cents: None,
        target: None,
    }
}

fn metric_number_target(
    label: &str,
    value: usize,
    target: HomeDashboardTarget,
) -> HomeDashboardMetric {
    HomeDashboardMetric {
        target: Some(target),
        ..metric_number(label, value)
    }
}

fn metric_cents_target(
    label: &str,
    cents: i64,
    target: HomeDashboardTarget,
) -> HomeDashboardMetric {
    HomeDashboardMetric {
        target: Some(target),
        ..metric_cents(label, cents)
    }
}

fn accounting_target(focus_id: Option<String>) -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Accounting),
        focus_id,
        external_url: None,
    }
}

fn reports_target() -> HomeDashboardTarget {
    HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Reports),
        focus_id: None,
        external_url: None,
    }
}

fn format_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}
