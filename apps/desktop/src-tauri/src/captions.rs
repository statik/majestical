//! The Captions settings commands and the head's cached describer key.
//!
//! The key is resolved (`MAJ_OPENROUTER_KEY`, then the Keychain) at
//! startup, when a catalog is adopted, and after a save or a clear — never
//! per scheduler tick, so a denied macOS prompt is asked once, not every
//! poll.
//!
//! The desktop twin of `crates/cli/src/describer_key.rs`: same rules, same
//! order, a different head. The pure decisions — where a new key goes, what
//! `set` does with the file's key, what a view may show — all live in
//! `majestical_services::describer_config`; this module executes them.
use crate::commands::{AppState, CatalogCfg, CommandError, blocking, require_catalog};
use crate::indexer::SchedulerWake;
use majestical_secrets::{HeadKeySource, KeyStore, ResolvedKey, SystemKeyStore};
use majestical_services::describer_config::{
    self, DescriberBackend, DescriberConfigView, DescriberProbe, KeyPresence, SetArgs,
};
use majestical_services::notices::Notices;
use serde::{Deserialize, Serialize};
use std::sync::{PoisonError, RwLock};
use tauri::State;

/// What the head says when the Keychain refuses to take a key. The store's
/// own message is deliberately dropped: this is the whole failure the user
/// is shown (mockup frame 8), and nothing about a key's storage belongs in
/// the Settings surface's error line.
const KEYCHAIN_WRITE_REFUSED: &str =
    "Could not store the key in the macOS Keychain — nothing was saved.";

/// Managed state: what the scheduler and doctor use as the head's key.
/// Refilled by [`refresh_key`] only — at startup, on `adopt_catalog`, and
/// after a save or a clear.
pub struct DescriberKeyCache(pub RwLock<ResolvedKey>);

impl Default for DescriberKeyCache {
    fn default() -> Self {
        Self(RwLock::new(ResolvedKey {
            key: None,
            source: HeadKeySource::Absent,
            notice: None,
        }))
    }
}

impl DescriberKeyCache {
    /// The cached key, cloned out so no lock is held across a service call.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        self.read(|resolved| resolved.key.clone())
    }

    /// What doctor's describer row and `describer_config::show` are told.
    #[must_use]
    pub fn presence(&self) -> KeyPresence {
        match self.read(|resolved| resolved.source) {
            HeadKeySource::Env => KeyPresence::Env,
            HeadKeySource::Keychain => KeyPresence::Keychain,
            HeadKeySource::Absent => KeyPresence::Absent,
        }
    }

    /// The store-failure notice the last resolve produced, if any. Never
    /// contains a key.
    fn notice(&self) -> Option<String> {
        self.read(|resolved| resolved.notice.clone())
    }

    /// A poisoned lock is recovered, never propagated: this state must not
    /// get stuck.
    fn read<T>(&self, f: impl FnOnce(&ResolvedKey) -> T) -> T {
        f(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The three things a re-resolve needs. One bundle rather than three
/// parameters because both [`CaptionDeps`] and `adopt_catalog` carry it, and
/// `adopt_catalog` already has four parameters of its own.
pub struct KeyRefresh<'a> {
    pub cache: &'a DescriberKeyCache,
    pub store: &'a dyn KeyStore,
    /// `MAJ_OPENROUTER_KEY`'s value, when set and not empty. A field rather
    /// than a read, so a test can say what the environment held without
    /// mutating the process's.
    pub env: Option<String>,
}

impl<'a> KeyRefresh<'a> {
    /// This process's environment, and `store`.
    #[must_use]
    pub fn ambient(cache: &'a DescriberKeyCache, store: &'a dyn KeyStore) -> Self {
        Self {
            cache,
            store,
            env: crate::commands::env_api_key(),
        }
    }
}

/// What the mutating impls need, kept to one parameter each.
pub struct CaptionDeps<'a> {
    pub keys: KeyRefresh<'a>,
    pub wake: &'a SchedulerWake,
}

