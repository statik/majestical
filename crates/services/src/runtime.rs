//! The one runtime-safety helper both async heads need: `maj mcp` (whose
//! `#[tool]` handlers run on its own tokio runtime) and the desktop app
//! (whose `#[tauri::command]`s run on Tauri's). Lives here rather than in
//! either head so neither carries a private copy of the rule.

/// Runs `f` on a plain, tokio-unaffiliated OS thread. A caller's own thread
/// is already inside a tokio runtime (`maj mcp`'s server runtime, or the
/// desktop app's Tauri runtime), but some `majestical_services`/
/// `majestical_index` calls open a Lance vector store
/// (`VectorStore`/`TextVectorStore::open`/`open_existing`) that builds and
/// enters ANOTHER tokio runtime internally — and entering any runtime while
/// the current thread already has one active panics ("Cannot start a runtime
/// from within a runtime"), regardless of whether it's the same `Runtime`
/// value. Two call paths hit this today: `index::run`'s real pass (embed/
/// keyframe/transcript-embed executors), and `search::search`'s semantic
/// layer (it opens the store to nearest-neighbor-search it whenever a query
/// has terms and a describer/encoder model is installed) — a real user with
/// a model fetched and an index built would panic on their first MCP or GUI
/// search without this. A genuinely separate `std::thread` (never
/// `spawn_blocking`, whose task still runs inside the runtime's own worker
/// context) has no such context to collide with.
///
/// # Errors
/// Returns whatever `f` returns.
///
/// # Panics
/// Resumes a panic from `f` on the calling thread rather than swallowing
/// it, so the spawned thread's failure surfaces exactly as an in-line call's
/// would.
pub fn run_off_tokio_runtime<T: Send>(
    f: impl FnOnce() -> anyhow::Result<T> + Send,
) -> anyhow::Result<T> {
    std::thread::scope(|scope| match scope.spawn(f).join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    })
}
