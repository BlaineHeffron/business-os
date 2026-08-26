use crate::http::test_support::EnvGuard;
use bos_contracts::home_dashboard::{
    HomeDashboardPreferencesUpdateRequest, HomeDashboardTargetView, HomeDashboardWidgetChart,
    HomeDashboardWidgetKind, HomeDashboardWidgetPreference, HomeDashboardWidgetState,
    HubSpotDealMappedStatus, HubSpotDealPipelineMapping, HubSpotDealPipelineMappingSaveRequest,
    HubSpotDealStageMapping,
};
use bos_integrations::{
    accounting_read::{BalanceSheetSummary, BillRecord, InvoiceRecord},
    stockforge_read::{
        SfAlertRecord, SfMaterialRecord, SfOrderCardRecord, SfReorderSuggestionRecord,
    },
};

const CLIENT: &str = "client";
const USER: &str = "user_jordan";

#[test]
fn preference_defaults_include_all_widgets_without_row() {
    let persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection_ref();

    let pref = super::store::load_preference(conn, CLIENT, USER).expect("preference");

    assert!(pref.revision.is_none());
    assert_eq!(pref.widgets.len(), 11);
    assert_eq!(
        pref.widgets
            .iter()
            .map(|widget| widget.kind)
            .collect::<Vec<_>>(),
        vec![
            HomeDashboardWidgetKind::BusinessSummary,
            HomeDashboardWidgetKind::SalesPipeline,
            HomeDashboardWidgetKind::OpenTasks,
            HomeDashboardWidgetKind::ImportantEmails,
            HomeDashboardWidgetKind::WorkQueueEvents,
            HomeDashboardWidgetKind::RecentOrders,
            HomeDashboardWidgetKind::FinancialOverview,
            HomeDashboardWidgetKind::InventoryAlerts,
            HomeDashboardWidgetKind::SystemHealth,
            HomeDashboardWidgetKind::HelpShortcuts,
            HomeDashboardWidgetKind::SystemDiagnostics,
        ]
    );
}

#[test]
fn business_summary_is_first_and_degrades_to_inventory_only() {
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "inventory"]);
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert_eq!(
        response.available_widgets[0],
        HomeDashboardWidgetKind::BusinessSummary
    );
    assert_eq!(
        response.widgets[0].kind,
        HomeDashboardWidgetKind::BusinessSummary
    );
    let widget = &response.widgets[0];
    assert_eq!(widget.title, "Business summary");
    // Inventory-sourced KPI is present and deep-links into Inventory.
    let orders = widget
        .metrics
        .iter()
        .find(|metric| metric.label == "Orders in pipeline")
        .expect("orders metric");
    assert_eq!(
        orders.target.as_ref().expect("orders target").view,
        Some(HomeDashboardTargetView::Inventory)
    );
    // Accounting slice is off, so its KPIs degrade out rather than blocking the card.
    assert!(widget
        .metrics
        .iter()
        .all(|metric| metric.label != "Revenue · month to date"));
}

#[test]
fn business_summary_is_unavailable_without_accounting_or_inventory() {
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "follow_up_tasks"],
    );
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(!response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::BusinessSummary));
    assert!(!response
        .widgets
        .iter()
        .any(|widget| widget.kind == HomeDashboardWidgetKind::BusinessSummary));
}

#[test]
fn sales_pipeline_without_mapping_returns_pending_setup() {
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "lead_discovery"],
    );
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SalesPipeline)
        .expect("sales pipeline widget");
    assert_eq!(widget.title, "Sales pipeline");
    assert_eq!(widget.state, HomeDashboardWidgetState::PendingSetup);
    assert_eq!(
        widget.error_code.as_deref(),
        Some("hubspot_deal_mapping_pending")
    );
    assert!(widget.metrics.is_empty());
    assert!(widget
        .summary
        .as_deref()
        .unwrap_or_default()
        .contains("Choose the HubSpot Deals pipeline"));
    let action = widget.action.as_ref().expect("setup action");
    assert_eq!(action.label, "Set up deals");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Settings));
    assert_eq!(action.target.focus_id.as_deref(), Some("hubspot_deals"));
    match widget.chart.as_ref().expect("funnel") {
        HomeDashboardWidgetChart::Funnel { stages } => {
            assert!(stages.is_empty(), "deals funnel stays empty until mapped");
        }
        other => panic!("expected funnel chart, got {other:?}"),
    }
}

#[test]
fn sales_pipeline_is_available_from_home_dashboard_without_lead_discovery() {
    let mut state = crate::http::test_support::test_state_configured(None, &["home_dashboard"]);
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SalesPipeline));
    assert!(response
        .widgets
        .iter()
        .any(|widget| widget.kind == HomeDashboardWidgetKind::SalesPipeline));
}