/// Re-resolves and replaces the cached key. A poisoned lock is recovered,
/// never propagated: this state must not get stuck.
///
/// Reads the STORED backend, so a caller that has just changed it must call
/// this AFTER the new config is written. With no catalog there is no backend
/// to ask about, so the environment alone answers and the store is never
/// touched.
pub fn refresh_key(keys: &KeyRefresh<'_>, cfg: Option<&CatalogCfg>) {
    // `wants_keychain` swallows its own read failure (the verb that follows
    // reports it), so its notices sink is a local one nothing drains.
    let wants = cfg.is_some_and(|cfg| {
        describer_config::wants_keychain(&cfg.catalog, &Notices::new())
    });
    let resolved = majestical_secrets::resolve(keys.env.clone(), wants, keys.store);
    *keys.cache.0.write().unwrap_or_else(PoisonError::into_inner) = resolved;
}

/// The Captions section's whole state: the configured describer (`null` when
/// there is none), whether this platform can store a key at all — the cue to
/// hide the save/remove controls — and any notices the read collected.
#[derive(Debug, Serialize)]
pub struct DescriberSettingsOutcome {
    pub describer: Option<DescriberConfigView>,
    pub keychain_supported: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// The Settings form as it arrives. No `Debug` would help here — `api_key`
/// is a real key — so it is hand-written below.
#[derive(Deserialize)]
pub struct SaveDescriberReq {
    pub backend: DescriberBackend,
    pub model: String,
    pub base_url: Option<String>,
    /// Absent or blank means "keep the stored key".
    pub api_key: Option<String>,
}

/// By hand, not derived: a derived `Debug` would print the key.
impl std::fmt::Debug for SaveDescriberReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveDescriberReq")
            .field("backend", &self.backend)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// `describer test`'s probe plus the call's notices — the probe struct is
/// services' and carries none of its own.
#[derive(Debug, Serialize)]
pub struct DescriberProbeOutcome {
    #[serde(flatten)]
    pub probe: DescriberProbe,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// A `CommandError` that keeps `notices`: the failure-path counterpart of an
/// outcome's `notices` field, the same contract `CommandError` already has
/// for a `ServiceError`'s own.
fn carrying(err: impl Into<anyhow::Error>, notices: Vec<String>) -> CommandError {
    let mut error = CommandError::from(err);
    error.notices.splice(0..0, notices);
    error
}

/// Reads back what is configured now — the shape every mutating verb answers
/// with, so the surface never has to ask again. `earlier` is whatever the
/// step before collected; a failing `show` carries those notices too.
fn settings_outcome(
    cfg: &CatalogCfg,
    cache: &DescriberKeyCache,
    store: &dyn KeyStore,
    earlier: Vec<String>,
) -> Result<DescriberSettingsOutcome, CommandError> {
    let notices = Notices::new();
    let shown = describer_config::show(&cfg.catalog, cache.presence(), &notices);
    let mut collected = earlier;
    collected.extend(cache.notice());
    collected.extend(notices.drain());
    match shown {
        Ok(describer) => Ok(DescriberSettingsOutcome {
            describer,
            keychain_supported: store.supported(),
            notices: collected,
        }),
        Err(err) => Err(carrying(err, collected)),
    }
}

/// The Captions section's state on load.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or
/// `describer.toml` can't be read or parsed.
pub fn describer_settings_impl(
    cfg: &CatalogCfg,
    cache: &DescriberKeyCache,
    store: &dyn KeyStore,
) -> Result<DescriberSettingsOutcome, CommandError> {
    settings_outcome(cfg, cache, store, Vec::new())
}

/// Saves the Settings form: the key first, then the config, then the cache,
/// then the scheduler, then the echo.
///
/// The Keychain write must come FIRST and stop everything on failure — the
/// file half of an `OpenRouter` plan is `Clear`, so writing the file first
/// and then failing the Keychain write would leave no key anywhere. Which
/// half a key goes to is `plan_key_write`'s decision, not this head's; a
/// local backend's token never reaches the store.
///
/// # Errors
/// Returns an error if the model is blank, the Keychain refuses the key, or
/// the config can't be written.
pub fn save_describer_impl(
    cfg: &CatalogCfg,
    req: SaveDescriberReq,
    deps: &CaptionDeps<'_>,
) -> Result<DescriberSettingsOutcome, CommandError> {
    let model = req.model.trim().to_string();
    if model.is_empty() {
        return Err(CommandError::new("a model name is required"));
    }
    let key = req
        .api_key
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty());
    // A blank field means "the backend's default", which `set` spells as
    // `None` — storing the empty string verbatim would point the describer
    // at nothing.
    let base_url = req
        .base_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty());
    let backend = req.backend.into();
    let write = describer_config::plan_key_write(backend, deps.keys.store.supported(), key);
    if let Some(key) = &write.keychain {
        deps.keys
            .store
            .store(key)
            .map_err(|_| CommandError::new(KEYCHAIN_WRITE_REFUSED))?;
    }
    let notices = Notices::new();
    // `set` drops a carried file key across a backend switch itself; that
    // rule is services', not this head's, so nothing here reimplements it.
    let stored = describer_config::set(
        &cfg.catalog,
        &SetArgs {
            backend,
            model,
            base_url,
            file_key: write.file,
        },
        &notices,
    );
    let earlier = notices.drain();
    if let Err(err) = stored {
        return Err(carrying(err, earlier));
    }
    // After `set`, never before: `wants_keychain` reads the STORED backend.
    refresh_key(&deps.keys, Some(cfg));
    deps.wake.nudge();
    settings_outcome(cfg, deps.keys.cache, deps.keys.store, earlier)
}

