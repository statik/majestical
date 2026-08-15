//! The 12 read-only MCP tools: each opens (or, for the two that never touch
//! the event log, guards) a fresh catalog handle and serializes the matching
//! `majestical_services` outcome straight through — see `super`'s module doc
//! for the shared wire contract.
use super::MajServer;
use majestical_services::notices::Notices;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

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

/// `browse_assets`'s `limit` default, sharing `majestical_services::browse`'s
/// own constant with `maj browse list`'s `--limit` — one source for the
/// default so the CLI, this tool's schema, and the service can never drift
/// apart.
fn default_browse_limit() -> usize {
    majestical_services::browse::DEFAULT_LIMIT
}

/// `browse_assets`'s `flatten` default: true, matching `maj browse list`
/// (flatten is opt-out there, via `--no-flatten`).
fn default_flatten() -> bool {
    true
}

/// Params for `browse_assets`, mirroring
/// `majestical_services::browse::BrowseRequest`.
#[derive(Debug, Deserialize, JsonSchema)]
struct BrowseAssetsArgs {
    /// Volume id (see `list_volumes`).
    volume: String,
    /// Folder path relative to the volume root (default: "", the root).
    #[serde(default)]
    path: String,
    /// Include the whole subtree under `path` (default true), not just its
    /// immediate children (false).
    #[serde(default = "default_flatten")]
    flatten: bool,
    /// "captured" (default: newest `mtime_ms` first), "name" (ascending),
    /// or "size" (descending).
    #[serde(default)]
    sort: Option<String>,
    /// Filter to one media kind (image, video, audio, pdf, other).
    #[serde(default)]
    kind: Option<String>,
    /// Max results (default 50).
    #[serde(default = "default_browse_limit")]
    limit: usize,
    /// Pagination offset (default 0).
    #[serde(default)]
    offset: usize,
}

/// `list_saved_searches`'s structured result — the service verb returns a
/// bare `Vec<SavedSearch>`, so this names the wire object's one field.
#[derive(Serialize)]
struct SavedSearchesResult {
    saved: Vec<majestical_services::search::SavedSearch>,
    /// Diagnostics collected during this call, verbatim — this struct exists
    /// only on the MCP wire, so there is no CLI rendering of these. Absent
    /// from the wire when empty, matching every outcome struct's own field.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    notices: Vec<String>,
}

/// Runs `req` on a plain OS thread, off this server's own tokio runtime —
/// `search::search`'s semantic layer opens a Lance vector store whenever a
/// query has terms and a model is installed, and that store builds and
/// enters its own tokio runtime internally (see
/// `majestical_services::runtime::run_off_tokio_runtime`'s doc for why that
/// panics from inside this server's `#[tool]` handler threads otherwise).
/// Opens its own `FsApp` inside the spawned thread
/// (never crossing the thread boundary as a reference), same reason
/// `write_tools::index_run_exec` does: `App`'s HLC clock holds a
/// `Box<dyn Clock>`, so `&FsApp` isn't `Send`.
fn run_search_off_runtime(
    catalog: &std::path::Path,
    machine_id: &str,
    author: &str,
    req: &majestical_services::search::SearchRequest,
) -> anyhow::Result<majestical_services::search::SearchOutcome> {
    majestical_services::runtime::run_off_tokio_runtime(|| {
        let mut app = majestical_services::app::FsApp::open(catalog, machine_id, author)?;
        Ok(majestical_services::search::search(&mut app, catalog, req)?)
    })
}

#[tool_router(router = read_tool_router, vis = "pub(super)")]
impl MajServer {
    /// Search the catalog: bare terms match names; `key:value` tokens are
    /// hard filters. Asset ids and timestamps in `results` are stable and
    /// safe to pass into `get_asset` or a later mutating-tool call.
    #[tool]
    fn search_assets(&self, Parameters(args): Parameters<SearchAssetsArgs>) -> CallToolResult {
        let req = majestical_services::search::SearchRequest {
            query: args.query,
            limit: args.limit,
            saved: args.saved,
            save: None,
        };
        match run_search_off_runtime(&self.catalog, &self.machine_id, &self.author, &req) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
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
                Err(err) => super::tool_error(err),
            },
            Ok(None) => CallToolResult::structured(super::with_notices(
                json!({ "found": false }),
                app.notices().drain(),
            )),
            Err(err) => super::tool_error(err),
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
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
        }
    }

    /// Every volume's folder tree, with a recursive asset count per folder
    /// (an asset with multiple instances under one folder's subtree still
    /// counts once).
    #[tool]
    fn browse_tree(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::browse::browse_tree(&app, &self.catalog) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
        }
    }

    /// Lists assets under one folder of one volume — the whole subtree by
    /// default (`flatten: true`), sorted newest-first by default
    /// (`sort: "captured"`). Rows are the same shape `search_assets`
    /// returns, plus `size`/`mtime_ms`/`kind`.
    #[tool]
    fn browse_assets(&self, Parameters(args): Parameters<BrowseAssetsArgs>) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        let req = majestical_services::browse::BrowseRequest {
            volume: args.volume,
            path: args.path,
            flatten: args.flatten,
            sort: args.sort,
            kind: args.kind,
            limit: args.limit,
            offset: args.offset,
        };
        match majestical_services::browse::browse_list(&app, &self.catalog, &req) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
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
            Ok(saved) => super::structured_ok(&SavedSearchesResult {
                saved,
                notices: app.notices().drain(),
            }),
            Err(err) => super::tool_error(err),
        }
    }

    /// Runs a previously saved search by name (see `list_saved_searches`).
    /// Same result shape as `search_assets`.
    #[tool]
    fn run_saved_search(&self, Parameters(args): Parameters<RunSavedSearchArgs>) -> CallToolResult {
        let req = majestical_services::search::SearchRequest {
            query: None,
            limit: args.limit,
            saved: Some(args.name),
            save: None,
        };
        match run_search_off_runtime(&self.catalog, &self.machine_id, &self.author, &req) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
        }
    }

    /// For every configured sync location, reports reachability plus what a
    /// push would send (`ahead`) and a pull would fetch (`behind`) — walked
    /// fresh from real files; never executes a transfer.
    #[tool]
    fn sync_status(&self) -> CallToolResult {
        match majestical_services::sync::status(&self.catalog) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error_split(err),
        }
    }

    /// Reports the derivation queue's current state per kind (thumbnails,
    /// embeddings, keyframes, keyframe images, transcripts, OCR, PDF text,
    /// captions) without doing any work, plus the last `index_run`'s
    /// per-item failures.
    #[tool]
    fn index_status(&self) -> CallToolResult {
        let app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::index::status(&app, &self.catalog) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
        }
    }

    /// Lists this machine's configured sync locations.
    #[tool]
    fn list_sync_locations(&self) -> CallToolResult {
        if let Err(result) = self.ensure_catalog() {
            return result;
        }
        match majestical_services::sync::locations_list(&self.catalog) {
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error_split(err),
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
        let notices = Notices::new();
        match majestical_services::describer_config::show(&self.catalog, &notices) {
            Ok(Some(view)) => match serde_json::to_value(&view) {
                Ok(describer) => CallToolResult::structured(super::with_notices(
                    json!({ "configured": true, "describer": describer }),
                    notices.drain(),
                )),
                Err(err) => super::tool_error(err),
            },
            Ok(None) => CallToolResult::structured(super::with_notices(
                json!({ "configured": false }),
                notices.drain(),
            )),
            Err(err) => super::tool_error(err),
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
            Ok(outcome) => super::structured_ok(&outcome),
            Err(err) => super::tool_error(err),
        }
    }
}
