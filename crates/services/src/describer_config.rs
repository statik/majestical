//! The configured describer backend (`describer.toml`), read by
//! `maj describer show|test`, `index run`/`index status`, and `search`'s
//! caption coverage remedy. Moved verbatim from
//! `crates/cli/src/describer_cmd.rs`; writing the config (`maj describer
//! set`) stays a CLI concern.
use crate::error::ServiceError;
use anyhow::{Context as _, Result};
use majestical_describe::DescriberConfig;
use std::path::{Path, PathBuf};

/// # Errors
/// Returns an error if the local state dir can't be resolved.
pub fn config_path(catalog_root: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog_root)?.join("describer.toml"))
}

/// Loads the configured describer, if any.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or an existing
/// config file can't be read/parsed.
pub fn load_config(catalog_root: &Path) -> Result<Option<DescriberConfig>> {
    let path = config_path(catalog_root)?;
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
pub fn show(catalog_root: &Path) -> Result<Option<DescriberConfigView>, ServiceError> {
    show_impl(catalog_root).map_err(ServiceError::from)
}

fn show_impl(catalog_root: &Path) -> Result<Option<DescriberConfigView>> {
    Ok(load_config(catalog_root)?.map(|config| to_view(&config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_describe::BackendKind;

    #[test]
    fn show_of_an_unconfigured_catalog_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(show(dir.path()).expect("show").is_none());
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
}
