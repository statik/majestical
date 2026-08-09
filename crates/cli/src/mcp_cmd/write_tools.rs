//! The 16 mutating MCP tools. Every tool takes `confirm: bool` (default
//! `false`): a dry run returns a structured description of what would
//! happen (`executed: false`) without touching anything; `confirm: true`
//! performs the operation for real (`executed: true`). Both arms serialize
//! through the same [`confirm_gate`]/[`inject_executed`] pair so an agent
//! can diff a dry-run response against the executed one — same shape,
//! different `executed` value, plus whatever extra fields the real outcome
//! carries.
//!
//! Three dry-run styles, matching what each operation actually affords:
//! - A handful of tools already compute a real plan with no side effects
//!   (`ingest::plan`, `para::archive(dry_run=true)`, `sync::status`) — the
//!   dry run IS that plan, serialized as-is.
//! - Most tools have no natural plan of their own (`tag_assets`,
//!   `catalog_init`, `scan_volume`, ...): the dry run builds a small
//!   `{"would": ...}` description from the request plus whatever current
//!   state is cheap to read (existing tags, whether a catalog already
//!   exists, directory contents) — real state, never a guess. A preview
//!   whose operation rejects an unknown asset id checks the id up front
//!   (`set_metadata`, and `tag_assets`'s `add`/`rm`/`confirm_suggestion`)
//!   so it fails there rather than promising a write that cannot happen.
//!   `rm` is the partial case: it never checks the asset at all, failing
//!   instead on a tag that is not currently set, so its preview catches an
//!   unknown id but still over-promises on a KNOWN asset whose tag is
//!   unset. `tag_assets`'s `reject_suggestion` is the deliberate
//!   exception: it validates nothing on execute either, so its preview
//!   must not validate either.
//! - `verify_volume`'s dry run only reports whether ASC MHL history exists
//!   yet; the actual verify always mutates (a new generation is appended),
//!   so there is no side-effect-free way to preview its `altered`/`missing`
//!   sets ahead of running it for real.
//!
//! Every mutating call funnels a `ServiceError`/`anyhow::Error` through
//! `super::tool_error` exactly like the read tools. Three operations carry
//! partial progress even on failure and need it to reach the caller instead
//! of being discarded by a plain tool-error text: `sync_push`/`sync_pull`/
//! `inbox_process`'s per-row outcomes (`overall_failed() == true` still
//! attaches the full structured outcome, `isError: true`), and
//! `move_para`'s archive op / `sync_pull`'s local-apply step, whose
//! `ServiceError::ParaArchivePartial`/`SyncPullApplyFailed` carry the
//! moves/rows already completed before the failure.
use super::MajServer;
use anyhow::Context as _;
use majestical_core::event::AssetId;
use majestical_services::app::FsApp;
use majestical_services::error::ServiceError;
use majestical_services::notices::Notices;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Wraps a mutating tool's already-computed `Result`: on success, folds
/// `"executed": confirm` into the result's JSON object and returns a
/// structured success; on failure, renders the error exactly like a read
/// tool does (`super::tool_error`'s full `{err:#}` chain). Takes the
/// already-evaluated `Result` — computed by an ordinary `if args.confirm {
/// ... } else { ... }` in the caller — rather than a `dry`/`exec` closure
/// pair: a dry-run branch typically only needs `&FsApp` while the exec
/// branch needs `&mut FsApp`, and constructing both branches as closures
/// handed to this function at once would make the borrow checker see a
/// live shared and exclusive borrow simultaneously, even though only one
/// closure is ever called. Sequential code has no such problem.
///
/// A dry-run helper (e.g. `add_sync_location_result`) reaches its
/// `ServiceError` through `?` on a call like `sync::locations_list`, which
/// erases it to `anyhow::Error` on the way — `downcast` recovers the
/// concrete type so a `WithNotices` carrier still gets split here rather
/// than leaking its carrier label (`"N diagnostic(s) were collected..."`)
/// as the tool's error text. An error that was never a `ServiceError` (most
/// callers) downcasts back to itself unchanged.
fn confirm_gate<T: Serialize>(confirm: bool, result: anyhow::Result<T>) -> CallToolResult {
    match result {
        Ok(value) => match inject_executed(&value, confirm) {
            Ok(json) => CallToolResult::structured(json),
            Err(result) => result,
        },
        Err(err) => match err.downcast::<ServiceError>() {
            Ok(err) => super::tool_error_split(err),
            Err(err) => super::tool_error(err),
        },
    }
}

/// Serializes `value` and folds `"executed": executed` into the resulting
/// JSON object — every mutating tool's response built in this module is an
/// object by construction, so this always finds one to add to. A non-object
/// serialization is a tool error too, same as an outright serialization
/// failure: silently shipping a response with no `executed` flag is the one
/// ambiguity this module must never allow (a caller diffing a dry-run
/// response against the executed one has nowhere else to look), so there is
/// no "pass the value through anyway" fallback.
fn inject_executed(
    value: &impl Serialize,
    executed: bool,
) -> Result<serde_json::Value, CallToolResult> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert("executed".to_string(), serde_json::Value::Bool(executed));
            Ok(serde_json::Value::Object(map))
        }
        Ok(other) => Err(super::tool_error(anyhow::anyhow!(
            "internal error: mutating tool response serialized as {other}, not a JSON object — \
             cannot attach the executed flag"
        ))),
        Err(err) => Err(super::tool_error(err)),
    }
}

/// Parses `sync_push`/`sync_pull`'s `only` transfer-class filter.
fn parse_only(only: Option<&str>) -> anyhow::Result<Option<majestical_services::sync::Only>> {
    let Some(only) = only else { return Ok(None) };
    let parsed = match only {
        "segments" => majestical_services::sync::Only::Segments,
        "thumbs" => majestical_services::sync::Only::Thumbs,
        "metadata" => majestical_services::sync::Only::Metadata,
        "vectors" => majestical_services::sync::Only::Vectors,
        "transcripts" => majestical_services::sync::Only::Transcripts,
        other => anyhow::bail!(
            "unknown 'only' value '{other}' — one of: segments, thumbs, metadata, vectors, \
             transcripts"
        ),
    };
    Ok(Some(parsed))
}

