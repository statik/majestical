//! The configured describer backend (`describer.toml`): loading it, and the
//! `show`/`set`/`test` verbs over it — plus the pure key-placement decisions
//! (where a caption run's key comes from, where a new key goes, what `set`
//! does with the file's key) that the heads execute. Nothing here reads the
//! environment or the Keychain; a head reports what it found.
use crate::error::ServiceError;
use anyhow::{Context as _, Result, bail};
use majestical_describe::{BackendKind, DescriberConfig, HttpDescriber, KeyVerdict};
use std::path::{Path, PathBuf};

// A real enum rather than a free string so the MCP JSON schema (and a future
// GUI dropdown) carries the closed value set, instead of a typo round-tripping
// to a call-time error. The kebab-case wire strings match `BackendKind`'s own
// `as_str`, which the stored `describer.toml` and `maj describer set
// --backend` already use. The doc comment below ships verbatim as the wire
// `description`, so it is written for the client.
/// Which service generates captions and tag suggestions: `ollama` and
/// `lm-studio` run locally, `open-router` is a hosted API.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum DescriberBackend {
    Ollama,
    LmStudio,
    OpenRouter,
}

impl From<DescriberBackend> for BackendKind {
    fn from(v: DescriberBackend) -> Self {
        match v {
            DescriberBackend::Ollama => Self::Ollama,
            DescriberBackend::LmStudio => Self::LmStudio,
            DescriberBackend::OpenRouter => Self::OpenRouter,
        }
    }
}

/// # Errors
/// Returns an error if the local state dir can't be resolved.
pub fn config_path(catalog_root: &Path, notices: &crate::notices::Notices) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog_root, notices)?.join("describer.toml"))
}

/// Loads the configured describer, if any.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or an existing
/// config file can't be read/parsed.
pub fn load_config(
    catalog_root: &Path,
    notices: &crate::notices::Notices,
) -> Result<Option<DescriberConfig>> {
    let path = config_path(catalog_root, notices)?;
    DescriberConfig::load(&path).with_context(|| format!("load {}", path.display()))
}

/// Which head-side source supplied a key. Services never looks; the head
/// reports what its own resolution (`MAJ_OPENROUTER_KEY`, then the
/// Keychain) found. The ordering between the two lives in the head.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyPresence {
    Env,
    Keychain,
    #[default]
    Absent,
}

/// Where the key a caption run would use comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Env,
    Keychain,
    File,
    /// No key anywhere. `none` on the wire; `Absent` here so it never reads
    /// as `Option::None`.
    #[serde(rename = "none")]
    Absent,
}

/// The same order the run resolves in: the head's key applies to
/// `OpenRouter` only — `effective_api_key`'s rule — and the file's key is
/// last.
#[must_use]
pub fn key_source(config: &DescriberConfig, presence: KeyPresence) -> KeySource {
    let head = match config.backend {
        BackendKind::OpenRouter => presence,
        BackendKind::Ollama | BackendKind::LmStudio => KeyPresence::Absent,
    };
    match (head, &config.api_key) {
        (KeyPresence::Env, _) => KeySource::Env,
        (KeyPresence::Keychain, _) => KeySource::Keychain,
        (KeyPresence::Absent, Some(_)) => KeySource::File,
        (KeyPresence::Absent, None) => KeySource::Absent,
    }
}

/// A describer config safe to hand to any head, including remote ones (MCP,
/// GUI): it has no key field at all, only where a key would come from, so
/// this struct can never expose the real key. `base_url`/`model` are always
/// present in a stored config, so they're plain `String`, not `Option`.
#[derive(serde::Serialize)]
pub struct DescriberConfigView {
    pub backend: String,
    pub base_url: String,
    pub model: String,
    pub key_source: KeySource,
}

/// Builds a render-safe view of `config` — the one place a
/// [`DescriberConfig`] is turned into something a head may show. `presence`
/// is what the head found outside the file; see [`key_source`].
#[must_use]
pub fn to_view(config: &DescriberConfig, presence: KeyPresence) -> DescriberConfigView {
    DescriberConfigView {
        backend: config.backend.as_str().to_string(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        key_source: key_source(config, presence),
    }
}

/// `maj describer show`: the configured describer and its key's source, or
/// `None` when no describer is configured on this machine yet.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or an existing
/// config file can't be read/parsed.
pub fn show(
    catalog_root: &Path,
    presence: KeyPresence,
    notices: &crate::notices::Notices,
) -> Result<Option<DescriberConfigView>, ServiceError> {
    show_impl(catalog_root, presence, notices).map_err(ServiceError::from)
}

fn show_impl(
    catalog_root: &Path,
    presence: KeyPresence,
    notices: &crate::notices::Notices,
) -> Result<Option<DescriberConfigView>> {
    Ok(load_config(catalog_root, notices)?.map(|config| to_view(&config, presence)))
}

/// What `set` does with the key field of `describer.toml`.
#[derive(Clone, PartialEq, Eq)]
pub enum FileKey {
    /// Carry the existing file's key forward — but only when the backend is
    /// unchanged. A stored key belongs to the backend that was configured
    /// when it was written, so a `set` that switches backends drops it
    /// rather than sending one service's credential to another; see
    /// [`set`].
    Keep,
    Set(String),
    Clear,
}

/// Hand-written because `Set` holds a real key, which must never reach a
/// log, a panic message, or a test failure.
impl std::fmt::Debug for FileKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => f.write_str("Keep"),
            Self::Set(_) => f.debug_tuple("Set").field(&"<redacted>").finish(),
            Self::Clear => f.write_str("Clear"),
        }
    }
}