#[test]
fn sales_pipeline_with_mapping_requires_live_hubspot_access() {
    let _hubspot_token = EnvGuard::unset("BOS_HUBSPOT_ACCESS_TOKEN");
    let mut state = crate::http::test_support::test_state_configured(None, &["home_dashboard"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        super::store::save_hubspot_deal_mapping(
            persistence.connection(),
            CLIENT,
            USER,
            &HubSpotDealPipelineMappingSaveRequest {
                mapping: sample_hubspot_mapping(),
                expected_revision: None,
                idempotency_key: "sales-pipeline-live-access".to_string(),
                actor_id: None,
            },
            1_000,
        )
        .expect("save mapping");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SalesPipeline)
        .expect("sales pipeline widget");
    assert_eq!(widget.state, HomeDashboardWidgetState::Unavailable);
    assert_eq!(
        widget.error_code.as_deref(),
        Some("hubspot_access_token_missing")
    );
    assert!(widget.metrics.is_empty());
}

#[test]
fn hubspot_deal_mapping_save_uses_store_core_revision() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let request = HubSpotDealPipelineMappingSaveRequest {
        mapping: sample_hubspot_mapping(),
        expected_revision: None,
        idempotency_key: "map-1".to_string(),
        actor_id: None,
    };

    let outcome = super::store::save_hubspot_deal_mapping(
        persistence.connection(),
        CLIENT,
        USER,
        &request,
        1_000,
    )
    .expect("save mapping");

    match outcome {
        crate::store_core::MutationOutcome::Applied { revision, .. } => assert_eq!(revision, 1),
        other => panic!("expected applied, got {other:?}"),
    }
    let loaded = super::store::load_hubspot_deal_mapping(persistence.connection_ref(), CLIENT)
        .expect("load mapping");
    assert_eq!(loaded.revision, Some(1));
    assert_eq!(loaded.mapping, Some(sample_hubspot_mapping()));
}

#[test]
fn system_health_is_operator_facing_and_keeps_raw_codes_out() {
    let _guard = EnvGuard::unset("BOS_DEBUG_ENABLED");
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "email_triage"]);
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemHealth));
    assert!(!response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemDiagnostics));
    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SystemHealth)
        .expect("system health widget");
    assert_eq!(widget.title, "System health");
    assert_eq!(metric_value(widget, "Needs attention"), Some("1"));
    assert_eq!(widget.items[0].label, "Gmail");
    assert_eq!(widget.items[0].detail.as_deref(), Some("Not connected"));
    assert_ne!(widget.items[0].label, "oauth_app_unconfigured");
    assert!(widget.error_code.is_none());
    // A problem provider row deep-links to its own view, where the connect
    // action lives (same slice gate as the row, so never a dead link).
    let gmail_target = widget.items[0]
        .target
        .as_ref()
        .expect("gmail deep-link target");
    assert_eq!(gmail_target.view, Some(HomeDashboardTargetView::Inbox));
    assert!(gmail_target.focus_id.is_none());
}

#[test]
fn system_health_distinguishes_unconfigured_drive_from_disconnected_oauth() {
    let _guard = EnvGuard::set_many(&[
        ("BOS_DEBUG_ENABLED", "0"),
        ("BOS_GMAIL_OAUTH_CLIENT_ID", "client-id"),
        ("BOS_GMAIL_OAUTH_CLIENT_SECRET", "client-secret"),
    ]);
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "drive_corpus"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        crate::slices::google_connector::store::store_credential(
            persistence.connection(),
            CLIENT,
            USER,
            crate::slices::google_connector::SERVICE_GMAIL,
            "refresh-token",
            &[crate::slices::google_connector::service::DRIVE_READONLY_SCOPE.to_string()],
            1_000,
        )
        .expect("google credential");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SystemHealth)
        .expect("system health widget");
    assert_eq!(
        item_detail(widget, "Google Drive"),
        Some(("Not configured", "warning"))
    );
    let drive = widget
        .items
        .iter()
        .find(|item| item.label == "Google Drive")
        .expect("drive row");
    let target = drive.target.as_ref().expect("drive settings target");
    assert_eq!(target.view, Some(HomeDashboardTargetView::Settings));
    assert_eq!(target.focus_id.as_deref(), Some("content_generation"));
}

#[test]
fn system_health_surfaces_sellable_readiness_checks() {
    let _gmail_gate = EnvGuard::unset("BOS_GMAIL_WRITE_ENABLED");
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &[
            "home_dashboard",
            "inventory",
            "owner_reports",
            "email_drafts",
            "admin_settings",
        ],
    );
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let mut failed = order("failed_deduction", "PACKED");
        failed.deduction_failed = true;
        crate::slices::inventory::store::upsert_order_snapshots(
            persistence.connection(),
            CLIENT,
            &[failed],
            1_000,
        )
        .expect("orders");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SystemHealth)
        .expect("system health widget");
    assert_eq!(
        item_detail(widget, "Owner digest"),
        Some(("Recipients missing", "critical"))
    );
    assert_eq!(
        item_detail(widget, "Order readiness"),
        Some(("1 failed deduction", "critical"))
    );
    let write_gates = item_detail(widget, "Write gates").expect("write gates");
    assert_eq!(write_gates.1, "warning");
    assert!(
        write_gates.0.contains("dry-run gate"),
        "write-gate row should expose dry-run posture: {}",
        write_gates.0
    );

    // Deep-link targets for the non-connection rows in this state.
    let find = |label: &str| {
        widget
            .items
            .iter()
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("{label} row"))
            .target
            .as_ref()
            .unwrap_or_else(|| panic!("{label} deep-link target"))
            .clone()
    };
    // Order readiness is operational → Inventory orders list, blocked filter.
    let readiness_target = find("Order readiness");
    assert_eq!(
        readiness_target.view,
        Some(HomeDashboardTargetView::Inventory)
    );
    assert_eq!(readiness_target.focus_id.as_deref(), Some("orders:blocked"));
    // Stockforge (inventory connector) → the Inventory view that hosts setup.
    let stockforge_target = find("Stockforge");
    assert_eq!(
        stockforge_target.view,
        Some(HomeDashboardTargetView::Inventory)
    );
    assert!(stockforge_target.focus_id.is_none());
    // Owner digest config is fixed through runtime settings.
    let digest_target = find("Owner digest");
    assert_eq!(digest_target.view, Some(HomeDashboardTargetView::Settings));
    assert_eq!(digest_target.focus_id.as_deref(), Some("system"));
    // Write gates are runtime flags → System settings panel.
    let gates_target = find("Write gates");
    assert_eq!(gates_target.view, Some(HomeDashboardTargetView::Settings));
    assert_eq!(gates_target.focus_id.as_deref(), Some("system"));
}

