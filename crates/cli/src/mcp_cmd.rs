//! `maj mcp`: serves the catalog to MCP clients over stdio (newline-
//! delimited JSON-RPC 2.0). Every tool handler is a thin wrapper over
//! `majestical_services` — no operation logic lives here, matching every
//! other head in this repo. Unlike the CLI's own subcommands, `maj mcp`
//! opens (or re-opens) the catalog fresh on EVERY tool call rather than
//! once at startup: a long-lived MCP session must see catalog changes made
//! by other processes (another `maj` invocation, a teammate's sync) between
//! calls, the same reasoning documented on `Cmd::Verify`/`Cmd::Model` in
//! main.rs for why those two don't open a catalog handle up front either.
//!
//! Wire contract: a tool's successful result serializes the corresponding
//! `majestical_services` outcome struct DIRECTLY as `structuredContent` —
//! no separate MCP-specific rendering layer. A `ServiceError` becomes a
//! tool-level error (`CallToolResult::error`, `isError: true`) carrying the
//! error's full Display chain (`{:#}`), which is where a `ServiceError`'s
//! remedy text (e.g. "run `maj catalog init` first") already lives.
use majestical_services::app::FsApp;
use rmcp::ServerHandler;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

/// `search_assets`'s `--limit` default, mirroring `maj search`'s own.
fn default_search_limit() -> usize {
    50
}

/// Params for `search_assets`, mirroring
/// `majestical_services::search::SearchRequest` minus `save` — this read
/// tool never writes a saved search.
#[derive(Debug, Deserialize, JsonSchema)]
struct SearchAssetsArgs {
    /// Bare terms match names; `key:value` tokens are hard filters (tag,
    /// vol or volume, para, kind, online, before, after, in). Omit when
    /// passing `saved`.
    #[serde(default)]
    query: Option<String>,
    /// Max results (default 50).
    #[serde(default = "default_search_limit")]
    limit: usize,
    /// Run a previously saved search by name instead of `query`.
    #[serde(default)]
    saved: Option<String>,
}

/// Params for `get_asset`.
#[derive(Debug, Deserialize, JsonSchema)]
struct GetAssetArgs {
    /// Asset id, e.g. `xxh3:0123...`. Asset ids (and every timestamp this
    /// server returns) are stable across catalog operations, so a value
    /// returned by `search_assets`/`run_saved_search` can be passed straight
    /// into `get_asset` (or a future mutating tool) without re-resolving.
    asset_id: String,
}

/// Params for `run_saved_search`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunSavedSearchArgs {
    /// Saved search name (see `list_saved_searches`).
    name: String,
    /// Max results (default 50).
    #[serde(default = "default_search_limit")]
    limit: usize,
}

/// `list_saved_searches`'s structured result — the service verb returns a
/// bare `Vec<SavedSearch>`, so this names the wire object's one field.
#[derive(Serialize)]
struct SavedSearchesResult {
    saved: Vec<majestical_services::search::SavedSearch>,
}

/// `maj mcp`'s server: everything it needs to open a fresh `FsApp`/catalog
/// handle per tool call. Holds no catalog state itself.
#[derive(Clone)]
struct MajServer {
    catalog: PathBuf,
    machine_id: String,
    author: String,
}

/// Builds a tool-level error result (`isError: true`) carrying `err`'s full
/// Display chain — for a [`majestical_services::error::ServiceError`],
/// that's where the remedy text (e.g. "run `maj catalog init` first")
/// already lives, so no MCP-specific remedy text is invented here.
fn tool_error(err: impl Into<anyhow::Error>) -> CallToolResult {
    let err = err.into();
    CallToolResult::error(vec![ContentBlock::text(format!("{err:#}"))])
}

/// Builds a successful structured-content result from any
/// `majestical_services` outcome struct — the wire-contract decision this
/// whole module follows: serialize the outcome directly, no MCP-specific
/// shape. Serialization failure (never expected for these plain-data
/// outcome structs, but never `unwrap`ped either) becomes a tool error
/// rather than a panic.
fn structured_ok<T: Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(v) => CallToolResult::structured(v),
        Err(err) => tool_error(err),
    }
}

/// A stub tool's uniform response: registers the tool name in the roster
/// now (so the roster is stable) without implementing its body — that
/// arrives with Task 8's mutating tools and confirm gate.
fn not_yet_implemented(tool: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "{tool}: not yet implemented over MCP — use the maj CLI"
    ))])
}

#[tool_router]
impl MajServer {
    /// Opens this call's `FsApp` fresh — never cached across calls (see the
    /// module doc). On failure (most commonly no catalog at this path —
    /// `ServiceError::NoCatalog`'s message names the `maj catalog init`
    /// remedy), returns the tool error the caller should propagate.
    fn open_app(&self) -> Result<FsApp, CallToolResult> {
        FsApp::open(&self.catalog, &self.machine_id, &self.author).map_err(tool_error)
    }

