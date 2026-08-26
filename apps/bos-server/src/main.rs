use std::path::PathBuf;

use bos_app::env_registry;
use bos_app::http::{build_router, AppState};
use bos_app::persistence::PersistencePool;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("repo-map") => print!("{}", bos_app::repo_map_markdown()),
        Some("slice-ids-json") => print!("{}", bos_app::slice_ids_json()),
        Some(other) => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
        None => serve(),
    }
}

fn serve() {
    // Without a subscriber every tracing::info!/warn! in the workspace is
    // silently dropped (learned the hard way: the pump ran blind for days).
    let filter = bos_app::env_registry::string(&bos_app::env_registry::BOS_LOG_LEVEL)
        .unwrap_or_else(|| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .init();

    let state_dir = env_registry::string(&env_registry::BOS_STATE_DIR)
        .map(PathBuf::from)
        .expect("BOS_STATE_DIR has a default");
    let bind = env_registry::string(&env_registry::BOS_SERVER_BIND)
        .expect("BOS_SERVER_BIND has a default");

    let persistence = match PersistencePool::open_at(&state_dir) {
        Ok(persistence) => persistence,
        Err(err) => {
            eprintln!(
                "failed to open persistence at {}: {err}",
                state_dir.display()
            );
            std::process::exit(1);
        }
    };

    // Client overlay: identity + enabled slices + seeds. A configured-but-
    // broken overlay is fatal — never run a client instance on the wrong
    // profile.
    let overlay = match bos_app::overlay::load_from_env() {
        Ok(overlay) => overlay,
        Err(err) => {
            eprintln!("client overlay failed to load: {err}");
            std::process::exit(1);
        }
    };
    if let Some(overlay) = overlay.as_ref() {
        tracing::info!(
            client_id = %overlay.identity.client_id,
            display_name = %overlay.identity.display_name,
            enabled_slices = ?overlay.slices.enabled,
            "client overlay loaded"
        );
        let mut conn = persistence.get().unwrap_or_else(|err| {
            eprintln!("pool get failed: {err}");
            std::process::exit(1);
        });
        if let Err(err) =
            bos_app::overlay::apply_seeds(conn.connection(), overlay, bos_app::http::now_ms())
        {
            eprintln!("client overlay seeds failed to apply: {err}");
            std::process::exit(1);
        }
    }

    let state = AppState::with_overlay(persistence, overlay.as_ref());
    bos_app::http::install_panic_hook(state.clone());
    bos_app::slices::email_triage::worker::spawn(state.clone());
    bos_app::slices::inventory::worker::spawn(state.clone());
    bos_app::slices::accounting::worker::spawn(state.clone());
    bos_app::slices::crm_cache::worker::spawn(state.clone());
    bos_app::slices::data_retention::worker::spawn(state.clone());
    bos_app::slices::call_inputs::worker::spawn(state.clone());
    bos_app::slices::lead_discovery::worker::spawn(state.clone());
    bos_app::slices::drive_corpus::worker::spawn(state.clone());
    bos_app::slices::search_console::worker::spawn(state.clone());
    bos_app::slices::claim_drafts::worker::spawn(state.clone());
    bos_app::slices::enrichment::worker::spawn(state.clone());
    bos_app::slices::owner_reports::worker::spawn(state.clone());
    bos_app::slices::shopify_sales::worker::spawn(state.clone());
    bos_app::outbox::spawn_delivery_pump(state.clone());
    bos_app::produce::spawn_auto_produce_pump(state.clone());
    let app = build_router(state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .unwrap_or_else(|err| panic!("bind {bind}: {err}"));
        println!("bos-server listening on {bind}");
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .expect("server run");
    });
}
