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
//!
//! Split into submodules by concern: `read_tools` (the 10 read-only tools),
//! `write_tools` (the 16 mutating tools, each gated behind a `confirm`
//! parameter — see that module's own doc for the dry-run/execute
//! contract), and `resources` (the `majestical://` MCP resources: thumbnails
//! and keyframe manifests). This file keeps only what every submodule
//! shares: the `MajServer` struct itself, the `open_app`/`ensure_catalog`
//! guards every tool and resource opens the catalog through, the
//! `tool_error`/`structured_ok` result builders, the `ServerHandler` impl
//! (tool routers summed, resources capability enabled, both resource
//! methods delegating to `resources`), and `serve`.
mod read_tools;
mod resources;
mod write_tools;

use majestical_services::app::FsApp;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServiceExt, tool_handler};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
}

#[tool_handler(router = (Self::read_tool_router() + Self::write_tool_router()))]
impl ServerHandler for MajServer {
    /// Overrides the `#[tool_handler]`-generated default (which only ever
    /// enables the tools capability) so the resources capability is
    /// advertised too — `#[tool_handler]` only fills in `get_info` when the
    /// impl block doesn't already define one, so this is the only place
    /// that needs to know both capabilities exist.
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
    }

    /// Advertises the `majestical://thumb/{asset_id}` and
    /// `majestical://keyframes/{asset_id}` URI templates — see
    /// `resources::templates` for what each hands back.
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, rmcp::ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resources::templates(),
        ))
    }

    /// Reads one `majestical://` resource — see `resources::read` for the
    /// URI dispatch, catalog guard, and blob lookup.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        resources::read(self, &request.uri).map(Into::into)
    }
}

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
