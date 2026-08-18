//! One command per verb the GUI uses, each a thin wrapper over
//! `majestical_services` returning that verb's outcome struct as-is —
//! parity by construction, the same rule `maj mcp` follows. Commands open a
//! fresh `FsApp` per call (a long-lived GUI must see changes other
//! processes make to the catalog between calls).
//!
//! Every `#[tauri::command]` here is a one-liner over a plain `*_impl`
//! function. The impls are what `tests/commands.rs` drives directly, and
//! keeping the command layer free of logic also keeps `lib.rs`'s blanket
//! `expect_used`/`exit` expectations (which the Tauri macros need) from
//! covering any code of ours. The five ingest commands follow the same
//! rule over [`crate::ingest`], which holds the one thing in this head
//! that is not a wrapper: a run that outlives its call.
//!
//! The `async` commands are the ones that must not hold an async worker:
//! the two searches (`search::search`'s semantic layer opens a Lance vector
//! store, which must run off any tokio runtime — see
//! [`majestical_services::runtime`] — and a search is slow enough to be
//! worth the blocking pool anyway), the mount-table listing, and the two
//! ingest calls that walk and hash a whole source directory. The rest read
//! the projection and return promptly.
use crate::config::{self, GuiConfig};
use crate::ingest::{
    INGEST_PROGRESS_EVENT, IngestProgress, IngestState, IngestStateWire, ProgressSink, StartIngest,
    cancel_ingest_impl, ingest_state_impl, list_unfinished_ingests_impl, plan_ingest_impl,
    start_ingest_impl,
};
use majestical_services::app::FsApp;
use majestical_services::browse::{BrowseListOutcome, BrowseRequest, BrowseTreeOutcome};
use majestical_services::catalog::AssetDetail;
use majestical_services::error::ServiceError;
use majestical_services::ingest::{IngestPlanOutcome, UnfinishedRunsOutcome};
use majestical_services::para::{self, ArchiveOutcome, ParaOutcome};
use majestical_services::search::{SavedSearch, SearchOutcome, SearchRequest};
use majestical_services::tags::{self, AssignOutcome, TagRenameOutcome, TagsListOutcome};
use majestical_services::volumes::VolumesOutcome;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use tauri::{AppHandle, Emitter, Manager, State};

/// The page size a caller gets when it omits `limit`. Applied inside the
/// impls, not the command wrappers, so it is reachable from a test.
const DEFAULT_LIMIT: usize = 50;

/// This app's catalog wiring — managed Tauri state, rebuilt when the user
/// picks or initializes a catalog.
#[derive(Clone, Debug)]
pub struct CatalogCfg {
    pub catalog: PathBuf,
    pub machine_id: String,
    pub author: String,
}

/// Managed state: `None` until a catalog is chosen or initialized.
pub struct AppState(pub RwLock<Option<CatalogCfg>>);

/// The one error shape every command returns: the full anyhow/`ServiceError`
/// Display chain (where the remedy text already lives, same rule as
/// `maj mcp`'s `tool_error`), plus any notices the failing call collected —
/// the failure-path counterpart of the outcome structs' `notices` field,
/// with the same absent-when-empty wire contract.
/// `Clone` for the one caller that needs the same failure twice: a failed
/// ingest run answers the `start_ingest` still waiting for its run id AND
/// stays in the managed state for a reloaded surface to read (see
/// [`crate::ingest::FinishedIngest`]).
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

impl CommandError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            notices: Vec::new(),
        }
    }
}

impl<E: Into<anyhow::Error>> From<E> for CommandError {
    fn from(err: E) -> Self {
        // `downcast` recovers a `ServiceError` anywhere in anyhow's context
        // chain, dropping any `.context()` strings on it from `message` —
        // put remedy text on the `ServiceError` itself. Wrapping it in
        // another typed error, or reformatting with `anyhow!("{e}")`,
        // hides it from the downcast and leaks the carrier label instead.
        let err: anyhow::Error = err.into();
        let (notices, err) = match err.downcast::<ServiceError>() {
            Ok(ServiceError::WithNotices { notices, source }) => {
                (notices, anyhow::Error::from(*source))
            }
            Ok(other) => (Vec::new(), anyhow::Error::from(other)),
            Err(err) => (Vec::new(), err),
        };
        Self {
            message: format!("{err:#}"),
            notices,
        }
    }
}