/// Removes the stored key from both places it can live.
///
/// An unreadable `describer.toml` stops the whole thing before the store is
/// touched: clearing the Keychain and then failing on the file would delete
/// the one copy of a key while reporting an error that never says so. The
/// Keychain goes first for the same reason its write does — a refusal then
/// leaves the file as it was.
///
/// # Errors
/// Returns an error if `describer.toml` can't be read, the Keychain refuses
/// the delete, or `describer.toml` can't be rewritten.
pub fn clear_describer_key_impl(
    cfg: &CatalogCfg,
    deps: &CaptionDeps<'_>,
) -> Result<DescriberSettingsOutcome, CommandError> {
    let notices = Notices::new();
    let loaded = describer_config::load_config(&cfg.catalog, &notices);
    let mut earlier = notices.drain();
    if let Err(err) = loaded {
        return Err(carrying(err, earlier));
    }
    if deps.keys.store.supported() {
        deps.keys.store.delete().map_err(|err| {
            CommandError::new(format!(
                "could not remove the key from the macOS Keychain: {err}"
            ))
        })?;
    }
    let notices = Notices::new();
    let cleared = describer_config::clear_file_key(&cfg.catalog, &notices);
    earlier.extend(notices.drain());
    if let Err(err) = cleared {
        return Err(carrying(err, earlier));
    }
    refresh_key(&deps.keys, Some(cfg));
    deps.wake.nudge();
    settings_outcome(cfg, deps.keys.cache, deps.keys.store, earlier)
}

/// Live-probes the configured backend with `key` — the cached one, supplied
/// by the caller rather than read here, the same seam every other head uses.
///
/// # Errors
/// Returns an error if no describer is configured or the backend can't be
/// reached at all.
pub fn test_describer_impl(
    cfg: &CatalogCfg,
    key: Option<String>,
) -> Result<DescriberProbeOutcome, CommandError> {
    let notices = Notices::new();
    let probe = describer_config::test(&cfg.catalog, key, &notices);
    let collected = notices.drain();
    match probe {
        Ok(probe) => Ok(DescriberProbeOutcome {
            probe,
            notices: collected,
        }),
        Err(err) => Err(carrying(err, collected)),
    }
}

/// The system store under the service name `MAJ_KEYCHAIN_SERVICE` names, or
/// the default — the same override `crates/cli/src/describer_key.rs` honors.
/// This is a safety requirement, not a convenience: without it an e2e or dev
/// run reads, writes and deletes the developer's own Keychain item.
#[must_use]
pub fn system_store() -> SystemKeyStore {
    SystemKeyStore::new(std::env::var(majestical_secrets::SERVICE_ENV).ok())
}

/// The configured describer and where its key comes from, or `null` when
/// none is configured yet.
///
/// # Errors
/// Returns an error if no catalog is selected or `describer.toml` can't be
/// read.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn describer_settings(
    state: State<'_, AppState>,
    cache: State<'_, DescriberKeyCache>,
) -> Result<DescriberSettingsOutcome, CommandError> {
    describer_settings_impl(&require_catalog(&state)?, &cache, &system_store())
}