/// Params for `tag_assets`.
#[derive(Debug, Deserialize, JsonSchema)]
struct TagAssetsArgs {
    /// Asset id, e.g. `xxh3:0123...`.
    asset: String,
    /// `add`/`rm` set or remove a folksonomy tag directly (use `tag`).
    /// `confirm_suggestion`/`reject_suggestion` act on pending AI tag
    /// suggestions (use `tags` — see `suggest_tags_review`).
    op: majestical_services::tags::TagOp,
    /// The tag for `add`/`rm`.
    #[serde(default)]
    tag: Option<String>,
    /// One or more tags for `confirm_suggestion`/`reject_suggestion`.
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

/// `tag_assets`'s validated op paired with the payload that op requires —
/// built once so the dry-run description and the real mutation can't
/// disagree about which fields a given [`majestical_services::tags::TagOp`]
/// needs.
enum ValidatedTagOp<'a> {
    Add(&'a str),
    Rm(&'a str),
    ConfirmSuggestion(&'a [String]),
    RejectSuggestion(&'a [String]),
}

fn non_empty_tags(tags: Option<&Vec<String>>) -> anyhow::Result<&[String]> {
    let tags = tags.map_or(&[][..], Vec::as_slice);
    anyhow::ensure!(!tags.is_empty(), "this op requires a non-empty 'tags' list");
    Ok(tags)
}

fn parse_tag_op(args: &TagAssetsArgs) -> anyhow::Result<ValidatedTagOp<'_>> {
    use majestical_services::tags::TagOp;
    match args.op {
        TagOp::Add => Ok(ValidatedTagOp::Add(
            args.tag.as_deref().context("op 'add' requires 'tag'")?,
        )),
        TagOp::Rm => Ok(ValidatedTagOp::Rm(
            args.tag.as_deref().context("op 'rm' requires 'tag'")?,
        )),
        TagOp::ConfirmSuggestion => Ok(ValidatedTagOp::ConfirmSuggestion(non_empty_tags(
            args.tags.as_ref(),
        )?)),
        TagOp::RejectSuggestion => Ok(ValidatedTagOp::RejectSuggestion(non_empty_tags(
            args.tags.as_ref(),
        )?)),
    }
}

fn tag_assets_result(
    catalog: &Path,
    app: &mut FsApp,
    args: &TagAssetsArgs,
) -> anyhow::Result<serde_json::Value> {
    let op = parse_tag_op(args)?;
    if !args.confirm {
        let projection = app.projection()?;
        let asset_id = AssetId(args.asset.clone());
        // `reject` appends the pair to this machine's rejection log as
        // given, without checking it against any current suggestion (see
        // [`majestical_services::tags::reject`]) — a rejection on an
        // unknown id is a harmless no-op line, not a failure. Guarding its
        // preview would fail where `confirm: true` succeeds, the exact
        // inverse of what the other three ops need.
        match &op {
            ValidatedTagOp::Add(_)
            | ValidatedTagOp::Rm(_)
            | ValidatedTagOp::ConfirmSuggestion(_) => {
                majestical_services::catalog::ensure_asset_known(&projection, &asset_id)?;
            }
            ValidatedTagOp::RejectSuggestion(_) => {}
        }
        let current_tags: Vec<String> = projection.tags(&asset_id).into_iter().collect();
        let would = match &op {
            ValidatedTagOp::Add(tag) => format!("add tag '{tag}' to {}", args.asset),
            ValidatedTagOp::Rm(tag) => format!("remove tag '{tag}' from {}", args.asset),
            ValidatedTagOp::ConfirmSuggestion(tags) => {
                format!("confirm suggested tag(s) {tags:?} on {}", args.asset)
            }
            ValidatedTagOp::RejectSuggestion(tags) => format!(
                "reject suggested tag(s) {tags:?} on {} (this machine only, never synced)",
                args.asset
            ),
        };
        return Ok(super::with_notices(
            json!({
                "asset": args.asset,
                "op": args.op,
                "current_tags": current_tags,
                "would": would,
            }),
            app.notices().drain(),
        ));
    }
    let done = match op {
        ValidatedTagOp::Add(tag) => {
            majestical_services::tags::tag_add(app, &args.asset, tag)?;
            json!({"asset": args.asset, "op": args.op, "tag": tag})
        }
        ValidatedTagOp::Rm(tag) => {
            majestical_services::tags::tag_rm(app, &args.asset, tag)?;
            json!({"asset": args.asset, "op": args.op, "tag": tag})
        }
        ValidatedTagOp::ConfirmSuggestion(tags) => {
            majestical_services::tags::confirm(app, &args.asset, tags)?;
            json!({"asset": args.asset, "op": args.op, "tags": tags})
        }
        // `reject` is app-less (a state-dir file, never an event), so it
        // records into the app's sink directly rather than a second one.
        ValidatedTagOp::RejectSuggestion(tags) => {
            majestical_services::tags::reject(catalog, &args.asset, tags, app.notices())?;
            json!({"asset": args.asset, "op": args.op, "tags": tags})
        }
    };
    Ok(super::with_notices(done, app.notices().drain()))
}

/// Params for `set_metadata`.
#[derive(Debug, Deserialize, JsonSchema)]
struct SetMetadataArgs {
    asset: String,
    field: String,
    value: String,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn set_metadata_result(
    app: &mut FsApp,
    args: &SetMetadataArgs,
) -> anyhow::Result<serde_json::Value> {
    // Dry-run only: `meta_set` builds the same projection and runs the same
    // check on the confirm path, so loading one here too would replay the
    // whole event log for a value that path never reads.
    if !args.confirm {
        let projection = app.projection()?;
        let asset_id = AssetId(args.asset.clone());
        majestical_services::catalog::ensure_asset_known(&projection, &asset_id)?;
        let current_value = projection.field(&asset_id, &args.field).map(str::to_string);
        return Ok(super::with_notices(
            json!({
                "asset": args.asset,
                "field": args.field,
                "current_value": current_value,
                "new_value": args.value,
                "would": format!(
                    "set field '{}' on {} to '{}'", args.field, args.asset, args.value
                ),
            }),
            app.notices().drain(),
        ));
    }
    majestical_services::meta::meta_set(app, &args.asset, &args.field, &args.value)?;
    Ok(super::with_notices(
        json!({"asset": args.asset, "field": args.field, "value": args.value}),
        app.notices().drain(),
    ))
}

/// Params for `move_para`. Fields required depend on `op`: `add` needs
/// `kind`+`name`; `rename` needs `node`+`name`; `archive` needs `node` (plus
/// optional `roots`).
#[derive(Debug, Deserialize, JsonSchema)]
struct MoveParaArgs {
    /// What to do to the node: create it, rename it, or archive it.
    op: majestical_services::para::ParaOp,
    /// PARA kind for `add` (project, area, resource, archive).
    #[serde(default)]
    kind: Option<String>,
    /// New node name for `add`/`rename`.
    #[serde(default)]
    name: Option<String>,
    /// Node reference (`<kind>/<name>` or a raw node id) for `rename`/
    /// `archive`.
    #[serde(default)]
    node: Option<String>,
    /// Materialized-directory root(s) to move for `archive` (default: none
    /// — only the archive event is emitted, nothing moves on disk).
    #[serde(default)]
    roots: Vec<PathBuf>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn move_para_add(app: &mut FsApp, args: &MoveParaArgs) -> anyhow::Result<serde_json::Value> {
    let kind_str = args.kind.as_deref().context("op 'add' requires 'kind'")?;
    let name = args.name.as_deref().context("op 'add' requires 'name'")?;
    let kind = majestical_services::para::parse_kind(kind_str)?;
    if !args.confirm {
        let projection = app.projection()?;
        let already_exists = projection
            .para_nodes()
            .any(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name));
        return Ok(super::with_notices(
            json!({
                "op": "add",
                "kind": kind_str,
                "name": name,
                "already_exists": already_exists,
                "would": format!("create a new {kind_str} node named '{name}'"),
            }),
            app.notices().drain(),
        ));
    }
    let majestical_services::para::NodeId(node_id) =
        majestical_services::para::add(app, kind_str, name)?;
    Ok(super::with_notices(
        json!({"op": "add", "kind": kind_str, "name": name, "node_id": node_id}),
        app.notices().drain(),
    ))
}

fn move_para_rename(app: &mut FsApp, args: &MoveParaArgs) -> anyhow::Result<serde_json::Value> {
    let node = args
        .node
        .as_deref()
        .context("op 'rename' requires 'node'")?;
    let name = args
        .name
        .as_deref()
        .context("op 'rename' requires 'name'")?;
    if !args.confirm {
        let projection = app.projection()?;
        let current_name = majestical_services::para::resolve_para_node(&projection, node)
            .ok()
            .and_then(|id| {
                projection
                    .para_node(&id)
                    .and_then(|st| st.name().map(str::to_string))
            });
        return Ok(super::with_notices(
            json!({
                "op": "rename",
                "node": node,
                "new_name": name,
                "current_name": current_name,
                "would": format!("rename {node} to '{name}'"),
            }),
            app.notices().drain(),
        ));
    }
    majestical_services::para::rename(app, node, name)?;
    Ok(super::with_notices(
        json!({"op": "rename", "node": node, "name": name}),
        app.notices().drain(),
    ))
}

/// `move_para`'s archive op: a natural-plan tool (`para::archive`'s own
/// `dry_run` flag IS the dry run) that also needs
/// [`ServiceError::ParaArchivePartial`]'s moves-so-far carried through on a
/// multi-root failure — bespoke rather than routed through [`confirm_gate`]
/// so that carrier can reach the caller as structured content instead of
/// being flattened into plain error text.
fn move_para_archive(app: &mut FsApp, args: &MoveParaArgs) -> CallToolResult {
    let Some(node) = args.node.as_deref() else {
        return super::tool_error(anyhow::anyhow!("op 'archive' requires 'node'"));
    };
    match majestical_services::para::archive(app, node, &args.roots, !args.confirm) {
        Ok(outcome) => match inject_executed(&outcome, args.confirm) {
            Ok(json) => CallToolResult::structured(json),
            Err(result) => result,
        },
        Err(ServiceError::ParaArchivePartial { moves, source }) => {
            CallToolResult::structured_error(json!({
                "moves": moves,
                "executed": true,
                "error": format!("{source:#}"),
            }))
        }
        Err(err) => super::tool_error(err),
    }
}

/// Params for `scan_volume`.
#[derive(Debug, Deserialize, JsonSchema)]
struct ScanVolumeArgs {
    dir: PathBuf,
    /// Stable volume id and label. Omit to auto-detect.
    #[serde(default)]
    volume: Option<String>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn scan_volume_result(app: &mut FsApp, args: &ScanVolumeArgs) -> anyhow::Result<serde_json::Value> {
    if !args.confirm {
        let (resolved_volume_id, resolved_volume_label) =
            majestical_services::scan::resolve_volume(&args.dir, args.volume.clone());
        let would_scan_files = walkdir::WalkDir::new(&args.dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .count();
        return Ok(super::with_notices(
            json!({
                "dir": args.dir,
                "resolved_volume_id": resolved_volume_id,
                "resolved_volume_label": resolved_volume_label,
                "would_scan_files": would_scan_files,
                "would": format!("hash {would_scan_files} file(s) under {} into the catalog", args.dir.display()),
            }),
            app.notices().drain(),
        ));
    }
    let outcome = majestical_services::scan::scan(app, &args.dir, args.volume.clone())?;
    Ok(super::with_notices(
        json!({"dir": args.dir, "assets": outcome.assets, "volume_id": outcome.volume_id}),
        app.notices().drain(),
    ))
}

/// Params for `verify_volume`.
#[derive(Debug, Deserialize, JsonSchema)]
struct VerifyVolumeArgs {
    dir: PathBuf,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

/// `verify_volume`'s real run always mutates (a new ASC MHL generation is
/// appended even on a clean pass), so unlike every other tool here there is
/// no side-effect-free way to preview `altered`/`missing` ahead of running
/// it — the dry run only reports whether history exists yet to verify
/// against. Polarity: a real run that finds anything `altered` or `missing`
/// is `isError: true` WITH the full report attached, never a silent
/// success — an agent must not mistake "ran" for "passed".
fn verify_volume_result(args: &VerifyVolumeArgs) -> CallToolResult {
    if !args.confirm {
        let has_history = args.dir.join("ascmhl").is_dir();
        let would = if has_history {
            "re-verify against the existing ASC MHL history and append a new generation"
        } else {
            "fail: no ASC MHL history yet at this directory"
        };
        return CallToolResult::structured(json!({
            "dir": args.dir,
            "has_history": has_history,
            "would": would,
            "executed": false,
        }));
    }
    match majestical_services::verify::verify_dir_op(&args.dir) {
        Ok(report) => {
            let failed = !report.altered.is_empty() || !report.missing.is_empty();
            match inject_executed(&report, true) {
                Ok(json) if failed => CallToolResult::structured_error(json),
                Ok(json) => CallToolResult::structured(json),
                Err(result) => result,
            }
        }
        Err(err) => super::tool_error(err),
    }
}

fn default_ingest_template() -> String {
    "{date}/{source-label}".to_string()
}

fn default_dedupe() -> majestical_services::ingest::DedupeMode {
    majestical_services::ingest::DedupeMode::Skip
}

/// Params for `ingest_source`.
#[derive(Debug, Deserialize, JsonSchema)]
struct IngestSourceArgs {
    source: PathBuf,
    /// Destination root(s); each gets an independently verified copy.
    /// Required once `confirm` is true; unused (but still validated as
    /// present) by the dry-run plan.
    dest: Vec<PathBuf>,
    /// Target PARA node (`<kind>/<name>` or a node id).
    para: String,
    /// Layout inside the node. Tokens: `{date}`, `{source-label}`.
    #[serde(default = "default_ingest_template")]
    template: String,
    /// `skip` (default) leaves a known duplicate alone; `copy` copies it
    /// anyway.
    #[serde(default = "default_dedupe")]
    dedupe: majestical_services::ingest::DedupeMode,
    /// Parallel copy workers (default: CPU cores, max 8).
    #[serde(default)]
    jobs: Option<usize>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn ingest_source_result(
    catalog: &Path,
    app: &mut FsApp,
    args: &IngestSourceArgs,
) -> anyhow::Result<serde_json::Value> {
    let planned = majestical_services::ingest::plan(
        app,
        &args.source,
        &args.para,
        &args.template,
        args.dedupe.into(),
    )?;
    if !args.confirm {
        return serde_json::to_value(&planned).map_err(anyhow::Error::from);
    }
    anyhow::ensure!(
        !args.dest.is_empty(),
        "ingest_source requires at least one 'dest' when confirm is true"
    );
    let run = majestical_services::ingest::run_ingest(
        app,
        catalog,
        &majestical_services::ingest::ExecuteIngest {
            plan: &planned.plan,
            dest: &args.dest,
            subdir: &planned.subdir,
            node_id: &planned.node_id,
            source_volume: (&planned.source_volume_id, &planned.source_volume_label),
            jobs: args.jobs,
            resume: None,
        },
        &mut |_line: &str| {},
    )?;
    serde_json::to_value(&run).map_err(anyhow::Error::from)
}

/// Params for `catalog_init`.
#[derive(Debug, Deserialize, JsonSchema)]
struct CatalogInitArgs {
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn catalog_init_result(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    confirm: bool,
) -> anyhow::Result<serde_json::Value> {
    let already_initialized = majestical_services::catalog::ensure_catalog(catalog).is_ok();
    if !confirm {
        let would = if already_initialized {
            "refuse: a catalog already exists at this path"
        } else {
            "initialize a new catalog at this path"
        };
        return Ok(json!({
            "path": catalog,
            "already_initialized": already_initialized,
            "would": would,
        }));
    }
    anyhow::ensure!(
        !already_initialized,
        "a catalog already exists at {} — refusing to re-initialize; point at a different path, \
         or use the existing catalog as-is",
        catalog.display()
    );
    majestical_services::catalog::init(catalog, machine_id, author)?;
    Ok(json!({"path": catalog}))
}

/// Params for `add_sync_location`.
#[derive(Debug, Deserialize, JsonSchema)]
struct AddSyncLocationArgs {
    name: String,
    path: PathBuf,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn add_sync_location_result(
    catalog: &Path,
    args: &AddSyncLocationArgs,
) -> anyhow::Result<serde_json::Value> {
    if !args.confirm {
        let listed = majestical_services::sync::locations_list(catalog)?;
        let already_configured = listed
            .locations
            .iter()
            .any(|location| location.name == args.name);
        return Ok(super::with_notices(
            json!({
                "name": args.name,
                "path": args.path,
                "already_configured": already_configured,
                "path_accessible": args.path.is_dir(),
                "would": format!("add sync location '{}' at {}", args.name, args.path.display()),
            }),
            listed.notices,
        ));
    }
    let notices = Notices::new();
    majestical_services::sync::location_add(catalog, &args.name, &args.path, &notices)?;
    Ok(super::with_notices(
        json!({"name": args.name, "path": args.path}),
        notices.drain(),
    ))
}

/// Params for `rm_sync_location`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RmSyncLocationArgs {
    name: String,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn rm_sync_location_result(
    catalog: &Path,
    args: &RmSyncLocationArgs,
) -> anyhow::Result<serde_json::Value> {
    if !args.confirm {
        let listed = majestical_services::sync::locations_list(catalog)?;
        let configured = listed
            .locations
            .iter()
            .any(|location| location.name == args.name);
        return Ok(super::with_notices(
            json!({
                "name": args.name,
                "configured": configured,
                "would": format!("remove sync location '{}'", args.name),
            }),
            listed.notices,
        ));
    }
    let notices = Notices::new();
    majestical_services::sync::location_rm(catalog, &args.name, &notices)?;
    Ok(super::with_notices(
        json!({"name": args.name}),
        notices.drain(),
    ))
}

/// Params shared by `sync_push`/`sync_pull`.
#[derive(Debug, Deserialize, JsonSchema)]
struct SyncTransferArgs {
    /// Target only this location (default: every configured one).
    #[serde(default)]
    location: Option<String>,
    /// One of: segments, thumbs, metadata, vectors, transcripts. Omit for
    /// every transfer class.
    #[serde(default)]
    only: Option<String>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

/// `sync_push`/`sync_pull`'s dry run: `sync::status`'s already-fresh plan,
/// narrowed to the requested location and to the `ahead` (push) or
/// `behind` (pull) counts — the exact rows a real transfer would move.
fn sync_transfer_dry_run(
    catalog: &Path,
    location: Option<&str>,
    push: bool,
) -> anyhow::Result<serde_json::Value> {
    let status = majestical_services::sync::status(catalog)?;
    let planned: Vec<serde_json::Value> = status
        .rows
        .iter()
        .filter(|row| {
            let name = match row {
                majestical_services::sync::StatusRow::Reachable { name, .. }
                | majestical_services::sync::StatusRow::Unreachable { name, .. }
                | majestical_services::sync::StatusRow::Failed { name, .. } => name,
            };
            location.is_none_or(|loc| loc == name.as_str())
        })
        .map(|row| match row {
            majestical_services::sync::StatusRow::Reachable {
                name,
                ahead,
                behind,
            } => {
                let counts = if push { ahead } else { behind };
                json!({"name": name, "reachable": true, "planned": counts})
            }
            majestical_services::sync::StatusRow::Unreachable { name, path } => {
                json!({"name": name, "reachable": false, "path": path})
            }
            majestical_services::sync::StatusRow::Failed { name, error } => {
                json!({"name": name, "reachable": false, "error": error})
            }
        })
        .collect();
    Ok(super::with_notices(
        json!({"readonly": status.readonly, "planned": planned}),
        status.notices,
    ))
}

/// Params for `inbox_process`.
#[derive(Debug, Deserialize, JsonSchema)]
struct InboxProcessArgs {
    inbox: PathBuf,
    /// Destination root(s), like `ingest_source`'s `dest`.
    dest: Vec<PathBuf>,
    /// PARA node for manifest-less drops. Required once any quiescent
    /// manifest-less item is present.
    #[serde(default)]
    triage_target: Option<String>,
    /// Leave processed contributions in place instead of moving them to
    /// `.processed/`.
    #[serde(default)]
    keep: bool,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

/// `inbox_process`'s dry run: a plain listing of the inbox's current
/// top-level entries — real state, but not a replay of `inbox::process`'s
/// own manifest validation/quiescence logic, which lives private to that
/// module.
fn inbox_dry_run(inbox: &Path) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        inbox.is_dir(),
        "inbox must be a directory: {} — check the path, or create it first",
        inbox.display()
    );
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(inbox).with_context(|| format!("reading {}", inbox.display()))? {
        let entry = entry.with_context(|| format!("reading {}", inbox.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let kind = if entry.path().is_dir() { "dir" } else { "file" };
        entries.push((name, kind));
    }
    entries.sort();
    let rows: Vec<serde_json::Value> = entries
        .iter()
        .map(|(name, kind)| json!({"name": name, "kind": kind}))
        .collect();
    Ok(json!({
        "inbox": inbox,
        "entries": rows,
        "would": format!("process {} top-level inbox entries", rows.len()),
    }))
}

/// Params for `index_run`.
#[derive(Debug, Deserialize, JsonSchema)]
struct IndexRunArgs {
    /// Subset of: thumbs, embeddings, keyframes, transcripts, ocr, pdf,
    /// captions. Omit to run every kind.
    #[serde(default)]
    kinds: Option<Vec<String>>,
    /// Stop after this many items per kind.
    #[serde(default)]
    limit: Option<usize>,
    /// Parallel workers (default: CPU cores, max 4).
    #[serde(default)]
    threads: Option<usize>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn parse_index_kinds(kinds: Option<&[String]>) -> anyhow::Result<BTreeSet<String>> {
    let Some(kinds) = kinds else {
        return Ok(majestical_services::index::VALID_KINDS
            .iter()
            .map(|s| (*s).to_string())
            .collect());
    };
    for kind in kinds {
        anyhow::ensure!(
            majestical_services::index::VALID_KINDS.contains(&kind.as_str()),
            "unknown kind '{kind}' — one of: {}",
            majestical_services::index::VALID_KINDS.join(", ")
        );
    }
    Ok(kinds.iter().cloned().collect())
}

fn index_run_dry(
    app: &FsApp,
    catalog: &Path,
    kinds: &BTreeSet<String>,
    args: &IndexRunArgs,
) -> anyhow::Result<serde_json::Value> {
    let status = majestical_services::index::status(app, catalog)?;
    // No fold here: notices ride NESTED on the embedded status, the same
    // convention `get_asset`'s found arm follows — a serialized outcome
    // keeps its own `notices`; only hand-built summaries fold at the top.
    Ok(json!({
        "kinds": kinds,
        "limit": args.limit,
        "threads": args.threads,
        "status": status,
        "would": format!(
            "run one derivation pass over kinds: {}",
            kinds.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
    }))
}

/// Unlike [`index_run_dry`], this opens its OWN `FsApp` — inside
/// [`majestical_services::runtime::run_off_tokio_runtime`]'s spawned thread, never crossing the
/// thread boundary as a reference — for that isolation to work: `App`'s HLC
/// clock holds a `Box<dyn Clock>`, so `&FsApp` itself isn't `Send`, and
/// reusing the caller's already-open `FsApp` across the thread boundary
/// would fail to compile for exactly that reason (correctly — nothing about
/// `FsApp` promises safe concurrent access from two threads at once).
fn index_run_exec(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    kinds: &BTreeSet<String>,
    args: &IndexRunArgs,
) -> anyhow::Result<serde_json::Value> {
    let req = majestical_services::index::IndexRunReq {
        kinds: kinds.clone(),
        limit: args.limit,
        threads: args.threads,
        api_key: crate::describer_cmd::env_api_key(),
    };
    let mut outcome = majestical_services::runtime::run_off_tokio_runtime(|| {
        let app = FsApp::open(catalog, machine_id, author)?;
        Ok(majestical_services::index::run(&app, catalog, &req)?)
    })?;
    let notices = Notices::new();
    majestical_services::index::update_failure_report(catalog, &outcome, kinds, &notices)?;
    // The marker update runs after the pass, so its diagnostics belong at the
    // end of the run's own list rather than in a second field.
    outcome.notices.extend(notices.drain());
    serde_json::to_value(&outcome).map_err(anyhow::Error::from)
}

/// Params for `set_describer`.
#[derive(Debug, Deserialize, JsonSchema)]
struct SetDescriberArgs {
    /// Which describer service to talk to.
    backend: majestical_services::describer_config::DescriberBackend,
    model: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn set_describer_result(
    catalog: &Path,
    args: &SetDescriberArgs,
) -> anyhow::Result<serde_json::Value> {
    let backend: majestical_describe::BackendKind = args.backend.into();
    let notices = Notices::new();
    if !args.confirm {
        let current = majestical_services::describer_config::show(catalog, &notices)?;
        return Ok(super::with_notices(
            json!({
                "backend": args.backend,
                "model": args.model,
                "base_url": args.base_url,
                "current": current,
                "would": format!(
                    "configure the describer backend to {} model '{}'",
                    backend.as_str(), args.model
                ),
            }),
            notices.drain(),
        ));
    }
    let view = majestical_services::describer_config::set(
        catalog,
        &majestical_services::describer_config::SetArgs {
            backend,
            model: args.model.clone(),
            base_url: args.base_url.clone(),
            api_key: args.api_key.clone(),
        },
        &notices,
    )?;
    Ok(super::with_notices(
        serde_json::to_value(&view)?,
        notices.drain(),
    ))
}

/// Params for `test_describer`.
#[derive(Debug, Deserialize, JsonSchema)]
struct TestDescriberArgs {
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn test_describer_result(catalog: &Path, confirm: bool) -> anyhow::Result<serde_json::Value> {
    let notices = Notices::new();
    let configured = majestical_services::describer_config::show(catalog, &notices)?;
    if !confirm {
        let would = if configured.is_some() {
            "probe the configured backend's connectivity, model presence, and vision capability"
        } else {
            "fail: no describer configured yet"
        };
        return Ok(super::with_notices(
            json!({"configured": configured, "would": would}),
            notices.drain(),
        ));
    }
    let probe = majestical_services::describer_config::test(
        catalog,
        crate::describer_cmd::env_api_key(),
        &notices,
    )?;
    Ok(super::with_notices(
        serde_json::to_value(&probe)?,
        notices.drain(),
    ))
}

/// Params for `rm_saved_search`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RmSavedSearchArgs {
    name: String,
    /// `false` (default) returns a dry-run description of what would
    /// happen; `true` executes.
    #[serde(default)]
    confirm: bool,
}

fn rm_saved_search_result(
    app: &mut FsApp,
    args: &RmSavedSearchArgs,
) -> anyhow::Result<serde_json::Value> {
    let exists = majestical_services::search::searches_list(app)?
        .into_iter()
        .any(|saved| saved.name == args.name);
    if !args.confirm {
        return Ok(super::with_notices(
            json!({
                "name": args.name,
                "exists": exists,
                "would": format!("remove saved search '{}'", args.name),
            }),
            app.notices().drain(),
        ));
    }
    majestical_services::search::searches_rm(app, &args.name)?;
    Ok(super::with_notices(
        json!({"name": args.name}),
        app.notices().drain(),
    ))
}

#[tool_router(router = write_tool_router, vis = "pub(super)")]
impl MajServer {
    /// Adds a named sync location, initializing its `events/`/`blobs/`
    /// layout. `false` reports whether the name is already configured and
    /// whether the path is currently accessible; `true` adds it.
    #[tool]
    fn add_sync_location(
        &self,
        Parameters(args): Parameters<AddSyncLocationArgs>,
    ) -> CallToolResult {
        confirm_gate(args.confirm, add_sync_location_result(&self.catalog, &args))
    }

    /// Initializes a new catalog directory. Refuses (even with `confirm:
    /// true`) if a catalog already exists at this server's catalog path.
    #[tool]
    fn catalog_init(&self, Parameters(args): Parameters<CatalogInitArgs>) -> CallToolResult {
        confirm_gate(
            args.confirm,
            catalog_init_result(&self.catalog, &self.machine_id, &self.author, args.confirm),
        )
    }

    /// Works one pass of the derivation queue (thumbnails, embeddings,
    /// keyframes, transcripts, OCR, PDF text, captions). Always a single
    /// pass — there is no `--watch` equivalent over MCP. The describer API
    /// key comes from `MAJ_OPENROUTER_KEY`, same as the CLI.
    #[tool]
    fn index_run(&self, Parameters(args): Parameters<IndexRunArgs>) -> CallToolResult {
        let kinds = match parse_index_kinds(args.kinds.as_deref()) {
            Ok(kinds) => kinds,
            Err(err) => return super::tool_error(err),
        };
        let result = if args.confirm {
            index_run_exec(&self.catalog, &self.machine_id, &self.author, &kinds, &args)
        } else {
            match self.open_app() {
                Ok(app) => index_run_dry(&app, &self.catalog, &kinds, &args),
                Err(result) => return result,
            }
        };
        confirm_gate(args.confirm, result)
    }

    /// Verified copy from a source directory into a PARA-routed
    /// destination. `false` returns `ingest::plan`'s dry-run plan (no
    /// destination is touched); `true` runs the verified copy.
    #[tool]
    fn ingest_source(&self, Parameters(args): Parameters<IngestSourceArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        confirm_gate(
            args.confirm,
            ingest_source_result(&self.catalog, &mut app, &args),
        )
    }

    /// Processes a shared inbox folder: validated contributions plus
    /// manifest-less drops. `false` lists the inbox's current top-level
    /// entries without touching anything; `true` runs a full pass.
    #[tool]
    fn inbox_process(&self, Parameters(args): Parameters<InboxProcessArgs>) -> CallToolResult {
        if !args.confirm {
            return confirm_gate(false, inbox_dry_run(&args.inbox));
        }
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match majestical_services::inbox::process(
            &mut app,
            &self.catalog,
            &majestical_services::inbox::ProcessRequest {
                inbox: args.inbox.clone(),
                dest: args.dest.clone(),
                triage_target: args.triage_target.clone(),
                keep: args.keep,
            },
        ) {
            Ok(outcome) => {
                let failed = outcome.overall_failed();
                match inject_executed(&outcome, true) {
                    Ok(json) if failed => CallToolResult::structured_error(json),
                    Ok(json) => CallToolResult::structured(json),
                    Err(result) => result,
                }
            }
            Err(err) => super::tool_error(err),
        }
    }

    /// Creates (`add`), renames (`rename`), or archives (`archive`) a PARA
    /// node. `archive`'s dry run is `para::archive`'s own real dry-run
    /// plan (`Planned` moves); `add`/`rename` build a `{"would": ...}`
    /// description instead, since creating/renaming has no natural plan of
    /// its own.
    #[tool]
    fn move_para(&self, Parameters(args): Parameters<MoveParaArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        match args.op {
            majestical_services::para::ParaOp::Add => {
                confirm_gate(args.confirm, move_para_add(&mut app, &args))
            }
            majestical_services::para::ParaOp::Rename => {
                confirm_gate(args.confirm, move_para_rename(&mut app, &args))
            }
            majestical_services::para::ParaOp::Archive => move_para_archive(&mut app, &args),
        }
    }

    /// Removes a saved search. `false` reports whether the name currently
    /// exists; `true` removes it.
    #[tool]
    fn rm_saved_search(&self, Parameters(args): Parameters<RmSavedSearchArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        confirm_gate(args.confirm, rm_saved_search_result(&mut app, &args))
    }

    /// Removes a sync location from config (never touches its files).
    /// `false` reports whether the name is currently configured; `true`
    /// removes it.
    #[tool]
    fn rm_sync_location(&self, Parameters(args): Parameters<RmSyncLocationArgs>) -> CallToolResult {
        confirm_gate(args.confirm, rm_sync_location_result(&self.catalog, &args))
    }

    /// Hashes every file under a directory into the catalog as `AssetSeen`
    /// events. `false` reports the resolved volume identity and how many
    /// files would be hashed; `true` scans for real.
    #[tool]
    fn scan_volume(&self, Parameters(args): Parameters<ScanVolumeArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        confirm_gate(args.confirm, scan_volume_result(&mut app, &args))
    }

    /// Configures the caption/tag-suggestion describer backend for this
    /// machine. `false` echoes the currently configured backend (if any)
    /// alongside what would be stored; `true` stores it.
    #[tool]
    fn set_describer(&self, Parameters(args): Parameters<SetDescriberArgs>) -> CallToolResult {
        confirm_gate(args.confirm, set_describer_result(&self.catalog, &args))
    }

    /// Sets an LWW metadata field on an asset. `false` reports the field's
    /// current value (if set) alongside the proposed one; `true` sets it.
    #[tool]
    fn set_metadata(&self, Parameters(args): Parameters<SetMetadataArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        confirm_gate(args.confirm, set_metadata_result(&mut app, &args))
    }

    /// Adds/removes a folksonomy tag, or confirms/rejects a pending AI tag
    /// suggestion. `false` reports the asset's current tags alongside what
    /// would change; `true` applies it.
    #[tool]
    fn tag_assets(&self, Parameters(args): Parameters<TagAssetsArgs>) -> CallToolResult {
        let mut app = match self.open_app() {
            Ok(app) => app,
            Err(result) => return result,
        };
        confirm_gate(
            args.confirm,
            tag_assets_result(&self.catalog, &mut app, &args),
        )
    }

    /// Fetches everything configured locations have that this catalog
    /// doesn't. `false` returns `sync::status`'s planned `behind` rows for
    /// the targeted location(s); `true` pulls for real. A row that ran but
    /// had per-file failures, or every targeted location failing/skipping
    /// outright, is `isError: true` with the full outcome attached — never
    /// a silently incomplete success. If the local-catalog apply step
    /// fails AFTER every location's transfer already completed, the
    /// completed rows still reach the caller (`isError: true`, `rows` +
    /// `error` attached) rather than being discarded.
    #[tool]
    fn sync_pull(&self, Parameters(args): Parameters<SyncTransferArgs>) -> CallToolResult {
        if !args.confirm {
            return confirm_gate(
                false,
                sync_transfer_dry_run(&self.catalog, args.location.as_deref(), false),
            );
        }
        let only = match parse_only(args.only.as_deref()) {
            Ok(only) => only,
            Err(err) => return super::tool_error(err),
        };
        match majestical_services::sync::pull(
            &self.catalog,
            &self.machine_id,
            &self.author,
            &majestical_services::sync::PullRequest {
                location: args.location.as_deref(),
                only,
            },
        ) {
            Ok(outcome) => {
                let failed = outcome.overall_failed();
                match inject_executed(&outcome, true) {
                    Ok(json) if failed => CallToolResult::structured_error(json),
                    Ok(json) => CallToolResult::structured(json),
                    Err(result) => result,
                }
            }
            Err(err) => {
                let (notices, err) = super::split_notices(err);
                match err {
                    ServiceError::SyncPullApplyFailed { rows, source } => {
                        CallToolResult::structured_error(super::with_notices(
                            json!({
                                "rows": rows,
                                "executed": true,
                                "error": format!("{source:#}"),
                            }),
                            notices,
                        ))
                    }
                    other => super::error_blocks_with_notices(notices, other),
                }
            }
        }
    }

    /// Replicates this catalog to configured locations. `false` returns
    /// `sync::status`'s planned `ahead` rows for the targeted location(s);
    /// `true` pushes for real. Same failure polarity as `sync_pull`: a
    /// per-file failure or an all-skipped/failed run is `isError: true`
    /// with the full outcome attached.
    #[tool]
    fn sync_push(&self, Parameters(args): Parameters<SyncTransferArgs>) -> CallToolResult {
        if !args.confirm {
            return confirm_gate(
                false,
                sync_transfer_dry_run(&self.catalog, args.location.as_deref(), true),
            );
        }
        let only = match parse_only(args.only.as_deref()) {
            Ok(only) => only,
            Err(err) => return super::tool_error(err),
        };
        match majestical_services::sync::push(
            &self.catalog,
            &majestical_services::sync::PushRequest {
                location: args.location.as_deref(),
                only,
            },
        ) {
            Ok(outcome) => {
                let failed = outcome.overall_failed();
                match inject_executed(&outcome, true) {
                    Ok(json) if failed => CallToolResult::structured_error(json),
                    Ok(json) => CallToolResult::structured(json),
                    Err(result) => result,
                }
            }
            Err(err) => super::tool_error_split(err),
        }
    }

    /// Probes the configured describer backend's connectivity, model
    /// presence, and (LM Studio only) vision capability. `false` reports
    /// whether a describer is configured without contacting it; `true`
    /// actually probes the live backend — the reason this tool is
    /// confirm-gated at all, unlike every other read-only-looking probe in
    /// this server.
    #[tool]
    fn test_describer(&self, Parameters(args): Parameters<TestDescriberArgs>) -> CallToolResult {
        confirm_gate(
            args.confirm,
            test_describer_result(&self.catalog, args.confirm),
        )
    }

    /// Re-verifies a destination against its ASC MHL history, appending a
    /// new generation. See [`verify_volume_result`]'s doc for the
    /// dry-run/polarity rationale — this tool is the one exception in this
    /// module where the real run always mutates regardless of what it
    /// finds.
    #[tool]
    #[expect(
        clippy::unused_self,
        reason = "verify needs no catalog (it re-checks a directory's own ASC MHL history), \
                  matching the CLI's own `Cmd::Verify` — every other tool method needs &self for \
                  self.catalog/open_app, so keeping the signature uniform beats a one-off \
                  associated function"
    )]
    fn verify_volume(&self, Parameters(args): Parameters<VerifyVolumeArgs>) -> CallToolResult {
        verify_volume_result(&args)
    }
}

/// Direct unit tests for this module's pure request-parsing helpers —
/// cheaper and more precise than driving them through a full `maj mcp`
/// round trip in `mcp_smoke.rs`, and closes cargo-mutants survivors none of
/// that suite's tool-level tests happen to call these functions with more
/// than one `only`/`op`/`kinds` value.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_only_maps_every_value_and_rejects_unknown() {
        assert_eq!(parse_only(None).expect("none"), None);
        assert_eq!(
            parse_only(Some("segments")).expect("segments"),
            Some(majestical_services::sync::Only::Segments)
        );
        assert_eq!(
            parse_only(Some("transcripts")).expect("transcripts"),
            Some(majestical_services::sync::Only::Transcripts)
        );
        assert!(parse_only(Some("bogus")).is_err());
    }

    #[test]
    fn non_empty_tags_rejects_missing_and_empty_but_not_real_tags() {
        assert!(non_empty_tags(None).is_err());
        assert!(non_empty_tags(Some(&vec![])).is_err());
        let tags = vec!["kf".to_string()];
        assert_eq!(non_empty_tags(Some(&tags)).expect("non-empty"), &tags[..]);
    }

    #[test]
    fn parse_index_kinds_defaults_to_every_kind_and_rejects_unknown() {
        let all: BTreeSet<String> = majestical_services::index::VALID_KINDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(parse_index_kinds(None).expect("default"), all);
        let thumbs = ["thumbs".to_string()];
        let parsed = parse_index_kinds(Some(&thumbs)).expect("thumbs");
        assert_eq!(parsed, BTreeSet::from(["thumbs".to_string()]));
        assert!(parse_index_kinds(Some(&["bogus".to_string()])).is_err());
    }
}