/// What the shell needs to decide between the first-run surface and the
/// search surface. An empty `catalog_path` means no catalog has been chosen
/// yet.
#[derive(Debug, Serialize)]
pub struct AppStatus {
    pub catalog_path: String,
    pub catalog_ready: bool,
}

/// `list_saved_searches`'s result: the service verb returns a bare
/// `Vec<SavedSearch>`, so this names the wire object's one field (`saved`,
/// matching `maj mcp`'s `SavedSearchesResult`) and carries the call's
/// notices, which every outcome struct otherwise carries itself.
#[derive(Serialize)]
pub struct SavedSearches {
    pub saved: Vec<SavedSearch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// One volume mounted on this machine right now: its stable volume id, its
/// label, and the directory it is mounted at.
///
/// The archive modal's candidate roots. `para::archive` takes filesystem
/// roots to move a node's materialized directory out of, and nothing in the
/// catalog records where a node was materialized — so the GUI offers what is
/// plugged in now, which is also the only place a move could succeed.
#[derive(Debug, Serialize)]
pub struct MountedRoot {
    pub volume: String,
    pub label: String,
    pub path: String,
}

/// Every volume mounted right now, from
/// [`majestical_services::volume_identity::mounted_volumes`]'s map of volume
/// id → mount point.
///
/// The label is the mount point's last path component, or
/// [`majestical_services::volume_identity::ROOT_LABEL`] for `/` — the same
/// derivation `volume_identity::resolve` does, repeated here rather than
/// calling `resolve` again, which would shell out to `diskutil` a second
/// time per mount for a string already on hand.
///
/// Takes no [`CatalogCfg`]: this reads the mount table, not the catalog.
#[must_use]
pub fn list_mounted_roots_impl() -> Vec<MountedRoot> {
    let mut roots = Vec::new();
    for (volume, path) in majestical_services::volume_identity::mounted_volumes() {
        let label = path.file_name().map_or_else(
            || majestical_services::volume_identity::ROOT_LABEL.to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        roots.push(MountedRoot {
            volume,
            label,
            path: path.display().to_string(),
        });
    }
    roots
}

/// This machine's identity for authored events — the hostname, which is
/// what a `maj` user passes as `--machine-id` on the same machine. Both the
/// machine id and the author take it, matching the CLI's own default of
/// `author = machine_id`.
#[must_use]
pub fn machine_identity() -> String {
    hostname::get().map_or_else(
        |_| "majestical-desktop".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub(crate) fn open_app(cfg: &CatalogCfg) -> Result<FsApp, CommandError> {
    Ok(FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)?)
}

/// `None` — no catalog chosen yet — is a status, not an error: it is
/// exactly what the first-run surface renders, so reporting it as a command
/// failure would make a normal first launch look broken.
#[must_use]
pub fn app_status_impl(cfg: Option<&CatalogCfg>) -> AppStatus {
    let Some(cfg) = cfg else {
        return AppStatus {
            catalog_path: String::new(),
            catalog_ready: false,
        };
    };
    AppStatus {
        catalog_path: cfg.catalog.display().to_string(),
        catalog_ready: majestical_services::catalog::ensure_catalog(&cfg.catalog).is_ok(),
    }
}

/// `limit` of `None` means [`DEFAULT_LIMIT`].
///
/// # Errors
/// Returns an error if the query fails to parse, names an unknown saved
/// search, or the catalog can't be read.
pub fn search_assets_impl(
    cfg: &CatalogCfg,
    query: Option<String>,
    saved: Option<String>,
    limit: Option<usize>,
) -> Result<SearchOutcome, CommandError> {
    let req = SearchRequest {
        query,
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        saved,
        save: None,
    };
    // The Lance scoped-thread rule (see `majestical_services::runtime`): the
    // semantic layer opens a vector store that builds its own tokio runtime.
    let outcome = majestical_services::runtime::run_off_tokio_runtime(|| {
        let mut app = FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)?;
        Ok(majestical_services::search::search(
            &mut app,
            &cfg.catalog,
            &req,
        )?)
    })?;
    Ok(outcome)
}

/// # Errors
/// Returns an error if no saved search has this name, or the catalog can't
/// be read.
pub fn run_saved_search_impl(
    cfg: &CatalogCfg,
    name: String,
    limit: Option<usize>,
) -> Result<SearchOutcome, CommandError> {
    search_assets_impl(cfg, None, Some(name), limit)
}

/// An unknown asset id is `Ok(None)`, not an error — the inspector renders
/// "not found" as a value, same as every other head.
///
/// # Errors
/// Returns an error if the catalog can't be opened or read.
pub fn get_asset_impl(
    cfg: &CatalogCfg,
    asset_id: &str,
) -> Result<Option<AssetDetail>, CommandError> {
    let app = open_app(cfg)?;
    Ok(majestical_services::catalog::get_asset(
        &app,
        &cfg.catalog,
        asset_id,
    )?)
}

/// # Errors
/// Returns an error if the catalog can't be opened or read.
pub fn list_volumes_impl(cfg: &CatalogCfg) -> Result<VolumesOutcome, CommandError> {
    let app = open_app(cfg)?;
    Ok(majestical_services::volumes::volumes_list(
        &app,
        &cfg.catalog,
    )?)
}

/// # Errors
/// Returns an error if the catalog can't be opened or read.
pub fn list_saved_searches_impl(cfg: &CatalogCfg) -> Result<SavedSearches, CommandError> {
    let app = open_app(cfg)?;
    let saved = majestical_services::search::searches_list(&app)?;
    Ok(SavedSearches {
        saved,
        notices: app.notices().drain(),
    })
}

/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
pub fn browse_tree_impl(cfg: &CatalogCfg) -> Result<BrowseTreeOutcome, CommandError> {
    let app = open_app(cfg)?;
    Ok(majestical_services::browse::browse_tree(
        &app,
        &cfg.catalog,
    )?)
}

/// `limit` defaults to [`majestical_services::browse::DEFAULT_LIMIT`],
/// `offset` to 0, `path` to the volume root, `flatten` to true — the same
/// defaults `maj browse list` and the MCP `browse_assets` tool apply, so all
/// three heads agree without a caller having to know the number.
///
/// # Errors
/// Returns an error if no catalog is selected, `volume` doesn't name a
/// cataloged volume, or `sort`/`kind` name an unrecognized value.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors browse_list's own argument list one-for-one — this module's rule is \
              commands stay one-liners over testable impls, which only holds if the impl \
              takes the same arguments the wrapper does; see browse_list's own #[expect] \
              for why the wrapper itself takes seven flat arguments instead of a struct"
)]
pub fn browse_list_impl(
    cfg: &CatalogCfg,
    volume: String,
    path: Option<String>,
    flatten: Option<bool>,
    sort: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<BrowseListOutcome, CommandError> {
    let app = open_app(cfg)?;
    let req = BrowseRequest {
        volume,
        path: path.unwrap_or_default(),
        flatten: flatten.unwrap_or(true),
        sort,
        kind,
        limit: limit.unwrap_or(majestical_services::browse::DEFAULT_LIMIT),
        offset: offset.unwrap_or(0),
    };
    Ok(majestical_services::browse::browse_list(
        &app,
        &cfg.catalog,
        &req,
    )?)
}

/// `maj tags list`: the catalog's live folksonomy vocabulary.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
pub fn list_tags_impl(cfg: &CatalogCfg) -> Result<TagsListOutcome, CommandError> {
    let app = open_app(cfg)?;
    Ok(tags::tags_list(&app, &cfg.catalog)?)
}

/// `maj tag rename`: renames a live tag to a name nothing carries yet.
///
/// # Errors
/// Returns an error if `from` and `to` are the same name, no asset carries
/// `from`, `to` has itself been renamed away, some asset already carries
/// `to` (that's a merge), or the event log can't be read or appended to.
pub fn rename_tag_impl(
    cfg: &CatalogCfg,
    from: &str,
    to: &str,
) -> Result<TagRenameOutcome, CommandError> {
    let mut app = open_app(cfg)?;
    Ok(tags::tag_rename(&mut app, from, to)?)
}

/// `maj tag merge`: folds one live tag into another live tag.
///
/// # Errors
/// Returns an error if `from` and `into_tag` are the same tag, no asset
/// carries `from`, `into_tag` has been renamed away, no asset carries
/// `into_tag` (that's a rename), or the event log can't be read or appended
/// to.
pub fn merge_tags_impl(
    cfg: &CatalogCfg,
    from: &str,
    into_tag: &str,
) -> Result<TagRenameOutcome, CommandError> {
    let mut app = open_app(cfg)?;
    Ok(tags::tag_merge(&mut app, from, into_tag)?)
}

/// `maj tag assign`: adds every tag in `tags` to every asset in `asset_ids`.
///
/// # Errors
/// Returns an error if `asset_ids` or `tags` is empty, every asset fails
/// (the joined per-asset reasons name why), or the event log can't be read
/// or appended to.
pub fn assign_tags_impl(
    cfg: &CatalogCfg,
    asset_ids: &[String],
    tags: &[String],
) -> Result<AssignOutcome, CommandError> {
    let mut app = open_app(cfg)?;
    Ok(tags::tags_assign(&mut app, asset_ids, tags)?)
}

/// `maj para file`: files every asset in `asset_ids` under one PARA node.
///
/// # Errors
/// Returns an error if `asset_ids` is empty, `node` doesn't resolve, every
/// asset fails, or the event log can't be read or appended to.
pub fn file_assets_impl(
    cfg: &CatalogCfg,
    asset_ids: &[String],
    node: &str,
) -> Result<AssignOutcome, CommandError> {
    let mut app = open_app(cfg)?;
    Ok(para::para_file(&mut app, asset_ids, node)?)
}

/// `maj para list`: every PARA node the catalog has ever created.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
pub fn list_para_impl(cfg: &CatalogCfg) -> Result<ParaOutcome, CommandError> {
    let app = open_app(cfg)?;
    Ok(para::para_list(&app, &cfg.catalog)?)
}

/// `maj para add`: creates a node and returns its freshly minted id.
///
/// # Errors
/// Returns an error if `kind` isn't a known PARA kind, an active node
/// already exists at `<kind>/<name>`, or the event log can't be read or
/// appended to.
pub fn add_para_node_impl(
    cfg: &CatalogCfg,
    kind: &str,
    name: &str,
) -> Result<String, CommandError> {
    let mut app = open_app(cfg)?;
    let para::NodeId(id) = para::add(&mut app, kind, name)?;
    Ok(id)
}

/// `maj para rename`: renames a node.
///
/// # Errors
/// Returns an error if `node` doesn't resolve to a known active node, or the
/// event log can't be read or appended to.
pub fn rename_para_node_impl(cfg: &CatalogCfg, node: &str, name: &str) -> Result<(), CommandError> {
    let mut app = open_app(cfg)?;
    Ok(para::rename(&mut app, node, name)?)
}

/// `maj para archive`: archives a node, moving each root's materialized
/// directory first. `dry_run` plans without touching disk or the event log
/// — the GUI's archive modal calls this once with `dry_run: true` to preview
/// and once more with `dry_run: false` to execute.
///
/// A multi-root run that fails partway through still reports the roots
/// already moved (or classified) BEFORE the failing one — folded into the
/// `CommandError`'s `notices` as `moved <from> -> <to>` lines, since the
/// wire shape carries no dedicated `moves` field (unlike MCP's
/// `move_para_archive`, which returns them as structured JSON). Built
/// directly here rather than through the blanket `From<E>` impl above,
/// which has no [`ServiceError::ParaArchivePartial`] arm and would
/// otherwise format only the trailing error, silently dropping every
/// completed move — the CLI's `cmd_para_archive` and MCP's
/// `move_para_archive` both render this same carrier on their own wires.
///
/// # Errors
/// Returns an error if `node` doesn't resolve, the resolved node has no
/// recorded kind/name, the node is of kind `archive`, a root's source
/// directory is missing (not `dry_run`, not already archived), a root's
/// archive target already exists, or a filesystem operation fails.
pub fn archive_node_impl(
    cfg: &CatalogCfg,
    node: &str,
    roots: &[String],
    dry_run: bool,
) -> Result<ArchiveOutcome, CommandError> {
    let mut app = open_app(cfg)?;
    let roots: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
    match para::archive(&mut app, node, &roots, dry_run) {
        Ok(outcome) => Ok(outcome),
        Err(ServiceError::ParaArchivePartial { moves, source }) => Err(CommandError {
            message: format!("{source:#}"),
            notices: moves
                .iter()
                .map(|mv| format!("moved {} -> {}", mv.from.display(), mv.to.display()))
                .collect(),
        }),
        Err(other) => Err(other.into()),
    }
}

/// Refuses a root that already holds a catalog: `catalog::init` is
/// idempotent, so without this guard "initialize" would silently adopt
/// someone else's catalog instead of creating one.
///
/// # Errors
/// Returns an error naming the existing catalog, or any failure creating
/// the new one.
pub fn initialize_catalog_impl(cfg: &CatalogCfg) -> Result<(), CommandError> {
    if majestical_services::catalog::ensure_catalog(&cfg.catalog).is_ok() {
        return Err(CommandError::new(format!(
            "a catalog already exists at {} — open it instead of initializing",
            cfg.catalog.display()
        )));
    }
    Ok(majestical_services::catalog::init(
        &cfg.catalog,
        &cfg.machine_id,
        &cfg.author,
    )?)
}

/// # Errors
/// Returns [`majestical_services::error::ServiceError::NoCatalog`]'s message
/// (naming the `maj catalog init` remedy) if this root holds no catalog.
pub fn use_existing_catalog_impl(cfg: &CatalogCfg) -> Result<(), CommandError> {
    Ok(majestical_services::catalog::ensure_catalog(&cfg.catalog)?)
}

/// Validates `catalog` for this app, persists it, and publishes it to the
/// managed state — the shared body of `initialize_catalog` and
/// `use_existing_catalog`, which differ only in `validate`. Nothing is
/// persisted or published when validation fails.
///
/// # Errors
/// Returns `validate`'s error, or any failure writing the config file.
pub fn adopt_catalog(
    config_dir: &Path,
    state: &AppState,
    catalog: PathBuf,
    validate: fn(&CatalogCfg) -> Result<(), CommandError>,
) -> Result<AppStatus, CommandError> {
    let identity = machine_identity();
    let cfg = CatalogCfg {
        catalog,
        machine_id: identity.clone(),
        author: identity,
    };
    validate(&cfg)?;
    config::store(
        config_dir,
        &GuiConfig {
            catalog: Some(cfg.catalog.clone()),
        },
    )?;
    let status = app_status_impl(Some(&cfg));
    *state.0.write().unwrap_or_else(PoisonError::into_inner) = Some(cfg);
    Ok(status)
}

/// Startup: republishes the catalog the user picked last run. A config
/// naming a catalog that has since disappeared is not an error here — the
/// state carries it and `app_status` reports `catalog_ready: false`, which
/// is what the first-run surface renders.
///
/// # Errors
/// Returns an error if the platform has no config directory.
pub fn restore_persisted_catalog(app: &AppHandle) -> Result<(), tauri::Error> {
    let config_dir = app.path().app_config_dir()?;
    let Some(catalog) = config::load(&config_dir).catalog else {
        return Ok(());
    };
    let identity = machine_identity();
    let state = app.state::<AppState>();
    *state.0.write().unwrap_or_else(PoisonError::into_inner) = Some(CatalogCfg {
        catalog,
        machine_id: identity.clone(),
        author: identity,
    });
    Ok(())
}

/// The chosen catalog, cloned out so no lock is held across a service call.
pub fn selected_catalog(state: &AppState) -> Option<CatalogCfg> {
    state
        .0
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn require_catalog(state: &State<'_, AppState>) -> Result<CatalogCfg, CommandError> {
    selected_catalog(state).ok_or_else(|| {
        CommandError::new("no catalog selected yet — initialize or choose one first")
    })
}

/// Runs a command impl on Tauri's blocking pool, so a slow search never
/// stalls an async worker. The impl itself still hops to a plain OS thread
/// for Lance's sake; this is the outer half of that pairing.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CommandError> + Send + 'static,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|err| CommandError::new(format!("background task failed: {err}")))?
}

/// Whether a catalog is selected and usable — what the shell reads on
/// startup to choose between the first-run surface and the search surface.
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn app_status(state: State<'_, AppState>) -> AppStatus {
    app_status_impl(selected_catalog(&state).as_ref())
}

/// Searches the catalog. `limit` defaults to 50 results.
///
/// # Errors
/// Returns an error if no catalog is selected, the query fails to parse or
/// names an unknown filter, or the catalog can't be read.
#[tauri::command]
pub async fn search_assets(
    state: State<'_, AppState>,
    query: Option<String>,
    saved: Option<String>,
    limit: Option<usize>,
) -> Result<SearchOutcome, CommandError> {
    let cfg = require_catalog(&state)?;
    blocking(move || search_assets_impl(&cfg, query, saved, limit)).await
}

/// Runs a saved search by name. Same outcome shape as [`search_assets`].
///
/// # Errors
/// Returns an error if no catalog is selected, no saved search has this
/// name, or the catalog can't be read.
#[tauri::command]
pub async fn run_saved_search(
    state: State<'_, AppState>,
    name: String,
    limit: Option<usize>,
) -> Result<SearchOutcome, CommandError> {
    let cfg = require_catalog(&state)?;
    blocking(move || run_saved_search_impl(&cfg, name, limit)).await
}

/// Everything the catalog knows about one asset, or `null` if it knows no
/// such asset.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn get_asset(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<Option<AssetDetail>, CommandError> {
    get_asset_impl(&require_catalog(&state)?, &asset_id)
}