/// Stores the describer configuration, and the key with it.
///
/// # Errors
/// Returns an error if no catalog is selected, the model is blank, the
/// Keychain refuses the key, or the config can't be written.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn save_describer(
    state: State<'_, AppState>,
    cache: State<'_, DescriberKeyCache>,
    wake: State<'_, SchedulerWake>,
    req: SaveDescriberReq,
) -> Result<DescriberSettingsOutcome, CommandError> {
    save_describer_impl(
        &require_catalog(&state)?,
        req,
        &CaptionDeps {
            keys: KeyRefresh::ambient(&cache, &system_store()),
            wake: &wake,
        },
    )
}

/// Removes the stored key from the Keychain and from `describer.toml`.
///
/// # Errors
/// Returns an error if no catalog is selected, the Keychain refuses the
/// delete, or `describer.toml` can't be read or rewritten.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn clear_describer_key(
    state: State<'_, AppState>,
    cache: State<'_, DescriberKeyCache>,
    wake: State<'_, SchedulerWake>,
) -> Result<DescriberSettingsOutcome, CommandError> {
    clear_describer_key_impl(
        &require_catalog(&state)?,
        &CaptionDeps {
            keys: KeyRefresh::ambient(&cache, &system_store()),
            wake: &wake,
        },
    )
}

/// Live-probes the configured backend: is it reachable, does it list the
/// model, and does it accept the key.
///
/// `async`, and on the blocking pool: this one makes network calls.
///
/// # Errors
/// Returns an error if no catalog is selected, no describer is configured,
/// or the backend can't be reached at all.
#[tauri::command]
pub async fn test_describer(
    state: State<'_, AppState>,
    cache: State<'_, DescriberKeyCache>,
) -> Result<DescriberProbeOutcome, CommandError> {
    let cfg = require_catalog(&state)?;
    let key = cache.key();
    blocking(move || test_describer_impl(&cfg, key)).await
}