#[test]
fn system_health_is_available_for_readiness_only_sources() {
    let _gmail_gate = EnvGuard::unset("BOS_GMAIL_WRITE_ENABLED");
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "owner_reports", "email_drafts"],
    );
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemHealth));
    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SystemHealth)
        .expect("system health widget");
    assert_eq!(
        item_detail(widget, "Owner digest"),
        Some(("Recipients missing", "critical"))
    );
    assert_eq!(
        item_detail(widget, "Write gates").map(|row| row.1),
        Some("warning")
    );
    assert!(widget
        .items
        .iter()
        .filter(|item| matches!(item.label.as_str(), "Owner digest" | "Write gates"))
        .all(|item| item.target.is_none()));
}

#[test]
fn claim_drafts_count_as_gmail_write_gate_source() {
    let _gmail_gate = EnvGuard::unset("BOS_GMAIL_WRITE_ENABLED");
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "claim_drafts"]);
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemHealth));
    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::SystemHealth)
        .expect("system health widget");
    assert_eq!(
        item_detail(widget, "Write gates"),
        Some(("0 live, 1 dry-run gate", "warning"))
    );
}

#[test]
fn provider_sync_health_marks_disabled_pumps() {
    let row =
        super::service::provider_sync_health_item("Accounting", true, false, false, true, false);

    assert_eq!(row.detail.as_deref(), Some("Pump off; manual sync only"));
    assert_eq!(row.tone.as_deref(), Some("warning"));
}

#[test]
fn new_widgets_are_available_from_their_source_slices() {
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &[
            "home_dashboard",
            "lead_discovery",
            "email_triage",
            "follow_up_tasks",
        ],
    );
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert_eq!(
        response.available_widgets,
        vec![
            HomeDashboardWidgetKind::SalesPipeline,
            HomeDashboardWidgetKind::OpenTasks,
            HomeDashboardWidgetKind::ImportantEmails,
            HomeDashboardWidgetKind::SystemHealth,
            HomeDashboardWidgetKind::HelpShortcuts,
        ]
    );
}

#[test]
fn work_queue_widget_targets_exact_queue_items() {
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "work_queue"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        seed_work_item(
            persistence.connection(),
            "wi_note_q1",
            "Review customer note",
            "Customer asked for a quote",
        );
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::WorkQueueEvents)
        .expect("work queue widget");
    assert_eq!(metric_value(widget, "Needs you"), Some("1"));
    let metric_target = widget.metrics[0].target.as_ref().expect("metric target");
    assert_eq!(metric_target.view, Some(HomeDashboardTargetView::Queue));
    assert!(metric_target.focus_id.is_none());
    let action = widget.action.as_ref().expect("queue action");
    assert_eq!(action.label, "Review queue");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Queue));
    assert!(action.target.focus_id.is_none());
    let row_target = widget.items[0].target.as_ref().expect("row target");
    assert_eq!(row_target.view, Some(HomeDashboardTargetView::Queue));
    assert_eq!(row_target.focus_id.as_deref(), Some("wi_note_q1"));
}

#[test]
fn funnel_chart_and_leads_target_serialize_with_snake_case_tags() {
    let chart = HomeDashboardWidgetChart::Funnel {
        stages: vec![
            bos_contracts::home_dashboard::HomeDashboardWidgetChartPoint {
                label: "Qualified".to_string(),
                value: 3,
                color: None,
                target: None,
            },
        ],
    };
    let chart_json = serde_json::to_value(&chart).expect("chart json");
    assert_eq!(chart_json["type"], "funnel");
    assert_eq!(chart_json["stages"][0]["label"], "Qualified");

    let target = bos_contracts::home_dashboard::HomeDashboardTarget {
        view: Some(HomeDashboardTargetView::Leads),
        focus_id: None,
        external_url: None,
    };
    let target_json = serde_json::to_value(&target).expect("target json");
    assert_eq!(target_json["view"], "leads");
    assert!(target_json.get("focus_id").is_none());
}

#[test]
fn inbox_widget_uses_configured_tabs_unread_count_and_targets() {
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "email_triage"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        seed_message(
            conn,
            "msg_primary",
            &["INBOX", "UNREAD", "CATEGORY_PERSONAL"],
        );
        seed_message(conn, "msg_updates", &["INBOX", "CATEGORY_UPDATES"]);
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::ImportantEmails)
        .expect("inbox widget");
    assert_eq!(widget.title, "Inbox");
    assert!(widget.metrics.is_empty());
    assert_eq!(metric_value(widget, "Unread"), None);
    assert_eq!(metric_value(widget, "Total"), None);
    assert_eq!(metric_value(widget, "Primary"), None);
    assert_eq!(metric_value(widget, "Updates"), None);
    let action = widget.action.as_ref().expect("inbox action");
    assert_eq!(action.label, "Open inbox");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Inbox));
    assert!(action.target.focus_id.is_none());
    let row_targets = widget
        .items
        .iter()
        .map(|item| item.target.as_ref().expect("row target"))
        .collect::<Vec<_>>();
    assert!(row_targets
        .iter()
        .all(|target| target.view == Some(HomeDashboardTargetView::Inbox)));
    assert_eq!(
        row_targets
            .iter()
            .map(|target| target.focus_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("msg_updates"), Some("msg_primary")]
    );
}