/// Every volume the catalog has ever seen, with asset counts and online
/// status.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn list_volumes(state: State<'_, AppState>) -> Result<VolumesOutcome, CommandError> {
    list_volumes_impl(&require_catalog(&state)?)
}

/// Every saved search (name and query text).
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn list_saved_searches(state: State<'_, AppState>) -> Result<SavedSearches, CommandError> {
    list_saved_searches_impl(&require_catalog(&state)?)
}

/// Every volume's folder tree, with a recursive asset count per folder.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn browse_tree(state: State<'_, AppState>) -> Result<BrowseTreeOutcome, CommandError> {
    browse_tree_impl(&require_catalog(&state)?)
}

/// Assets under one folder of one volume, sorted, optionally kind-filtered,
/// and paginated — see [`browse_list_impl`] for the argument defaults. This
/// wrapper's argument order must match `browse_list_impl`'s exactly: it's a
/// plain positional forward with no argument names at the call site to
/// catch a transposition, and — unlike the impl — it's untestable without a
/// webview (see `tests/commands.rs`'s pass-through test for the coverage
/// this layer doesn't get on its own).
///
/// # Errors
/// Returns an error if no catalog is selected, `volume` doesn't name a
/// cataloged volume, or `sort`/`kind` name an unrecognized value.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors BrowseRequest's seven fields one-for-one; a struct param would \
              arrive nested under its own key on the invoke wire, changing the flat-args \
              shape all heads share, so collapsing them into a struct here would only \
              move the count, not remove it"
)]
#[tauri::command]
pub fn browse_list(
    state: State<'_, AppState>,
    volume: String,
    path: Option<String>,
    flatten: Option<bool>,
    sort: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<BrowseListOutcome, CommandError> {
    browse_list_impl(
        &require_catalog(&state)?,
        volume,
        path,
        flatten,
        sort,
        kind,
        limit,
        offset,
    )
}

/// The catalog's live folksonomy vocabulary.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn list_tags(state: State<'_, AppState>) -> Result<TagsListOutcome, CommandError> {
    list_tags_impl(&require_catalog(&state)?)
}