/// Args for `maj describer set`, bundled to keep [`set`] within the house
/// 5-positional-parameter limit.
pub struct SetArgs {
    pub backend: BackendKind,
    pub model: String,
    pub base_url: Option<String>,
    pub file_key: FileKey,
}

/// Where a newly supplied key goes. An `OpenRouter` key goes to a supported
/// Keychain and the file is written keyless; every other key goes to the
/// file. No key supplied leaves the stored key alone, unless `set` is also
/// switching backends — see [`FileKey::Keep`]. `keychain` is written before
/// `file`; see [`plan_key_write`].
pub struct KeyWrite {
    pub keychain: Option<String>,
    pub file: FileKey,
}

/// Hand-written for the same reason as [`FileKey`]'s: `keychain` is a key.
impl std::fmt::Debug for KeyWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyWrite")
            .field("keychain", &self.keychain.as_ref().map(|_| "<redacted>"))
            .field("file", &self.file)
            .finish()
    }
}

/// The one rule for a key a user hands to any head; the head executes the
/// Keychain half and passes [`KeyWrite::file`] to [`set`].
///
/// `backend` is the backend BEING SET. The Keychain is the destination only
/// for `OpenRouter`: the head's key applies to `OpenRouter` alone
/// (`effective_api_key`'s rule), so a local backend's token is only ever
/// read from the file — and the one machine-wide Keychain item must not be
/// overwritten by it, or a later switch to `OpenRouter` would send a local
/// proxy's token to openrouter.ai.
///
/// The head MUST write `keychain` first and stop on failure: `file` is
/// `Clear` in that case, so writing the file first and then failing the
/// Keychain write would leave no key anywhere.
#[must_use]
pub fn plan_key_write(
    backend: BackendKind,
    keychain_supported: bool,
    key: Option<String>,
) -> KeyWrite {
    let to_keychain = match backend {
        BackendKind::OpenRouter => keychain_supported,
        BackendKind::Ollama | BackendKind::LmStudio => false,
    };
    match (key, to_keychain) {
        (None, _) => KeyWrite {
            keychain: None,
            file: FileKey::Keep,
        },
        (Some(key), true) => KeyWrite {
            keychain: Some(key),
            file: FileKey::Clear,
        },
        (Some(key), false) => KeyWrite {
            keychain: None,
            file: FileKey::Set(key),
        },
    }
}

/// `maj describer set`: stores this machine's describer backend config,
/// defaulting `base_url` to the backend's own default when not given. The
/// file's key follows [`SetArgs::file_key`] — note a [`FileKey::Keep`] that
/// switches backends DROPS the file's key with a notice, and a host move
/// under the same backend carries it with a warning. Returns no view: a view names
/// the key's source, which depends on what the head found, so the head
/// calls [`show`] for its echo.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved, the config
/// file can't be written, or — with [`FileKey::Keep`] — an existing config
/// file can't be read/parsed (its key would otherwise be silently dropped).
pub fn set(
    catalog_root: &Path,
    args: &SetArgs,
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    set_impl(catalog_root, args, notices).map_err(ServiceError::from)
}

fn set_impl(catalog_root: &Path, args: &SetArgs, notices: &crate::notices::Notices) -> Result<()> {
    // Resolved once: `set` replaces the whole config, so an omitted
    // `base_url` is a reset to the backend's default, not "leave it alone" —
    // and [`carried_key`] has to compare what will actually be stored.
    let base_url = args
        .base_url
        .clone()
        .unwrap_or_else(|| args.backend.default_base_url().to_string());
    let api_key = match &args.file_key {
        FileKey::Keep => carried_key(
            load_config(catalog_root, notices)?,
            args.backend,
            &base_url,
            notices,
        ),
        FileKey::Set(key) => Some(key.clone()),
        FileKey::Clear => None,
    };
    let config = DescriberConfig {
        backend: args.backend,
        base_url,
        model: args.model.clone(),
        api_key,
    };
    let path = config_path(catalog_root, notices)?;
    config
        .store(&path)
        .with_context(|| format!("write {}", path.display()))
}

