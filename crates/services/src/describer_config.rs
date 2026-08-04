//! The configured describer backend (`describer.toml`), read by
//! `maj describer show|test`, `index run`/`index status`, and `search`'s
//! caption coverage remedy — plus `maj describer set`/`test` themselves.
//! Moved from `crates/cli/src/describer_cmd.rs`.
use crate::error::ServiceError;
use anyhow::{Context as _, Result, bail};
use majestical_describe::{BackendKind, DescriberConfig, HttpDescriber};
use std::path::{Path, PathBuf};

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

/// A describer config safe to hand to any head, including remote ones (MCP,
/// GUI): the API key, when configured, is replaced with a fixed redaction
/// marker rather than carried through — this struct can never expose the
/// real key. `base_url`/`model` are always present in a stored config, so
/// unlike `api_key` they're plain `String`, not `Option`.
#[derive(serde::Serialize)]
pub struct DescriberConfigView {
    pub backend: String,
    pub base_url: String,
    pub model: String,
    /// `Some("redacted")` when a key is configured; never the real key.
    pub api_key: Option<String>,
}

/// The fixed marker `api_key` carries when a key is configured. Its exact
/// text is never rendered directly — every head decides its own display
/// string from whether this is `Some`/`None` — so this only needs to be
/// distinguishable from a real key, never to match any particular wording.
const REDACTED_MARKER: &str = "redacted";

/// Builds a render-safe view of `config` — the one place a real API key is
/// read out of a [`DescriberConfig`] before it reaches a head. Shared by
/// `maj describer show` (via [`show`]) and `maj describer set`'s immediate
/// echo of what it just stored, so redaction can't drift between the two.
#[must_use]
pub fn to_view(config: &DescriberConfig) -> DescriberConfigView {
    DescriberConfigView {
        backend: config.backend.as_str().to_string(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        api_key: config.api_key.as_ref().map(|_| REDACTED_MARKER.to_string()),
    }
}

/// `maj describer show`: the configured describer with its API key
/// redacted, or `None` when no describer is configured on this machine yet.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or an existing
/// config file can't be read/parsed.
pub fn show(
    catalog_root: &Path,
    notices: &crate::notices::Notices,
) -> Result<Option<DescriberConfigView>, ServiceError> {
    show_impl(catalog_root, notices).map_err(ServiceError::from)
}

fn show_impl(
    catalog_root: &Path,
    notices: &crate::notices::Notices,
) -> Result<Option<DescriberConfigView>> {
    Ok(load_config(catalog_root, notices)?.map(|config| to_view(&config)))
}

/// Args for `maj describer set`, bundled to keep [`set`] within the house
/// 5-positional-parameter limit.
pub struct SetArgs {
    pub backend: BackendKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

/// `maj describer set`: stores this machine's describer backend config,
/// defaulting `base_url` to the backend's own default when not given.
/// Returns the redacted view of what was just stored so the CLI can echo it
/// back immediately without a second load.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or the config
/// file can't be written.
pub fn set(
    catalog_root: &Path,
    args: &SetArgs,
    notices: &crate::notices::Notices,
) -> Result<DescriberConfigView, ServiceError> {
    set_impl(catalog_root, args, notices).map_err(ServiceError::from)
}

fn set_impl(
    catalog_root: &Path,
    args: &SetArgs,
    notices: &crate::notices::Notices,
) -> Result<DescriberConfigView> {
    let config = DescriberConfig {
        backend: args.backend,
        base_url: args
            .base_url
            .clone()
            .unwrap_or_else(|| args.backend.default_base_url().to_string()),
        model: args.model.clone(),
        api_key: args.api_key.clone(),
    };
    let path = config_path(catalog_root, notices)?;
    config
        .store(&path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(to_view(&config))
}

/// Everything `maj describer test` renders: the configured model, whether
/// the live backend actually lists it, and (LM Studio only) whether it
/// reports vision support. `reachable` isn't carried here — [`test`]
/// returns an error instead when the backend can't be reached at all,
/// so by the time a [`DescriberProbe`] exists reachability is already a
/// given.
#[derive(Debug, serde::Serialize)]
pub struct DescriberProbe {
    pub model: String,
    pub model_listed: bool,
    pub vision: Option<bool>,
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
    let describer = HttpDescriber::new(config, api_key);
    let report = describer
        .probe()
        .with_context(|| format!("describer test against {base_url}"))?;
    Ok(DescriberProbe {
        model,
        model_listed: report.model_listed,
        vision: report.vision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notices::Notices;
    use majestical_describe::BackendKind;

    #[test]
    fn show_of_an_unconfigured_catalog_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(show(dir.path(), &Notices::new()).expect("show").is_none());
    }

    #[test]
    fn to_view_redacts_a_configured_key_without_carrying_it() {
        let config = DescriberConfig {
            backend: BackendKind::OpenRouter,
            base_url: "https://openrouter.ai/api".to_string(),
            model: "some-model".to_string(),
            api_key: Some("sk-super-secret".to_string()),
        };
        let view = to_view(&config);
        assert_eq!(view.backend, "open-router");
        assert_eq!(view.model, "some-model");
        let Some(marker) = view.api_key else {
            panic!("a configured key must still render as Some(..)");
        };
        assert_ne!(marker, "sk-super-secret", "the real key must never surface");
    }

    #[test]
    fn to_view_of_no_key_is_none() {
        let config = DescriberConfig {
            backend: BackendKind::Ollama,
            base_url: BackendKind::Ollama.default_base_url().to_string(),
            model: "llava".to_string(),
            api_key: None,
        };
        assert!(to_view(&config).api_key.is_none());
    }

    #[test]
    fn set_stores_the_config_and_returns_a_redacted_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = set(
            dir.path(),
            &SetArgs {
                backend: BackendKind::Ollama,
                model: "llava".to_string(),
                base_url: None,
                api_key: Some("sk-secret".to_string()),
            },
            &Notices::new(),
        )
        .expect("set");
        assert_eq!(view.model, "llava");
        assert_eq!(view.base_url, BackendKind::Ollama.default_base_url());
        assert!(view.api_key.is_some());

        let stored = load_config(dir.path(), &crate::notices::Notices::new())
            .expect("load_config")
            .expect("config must be present after set");
        assert_eq!(stored.api_key.as_deref(), Some("sk-secret"));
    }

    #[test]
    fn set_defaults_base_url_to_the_backends_default_when_not_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = set(
            dir.path(),
            &SetArgs {
                backend: BackendKind::LmStudio,
                model: "some-model".to_string(),
                base_url: None,
                api_key: None,
            },
            &Notices::new(),
        )
        .expect("set");
        assert_eq!(view.base_url, BackendKind::LmStudio.default_base_url());
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
                api_key: None,
            },
            &Notices::new(),
        )
        .expect("set");
        let err = test(dir.path(), None, &Notices::new())
            .expect_err("must fail: nothing listens on port 1");
        assert!(err.to_string().contains("describer test against"));
    }
}
