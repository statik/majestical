//! Per-machine, per-catalog describer configuration (`describer.toml` in
//! the state dir). Never synced: endpoints and API keys are machine-local.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Ollama,
    LmStudio,
    OpenRouter,
}

impl BackendKind {
    #[must_use]
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Ollama => "http://localhost:11434",
            Self::LmStudio => "http://localhost:1234",
            Self::OpenRouter => "https://openrouter.ai/api",
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::LmStudio => "lm-studio",
            Self::OpenRouter => "open-router",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DescriberConfig {
    pub backend: BackendKind,
    pub base_url: String,
    pub model: String,
    /// `OpenRouter` key. `MAJ_OPENROUTER_KEY` (passed in by the caller as
    /// `env_key`) overrides so the file can stay keyless — but only when
    /// `backend` is `OpenRouter`; see `effective_api_key`.
    pub api_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("serialize describer config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl DescriberConfig {
    /// Load config from `path`; `Ok(None)` when the file does not exist.
    ///
    /// # Errors
    /// Returns `ConfigError` on unreadable or unparsable file contents.
    pub fn load(path: &Path) -> Result<Option<Self>, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Some(config))
    }

    /// Write config to `path`, created with 0600 permissions from the start
    /// (may hold an API key, so it must never exist world/group-readable
    /// even for the instant between create and chmod).
    ///
    /// # Errors
    /// Returns `ConfigError` when serialization or the write fails.
    pub fn store(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        let write_error = |source| ConfigError::Write {
            path: path.display().to_string(),
            source,
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(write_error)?;
        file.write_all(text.as_bytes()).map_err(write_error)?;
        Ok(())
    }

    /// The key to send: the environment override wins, but only for
    /// `OpenRouter` — `MAJ_OPENROUTER_KEY` naming that host explicitly, so it
    /// must never leak as a Bearer header to an Ollama/LM Studio `base_url`
    /// a user has pointed at a non-local host. Every other backend always
    /// uses the file's key (or none).
    #[must_use]
    pub fn effective_api_key(&self, env_key: Option<String>) -> Option<String> {
        if self.backend == BackendKind::OpenRouter {
            return env_key.or_else(|| self.api_key.clone());
        }
        self.api_key.clone()
    }

    /// Blob derivation tag for this backend model, filesystem-safe:
    /// `describe-` + model with `/` and `:` mapped to `-`.
    #[must_use]
    pub fn model_tag(&self) -> String {
        let sanitized: String = self
            .model
            .chars()
            .map(|c| if c == '/' || c == ':' { '-' } else { c })
            .collect();
        format!("describe-{sanitized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_urls_per_backend() {
        assert_eq!(
            BackendKind::Ollama.default_base_url(),
            "http://localhost:11434"
        );
        assert_eq!(
            BackendKind::LmStudio.default_base_url(),
            "http://localhost:1234"
        );
        assert_eq!(
            BackendKind::OpenRouter.default_base_url(),
            "https://openrouter.ai/api"
        );
    }

    #[test]
    fn round_trips_through_toml_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        let config = DescriberConfig {
            backend: BackendKind::OpenRouter,
            base_url: "https://openrouter.ai/api".into(),
            model: "qwen/qwen3-vl-8b".into(),
            api_key: Some("sk-secret".into()),
        };
        config.store(&path).expect("store");
        let loaded = DescriberConfig::load(&path)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, config);
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "describer.toml must be 0600");
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            DescriberConfig::load(&dir.path().join("nope.toml"))
                .expect("ok")
                .is_none()
        );
    }

    /// `load`'s `NotFound` guard must be exact: any other io error (here, an
    /// `IsADirectory`/similar error from reading a directory as a file) has
    /// to surface as `ConfigError::Read`, not silently swallowed into
    /// `Ok(None)` the way a missing file is. Without this, a mutant that
    /// widens the guard to match unconditionally still passes
    /// `load_missing_file_is_none` (a real `NotFound` either way).
    #[test]
    fn load_non_not_found_io_error_is_a_read_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = DescriberConfig::load(dir.path()).expect_err("directory is not a file");
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    /// `BackendKind::as_str` has no other caller under test (`describer_cmd`
    /// only prints it), so a mutant collapsing it to `""`/`"xyzzy"` for
    /// every variant would otherwise survive — mirrors
    /// `default_base_urls_per_backend` for the sibling method.
    #[test]
    fn as_str_per_backend() {
        assert_eq!(BackendKind::Ollama.as_str(), "ollama");
        assert_eq!(BackendKind::LmStudio.as_str(), "lm-studio");
        assert_eq!(BackendKind::OpenRouter.as_str(), "open-router");
    }

    #[test]
    fn env_key_wins_over_file_key() {
        let config = DescriberConfig {
            backend: BackendKind::OpenRouter,
            base_url: "u".into(),
            model: "m".into(),
            api_key: Some("file-key".into()),
        };
        assert_eq!(
            config.effective_api_key(Some("env-key".into())).as_deref(),
            Some("env-key")
        );
        assert_eq!(config.effective_api_key(None).as_deref(), Some("file-key"));
    }

    #[test]
    fn env_key_ignored_for_non_openrouter_backends() {
        let config = DescriberConfig {
            backend: BackendKind::Ollama,
            base_url: "u".into(),
            model: "m".into(),
            api_key: Some("file-key".into()),
        };
        assert_eq!(
            config.effective_api_key(Some("env-key".into())).as_deref(),
            Some("file-key"),
            "env key must not override for non-OpenRouter backends"
        );

        let keyless = DescriberConfig {
            api_key: None,
            ..config
        };
        assert_eq!(
            keyless.effective_api_key(Some("env-key".into())),
            None,
            "non-OpenRouter backend with no file key must stay keyless, never fall back to env"
        );
    }

    #[test]
    fn model_tag_sanitizes_slashes_and_colons() {
        let config = DescriberConfig {
            backend: BackendKind::Ollama,
            base_url: "u".into(),
            model: "qwen3-vl:8b".into(),
            api_key: None,
        };
        assert_eq!(config.model_tag(), "describe-qwen3-vl-8b");
    }
}