/// Renames a live tag to a name nothing carries yet.
///
/// # Errors
/// Returns an error if `from` and `to` are the same name, no asset carries
/// `from`, `to` has itself been renamed away, some asset already carries
/// `to` (that's a merge), or the event log can't be read or appended to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn rename_tag(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<TagRenameOutcome, CommandError> {
    rename_tag_impl(&require_catalog(&state)?, &from, &to)
}

/// Folds one live tag into another live tag. The Rust parameter is named
/// `into_tag` rather than `into` (a reserved keyword); Tauri's default
/// `rename_all = "camelCase"` renders that on the wire as `intoTag`, and
/// `api.ts`'s `mergeTags` wrapper sends exactly that key — see its own
/// comment for the other half of this pairing.
///
/// # Errors
/// Returns an error if `from` and `into_tag` are the same tag, no asset
/// carries `from`, `into_tag` has been renamed away, no asset carries
/// `into_tag` (that's a rename), or the event log can't be read or appended
/// to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn merge_tags(
    state: State<'_, AppState>,
    from: String,
    into_tag: String,
) -> Result<TagRenameOutcome, CommandError> {
    merge_tags_impl(&require_catalog(&state)?, &from, &into_tag)
}

/// Adds every tag in `tags` to every asset in `asset_ids`.
///
/// # Errors
/// Returns an error if `asset_ids` or `tags` is empty, every asset fails, or
/// the event log can't be read or appended to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn assign_tags(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    tags: Vec<String>,
) -> Result<AssignOutcome, CommandError> {
    assign_tags_impl(&require_catalog(&state)?, &asset_ids, &tags)
}