    /// Guards a tool that never opens `FsApp` — `list_sync_locations` and
    /// `get_describer` read state-dir-relative config directly, without
    /// ever touching the event log — against a missing catalog, via the
    /// same `majestical_services::catalog::ensure_catalog` predicate
    /// `FsApp::open` and `sync`'s own guard use, so it gives the identical
    /// `ServiceError::NoCatalog` remedy every `open_app`-based tool already
    /// gives on a missing catalog. Without this guard, resolving the state
    /// dir for a nonexistent catalog root fails first with a raw
    /// `canonicalize` OS error instead of the "run `maj catalog init`
    /// first" remedy.
    fn ensure_catalog(&self) -> Result<(), CallToolResult> {
        majestical_services::catalog::ensure_catalog(&self.catalog).map_err(tool_error)
    }

    /// Search the catalog: bare terms match names; `key:value` tokens are
    /// hard filters. Asset ids and timestamps in `results` are stable and
    /// safe to pass into `get_asset` or a later mutating-tool call.
    #[tool]
    fn search_assets(&self, Parameters(args): Parameters<SearchAssetsArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        let req = majestical_services::search::SearchRequest {
            query: args.query,
            limit: args.limit,
            saved: args.saved,
            save: None,
        };
        match majestical_services::search::search(&mut app, &self.catalog, &req) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// Fetches everything the catalog knows about one asset: instances,
    /// tags, PARA assignment, metadata fields, and verification history.
    /// `verifications` is the FULL recorded history for the asset (every
    /// check ever recorded), not just the latest per volume. `para`, when
    /// set, may name an archived node — archived PARA nodes render exactly
    /// like live ones here. An unknown asset id is a value, not an error:
    /// returns `{"found": false}`; a known asset returns
    /// `{"found": true, "asset": {...}}`.
    #[tool]
    fn get_asset(&self, Parameters(args): Parameters<GetAssetArgs>) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::catalog::get_asset(&app, &self.catalog, &args.asset_id) {
            Ok(Some(detail)) => match serde_json::to_value(&detail) {
                Ok(asset) => CallToolResult::structured(json!({ "found": true, "asset": asset })),
                Err(err) => tool_error(err),
            },
            Ok(None) => CallToolResult::structured(json!({ "found": false })),
            Err(err) => tool_error(err),
        }
    }

    /// Lists every volume the catalog has ever seen, with per-volume asset
    /// counts and online status.
    #[tool]
    fn list_volumes(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::volumes::volumes_list(&app, &self.catalog) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// Lists every saved search (name and query text).
    #[tool]
    fn list_saved_searches(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::search::searches_list(&app) {
            Ok(saved) => structured_ok(&SavedSearchesResult { saved }),
            Err(err) => tool_error(err),
        }
    }

    /// Runs a previously saved search by name (see `list_saved_searches`).
    /// Same result shape as `search_assets`.
    #[tool]
    fn run_saved_search(&self, Parameters(args): Parameters<RunSavedSearchArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        let req = majestical_services::search::SearchRequest {
            query: None,
            limit: args.limit,
            saved: Some(args.name),
            save: None,
        };
        match majestical_services::search::search(&mut app, &self.catalog, &req) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// For every configured sync location, reports reachability plus what a
    /// push would send (`ahead`) and a pull would fetch (`behind`) — walked
    /// fresh from real files; never executes a transfer.
    #[tool]
    fn sync_status(&self) -> CallToolResult {
        match majestical_services::sync::status(&self.catalog) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// Reports the derivation queue's current state per kind (thumbnails,
    /// embeddings, keyframes, transcripts, OCR, PDF text, captions) without
    /// doing any work, plus the last `index_run`'s per-item failures.
    #[tool]
    fn index_status(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::index::status(&app, &self.catalog) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// Lists this machine's configured sync locations.
    #[tool]
    fn list_sync_locations(&self) -> CallToolResult {
        if let Err(result) = self.ensure_catalog() {
            return result;
        }
        match majestical_services::sync::locations_list(&self.catalog) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// The configured describer backend for this machine, API key redacted.
    /// Returns `{"configured": false}` when none is set, else
    /// `{"configured": true, "describer": {...}}`.
    #[tool]
    fn get_describer(&self) -> CallToolResult {
        if let Err(result) = self.ensure_catalog() {
            return result;
        }
        match majestical_services::describer_config::show(&self.catalog) {
            Ok(Some(view)) => match serde_json::to_value(&view) {
                Ok(describer) => CallToolResult::structured(
                    json!({ "configured": true, "describer": describer }),
                ),
                Err(err) => tool_error(err),
            },
            Ok(None) => CallToolResult::structured(json!({ "configured": false })),
            Err(err) => tool_error(err),
        }
    }

    /// Lists every pending AI tag suggestion not yet confirmed or rejected,
    /// sorted by asset then tag.
    #[tool]
    fn suggest_tags_review(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::tags::suggestions(&app, &self.catalog) {
            Ok(outcome) => structured_ok(&outcome),
            Err(err) => tool_error(err),
        }
    }

    /// Adds a named sync location. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn add_sync_location(&self) -> CallToolResult {
        not_yet_implemented("add_sync_location")
    }

    /// Initializes a new catalog directory. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn catalog_init(&self) -> CallToolResult {
        not_yet_implemented("catalog_init")
    }

    /// Works the derivation queue. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn index_run(&self) -> CallToolResult {
        not_yet_implemented("index_run")
    }

    /// Verified copy from a source directory into a PARA-routed
    /// destination. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn ingest_source(&self) -> CallToolResult {
        not_yet_implemented("ingest_source")
    }

    /// Processes a shared inbox folder. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn inbox_process(&self) -> CallToolResult {
        not_yet_implemented("inbox_process")
    }