#[test]
fn tasks_widget_counts_due_today_and_targets_tasks() {
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "follow_up_tasks"],
    );
    state.client_id = CLIENT.into();
    let today = crate::slices::accounting::service::today_string(crate::http::now_ms());
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        seed_task(conn, "task_due", "Call supplier", Some(&today));
        seed_task(conn, "task_later", "Check proof", Some("2099-01-01"));
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::OpenTasks)
        .expect("tasks widget");
    assert_eq!(widget.title, "Tasks");
    assert_eq!(metric_value(widget, "Due today"), Some("1"));
    let action = widget.action.as_ref().expect("tasks action");
    assert_eq!(action.label, "View tasks");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Tasks));
    assert!(action.target.focus_id.is_none());
    let row_targets = widget
        .items
        .iter()
        .map(|item| item.target.as_ref().expect("row target"))
        .collect::<Vec<_>>();
    assert!(row_targets
        .iter()
        .all(|target| target.view == Some(HomeDashboardTargetView::Tasks)));
    assert_eq!(
        row_targets
            .iter()
            .map(|target| target.focus_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("task_due"), Some("task_later")]
    );
}

#[test]
fn orders_widget_uses_stage_donut_and_inventory_targets() {
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "inventory"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        crate::slices::inventory::store::upsert_order_snapshots(
            conn,
            CLIENT,
            &[
                order("order_new", "NEW"),
                order("order_picking", "PICKING"),
                order("order_packed", "PACKED"),
            ],
            1_000,
        )
        .expect("orders");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::RecentOrders)
        .expect("orders widget");
    assert_eq!(widget.title, "Orders in production");
    assert_eq!(metric_value(widget, "In production"), Some("3"));
    match widget.chart.as_ref().expect("orders chart") {
        HomeDashboardWidgetChart::Donut { segments } => {
            assert_eq!(segments.iter().find(|s| s.label == "New").unwrap().value, 1);
            assert_eq!(
                segments
                    .iter()
                    .find(|s| s.label == "Picking")
                    .unwrap()
                    .value,
                1
            );
            assert_eq!(
                segments.iter().find(|s| s.label == "Packed").unwrap().value,
                1
            );
        }
        other => panic!("expected donut chart, got {other:?}"),
    }
    let action = widget.action.as_ref().expect("orders action");
    assert_eq!(action.label, "View orders");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Inventory));
    assert!(action.target.focus_id.is_none());
    let blocked = widget
        .metrics
        .iter()
        .find(|metric| metric.label == "Blocked")
        .expect("blocked metric");
    let blocked_target = blocked.target.as_ref().expect("blocked target");
    assert_eq!(
        blocked_target.view,
        Some(HomeDashboardTargetView::Inventory)
    );
    assert_eq!(blocked_target.focus_id.as_deref(), Some("orders:blocked"));
    let row_targets = widget
        .items
        .iter()
        .map(|item| item.target.as_ref().expect("row target"))
        .collect::<Vec<_>>();
    assert!(row_targets
        .iter()
        .all(|target| target.view == Some(HomeDashboardTargetView::Inventory)));
    assert_eq!(
        row_targets
            .iter()
            .map(|target| target.focus_id.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("order:order_picking"),
            Some("order:order_packed"),
            Some("order:order_new"),
        ]
    );
}

#[test]
fn inventory_widget_uses_out_of_stock_metric_top_sku_bar_and_targets() {
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "inventory"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        let mut catalog = material("kit", "Display Kit", 500.0);
        catalog.sale_depletion_policy = Some("COMPONENTS".to_string());
        catalog.replenishment_policy = Some("NONE".to_string());
        catalog.unit_cost_cents = 1_000_000;
        crate::slices::inventory::store::upsert_material_snapshots(
            conn,
            CLIENT,
            &[
                material("m1", "example Blue", 0.0),
                material("m2", "Clear Coat", 8.0),
                catalog,
            ],
            1_000,
        )
        .expect("materials");
        crate::slices::inventory::store::upsert_alert_snapshots(
            conn,
            CLIENT,
            &[
                alert("a1", "m1", "CRITICAL"),
                alert("a-kit", "kit", "CRITICAL"),
            ],
            1_001,
        )
        .expect("alerts");
        crate::slices::inventory::store::upsert_reorder_snapshots(
            conn,
            CLIENT,
            &[suggestion("r1", "PENDING")],
            1_002,
        )
        .expect("reorders");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::InventoryAlerts)
        .expect("inventory widget");
    assert_eq!(widget.title, "Inventory");
    assert!(widget
        .summary
        .as_deref()
        .is_some_and(|summary| summary.starts_with("Stocked report")));
    assert_eq!(metric_value(widget, "Out of stock"), Some("1"));
    assert_eq!(metric_value(widget, "Alerts"), Some("1"));
    assert_eq!(metric_value(widget, "Reorder"), Some("1"));
    match widget.chart.as_ref().expect("inventory chart") {
        HomeDashboardWidgetChart::Bar { items } => {
            assert_eq!(items[0].label, "SKU-m2");
            assert!(items.iter().all(|item| item.label != "SKU-kit"));
            assert!(items[0].value > 0);
            let target = items[0].target.as_ref().expect("top SKU target");
            assert_eq!(target.view, Some(HomeDashboardTargetView::Inventory));
            assert_eq!(target.focus_id.as_deref(), Some("material:m2"));
        }
        other => panic!("expected bar chart, got {other:?}"),
    }
    for (label, focus_id) in [
        ("Critical", "alerts:critical"),
        ("Out of stock", "stock:out"),
        ("Reorder", "reorder"),
    ] {
        let metric = widget
            .metrics
            .iter()
            .find(|metric| metric.label == label)
            .expect("metric");
        let target = metric.target.as_ref().expect("metric target");
        assert_eq!(target.view, Some(HomeDashboardTargetView::Inventory));
        assert_eq!(target.focus_id.as_deref(), Some(focus_id));
    }
    let action = widget.action.as_ref().expect("inventory action");
    assert_eq!(action.label, "Open stocked report");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Inventory));
    assert!(action.target.focus_id.is_none());
    assert!(action.target.external_url.is_none());
    assert_eq!(widget.items.len(), 1, "catalog kit alert excluded");
    let row_target = widget.items[0].target.as_ref().expect("row target");
    assert_eq!(row_target.view, Some(HomeDashboardTargetView::Inventory));
    assert_eq!(row_target.focus_id.as_deref(), Some("alert:a1"));
}

