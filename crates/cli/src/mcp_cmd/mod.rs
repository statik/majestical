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
//! remedy text (e.g. "run `maj catalog init` first") already lives. When
//! the failing call had already collected notices (a `ServiceError::
//! WithNotices` carrier — the four sync verbs attach these on failure, see
//! `majestical_services::notices::Notices::attach_on_err`), those notices
//! lead as their own text blocks, one per line, before the inner error's
//! Display chain: MCP has no stderr, so these blocks are the only channel
//! a failure's warnings have. See `split_notices`/`tool_error_split` below.
//!
//! Split into submodules by concern: `read_tools` (the 13 read-only tools),
//! `write_tools` (the 20 mutating tools, each gated behind a `confirm`
//! parameter — see that module's own doc for the dry-run/execute
//! contract), and `resources` (the `majestical://` MCP resources: thumbnails
//! and keyframe manifests). This file keeps only what every submodule
//! shares: the `MajServer` struct itself, the `open_app`/`ensure_catalog`
//! guards every tool and resource opens the catalog through, the
//! `tool_error`/`structured_ok`/`with_notices` result builders plus the
//! failure-path `split_notices`/`error_blocks_with_notices`/
//! `tool_error_split` trio, the `ServerHandler` impl (tool routers summed,
//! resources capability enabled, both resource methods delegating to
//! `resources`), and `serve`.
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

/// Splits a carrier into its notices and inner error; non-carriers come
/// back with no notices. The MCP analogue of the CLI's
/// `surface_err_notices`, split out so tools that must MATCH on the inner
/// error (`sync_pull`'s `SyncPullApplyFailed` arm) can do so after the split.
fn split_notices(
    err: majestical_services::error::ServiceError,
) -> (Vec<String>, majestical_services::error::ServiceError) {
    match err {
        majestical_services::error::ServiceError::WithNotices { notices, source } => {
            (notices, *source)
        }
        other => (Vec::new(), other),
    }
}

/// Builds the error-result content blocks from already-split notices and an
/// inner error: one text block per notice, in push order, followed by the
/// inner error's full Display chain. Shared tail of [`tool_error_split`],
/// reused directly by callers (`sync_pull`) that must match on the inner
/// error before the blocks can be built.
fn error_blocks_with_notices(
    notices: Vec<String>,
    err: majestical_services::error::ServiceError,
) -> CallToolResult {
    let mut blocks: Vec<ContentBlock> = notices.into_iter().map(ContentBlock::text).collect();
    blocks.push(ContentBlock::text(format!(
        "{:#}",
        anyhow::Error::from(err)
    )));
    CallToolResult::error(blocks)
}

/// The failure-path analogue of [`with_notices`]: a tool error whose
/// leading content blocks are the notices the failing call collected — one
/// text block per line, in push order — followed by the inner error's full
/// Display chain. MCP has no stderr; these blocks are the only channel a
/// failure's warnings have.
fn tool_error_split(err: majestical_services::error::ServiceError) -> CallToolResult {
    let (notices, err) = split_notices(err);
    error_blocks_with_notices(notices, err)
}

/// Folds any service-collected diagnostics into a hand-built tool response —
/// the analogue of an outcome struct's own `notices` field for the read and
/// write tools that assemble their response from `json!` rather than
/// serializing an outcome. Absent when empty, same contract.
///
/// `value` must be a JSON object: indexing a non-object by string panics.
/// Every caller passes an object by construction (a `json!({..})` literal or
/// a serialized outcome struct), the same assumption [`inject_executed`] in
/// `write_tools` makes — it checks for the object explicitly because it also
/// handles serialization failures, which this function never sees.
fn with_notices(mut value: serde_json::Value, notices: Vec<String>) -> serde_json::Value {
    if !notices.is_empty() {
        value["notices"] = serde_json::Value::from(notices);
    }
    value
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_split_leads_with_the_carried_notices() {
        use majestical_services::error::ServiceError;
        let err = ServiceError::WithNotices {
            notices: vec!["warned first".to_string(), "warned second".to_string()],
            source: Box::new(ServiceError::NoCatalog {
                root: std::path::PathBuf::from("/nowhere"),
            }),
        };
        let result = tool_error_split(err);
        assert_eq!(result.is_error, Some(true));
        let texts: Vec<String> = result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|t| t.text.clone()))
            .collect();
        assert_eq!(texts[0], "warned first");
        assert_eq!(texts[1], "warned second");
        assert!(
            texts[2].starts_with("no catalog"),
            "must render the INNER error, got: {}",
            texts[2]
        );
        assert!(
            !texts[2].contains("diagnostic(s) were collected"),
            "the carrier's own label must never render"
        );
    }
}