/// Files every asset in `asset_ids` under one PARA node.
///
/// # Errors
/// Returns an error if `asset_ids` is empty, `node` doesn't resolve, every
/// asset fails, or the event log can't be read or appended to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn file_assets(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    node: String,
) -> Result<AssignOutcome, CommandError> {
    file_assets_impl(&require_catalog(&state)?, &asset_ids, &node)
}

/// Every PARA node the catalog has ever created.
///
/// # Errors
/// Returns an error if no catalog is selected or the catalog can't be read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn list_para(state: State<'_, AppState>) -> Result<ParaOutcome, CommandError> {
    list_para_impl(&require_catalog(&state)?)
}

/// Creates a PARA node and returns its freshly minted id.
///
/// # Errors
/// Returns an error if `kind` isn't a known PARA kind, an active node
/// already exists at `<kind>/<name>`, or the event log can't be read or
/// appended to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn add_para_node(
    state: State<'_, AppState>,
    kind: String,
    name: String,
) -> Result<String, CommandError> {
    add_para_node_impl(&require_catalog(&state)?, &kind, &name)
}

/// Renames a PARA node.
///
/// # Errors
/// Returns an error if `node` doesn't resolve to a known active node, or the
/// event log can't be read or appended to.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn rename_para_node(
    state: State<'_, AppState>,
    node: String,
    name: String,
) -> Result<(), CommandError> {
    rename_para_node_impl(&require_catalog(&state)?, &node, &name)
}