#[test]
fn replacing_preferences_is_revised_and_backfills_new_defaults() {
    let mut persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection();
    let request = HomeDashboardPreferencesUpdateRequest {
        widgets: vec![HomeDashboardWidgetPreference {
            kind: HomeDashboardWidgetKind::FinancialOverview,
            enabled: false,
        }],
        expected_revision: None,
        idempotency_key: "pref-1".to_string(),
        actor_id: None,
    };

    super::store::replace_preference(conn, CLIENT, USER, USER, &request, 1_000).expect("replace");
    let pref = super::store::load_preference(conn, CLIENT, USER).expect("preference");

    assert_eq!(pref.revision, Some(1));
    assert_eq!(
        pref.widgets[0].kind,
        HomeDashboardWidgetKind::FinancialOverview
    );
    assert!(!pref.widgets[0].enabled);
    assert!(
        pref.widgets
            .iter()
            .any(|widget| widget.kind == HomeDashboardWidgetKind::OpenTasks),
        "missing kinds are appended from defaults"
    );
    for kind in [
        HomeDashboardWidgetKind::SalesPipeline,
        HomeDashboardWidgetKind::SystemHealth,
        HomeDashboardWidgetKind::HelpShortcuts,
        HomeDashboardWidgetKind::SystemDiagnostics,
    ] {
        assert!(
            pref.widgets.iter().any(|widget| widget.kind == kind),
            "new default kind {kind:?} is appended for existing preferences"
        );
    }
}

#[test]
fn legacy_financial_preferences_merge_without_hiding_enabled_card() {
    let persistence = crate::persistence::Persistence::open_in_memory().expect("db");
    let conn = persistence.connection_ref();
    super::store::insert_preference_json_for_test(
        conn,
        CLIENT,
        USER,
        r#"[
          {"kind":"financials","enabled":false},
          {"kind":"outstanding_invoices","enabled":true},
          {"kind":"open_tasks","enabled":true}
        ]"#,
    )
    .expect("legacy preference");

    let pref = super::store::load_preference(conn, CLIENT, USER).expect("preference");

    let merged: Vec<_> = pref
        .widgets
        .iter()
        .filter(|widget| widget.kind == HomeDashboardWidgetKind::FinancialOverview)
        .collect();
    assert_eq!(merged.len(), 1);
    assert!(merged[0].enabled);
}

