//! The desktop head: a Tauri shell over `majestical_services`. Commands are
//! thin wrappers returning services outcome structs as-is (parity by
//! construction, same rule as `maj mcp`).

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
        .run(tauri::generate_context!())
        .expect("error while running majestical desktop");
}