/// The file key [`FileKey::Keep`] carries into the newly stored config: the
/// stored one when the backend is unchanged, and nothing when it changed.
///
/// A key in `describer.toml` is a credential for the backend that was
/// configured when it was written, and `base_url` moves with the backend.
/// Carrying it across a switch would send it to a different host — a
/// pre-phase-7G config holds an `OpenRouter` key in `api_key`, so one
/// `describer set --backend lm-studio` would hand a paid hosted key to
/// whatever local process the new URL names, and the reverse would send a
/// local proxy's token to openrouter.ai. Dropping it is not silent: the
/// notice says which backend it belonged to, and never the key itself.
/// A changed `base_url` under the SAME backend is a warning, not a drop.
/// It can be a port move on the same machine, and because `set` replaces the
/// whole config an omitted `--base-url` already resets a custom URL to the
/// default — so dropping here would destroy the key on a bare `--model`
/// change, the very case [`FileKey::Keep`] exists for. The key is carried
/// and the notice names where it will now be sent.
fn carried_key(
    stored: Option<DescriberConfig>,
    backend: BackendKind,
    base_url: &str,
    notices: &crate::notices::Notices,
) -> Option<String> {
    let stored = stored?;
    if stored.backend != backend {
        if stored.api_key.is_some() {
            notices.push(format!(
                "note: the stored API key belonged to {} and was not carried over to {} — \
                 supply a new key if {} needs one",
                stored.backend.as_str(),
                backend.as_str(),
                backend.as_str()
            ));
        }
        return None;
    }
    // Before reading the file's key, because there may be one this function
    // cannot see: an `OpenRouter` key lives in the Keychain and leaves the
    // file keyless, and it is the costliest key to send somewhere new. The
    // wording therefore never asserts that a key exists.
    // `key_source`'s rule: only `OpenRouter` takes a head-side key, so only
    // there can one exist that this function cannot see. A match, not an
    // `==`: a fourth backend must not answer "nothing unseen" by default.
    let may_hold_an_unseen_key = match backend {
        BackendKind::OpenRouter => true,
        BackendKind::Ollama | BackendKind::LmStudio => false,
    };
    if stored.base_url != base_url && (stored.api_key.is_some() || may_hold_an_unseen_key) {
        notices.push(format!(
            "note: any stored API key will now be sent to {base_url} (was {}) — \
             clear the stored key if that is not intended",
            stored.base_url
        ));
    }
    stored.api_key
}

/// What a head reports after `clear-key`. Built by the head (it owns the
/// Keychain half); defined here so all three heads share one shape.
#[derive(Debug, serde::Serialize)]
pub struct ClearKeyOutcome {
    pub keychain_cleared: bool,
    pub file_cleared: bool,
    /// `MAJ_OPENROUTER_KEY` is set, so a key is still supplied.
    pub env_still_supplies: bool,
}

/// True when the stored config's backend is `OpenRouter` — the head's cue to
/// consult the Keychain. Unconfigured is `false`: there is nothing a key
/// would be used for. Unreadable is `false` too: the verb that follows
/// reports the error, and nothing can run meanwhile.
#[must_use]
pub fn wants_keychain(catalog_root: &Path, notices: &crate::notices::Notices) -> bool {
    match load_config(catalog_root, notices) {
        Ok(Some(config)) => match config.backend {
            BackendKind::OpenRouter => true,
            BackendKind::Ollama | BackendKind::LmStudio => false,
        },
        Ok(None) | Err(_) => false,
    }
}

/// Removes `api_key` from an existing `describer.toml`. `Ok(false)` when no
/// config exists or it held no key (and then nothing is written).
///
/// # Errors
/// Returns an error if the local state dir can't be resolved, or the file
/// can't be read/parsed or rewritten.
pub fn clear_file_key(
    catalog_root: &Path,
    notices: &crate::notices::Notices,
) -> Result<bool, ServiceError> {
    clear_file_key_impl(catalog_root, notices).map_err(ServiceError::from)
}