#[test]
fn financial_overview_merges_ar_ap_cash_revenue_and_sparkline() {
    let today = crate::slices::accounting::service::today_string(crate::http::now_ms());
    let current_month_start = format!("{}-01", &today[..7]);
    let (recent_start, recent_end) =
        crate::slices::accounting::service::last_n_days_window(&today, 2)
            .expect("current two-day window");
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "accounting"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        crate::slices::accounting::store::upsert_invoice_snapshots(
            conn,
            CLIENT,
            &[InvoiceRecord {
                invoice_id: "inv_1".to_string(),
                doc_number: Some("1001".to_string()),
                customer_id: None,
                customer_name: Some("Acme".to_string()),
                txn_date: Some("2026-06-01".to_string()),
                due_date: Some("2099-06-30".to_string()),
                total_amt_cents: 10_000,
                balance_cents: 4_000,
                voided: false,
                updated_at: "2026-06-01T00:00:00Z".to_string(),
            }],
            1_000,
        )
        .expect("invoice cache");
        crate::slices::accounting::store::upsert_bill_snapshots(
            conn,
            CLIENT,
            &[BillRecord {
                bill_id: "bill_1".to_string(),
                vendor_id: Some("v1".to_string()),
                vendor_name: Some("Champion".to_string()),
                txn_date: Some("2026-06-01".to_string()),
                due_date: Some("2099-06-15".to_string()),
                total_amt_cents: 7_500,
                balance_cents: 2_500,
                voided: false,
                updated_at: "2026-06-01T00:00:00Z".to_string(),
            }],
            1_001,
        )
        .expect("bill cache");
        let mut bill_cursor = crate::slices::accounting::store::QboSyncCursor::initial();
        bill_cursor.backfill_complete = true;
        crate::slices::accounting::store::put_cursor(
            conn,
            CLIENT,
            crate::slices::accounting::store::ENTITY_BILL,
            &bill_cursor,
            1_001,
        )
        .expect("bill cursor");
        crate::slices::accounting::store::upsert_balance_sheet_snapshot(
            conn,
            CLIENT,
            "2026-06-10",
            BalanceSheetSummary {
                cash_on_hand_cents: 90_000,
            },
            1_002,
        )
        .expect("balance sheet");
        for (date, cents) in [(recent_start, 1_000), (recent_end, 2_000)] {
            crate::slices::accounting::store::upsert_pnl_snapshot(
                conn,
                CLIENT,
                &crate::slices::accounting::store::PnlSnapshotRow {
                    period_kind: "day".to_string(),
                    period_start: date.clone(),
                    period_end: date,
                    total_income_cents: cents,
                    total_cogs_cents: 0,
                    gross_profit_cents: cents,
                    is_complete: true,
                },
                1_003,
            )
            .expect("day pnl");
        }
        crate::slices::accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &crate::slices::accounting::store::PnlSnapshotRow {
                period_kind: "month".to_string(),
                period_start: current_month_start,
                period_end: today,
                total_income_cents: 30_000,
                total_cogs_cents: 12_000,
                gross_profit_cents: 18_000,
                is_complete: false,
            },
            1_004,
        )
        .expect("month pnl");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::FinancialOverview)
        .expect("financial overview widget");
    assert_eq!(widget.title, "Financial overview");
    assert_eq!(metric_value(widget, "Accounts receivable"), Some("$40.00"));
    assert_eq!(metric_value(widget, "Accounts payable"), Some("$25.00"));
    assert_eq!(metric_value(widget, "Cash on hand"), Some("$900.00"));
    assert_eq!(
        metric_value(widget, "Revenue · month to date"),
        Some("$300.00")
    );
    let invoices_item = widget
        .items
        .iter()
        .find(|item| item.label == "Open invoices")
        .expect("open invoices item");
    let invoices_target = invoices_item.target.as_ref().expect("invoices target");
    assert_eq!(
        invoices_target.view,
        Some(HomeDashboardTargetView::Accounting)
    );
    assert_eq!(invoices_target.focus_id.as_deref(), Some("invoices"));
    let bills_item = widget
        .items
        .iter()
        .find(|item| item.label == "Open bills")
        .expect("open bills item");
    assert!(bills_item.target.is_none());
    let action = widget.action.as_ref().expect("accounting action");
    assert_eq!(action.label, "Open accounting");
    assert_eq!(
        action.target.view,
        Some(HomeDashboardTargetView::Accounting)
    );
    assert!(action.target.focus_id.is_none());
    match widget.chart.as_ref().expect("sparkline") {
        HomeDashboardWidgetChart::Sparkline { points } => {
            assert_eq!(points.len(), 2);
            assert_eq!(points[0].value, 10);
            assert_eq!(points[1].value, 20);
        }
        other => panic!("expected sparkline chart, got {other:?}"),
    }
}

#[test]
fn financial_overview_marks_unsynced_ap_and_cash_as_unknown() {
    let today = crate::slices::accounting::service::today_string(crate::http::now_ms());
    let current_month_start = format!("{}-01", &today[..7]);
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "accounting"]);
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        let conn = persistence.connection();
        crate::slices::accounting::store::upsert_invoice_snapshots(
            conn,
            CLIENT,
            &[InvoiceRecord {
                invoice_id: "inv_1".to_string(),
                doc_number: Some("1001".to_string()),
                customer_id: None,
                customer_name: Some("Acme".to_string()),
                txn_date: Some("2026-06-01".to_string()),
                due_date: Some("2099-06-30".to_string()),
                total_amt_cents: 10_000,
                balance_cents: 4_000,
                voided: false,
                updated_at: "2026-06-01T00:00:00Z".to_string(),
            }],
            1_000,
        )
        .expect("invoice cache");
        crate::slices::accounting::store::upsert_pnl_snapshot(
            conn,
            CLIENT,
            &crate::slices::accounting::store::PnlSnapshotRow {
                period_kind: "month".to_string(),
                period_start: current_month_start,
                period_end: today,
                total_income_cents: 30_000,
                total_cogs_cents: 12_000,
                gross_profit_cents: 18_000,
                is_complete: false,
            },
            1_001,
        )
        .expect("month pnl");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    let widget = response
        .widgets
        .iter()
        .find(|widget| widget.kind == HomeDashboardWidgetKind::FinancialOverview)
        .expect("financial overview widget");
    assert_eq!(metric_value(widget, "Accounts receivable"), Some("$40.00"));
    assert_eq!(metric_value(widget, "Accounts payable"), Some("—"));
    assert_eq!(metric_value(widget, "Cash on hand"), Some("—"));
    assert_eq!(
        metric_value(widget, "Revenue · month to date"),
        Some("$300.00")
    );
    let bills = widget
        .items
        .iter()
        .find(|item| item.label == "Open bills")
        .expect("open bills item");
    assert_eq!(bills.detail.as_deref(), Some("Pending sync"));
}

#[test]
fn financial_widgets_are_not_available_when_policy_blocks_scope() {
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "accounting", "follow_up_tasks"],
    );
    state.client_id = CLIENT.into();
    state.accounting_visibility_policy = crate::overlay::AccountingVisibilityPolicy::AuthorizerOnly;
    {
        let mut persistence = state.persistence();
        crate::slices::accounting::store::upsert_invoice_snapshots(
            persistence.connection(),
            CLIENT,
            &[bos_integrations::accounting_read::InvoiceRecord {
                invoice_id: "inv_1".to_string(),
                doc_number: Some("1001".to_string()),
                customer_id: None,
                customer_name: Some("Acme".to_string()),
                txn_date: Some("2026-06-01".to_string()),
                due_date: Some("2026-06-30".to_string()),
                total_amt_cents: 10_000,
                balance_cents: 10_000,
                voided: false,
                updated_at: "2026-06-01T00:00:00Z".to_string(),
            }],
            1_000,
        )
        .expect("invoice cache");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(!response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::FinancialOverview));
    assert!(!response
        .widgets
        .iter()
        .any(|widget| widget.kind == HomeDashboardWidgetKind::FinancialOverview));
}

