//! Per-machine, per-catalog describer configuration (`describer.toml` in
//! the state dir). Never synced: endpoints and API keys are machine-local.

use std::os::unix::fs::PermissionsExt;
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
    /// `env_key`) overrides so the file can stay keyless.
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

    /// Write config to `path` with 0600 permissions (may hold an API key).
    ///
    /// # Errors
    /// Returns `ConfigError` when serialization or the write fails.
    pub fn store(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(|source| ConfigError::Write {
            path: path.display().to_string(),
            source,
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| ConfigError::Write {
                path: path.display().to_string(),
                source,
            },
        )?;
        Ok(())
    }

    /// The key to send: environment override first, then the file's.
    #[must_use]
    pub fn effective_api_key(&self, env_key: Option<String>) -> Option<String> {
        env_key.or_else(|| self.api_key.clone())
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
