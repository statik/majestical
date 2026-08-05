//! The desktop head: a Tauri shell over `majestical_services`. Commands are
//! thin wrappers returning services outcome structs as-is (parity by
//! construction, same rule as `maj mcp`).
pub mod commands;
pub mod config;
pub mod thumb_protocol;

/// Builds and runs the Tauri app.
///
/// # Panics
/// Panics only if the Tauri runtime itself fails to start — there is no
/// meaningful recovery for a desktop app that cannot open a window.
pub fn run() {
    #[expect(
        clippy::expect_used,
        reason = "no recovery exists if the shell cannot start"
    )]
    #[expect(
        clippy::exit,
        reason = "tauri::generate_context! expands to a process::exit for a malformed \
                  context; the call is inside the macro, not ours to restructure"
    )]
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // ARMING THE UPDATER takes three edits, listed in docs/RELEASING.md.
        // Two of them must land in the same commit: the `plugins.updater`
        // block in tauri.conf.json, and
        //
        //     .plugin(tauri_plugin_updater::Builder::new().build())
        //
        // here. Neither works alone. The updater's `Config::pubkey` has no
        // serde default, so a registered plugin with no config block fails to
        // deserialize `plugins.updater` from `null`, which makes plugin
        // initialization — and therefore `run` below — return an error the
        // app cannot start through. Verified by running the built binary: it
        // panicked with `PluginInitialization("updater", ...)` before opening
        // a window. Until the signing keypair exists the app ships with
        // neither, and `lib/updater.ts` treats the resulting "no such
        // command" as one more reason there is no update to offer.
        .plugin(tauri_plugin_process::init())
        .manage(commands::AppState(std::sync::RwLock::new(None)))
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
            commands::initialize_catalog,
            commands::use_existing_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running majestical desktop");
}