#[test]
fn diagnostics_widget_is_unavailable_when_debug_env_is_off() {
    let _guard = EnvGuard::unset("BOS_DEBUG_ENABLED");
    let mut state =
        crate::http::test_support::test_state_configured(None, &["home_dashboard", "debug"]);
    state.client_id = CLIENT.into();
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(!response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemDiagnostics));
    assert!(!response
        .widgets
        .iter()
        .any(|widget| widget.kind == HomeDashboardWidgetKind::SystemDiagnostics));
}

#[test]
fn diagnostics_widget_renders_last_and_targets_debug_when_enabled() {
    let _guard = EnvGuard::set("BOS_DEBUG_ENABLED", "1");
    let mut state = crate::http::test_support::test_state_configured(
        None,
        &["home_dashboard", "debug", "follow_up_tasks"],
    );
    state.client_id = CLIENT.into();
    {
        let mut persistence = state.persistence();
        super::store::replace_preference(
            persistence.connection(),
            CLIENT,
            USER,
            USER,
            &HomeDashboardPreferencesUpdateRequest {
                widgets: vec![
                    HomeDashboardWidgetPreference {
                        kind: HomeDashboardWidgetKind::OpenTasks,
                        enabled: true,
                    },
                    HomeDashboardWidgetPreference {
                        kind: HomeDashboardWidgetKind::SystemDiagnostics,
                        enabled: true,
                    },
                ],
                expected_revision: None,
                idempotency_key: "pref-debug-order".to_string(),
                actor_id: None,
            },
            1_000,
        )
        .expect("replace preference");
        crate::slices::debug::store::insert_panic_diagnostic(
            persistence.connection(),
            &crate::slices::debug::store::PanicDiagnosticInsert {
                diagnostic_id: "panic_1",
                client_id: CLIENT,
                message: "boom",
                location: Some("home_dashboard_test"),
                backtrace: "stack",
                thread_name: Some("test"),
                occurred_at_ms: 2_000,
            },
        )
        .expect("panic diagnostic");
    }
    let scope = crate::http::OperatorScope::User(USER.to_string());

    let response = super::service::dashboard_response(&state, &scope, USER).expect("dashboard");

    assert!(response
        .available_widgets
        .contains(&HomeDashboardWidgetKind::SystemDiagnostics));
    assert_eq!(
        response.available_widgets.last().copied(),
        Some(HomeDashboardWidgetKind::SystemDiagnostics)
    );
    let widget = response.widgets.last().expect("diagnostics widget");
    assert_eq!(widget.kind, HomeDashboardWidgetKind::SystemDiagnostics);
    assert_eq!(widget.title, "System diagnostics");
    assert_eq!(widget.metrics[0].label, "Errors");
    assert_eq!(widget.metrics[0].value.as_deref(), Some("1"));
    let action = widget.action.as_ref().expect("debug action");
    assert_eq!(action.label, "Open Debug");
    assert_eq!(action.target.view, Some(HomeDashboardTargetView::Debug));
    assert!(action.target.focus_id.is_none());
    let target = widget.items[0].target.as_ref().expect("row target");
    assert_eq!(target.view, Some(HomeDashboardTargetView::Debug));
    assert_eq!(target.focus_id.as_deref(), Some("panic:panic_1"));
}

fn metric_value<'a>(
    widget: &'a bos_contracts::home_dashboard::HomeDashboardWidget,
    label: &str,
) -> Option<&'a str> {
    widget
        .metrics
        .iter()
        .find(|metric| metric.label == label)
        .and_then(|metric| metric.value.as_deref())
}

fn item_detail<'a>(
    widget: &'a bos_contracts::home_dashboard::HomeDashboardWidget,
    label: &str,
) -> Option<(&'a str, &'a str)> {
    widget
        .items
        .iter()
        .find(|item| item.label == label)
        .and_then(|item| Some((item.detail.as_deref()?, item.tone.as_deref()?)))
}

fn sample_hubspot_mapping() -> HubSpotDealPipelineMapping {
    HubSpotDealPipelineMapping {
        pipeline_id: "default".to_string(),
        stage_mappings: vec![
            HubSpotDealStageMapping {
                stage_id: "appointmentscheduled".to_string(),
                label: Some("Appointment scheduled".to_string()),
                status: HubSpotDealMappedStatus::Open,
            },
            HubSpotDealStageMapping {
                stage_id: "closedwon".to_string(),
                label: Some("Closed won".to_string()),
                status: HubSpotDealMappedStatus::Won,
            },
            HubSpotDealStageMapping {
                stage_id: "closedlost".to_string(),
                label: Some("Closed lost".to_string()),
                status: HubSpotDealMappedStatus::Lost,
            },
        ],
        started_date_property: "createdate".to_string(),
        closed_date_property: "closedate".to_string(),
    }
}

fn seed_message(conn: &mut rusqlite::Connection, id: &str, labels: &[&str]) {
    crate::slices::email_triage::store::record_inbound_message(
        conn,
        CLIENT,
        &crate::slices::email_triage::store::InboundMessageRecord {
            source_key: id.to_string(),
            message_id: id.to_string(),
            thread_id: Some(format!("thread-{id}")),
            internal_date_ms: Some(1_000),
            from_addr: Some("customer@example.test".to_string()),
            to_addr: Some("ops@example.test".to_string()),
            subject: Some(format!("Message {id}")),
            body_excerpt: "Body".to_string(),
            body_full: "Body".to_string(),
            headers: Vec::new(),
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            resolved_category: bos_contracts::email_triage::FALLBACK_CATEGORY_ID.to_string(),
            matched_rule_id: None,
            ingested_at_ms: 1_000,
            ai_triage_status: None,
            ai_triage_rationale: None,
            attachments: Vec::new(),
            source_user_id: Some(USER.to_string()),
        },
    )
    .expect("seed message");
}