/// Every volume mounted on this machine right now — the roots the archive
/// modal previews a node's move against. Reads the mount table, so it
/// answers whether or not a catalog is selected.
///
/// `async`, and on the blocking pool: resolving each mount's identity shells
/// out to `diskutil` once per mounted volume (see
/// [`majestical_services::volume_identity::resolve`]), which is the same
/// work `search_assets` already hands to that pool rather than run on the
/// main thread.
///
/// # Errors
/// Returns an error only if the background task itself fails to run;
/// enumerating mounts cannot fail, it answers with what it could read.
#[tauri::command]
pub async fn list_mounted_roots() -> Result<Vec<MountedRoot>, CommandError> {
    blocking(|| Ok(list_mounted_roots_impl())).await
}

/// Archives a PARA node. `dry_run: true` previews without touching disk or
/// the event log; the GUI's archive modal calls this once to preview and
/// once more with `dry_run: false` to execute.
///
/// # Errors
/// Returns an error if `node` doesn't resolve, the resolved node has no
/// recorded kind/name, the node is of kind `archive`, a root's source
/// directory is missing (not `dry_run`, not already archived), a root's
/// archive target already exists, or a filesystem operation fails.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn archive_node(
    state: State<'_, AppState>,
    node: String,
    roots: Vec<String>,
    dry_run: bool,
) -> Result<ArchiveOutcome, CommandError> {
    archive_node_impl(&require_catalog(&state)?, &node, &roots, dry_run)
}

