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
//! covering any code of ours.
//!
//! Only the two search commands are `async`: `search::search`'s semantic
//! layer opens a Lance vector store, which must run off any tokio runtime
//! (see [`majestical_services::runtime`]), and a search is the one call
//! slow enough to be worth handing to the blocking pool as well. The rest
//! read the projection and return promptly.
use crate::config::{self, GuiConfig};
use majestical_services::app::FsApp;
use majestical_services::catalog::AssetDetail;
use majestical_services::error::ServiceError;
use majestical_services::search::{SavedSearch, SearchOutcome, SearchRequest};
use majestical_services::volumes::VolumesOutcome;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use tauri::{AppHandle, Manager, State};

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
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

impl CommandError {
    fn new(message: impl Into<String>) -> Self {
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

fn open_app(cfg: &CatalogCfg) -> Result<FsApp, CommandError> {
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