fn seed_task(conn: &mut rusqlite::Connection, task_id: &str, title: &str, due_date: Option<&str>) {
    let tx = conn.transaction().expect("tx");
    crate::slices::follow_up_tasks::store::insert_task_within(
        &tx,
        CLIENT,
        &bos_contracts::follow_up_tasks::TaskRecord {
            task_id: task_id.to_string(),
            title: title.to_string(),
            due_date: due_date.map(str::to_string),
            context: String::new(),
            source_kind: "manual".to_string(),
            source_ref: task_id.to_string(),
            source_user_id: Some(USER.to_string()),
            source_item_id: None,
            status: bos_contracts::follow_up_tasks::TaskStatus::Open,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        },
        1_000,
    )
    .expect("seed task");
    tx.commit().expect("commit");
}

fn seed_work_item(conn: &mut rusqlite::Connection, item_id: &str, title: &str, summary: &str) {
    crate::slices::work_queue::store::insert_item(
        conn,
        CLIENT,
        &bos_contracts::work_queue::WorkItem {
            item_id: item_id.to_string(),
            source_kind: "note".to_string(),
            source_ref: item_id.to_string(),
            category_id: bos_contracts::email_triage::FALLBACK_CATEGORY_ID.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            packet_kinds: vec!["crm_activity".to_string()],
            status: bos_contracts::work_queue::WorkItemStatus::Open,
            accept_actor: None,
            ai_suggested: false,
            rationale: String::new(),
            produce_guidance: String::new(),
            source_user_id: Some(USER.to_string()),
            assignee_user_id: None,
            visible_to_user_ids: vec![USER.to_string()],
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        },
    )
    .expect("seed work item");
}

fn material(id: &str, name: &str, quantity: f64) -> SfMaterialRecord {
    SfMaterialRecord {
        material_id: id.to_string(),
        name: name.to_string(),
        sku: Some(format!("SKU-{id}")),
        category: Some("LIQUID".to_string()),
        current_quantity: quantity,
        reserved_qty: None,
        incoming_qty: None,
        unit: Some("gal".to_string()),
        warning_threshold: Some(20.0),
        critical_threshold: Some(5.0),
        threshold_type: Some("ABSOLUTE".to_string()),
        unit_cost_cents: 5_000,
        lead_time_days: Some(14),
        vendor_name: Some("Champion".to_string()),
        is_active: true,
        is_purchasable: Some(true),
        replenishment_policy: Some("PURCHASE".to_string()),
        sale_depletion_policy: Some("STOCK".to_string()),
        updated_at: Some("2026-06-09T00:00:00Z".to_string()),
    }
}

fn alert(id: &str, material_id: &str, severity: &str) -> SfAlertRecord {
    SfAlertRecord {
        alert_id: id.to_string(),
        material_id: Some(material_id.to_string()),
        material_name: Some("example Blue".to_string()),
        material_sku: Some(format!("SKU-{material_id}")),
        severity: severity.to_string(),
        status: "ACTIVE".to_string(),
        current_quantity: Some(0.0),
        threshold_value: Some(5.0),
        percentage_remaining: Some(0.0),
        message: Some("low stock".to_string()),
        created_at: Some("2026-06-09T00:00:00Z".to_string()),
    }
}

fn suggestion(id: &str, status: &str) -> SfReorderSuggestionRecord {
    SfReorderSuggestionRecord {
        suggestion_id: id.to_string(),
        material_id: Some("m1".to_string()),
        material_name: Some("example Blue".to_string()),
        material_sku: Some("SKU-m1".to_string()),
        vendor_name: Some("Champion".to_string()),
        urgency: "HIGH".to_string(),
        status: status.to_string(),
        current_quantity: Some(0.0),
        suggested_quantity: Some(50.0),
        unit: Some("gal".to_string()),
        estimated_cost_cents: 250_000,
        days_until_stockout: Some(0.0),
        lead_time_days: Some(14),
        reasoning: Some("burn rate".to_string()),
        created_at: None,
    }
}

fn order(id: &str, status: &str) -> SfOrderCardRecord {
    SfOrderCardRecord {
        order_id: id.to_string(),
        order_number: format!("#{id}"),
        external_order_id: Some(format!("shopify-{id}")),
        platform: Some("shopify".to_string()),
        board_status: status.to_string(),
        raw_status: None,
        customer_name: Some("Dana".to_string()),
        customer_email: Some("dana@example.test".to_string()),
        total_amount_cents: 21_999,
        currency: Some("USD".to_string()),
        order_date: Some("2026-06-09".to_string()),
        processed_at: None,
        item_count: 2,
        unit_count: 3,
        mapped_line_count: 2,
        line_material_ids: vec!["m1".to_string()],
        line_identity_complete: true,
        carrier: None,
        tracking_number: None,
        shipment_refs: None,
        shipment_id: None,
        ship_date: None,
        photo_count: 0,
        pack_station_container_id: None,
        needs_mapping: false,
        blocked: false,
        deducted: false,
        deduction_failed: false,
        label_needed: false,
        packed_missing_photo: false,
        exception: false,
        depletion_total: 0,
        depletion_applied: 0,
        depletion_failed: 0,
        depletion_reversed: 0,
        blocked_reasons_json: "[]".to_string(),
    }
}