/// Creates a catalog at `path`, then selects and persists it.
///
/// # Errors
/// Returns an error if a catalog already exists at `path`, the catalog
/// can't be created, or the choice can't be persisted.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn initialize_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<AppStatus, CommandError> {
    adopt_catalog(
        &app.path().app_config_dir()?,
        &state,
        PathBuf::from(path),
        initialize_catalog_impl,
    )
}

/// Selects and persists the existing catalog at `path`.
///
/// # Errors
/// Returns an error if `path` holds no catalog (naming the `maj catalog
/// init` remedy) or the choice can't be persisted.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn use_existing_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<AppStatus, CommandError> {
    adopt_catalog(
        &app.path().app_config_dir()?,
        &state,
        PathBuf::from(path),
        use_existing_catalog_impl,
    )
}

/// Plans an ingest: what would be copied, skipped as a known duplicate, or
/// rejected, and the destination subdir the layout template renders to.
/// Copies nothing and writes nothing. `template` defaults to
/// [`crate::ingest::DEFAULT_INGEST_TEMPLATE`].
///
/// `async`, and on the blocking pool: planning walks the whole source and
/// hashes every file whose size matches something the catalog already knows.
///
/// # Errors
/// Returns an error if no catalog is selected, `para` doesn't resolve to an
/// active PARA node, or the source walk or the template fails.
#[tauri::command]
pub async fn plan_ingest(
    state: State<'_, AppState>,
    source: String,
    para: String,
    template: Option<String>,
) -> Result<IngestPlanOutcome, CommandError> {
    let cfg = require_catalog(&state)?;
    blocking(move || plan_ingest_impl(&cfg, Path::new(&source), &para, template)).await
}