    /// Renames or archives a PARA node. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn move_para(&self) -> CallToolResult {
        not_yet_implemented("move_para")
    }

    /// Removes a saved search. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn rm_saved_search(&self) -> CallToolResult {
        not_yet_implemented("rm_saved_search")
    }

    /// Removes a sync location. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn rm_sync_location(&self) -> CallToolResult {
        not_yet_implemented("rm_sync_location")
    }

    /// Hashes a directory into the catalog as `AssetSeen` events. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn scan_volume(&self) -> CallToolResult {
        not_yet_implemented("scan_volume")
    }

    /// Configures the caption/tag-suggestion describer backend. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn set_describer(&self) -> CallToolResult {
        not_yet_implemented("set_describer")
    }

    /// Sets an LWW metadata field on an asset. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn set_metadata(&self) -> CallToolResult {
        not_yet_implemented("set_metadata")
    }

    /// Confirms or rejects pending AI tag suggestions. Not yet implemented
    /// over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn tag_assets(&self) -> CallToolResult {
        not_yet_implemented("tag_assets")
    }

    /// Fetches everything configured locations have that this catalog
    /// doesn't. Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn sync_pull(&self) -> CallToolResult {
        not_yet_implemented("sync_pull")
    }

    /// Replicates this catalog to configured locations. Not yet implemented
    /// over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn sync_push(&self) -> CallToolResult {
        not_yet_implemented("sync_push")
    }

    /// Probes the configured describer backend's connectivity/capability.
    /// Not yet implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn test_describer(&self) -> CallToolResult {
        not_yet_implemented("test_describer")
    }

    /// Re-verifies a destination against its ASC MHL history. Not yet
    /// implemented over MCP.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "the #[tool] router requires &self on every tool method; a stub's message needs none of MajServer's state"
    )]
    fn verify_volume(&self) -> CallToolResult {
        not_yet_implemented("verify_volume")
    }
}

#[tool_handler]
impl ServerHandler for MajServer {}

/// Serves the catalog at `catalog` to an MCP client over stdio
/// (newline-delimited JSON-RPC 2.0), blocking until the client disconnects.
/// Builds its own tokio runtime — the only subcommand that needs one; every
/// other `maj` verb stays synchronous.
///
/// # Errors
/// Returns an error if the tokio runtime can't be built, the server fails
/// to start (e.g. a malformed `initialize` handshake), or the service loop
/// itself ends in an error.
pub fn serve(catalog: &Path, machine_id: &str, author: &str) -> anyhow::Result<()> {
    // `.enable_time()` is required, not optional: rmcp's shutdown path calls
    // `tokio::time::timeout` when the transport closes (e.g. a client
    // closing stdin at the end of a normal session), and without a timer
    // driver that panics the worker thread instead of shutting down
    // cleanly — every clean client disconnect would exit nonzero. Verified
    // live: `read_tool_then_clean_stdin_close_exits_success` in
    // `mcp_smoke.rs` fails without this.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|err| anyhow::anyhow!("building tokio runtime for mcp server: {err}"))?;
    runtime.block_on(serve_async(catalog, machine_id, author))
}

async fn serve_async(catalog: &Path, machine_id: &str, author: &str) -> anyhow::Result<()> {
    let server = MajServer {
        catalog: catalog.to_path_buf(),
        machine_id: machine_id.to_string(),
        author: author.to_string(),
    };
    let service = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|err| anyhow::anyhow!("starting mcp server: {err}"))?;
    service
        .waiting()
        .await
        .map_err(|err| anyhow::anyhow!("mcp server loop ended in an error: {err}"))?;
    Ok(())
}
