//! The desktop head: a Tauri shell over `majestical_services`. Commands are
//! thin wrappers returning services outcome structs as-is (parity by
//! construction, same rule as `maj mcp`).
pub mod commands;
pub mod config;
pub mod ingest;
pub mod thumb_protocol;

/// Builds and runs the Tauri app.
///
/// # Panics
/// Panics only if the Tauri runtime itself fails to start — there is no
/// meaningful recovery for a desktop app that cannot open a window.
pub fn run() {
    // `mut` is only exercised by the `#[cfg(debug_assertions)]` block below;
    // a release build never reassigns `builder`, so only there does the
    // compiler have grounds to flag it as unused.
    #[cfg_attr(
        not(debug_assertions),
        expect(
            unused_mut,
            reason = "reassigned only by the debug-only plugin block below"
        )
    )]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Paired with the `plugins.updater` block in tauri.conf.json, and it
        // must stay paired: the updater's `Config::pubkey` has no serde
        // default, so registering the plugin with no config block fails to
        // deserialize `plugins.updater` from `null`, which makes plugin
        // initialization — and therefore `run` below — return an error the
        // app cannot start through. Removing one without the other does not
        // degrade the update check, it stops the app from opening a window.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    // e2e harness only: both crates are plain `[dependencies]` (Cargo has no
    // debug-only dependency section), but gating *registration* behind
    // `debug_assertions` means a release binary never starts the embedded
    // WebDriver server or the execute/mock bridge — no listening server
    // ships in the product build.
    #[cfg(debug_assertions)]
    {
        builder = builder
            .plugin(tauri_plugin_wdio::init())
            .plugin(tauri_plugin_wdio_webdriver::init());
    }

    #[expect(
        clippy::expect_used,
        reason = "no recovery exists if the shell cannot start"
    )]
    #[expect(
        clippy::exit,
        reason = "tauri::generate_context! expands to a process::exit for a malformed \
                  context; the call is inside the macro, not ours to restructure"
    )]
    builder
        .manage(commands::AppState(std::sync::RwLock::new(None)))
        // The one in-flight ingest run. Managed state, not webview state:
        // the run is a plain OS thread that keeps copying across a reload,
        // and `ingest_state` is how the surface finds it again.
        .manage(ingest::IngestState(std::sync::RwLock::new(None)))
        .setup(|app| Ok(commands::restore_persisted_catalog(app.handle())?))
        .register_uri_scheme_protocol("thumb", |ctx, request| {
            thumb_protocol::respond(ctx.app_handle(), &request.uri().to_string())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_status,
            commands::search_assets,
            commands::get_asset,
            commands::list_volumes,
            commands::list_saved_searches,
            commands::run_saved_search,
            commands::browse_tree,
            commands::browse_list,
            commands::list_tags,
            commands::rename_tag,
            commands::merge_tags,
            commands::assign_tags,
            commands::file_assets,
            commands::list_para,
            commands::add_para_node,
            commands::rename_para_node,
            commands::archive_node,
            commands::list_mounted_roots,
            commands::plan_ingest,
            commands::start_ingest,
            commands::cancel_ingest,
            commands::ingest_state,
            commands::list_unfinished_ingests,
            commands::initialize_catalog,
            commands::use_existing_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running majestical desktop");
}