fn clear_file_key_impl(catalog_root: &Path, notices: &crate::notices::Notices) -> Result<bool> {
    let Some(config) = load_config(catalog_root, notices)? else {
        return Ok(false);
    };
    if config.api_key.is_none() {
        return Ok(false);
    }
    let keyless = DescriberConfig {
        api_key: None,
        ..config
    };
    let path = config_path(catalog_root, notices)?;
    keyless
        .store(&path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// What `describer test` learned about the key.
///
/// `Missing` is `OpenRouter` with no key from the caller or the file — the
/// state in which a caption pass fails every item before any request, so no
/// request is made here either. `NotChecked` is a backend with no key
/// endpoint, or a key endpoint that did not judge the key (unreachable, or
/// any answer but success or 401); only that last case pushes a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCheck {
    Accepted,
    Rejected,
    Missing,
    NotChecked,
}

/// Everything `maj describer test` renders: the configured model, whether
/// the live backend actually lists it, (LM Studio only) whether it reports
/// vision support, and (`OpenRouter` only) what it said about the key.
/// `reachable` isn't carried here — [`test`] returns an error instead when
/// the backend can't be reached at all, so by the time a [`DescriberProbe`]
/// exists reachability is already a given.
#[derive(Debug, serde::Serialize)]
pub struct DescriberProbe {
    pub model: String,
    pub model_listed: bool,
    pub vision: Option<bool>,
    pub key: KeyCheck,
}

/// `maj describer test`: live-probes the configured backend. `api_key`
/// comes from the caller (the CLI reads `MAJ_OPENROUTER_KEY`) rather than
/// being read from the environment here, so a non-CLI head (MCP, GUI) can
/// supply it a different way.
///
/// # Errors
/// Returns an error if no describer is configured, or the backend can't be
/// reached at all.
pub fn test(
    catalog_root: &Path,
    api_key: Option<String>,
    notices: &crate::notices::Notices,
) -> Result<DescriberProbe, ServiceError> {
    test_impl(catalog_root, api_key, notices).map_err(ServiceError::from)
}

fn test_impl(
    catalog_root: &Path,
    api_key: Option<String>,
    notices: &crate::notices::Notices,
) -> Result<DescriberProbe> {
    let Some(config) = load_config(catalog_root, notices)? else {
        bail!("no describer configured — run `maj describer set`");
    };
    let base_url = config.base_url.clone();
    let model = config.model.clone();
    // Asked before `HttpDescriber::new` consumes `config` and `api_key`.
    let has_key = config.effective_api_key(api_key.clone()).is_some();
    let describer = HttpDescriber::new(config, api_key);
    let report = describer
        .probe()
        .with_context(|| format!("describer test against {base_url}"))?;
    let key = match (describer.backend(), has_key) {
        (BackendKind::OpenRouter, true) => check_key(&describer, notices),
        (BackendKind::OpenRouter, false) => KeyCheck::Missing,
        (BackendKind::Ollama | BackendKind::LmStudio, true | false) => KeyCheck::NotChecked,
    };
    Ok(DescriberProbe {
        model,
        model_listed: report.model_listed,
        vision: report.vision,
        key,
    })
}

/// Asks the backend about the key. An endpoint that judged nothing is a
/// notice, not an error: the probe itself succeeded. The error renders
/// outermost-only (`{err}`), which names the URL and never the key.
fn check_key(describer: &HttpDescriber, notices: &crate::notices::Notices) -> KeyCheck {
    match describer.check_key() {
        Ok(KeyVerdict::Accepted) => KeyCheck::Accepted,
        Ok(KeyVerdict::Rejected) => KeyCheck::Rejected,
        Err(err) => {
            notices.push(format!("note: the key was not checked ({err})"));
            KeyCheck::NotChecked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notices::Notices;
    use majestical_describe::BackendKind;

    #[test]
    fn describer_backend_wire_strings_are_pinned() {
        for (backend, wire) in [
            (DescriberBackend::Ollama, "ollama"),
            (DescriberBackend::LmStudio, "lm-studio"),
            (DescriberBackend::OpenRouter, "open-router"),
        ] {
            assert_eq!(
                serde_json::to_value(backend).expect("ser"),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<DescriberBackend>(serde_json::json!(wire)).expect("de"),
                backend
            );
        }
        assert!(serde_json::from_value::<DescriberBackend>(serde_json::json!("bogus")).is_err());
    }

    #[test]
    fn show_of_an_unconfigured_catalog_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            show(dir.path(), KeyPresence::Absent, &Notices::new())
                .expect("show")
                .is_none()
        );
    }

    fn config(backend: BackendKind, api_key: Option<&str>) -> DescriberConfig {
        DescriberConfig {
            backend,
            base_url: backend.default_base_url().to_string(),
            model: "m".to_string(),
            api_key: api_key.map(str::to_string),
        }
    }

    fn set_args(backend: BackendKind, model: &str, file_key: FileKey) -> SetArgs {
        SetArgs {
            backend,
            model: model.to_string(),
            base_url: None,
            file_key,
        }
    }

    /// The substring that selects `carried_key`'s host-move notice. One
    /// place, so a reworded notice cannot quietly stop matching in four
    /// tests at once.
    const MOVED: &str = "will now be sent to";

    fn stored(root: &Path) -> DescriberConfig {
        load_config(root, &Notices::new())
            .expect("load_config")
            .expect("a config must be stored")
    }

    #[test]
    fn the_key_source_of_openrouter_is_the_heads_key_then_the_file() {
        for (presence, file_key, source) in [
            (KeyPresence::Env, None, KeySource::Env),
            (KeyPresence::Keychain, None, KeySource::Keychain),
            (KeyPresence::Absent, Some("sk-test"), KeySource::File),
            (KeyPresence::Absent, None, KeySource::Absent),
            (KeyPresence::Env, Some("sk-test"), KeySource::Env),
            (KeyPresence::Keychain, Some("sk-test"), KeySource::Keychain),
        ] {
            assert_eq!(
                key_source(&config(BackendKind::OpenRouter, file_key), presence),
                source,
                "{presence:?}, file key present: {}",
                file_key.is_some()
            );
        }
    }

    /// The head's key is `OpenRouter`'s alone — `effective_api_key`'s rule —
    /// so a local backend reports only what its own file holds.
    #[test]
    fn the_key_source_of_a_local_backend_is_only_ever_the_file() {
        assert_eq!(
            key_source(&config(BackendKind::Ollama, None), KeyPresence::Env),
            KeySource::Absent
        );
        assert_eq!(
            key_source(
                &config(BackendKind::Ollama, Some("sk-test")),
                KeyPresence::Env
            ),
            KeySource::File
        );
        assert_eq!(
            key_source(&config(BackendKind::LmStudio, None), KeyPresence::Keychain),
            KeySource::Absent
        );
    }

    /// [`key_source`] restates `effective_api_key`'s precedence rather than
    /// calling it (it never holds the head's key), so this pins the two
    /// together: a source is named exactly when a run would have a key, and
    /// it is the file exactly when the run's key is the file's.
    #[test]
    fn the_key_source_agrees_with_the_key_a_run_would_use() {
        for backend in [
            BackendKind::Ollama,
            BackendKind::LmStudio,
            BackendKind::OpenRouter,
        ] {
            for presence in [KeyPresence::Env, KeyPresence::Keychain, KeyPresence::Absent] {
                for file_key in [Some("sk-test"), None] {
                    let config = config(backend, file_key);
                    let head_key = match presence {
                        KeyPresence::Env | KeyPresence::Keychain => Some("sk-test-2".to_string()),
                        KeyPresence::Absent => None,
                    };
                    let effective = config.effective_api_key(head_key);
                    let source = key_source(&config, presence);
                    let case = format!(
                        "{backend:?}, {presence:?}, file key: {}",
                        file_key.is_some()
                    );
                    assert_eq!(source == KeySource::Absent, effective.is_none(), "{case}");
                    assert_eq!(
                        source == KeySource::File,
                        effective.is_some() && effective.as_deref() == file_key,
                        "{case}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_key_source_wire_strings_are_pinned() {
        for (source, wire) in [
            (KeySource::Env, "env"),
            (KeySource::Keychain, "keychain"),
            (KeySource::File, "file"),
            (KeySource::Absent, "none"),
        ] {
            assert_eq!(
                serde_json::to_value(source).expect("ser"),
                serde_json::json!(wire)
            );
        }
    }

    #[test]
    fn the_view_carries_a_source_and_never_a_key() {
        let view = to_view(
            &config(BackendKind::OpenRouter, Some("sk-test")),
            KeyPresence::Absent,
        );
        let wire = serde_json::to_string(&view).expect("ser");
        assert!(!wire.contains("sk-test"), "{wire}");
        assert!(!wire.contains("api_key"), "{wire}");
        let value: serde_json::Value = serde_json::from_str(&wire).expect("de");
        assert_eq!(value["key_source"], serde_json::json!("file"));
        assert_eq!(value["backend"], serde_json::json!("open-router"));
        assert_eq!(value["model"], serde_json::json!("m"));
    }

    #[test]
    fn show_names_the_source_the_head_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Keep),
            &Notices::new(),
        )
        .expect("set");
        let view = show(dir.path(), KeyPresence::Keychain, &Notices::new())
            .expect("show")
            .expect("configured");
        assert_eq!(view.key_source, KeySource::Keychain);
    }

    #[test]
    fn only_an_openrouter_key_goes_to_a_supported_keychain() {
        for backend in [
            BackendKind::Ollama,
            BackendKind::LmStudio,
            BackendKind::OpenRouter,
        ] {
            for supported in [true, false] {
                let nothing = plan_key_write(backend, supported, None);
                assert!(nothing.keychain.is_none(), "{backend:?} {supported}");
                assert_eq!(nothing.file, FileKey::Keep, "{backend:?} {supported}");

                let write = plan_key_write(backend, supported, Some("sk-test".to_string()));
                let to_keychain = match backend {
                    BackendKind::OpenRouter => supported,
                    BackendKind::Ollama | BackendKind::LmStudio => false,
                };
                if to_keychain {
                    assert_eq!(write.keychain.as_deref(), Some("sk-test"));
                    assert_eq!(write.file, FileKey::Clear);
                } else {
                    assert!(write.keychain.is_none(), "{backend:?} {supported}");
                    assert_eq!(
                        write.file,
                        FileKey::Set("sk-test".to_string()),
                        "{backend:?} {supported}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_file_key_never_debug_prints_the_key() {
        let rendered = format!("{:?}", FileKey::Set("sk-test".to_string()));
        assert_eq!(rendered, "Set(\"<redacted>\")");
        assert_eq!(format!("{:?}", FileKey::Keep), "Keep");
        assert_eq!(format!("{:?}", FileKey::Clear), "Clear");
    }

    #[test]
    fn a_key_write_never_debug_prints_the_key() {
        let to_keychain = format!(
            "{:?}",
            plan_key_write(BackendKind::OpenRouter, true, Some("sk-test".to_string()))
        );
        assert!(!to_keychain.contains("sk-test"), "{to_keychain}");
        assert!(to_keychain.contains("<redacted>"), "{to_keychain}");
        let to_file = format!(
            "{:?}",
            plan_key_write(BackendKind::OpenRouter, false, Some("sk-test".to_string()))
        );
        assert!(!to_file.contains("sk-test"), "{to_file}");
        assert!(to_file.contains("<redacted>"), "{to_file}");
    }

    #[test]
    fn set_stores_the_config_with_the_key_it_was_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &set_args(
                BackendKind::Ollama,
                "llava",
                FileKey::Set("sk-test".to_string()),
            ),
            &Notices::new(),
        )
        .expect("set");

        let stored = stored(dir.path());
        assert_eq!(stored.model, "llava");
        assert_eq!(stored.base_url, BackendKind::Ollama.default_base_url());
        assert_eq!(stored.api_key.as_deref(), Some("sk-test"));
    }

    /// A host change under the same backend keeps the key — it is usually a
    /// port move, and an omitted `--base-url` resets to the default on its
    /// own — but it must say where the key is now going.
    #[test]
    fn moving_the_same_backend_to_another_url_keeps_the_key_and_says_where_it_goes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        let mut first = set_args(
            BackendKind::LmStudio,
            "first",
            FileKey::Set("sk-test".to_string()),
        );
        first.base_url = Some("http://127.0.0.1:1234".to_string());
        set(dir.path(), &first, &notices).expect("set with a key");
        // Nothing this first `set` said is under test; start the move clean.
        drop(notices.drain());

        let mut moved = set_args(BackendKind::LmStudio, "second", FileKey::Keep);
        moved.base_url = Some("http://127.0.0.1:1235".to_string());
        set(dir.path(), &moved, &notices).expect("set at a new url");

        let after = stored(dir.path());
        assert_eq!(after.api_key.as_deref(), Some("sk-test"));
        assert_eq!(after.base_url, "http://127.0.0.1:1235");
        let said: Vec<String> = notices
            .drain()
            .into_iter()
            .filter(|notice| notice.contains(MOVED))
            .collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("http://127.0.0.1:1235"), "{said:?}");
        assert!(said[0].contains("http://127.0.0.1:1234"), "{said:?}");
        assert!(!said[0].contains("sk-test"), "{said:?}");

        // Same backend, same url: nothing to say.
        set(dir.path(), &moved, &notices).expect("set again");
        assert_eq!(stored(dir.path()).api_key.as_deref(), Some("sk-test"));
        assert!(
            !notices.drain().iter().any(|n| n.contains(MOVED)),
            "an unchanged url must not warn"
        );
    }

    /// The costliest key is the one this function cannot see: an
    /// `OpenRouter` key lives in the Keychain and leaves `describer.toml`
    /// keyless, so an early return on the file's key would move a paid
    /// hosted token to a new host in silence.
    #[test]
    fn moving_openrouter_warns_even_though_its_key_is_not_in_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        // Exactly what a macOS `describer set --api-key` leaves behind: the
        // key is in the Keychain, the file has none.
        set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Clear),
            &notices,
        )
        .expect("set keyless");
        assert_eq!(stored(dir.path()).api_key, None);
        drop(notices.drain());

        let mut moved = set_args(BackendKind::OpenRouter, "m", FileKey::Keep);
        moved.base_url = Some("http://127.0.0.1:18716".to_string());
        set(dir.path(), &moved, &notices).expect("set at a new url");

        let said: Vec<String> = notices
            .drain()
            .into_iter()
            .filter(|notice| notice.contains(MOVED))
            .collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("http://127.0.0.1:18716"), "{said:?}");
        // It must not claim a key exists: this function cannot know.
        assert!(said[0].contains("any stored API key"), "{said:?}");

        // An omitted `--base-url` is a reset to the backend's default, so
        // the notice must name the RESOLVED url, not the `None` it was
        // given. Nothing else exercises that branch.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut custom = set_args(BackendKind::OpenRouter, "m", FileKey::Clear);
        custom.base_url = Some("http://127.0.0.1:18717".to_string());
        set(dir.path(), &custom, &notices).expect("set at a custom url");
        drop(notices.drain());
        set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Keep),
            &notices,
        )
        .expect("set with no --base-url");
        let reset: Vec<String> = notices
            .drain()
            .into_iter()
            .filter(|notice| notice.contains(MOVED))
            .collect();
        assert_eq!(reset.len(), 1, "{reset:?}");
        assert!(
            reset[0].contains(BackendKind::OpenRouter.default_base_url()),
            "{reset:?}"
        );

        // A local backend with no file key has nothing to warn about: its
        // key could only ever have been the file's.
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &set_args(BackendKind::LmStudio, "m", FileKey::Clear),
            &notices,
        )
        .expect("set keyless local");
        drop(notices.drain());
        let mut moved = set_args(BackendKind::LmStudio, "m", FileKey::Keep);
        moved.base_url = Some("http://127.0.0.1:1235".to_string());
        set(dir.path(), &moved, &notices).expect("move local");
        assert!(
            !notices.drain().iter().any(|n| n.contains(MOVED)),
            "a local backend with no stored key must stay quiet"
        );
    }

    /// A key in the file belongs to the backend that was configured when it
    /// was stored. Switching backends without supplying a new one must NOT
    /// carry it over: a pre-7G `describer.toml` holds an `OpenRouter` key in
    /// `api_key`, so carrying it would hand a paid hosted key to whatever
    /// local process the new `base_url` points at — and the reverse sends a
    /// local proxy's token to openrouter.ai.
    #[test]
    fn switching_backends_without_a_new_key_does_not_carry_the_old_one_over() {
        for (stored_backend, next) in [
            (BackendKind::OpenRouter, BackendKind::LmStudio),
            (BackendKind::LmStudio, BackendKind::OpenRouter),
            (BackendKind::Ollama, BackendKind::LmStudio),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let notices = Notices::new();
            set(
                dir.path(),
                &set_args(stored_backend, "first", FileKey::Set("sk-test".to_string())),
                &notices,
            )
            .expect("set with a key");

            set(
                dir.path(),
                &set_args(next, "second", FileKey::Keep),
                &notices,
            )
            .expect("set after switching backend");

            let stored = stored(dir.path());
            assert_eq!(stored.backend, next, "{stored_backend:?} -> {next:?}");
            assert_eq!(stored.api_key, None, "{stored_backend:?} -> {next:?}");
            let dropped: Vec<String> = notices
                .drain()
                .into_iter()
                .filter(|notice| notice.contains("was not carried over"))
                .collect();
            assert_eq!(dropped.len(), 1, "{dropped:?}");
            assert!(!dropped[0].contains("sk-test"), "{dropped:?}");
            assert!(
                dropped[0].contains(stored_backend.as_str()) && dropped[0].contains(next.as_str()),
                "{dropped:?}"
            );
        }
    }

    #[test]
    fn set_without_a_key_keeps_the_stored_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        set(
            dir.path(),
            &set_args(
                BackendKind::OpenRouter,
                "first",
                FileKey::Set("sk-test".to_string()),
            ),
            &notices,
        )
        .expect("set with a key");

        set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "second", FileKey::Keep),
            &notices,
        )
        .expect("set keeping the key");

        let stored = stored(dir.path());
        assert_eq!(stored.model, "second");
        assert_eq!(stored.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn set_keeping_a_key_on_an_unconfigured_catalog_stores_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &set_args(BackendKind::Ollama, "llava", FileKey::Keep),
            &Notices::new(),
        )
        .expect("set");
        assert_eq!(stored(dir.path()).api_key, None);
    }

    #[test]
    fn set_with_clear_removes_the_stored_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        set(
            dir.path(),
            &set_args(
                BackendKind::OpenRouter,
                "m",
                FileKey::Set("sk-test".to_string()),
            ),
            &notices,
        )
        .expect("set with a key");

        set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Clear),
            &notices,
        )
        .expect("set clearing the key");

        assert_eq!(stored(dir.path()).api_key, None);
    }

    /// The broken line is the one holding the key, because that is the line
    /// a TOML parse error quotes back.
    const UNPARSABLE: &[u8] = b"api_key = \"sk-test\" oops";

    fn plant_unparsable_config(root: &Path) -> PathBuf {
        let path = config_path(root, &Notices::new()).expect("config path");
        std::fs::write(&path, UNPARSABLE).expect("plant broken config");
        path
    }

    /// Dropping the key of a file that does not parse would be silent data
    /// loss, so `Keep` refuses instead — and no rendering of the refusal,
    /// chain included, quotes the file back.
    #[test]
    fn set_keeping_a_key_over_an_unparsable_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = plant_unparsable_config(dir.path());

        let err = set(
            dir.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Keep),
            &Notices::new(),
        )
        .expect_err("an unparsable file must not be overwritten");

        let chain = format!("{:#}", anyhow::Error::from(err));
        assert!(chain.contains("describer.toml"), "{chain}");
        assert!(!chain.contains("sk-test"), "{chain}");
        assert_eq!(std::fs::read(&path).expect("read back"), UNPARSABLE);
    }

    #[test]
    fn clear_file_key_of_an_unconfigured_catalog_is_false_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        assert!(!clear_file_key(dir.path(), &notices).expect("clear"));
        let path = config_path(dir.path(), &notices).expect("config path");
        assert!(!path.exists());
    }

    #[test]
    fn clear_file_key_removes_the_key_once_and_leaves_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notices = Notices::new();
        set(
            dir.path(),
            &SetArgs {
                backend: BackendKind::OpenRouter,
                model: "some-model".to_string(),
                base_url: Some("https://example.test/api".to_string()),
                file_key: FileKey::Set("sk-test".to_string()),
            },
            &notices,
        )
        .expect("set");

        assert!(clear_file_key(dir.path(), &notices).expect("first clear"));
        assert!(!clear_file_key(dir.path(), &notices).expect("second clear"));

        assert_eq!(
            stored(dir.path()),
            DescriberConfig {
                backend: BackendKind::OpenRouter,
                base_url: "https://example.test/api".to_string(),
                model: "some-model".to_string(),
                api_key: None,
            }
        );
    }

    #[test]
    fn clear_file_key_of_an_unparsable_file_is_an_error_that_never_quotes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = plant_unparsable_config(dir.path());

        let err = clear_file_key(dir.path(), &Notices::new()).expect_err("must fail");

        let chain = format!("{:#}", anyhow::Error::from(err));
        assert!(chain.contains("describer.toml"), "{chain}");
        assert!(!chain.contains("sk-test"), "{chain}");
        assert_eq!(std::fs::read(&path).expect("read back"), UNPARSABLE);
    }

    #[test]
    fn only_a_stored_openrouter_config_wants_the_keychain() {
        let notices = Notices::new();
        let unconfigured = tempfile::tempdir().expect("tempdir");
        assert!(!wants_keychain(unconfigured.path(), &notices));

        let ollama = tempfile::tempdir().expect("tempdir");
        set(
            ollama.path(),
            &set_args(BackendKind::Ollama, "m", FileKey::Keep),
            &notices,
        )
        .expect("set");
        assert!(!wants_keychain(ollama.path(), &notices));

        let openrouter = tempfile::tempdir().expect("tempdir");
        set(
            openrouter.path(),
            &set_args(BackendKind::OpenRouter, "m", FileKey::Keep),
            &notices,
        )
        .expect("set");
        assert!(wants_keychain(openrouter.path(), &notices));

        let unparsable = tempfile::tempdir().expect("tempdir");
        plant_unparsable_config(unparsable.path());
        assert!(!wants_keychain(unparsable.path(), &notices));
    }

    #[test]
    fn set_defaults_base_url_to_the_backends_default_when_not_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &set_args(BackendKind::LmStudio, "some-model", FileKey::Keep),
            &Notices::new(),
        )
        .expect("set");
        assert_eq!(
            stored(dir.path()).base_url,
            BackendKind::LmStudio.default_base_url()
        );
    }

    /// The MCP `test_describer` tool serializes the probe as-is, so this is
    /// the wire shape a client reads the key verdict from.
    #[test]
    fn the_probe_carries_the_key_check_in_snake_case() {
        for (key, wire) in [
            (KeyCheck::Accepted, "accepted"),
            (KeyCheck::Rejected, "rejected"),
            (KeyCheck::Missing, "missing"),
            (KeyCheck::NotChecked, "not_checked"),
        ] {
            let probe = DescriberProbe {
                model: "m".to_string(),
                model_listed: true,
                vision: None,
                key,
            };
            assert_eq!(
                serde_json::to_value(&probe).expect("ser")["key"],
                serde_json::json!(wire)
            );
        }
    }

    /// Configures `backend` against `server` with no key in the file, so
    /// the only key a test can see is the one it passes to [`test`].
    fn configure(root: &Path, backend: BackendKind, server: &httpmock::MockServer) {
        set(
            root,
            &SetArgs {
                backend,
                model: "m".to_string(),
                base_url: Some(server.base_url()),
                file_key: FileKey::Keep,
            },
            &Notices::new(),
        )
        .expect("set");
    }

    /// Serves the model list [`test`] probes before it looks at the key.
    fn serve_models(server: &httpmock::MockServer) {
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/v1/models");
            then.status(200)
                .json_body(serde_json::json!({"data": [{"id": "m"}]}));
        });
    }

    fn serve_key(server: &httpmock::MockServer, status: u16) -> httpmock::Mock<'_> {
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/v1/key");
            then.status(status).json_body(serde_json::json!({}));
        })
    }

    #[test]
    fn test_reports_an_accepted_key() {
        let server = httpmock::MockServer::start();
        serve_models(&server);
        let key = serve_key(&server, 200);
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, &server);

        let probe = test(dir.path(), Some("sk-test".into()), &Notices::new()).expect("test");

        assert!(probe.model_listed);
        assert_eq!(probe.key, KeyCheck::Accepted);
        key.assert_calls(1);
    }

    #[test]
    fn test_reports_a_rejected_key() {
        let server = httpmock::MockServer::start();
        serve_models(&server);
        let key = serve_key(&server, 401);
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, &server);

        let probe = test(dir.path(), Some("sk-test".into()), &Notices::new()).expect("test");

        assert_eq!(probe.key, KeyCheck::Rejected);
        key.assert_calls(1);
    }

    /// Only `OpenRouter` has a key endpoint: a local backend that HAS a key
    /// must not be asked about it. The key sits in the file because that is
    /// the only place a local backend's key is read from — a caller's key is
    /// ignored for it, which would leave this gate untested.
    #[test]
    fn test_does_not_check_a_key_for_ollama() {
        let server = httpmock::MockServer::start();
        serve_models(&server);
        let key = serve_key(&server, 200);
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &SetArgs {
                backend: BackendKind::Ollama,
                model: "m".to_string(),
                base_url: Some(server.base_url()),
                file_key: FileKey::Set("sk-test".to_string()),
            },
            &Notices::new(),
        )
        .expect("set");
        let notices = Notices::new();

        let probe = test(dir.path(), None, &notices).expect("test");

        assert_eq!(probe.key, KeyCheck::NotChecked);
        key.assert_calls(0);
        assert!(key_notices(&notices).is_empty());
    }

    /// A key endpoint that answers neither success nor 401 judged nothing:
    /// the probe still succeeds, and a notice says the key went unchecked —
    /// without carrying the key.
    #[test]
    fn test_with_a_key_endpoint_that_judges_nothing_is_not_checked_with_a_notice() {
        let server = httpmock::MockServer::start();
        serve_models(&server);
        let key = serve_key(&server, 500);
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, &server);
        let notices = Notices::new();

        let probe = test(dir.path(), Some("sk-test".into()), &notices).expect("test");

        assert_eq!(probe.key, KeyCheck::NotChecked);
        key.assert_calls(1);
        let all = notices.drain();
        assert!(
            all.iter().all(|notice| !notice.contains("sk-test")),
            "{all:?}"
        );
        let about_the_key: Vec<&String> = all
            .iter()
            .filter(|notice| notice.starts_with("note: the key was not checked ("))
            .collect();
        assert_eq!(about_the_key.len(), 1, "{all:?}");
    }

    /// With no key from the caller or the file there is nothing to judge —
    /// a keyless request would only earn a 401 that reads as `Rejected` —
    /// but the next caption pass would fail every item for want of a key, so
    /// the probe says `Missing` rather than the harmless-looking `NotChecked`.
    #[test]
    fn test_without_a_key_does_not_call_the_key_endpoint() {
        let server = httpmock::MockServer::start();
        serve_models(&server);
        let key = serve_key(&server, 401);
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, &server);
        let notices = Notices::new();

        let probe = test(dir.path(), None, &notices).expect("test");

        assert_eq!(probe.key, KeyCheck::Missing);
        key.assert_calls(0);
        assert!(key_notices(&notices).is_empty());
    }

    fn key_notices(notices: &Notices) -> Vec<String> {
        notices
            .drain()
            .into_iter()
            .filter(|notice| notice.contains("key"))
            .collect()
    }

    #[test]
    fn test_of_an_unconfigured_catalog_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = test(dir.path(), None, &Notices::new()).expect_err("must fail");
        assert!(err.to_string().contains("no describer configured"));
    }

    #[test]
    fn test_of_an_unreachable_backend_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(
            dir.path(),
            &SetArgs {
                backend: BackendKind::Ollama,
                model: "llava".to_string(),
                base_url: Some("http://127.0.0.1:1".to_string()),
                file_key: FileKey::Keep,
            },
            &Notices::new(),
        )
        .expect("set");
        let err = test(dir.path(), None, &Notices::new())
            .expect_err("must fail: nothing listens on port 1");
        assert!(err.to_string().contains("describer test against"));
    }
}
