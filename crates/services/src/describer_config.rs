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
    /// Carry the existing file's key forward, if it has one.
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

/// Where a newly supplied key goes. With a supported Keychain the key goes
/// there and the file is written keyless; otherwise the file holds it. No
/// key supplied means nothing about the key changes. `keychain` is written
/// before `file`; see [`plan_key_write`].
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
/// The head MUST write `keychain` first and stop on failure: `file` is
/// `Clear` in that case, so writing the file first and then failing the
/// Keychain write would leave no key anywhere.
#[must_use]
pub fn plan_key_write(keychain_supported: bool, key: Option<String>) -> KeyWrite {
    match (key, keychain_supported) {
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
/// file's key follows [`SetArgs::file_key`]. Returns no view: a view names
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
    let api_key = match &args.file_key {
        FileKey::Keep => load_config(catalog_root, notices)?.and_then(|stored| stored.api_key),
        FileKey::Set(key) => Some(key.clone()),
        FileKey::Clear => None,
    };
    let config = DescriberConfig {
        backend: args.backend,
        base_url: args
            .base_url
            .clone()
            .unwrap_or_else(|| args.backend.default_base_url().to_string()),
        model: args.model.clone(),
        api_key,
    };
    let path = config_path(catalog_root, notices)?;
    config
        .store(&path)
        .with_context(|| format!("write {}", path.display()))
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
    fn a_new_key_goes_to_the_keychain_when_there_is_one_and_to_the_file_otherwise() {
        let nothing = plan_key_write(true, None);
        assert!(nothing.keychain.is_none());
        assert_eq!(nothing.file, FileKey::Keep);
        let nothing = plan_key_write(false, None);
        assert!(nothing.keychain.is_none());
        assert_eq!(nothing.file, FileKey::Keep);

        let keychain = plan_key_write(true, Some("sk-test".to_string()));
        assert_eq!(keychain.keychain.as_deref(), Some("sk-test"));
        assert_eq!(keychain.file, FileKey::Clear);

        let file = plan_key_write(false, Some("sk-test".to_string()));
        assert!(file.keychain.is_none());
        assert_eq!(file.file, FileKey::Set("sk-test".to_string()));
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
        let to_keychain = format!("{:?}", plan_key_write(true, Some("sk-test".to_string())));
        assert!(!to_keychain.contains("sk-test"), "{to_keychain}");
        assert!(to_keychain.contains("<redacted>"), "{to_keychain}");
        let to_file = format!("{:?}", plan_key_write(false, Some("sk-test".to_string())));
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