/// Starts the verified copy and returns the run's id. One run at a time:
/// while one is in flight this refuses, naming the run holding the slot.
///
/// Progress is forwarded to the webview as [`INGEST_PROGRESS_EVENT`] events
/// carrying [`IngestProgress`]; `BytesCopied` is coalesced on the way (see
/// `ingest::BytesThrottle`). The run itself lives on its own thread and outlives
/// the webview — a surface that reloads mid-run reads [`ingest_state`] and
/// re-subscribes.
///
/// # Errors
/// Returns an error if no catalog is selected, a run is already in flight,
/// or this one failed before it was named (see [`crate::ingest::start_ingest_impl`]).
#[tauri::command]
pub async fn start_ingest(
    app: AppHandle,
    state: State<'_, AppState>,
    source: String,
    dests: Vec<String>,
    para: String,
    template: Option<String>,
    resume: Option<String>,
) -> Result<String, CommandError> {
    let cfg = require_catalog(&state)?;
    let req = StartIngest {
        source: PathBuf::from(source),
        dests: dests.into_iter().map(PathBuf::from).collect(),
        para,
        template,
        resume,
    };
    let emitter = app.clone();
    let emit: ProgressSink = Arc::new(move |progress: IngestProgress| {
        // A webview that is reloading, hidden, or gone is not a run
        // failure: the run keeps copying and `ingest_state` re-answers for
        // whoever comes back.
        let _ = emitter.emit(INGEST_PROGRESS_EVENT, progress);
    });
    blocking(move || {
        let ingest = app
            .try_state::<IngestState>()
            .ok_or_else(|| CommandError::new("the ingest job state is not managed"))?;
        start_ingest_impl(&cfg, &ingest, req, emit)
    })
    .await
}

/// Asks the in-flight run to stop after the files already in flight. A
/// no-op when nothing is running, so the surface can wire Stop
/// unconditionally; the stopped run stays resumable by its id.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn cancel_ingest(ingest: State<'_, IngestState>) {
    cancel_ingest_impl(&ingest);
}

/// The in-flight run, or the last one to finish — what the Ingest surface
/// reads on mount so a webview reload never loses a running copy. The
/// finished run's outcome, not the events, is the authority on what landed.
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn ingest_state(ingest: State<'_, IngestState>) -> IngestStateWire {
    ingest_state_impl(&ingest)
}

/// Every run a `resume` could still finish, newest first.
///
/// # Errors
/// Returns an error if no catalog is selected or this machine's run
/// journals can't be listed.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn list_unfinished_ingests(
    state: State<'_, AppState>,
) -> Result<UnfinishedRunsOutcome, CommandError> {
    list_unfinished_ingests_impl(&require_catalog(&state)?)
}

/// The two functions here that `tests/commands.rs` cannot reach: it drives
/// the `*_impl` layer with a `CatalogCfg` in hand, so nothing over there
/// resolves this machine's identity or reads the managed state back out.
#[cfg(test)]
mod tests {
    use super::{AppState, CatalogCfg, machine_identity, selected_catalog};
    use std::path::PathBuf;
    use std::sync::RwLock;

    /// Events this app authors are stamped with the hostname, so a `maj` user
    /// on the same machine passing `--machine-id $(hostname)` writes events
    /// that converge with the app's rather than looking like a second peer.
    #[test]
    fn the_machine_identity_is_this_machines_hostname() {
        let expected = hostname::get().expect("a hostname on a test machine");

        assert_eq!(machine_identity(), expected.to_string_lossy());
    }

    #[test]
    fn the_selected_catalog_is_whatever_was_last_published_to_the_state() {
        let state = AppState(RwLock::new(None));

        assert!(
            selected_catalog(&state).is_none(),
            "before a catalog is chosen there is nothing to select"
        );

        *state.0.write().expect("state") = Some(CatalogCfg {
            catalog: PathBuf::from("/catalogs/main"),
            machine_id: "m".to_string(),
            author: "a".to_string(),
        });

        let selected = selected_catalog(&state).expect("the published catalog");
        assert_eq!(selected.catalog, PathBuf::from("/catalogs/main"));
        assert_eq!(selected.machine_id, "m");
        assert_eq!(selected.author, "a");
    }
}