/// Test-only: the process environment is global, so every unit test in this
/// crate that sets a variable takes this first. Shared with `indexer.rs`'s
/// cached-key test rather than declared twice — two locks would not
/// serialize against each other.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test-only: runs `f` with `MAJ_STATE_DIR` pointed at a fresh tempdir, so
/// `describer.toml` lands there instead of in the user's real data dir. The
/// eleven-line twin of `tests/commands.rs`'s helper, which a unit-test
/// module cannot import.
#[cfg(test)]
pub(crate) fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = tempfile::tempdir().expect("state dir");
    // SAFETY: serialized by ENV_LOCK; no other thread reads env mid-test.
    unsafe { std::env::set_var("MAJ_STATE_DIR", state.path()) };
    let out = f();
    drop(state);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CatalogCfg;
    use crate::indexer::SchedulerWake;
    use majestical_describe::BackendKind;
    use majestical_secrets::{KeyStore, MemoryKeyStore, PanickingKeyStore};
    use majestical_services::describer_config::{
        self, DescriberBackend, FileKey, KeyPresence, KeySource, SetArgs,
    };
    use majestical_services::notices::Notices;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// `dir` itself is the catalog root: the state dir these verbs write to
    /// is keyed off a CANONICALIZED root, so the directory has to exist. No
    /// catalog need be initialized inside it — `describer.toml` is
    /// per-machine state, not catalog content, the same shape
    /// `crates/cli/src/describer_key.rs`'s tests use.
    fn cfg_for(dir: &Path) -> CatalogCfg {
        CatalogCfg {
            catalog: dir.to_path_buf(),
            machine_id: "gui-test".into(),
            author: "gui-test".into(),
        }
    }

    /// Where `describer.toml` lands for `cfg` under this test's state dir.
    fn config_path(cfg: &CatalogCfg) -> PathBuf {
        describer_config::config_path(&cfg.catalog, &Notices::new()).expect("config path")
    }

    /// The file `set` wrote, for the two assertions that must look at it:
    /// that it holds no key, and that it was never written at all.
    fn config_text(cfg: &CatalogCfg) -> String {
        std::fs::read_to_string(config_path(cfg)).expect("read describer.toml")
    }

    fn save_req(backend: DescriberBackend, api_key: Option<&str>) -> SaveDescriberReq {
        SaveDescriberReq {
            backend,
            model: "m".to_string(),
            base_url: None,
            api_key: api_key.map(str::to_string),
        }
    }

    fn deps<'a>(
        cache: &'a DescriberKeyCache,
        store: &'a dyn KeyStore,
        wake: &'a SchedulerWake,
    ) -> CaptionDeps<'a> {
        CaptionDeps {
            keys: KeyRefresh {
                cache,
                store,
                env: None,
            },
            wake,
        }
    }

    /// Whether the scheduler was nudged: a nudged `wait` returns at once and
    /// consumes the nudge, an un-nudged one sleeps the whole pause. Same
    /// timing shape as `indexer.rs`'s own
    /// `wake_returns_early_when_nudged_and_consumes_the_nudge`.
    fn nudged(wake: &SchedulerWake) -> bool {
        let start = Instant::now();
        wake.wait(Duration::from_millis(400));
        start.elapsed() < Duration::from_millis(200)
    }

    #[test]
    fn settings_of_an_unconfigured_catalog_is_null_describer() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let cache = DescriberKeyCache::default();

            let outcome = describer_settings_impl(&cfg, &cache, &MemoryKeyStore::default())
                .expect("settings");
            assert!(outcome.describer.is_none());
            assert!(outcome.keychain_supported);
            assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);

            let unsupported = MemoryKeyStore {
                unsupported: true,
                ..MemoryKeyStore::default()
            };
            let outcome = describer_settings_impl(&cfg, &cache, &unsupported).expect("settings");
            assert!(!outcome.keychain_supported);
        });
    }

    /// The whole save path in one: the key reaches the Keychain, nothing of
    /// it reaches `describer.toml`, the cache is refilled from the backend
    /// `set` just stored (not the one it replaced), and the scheduler is
    /// woken so a caption batch can start without waiting out a tick.
    #[test]
    fn save_stores_the_key_in_the_keychain_and_leaves_the_file_keyless() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let store = MemoryKeyStore::default();
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();

            let outcome = save_describer_impl(
                &cfg,
                save_req(DescriberBackend::OpenRouter, Some("sk-test")),
                &deps(&cache, &store, &wake),
            )
            .expect("save");

            assert_eq!(store.held().as_deref(), Some("sk-test"));
            assert_eq!(cache.key().as_deref(), Some("sk-test"));
            assert_eq!(cache.presence(), KeyPresence::Keychain);
            let view = outcome.describer.expect("configured");
            assert_eq!(view.key_source, KeySource::Keychain);
            assert_eq!(view.model, "m");
            let text = config_text(&cfg);
            assert!(!text.contains("api_key"), "describer.toml must hold no key");
            assert!(!text.contains("sk-test"), "describer.toml must hold no key");
            assert!(nudged(&wake), "the scheduler must be woken after a save");
        });
    }

    /// The Settings form sends its key field on every save, so a user who
    /// only changes the model sends a blank one — that must leave the stored
    /// key exactly where it is, not clear it.
    #[test]
    fn save_with_a_blank_key_keeps_the_stored_key() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let store = MemoryKeyStore::holding("sk-test");
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();

            let mut req = save_req(DescriberBackend::OpenRouter, Some("   "));
            req.model = "m2".to_string();
            let outcome =
                save_describer_impl(&cfg, req, &deps(&cache, &store, &wake)).expect("save");

            assert_eq!(store.held().as_deref(), Some("sk-test"));
            assert_eq!(cache.key().as_deref(), Some("sk-test"));
            let view = outcome.describer.expect("configured");
            assert_eq!(view.model, "m2", "the rest of the form still applies");
            assert_eq!(view.key_source, KeySource::Keychain);
        });
    }

    #[test]
    fn save_with_a_blank_model_is_refused_and_writes_nothing() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let store = MemoryKeyStore::default();
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();

            let mut req = save_req(DescriberBackend::OpenRouter, Some("sk-test"));
            req.model = "   ".to_string();
            let err = save_describer_impl(&cfg, req, &deps(&cache, &store, &wake))
                .expect_err("a blank model is refused");

            assert_eq!(err.message, "a model name is required");
            assert_eq!(store.held(), None, "nothing reaches the Keychain");
            assert!(!config_path(&cfg).exists(), "nothing reaches the file");
            assert!(
                !nudged(&wake),
                "a refused save must not wake the scheduler"
            );
        });
    }

    /// Mockup frame 8: the Keychain refusing the write is the whole error,
    /// and the store's own words are dropped with it. Nothing is written
    /// anywhere, because the file half of the plan is `Clear`.
    #[test]
    fn a_refused_keychain_write_saves_nothing_and_says_so_without_the_key() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let store = MemoryKeyStore::failing("denied");
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();

            let err = save_describer_impl(
                &cfg,
                save_req(DescriberBackend::OpenRouter, Some("sk-test")),
                &deps(&cache, &store, &wake),
            )
            .expect_err("a refused Keychain write fails the save");

            assert_eq!(
                err.message,
                "Could not store the key in the macOS Keychain — nothing was saved."
            );
            let rendered = format!("{err:?}");
            assert!(!rendered.contains("sk-test"), "the key must never render");
            assert!(!rendered.contains("denied"), "the store's message is dropped");
            assert!(!config_path(&cfg).exists(), "nothing was saved");
        });
    }

    /// A local backend's token is only ever the file's: the one Keychain
    /// item belongs to `OpenRouter`, and a `PanickingKeyStore` proves the
    /// save never reaches for it.
    #[test]
    fn save_for_ollama_never_touches_the_store() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();

            let outcome = save_describer_impl(
                &cfg,
                save_req(DescriberBackend::Ollama, Some("sk-test")),
                &deps(&cache, &PanickingKeyStore, &wake),
            )
            .expect("save");

            let view = outcome.describer.expect("configured");
            assert_eq!(view.backend, "ollama");
            assert_eq!(view.key_source, KeySource::File);
            assert_eq!(cache.presence(), KeyPresence::Absent);
        });
    }

    /// Both halves of a clear, seeded together so each is discriminated: the
    /// Keychain item goes, the file's `api_key` goes, and the cache that the
    /// scheduler and doctor read empties with them.
    #[test]
    fn clear_empties_the_cache_and_reports_source_none() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let store = MemoryKeyStore::holding("sk-test");
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();
            describer_config::set(
                &cfg.catalog,
                &SetArgs {
                    backend: BackendKind::OpenRouter,
                    model: "m".to_string(),
                    base_url: None,
                    file_key: FileKey::Set("sk-test-2".to_string()),
                },
                &Notices::new(),
            )
            .expect("seed a config holding a file key too");

            let outcome =
                clear_describer_key_impl(&cfg, &deps(&cache, &store, &wake)).expect("clear");

            assert_eq!(store.held(), None, "the Keychain item must be deleted");
            assert!(
                !config_text(&cfg).contains("api_key"),
                "the file's key must be deleted"
            );
            assert_eq!(cache.key(), None);
            assert_eq!(cache.presence(), KeyPresence::Absent);
            assert_eq!(
                outcome.describer.expect("configured").key_source,
                KeySource::Absent
            );
            assert!(nudged(&wake), "the scheduler must be woken after a clear");
        });
    }

    /// `MAJ_OPENROUTER_KEY` wins and the store is never consulted — the
    /// `PanickingKeyStore` is the assertion.
    #[test]
    fn env_overrides_and_the_view_says_env() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();
            let deps = CaptionDeps {
                keys: KeyRefresh {
                    cache: &cache,
                    store: &PanickingKeyStore,
                    env: Some("sk-test".to_string()),
                },
                wake: &wake,
            };

            let outcome =
                save_describer_impl(&cfg, save_req(DescriberBackend::OpenRouter, None), &deps)
                    .expect("save");

            assert_eq!(cache.key().as_deref(), Some("sk-test"));
            assert_eq!(cache.presence(), KeyPresence::Env);
            assert_eq!(
                outcome.describer.expect("configured").key_source,
                KeySource::Env
            );
        });
    }

    /// A blank `base_url` is the Settings form's empty field, which means
    /// "the backend's default" — storing it verbatim would point the
    /// describer at nothing.
    #[test]
    fn save_with_a_blank_base_url_uses_the_backend_default() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let cache = DescriberKeyCache::default();
            let wake = SchedulerWake::default();
            let mut req = save_req(DescriberBackend::Ollama, None);
            req.base_url = Some("  ".to_string());

            let outcome = save_describer_impl(&cfg, req, &deps(&cache, &PanickingKeyStore, &wake))
                .expect("save");

            assert_eq!(
                outcome.describer.expect("configured").base_url,
                "http://localhost:11434"
            );
        });
    }

    /// No network: an unconfigured catalog is refused before any request.
    #[test]
    fn test_describer_without_a_describer_is_an_error() {
        with_state_dir(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let cfg = cfg_for(dir.path());
            let err = test_describer_impl(&cfg, None).expect_err("nothing to probe");
            assert!(err.message.contains("no describer configured"), "{err:?}");
        });
    }
}
