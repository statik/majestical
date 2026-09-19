//! Per-machine, per-catalog describer configuration (`describer.toml` in
//! the state dir). Never synced: endpoints and API keys are machine-local.

use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Ollama,
    LmStudio,
    OpenRouter,
}

/// Hand-written because the derived error for a value outside the set
/// ("unknown variant `…`") quotes the value back, and a key pasted into the
/// wrong field of `describer.toml` would ride out in it.
impl<'de> serde::Deserialize<'de> for BackendKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::ALL
            .into_iter()
            .find(|backend| backend.as_str() == name)
            .ok_or_else(|| {
                serde::de::Error::custom(
                    "unknown backend — expected one of ollama, lm-studio, open-router",
                )
            })
    }
}

impl BackendKind {
    pub const ALL: [Self; 3] = [Self::Ollama, Self::LmStudio, Self::OpenRouter];

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

/// The one place the key environment variable's name lives. Heads (CLI,
/// GUI, MCP) read it and pass the value in as `env_key`; this crate — and
/// `majestical-services` below it — never touches the environment itself,
/// so a head can supply the key some other way.
pub const OPENROUTER_KEY_ENV: &str = "MAJ_OPENROUTER_KEY";

/// Every field must accept any TOML string, or deserialize through a
/// hand-written impl with fixed error text (see [`BackendKind`]): a refused
/// value's error reaches CLI/MCP output, and serde's own quotes the value.
/// `no_field_of_the_config_ever_echoes_a_planted_value` enforces it.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DescriberConfig {
    pub backend: BackendKind,
    pub base_url: String,
    pub model: String,
    /// `OpenRouter` key. [`OPENROUTER_KEY_ENV`] (passed in by the caller as
    /// `env_key`) overrides so the file can stay keyless — but only when
    /// `backend` is `OpenRouter`; see `effective_api_key`.
    pub api_key: Option<String>,
}

/// Hand-written because `api_key` is a real key, which must never reach a
/// log, a panic message, or a test failure.
impl std::fmt::Debug for DescriberConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DescriberConfig")
            .field("backend", &self.backend)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
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
    /// Carries no `toml::de::Error`, neither rendered nor chained: its
    /// Display quotes the offending source line and its Debug holds the whole
    /// file, and that line can be `api_key = "sk-…"`. `message` holds no bytes
    /// of any value in the file. It is one of: the parser's own text without
    /// the snippet, which names fields ("missing field `model`") and the
    /// tokens the parser wanted ("expected newline, `#`"); the fixed sentence
    /// `without_echoed_values` puts in place of a type error; or
    /// [`BackendKind`]'s fixed text for a `backend` outside its set.
    #[error("parse {path}: line {line}: {message}")]
    Parse {
        path: String,
        line: usize,
        message: String,
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
        parse(path, &text).map(Some)
    }

    /// Write config to `path` via a same-directory `<path>.tmp` + rename, so
    /// a run killed mid-write never leaves a truncated file — the key in it
    /// may exist nowhere else. The temp file is created with 0600 permissions
    /// from the start (may hold an API key, so it must never exist
    /// world/group-readable even for the instant between create and chmod),
    /// and being a fresh file it replaces an older, wider one's mode too — and
    /// a `describer.toml` that is a symlink is replaced by a regular file.
    ///
    /// A store that fails once the temp file exists removes it before
    /// returning. The temp name is fixed rather than per-writer for the case
    /// that can't clean up after itself: a leftover from a killed run can hold
    /// a key, and a fixed name is the one the next store removes. Overlapping
    /// stores are not guarded against: only a settings save writes this file.
    ///
    /// # Errors
    /// Returns `ConfigError` when serialization or the write fails.
    pub fn store(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        let write_error = |source| ConfigError::Write {
            path: path.display().to_string(),
            source,
        };
        let tmp = tmp_path(path);
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigError::Write {
                    path: tmp.display().to_string(),
                    source,
                });
            }
        }
        let mut file = create_private(&tmp).map_err(write_error)?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        let stored = written.and_then(|()| std::fs::rename(&tmp, path));
        if stored.is_err() {
            // Best effort: the error worth reporting is the store's own.
            let _ = std::fs::remove_file(&tmp);
        }
        stored.map_err(write_error)
    }

    /// The key to send: the environment override wins, but only for
    /// `OpenRouter` — [`OPENROUTER_KEY_ENV`] naming that host explicitly, so it
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

/// Generic so the tests can hold it to a struct with fields
/// [`DescriberConfig`] does not have yet.
fn parse<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|error| ConfigError::Parse {
        path: path.display().to_string(),
        line: line_of(text, error.span()),
        message: without_echoed_values(error.message()),
    })
}

/// The 1-based line `span` starts on; 1 when the parser gave no position or
/// one that is not inside `text`.
fn line_of(text: &str, span: Option<std::ops::Range<usize>>) -> usize {
    span.and_then(|span| text.get(..span.start))
        .map_or(1, |before| before.matches('\n').count() + 1)
}

/// What a type error is reported as; see [`without_echoed_values`].
const WRONG_TYPE: &str = "a value has the wrong type — \
    every value in this file is a quoted string";

/// `message`, unless it is one of serde's type errors ("invalid type: integer
/// `5`, expected a string"), which quote the refused value back — in
/// backticks, or for a string in escaped double quotes that no cut is sound
/// for. Those become [`WRONG_TYPE`] whole, so every byte shown is ours.
fn without_echoed_values(message: &str) -> String {
    if message.starts_with("invalid type:") || message.starts_with("invalid value:") {
        return WRONG_TYPE.to_string();
    }
    message.to_string()
}

/// The temp file [`DescriberConfig::store`] writes before renaming it over
/// `path`.
fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Creates `path` anew, refusing an existing file: `mode` applies only on
/// create, so an existing file would keep whatever mode it had.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
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

    const VALID_HEAD: &str =
        "backend = \"open-router\"\nbase_url = \"https://openrouter.ai/api\"\nmodel = \"m\"\n\n";

    fn load_error_of(text: &str) -> ConfigError {
        load_error_in_dir(text).0
    }

    /// [`load_error_of`] plus the temp dir the file sat in, for a caller that
    /// has to tell the dir's own name from the file's contents.
    fn load_error_in_dir(text: &str) -> (ConfigError, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        std::fs::write(&path, text).expect("plant config");
        let err = DescriberConfig::load(&path).expect_err("must not parse");
        (err, dir)
    }

    /// Every rendering a head can reach: Display, Debug, and the whole chain
    /// as `majestical-services` wraps it (`{:#}` and `{:?}` of an anyhow
    /// error with a context on top).
    fn renderings_of(err: ConfigError) -> Vec<String> {
        let display = err.to_string();
        let debug = format!("{err:?}");
        let wrapped = anyhow::Error::from(err).context("load describer.toml");
        vec![
            display,
            debug,
            format!("{wrapped:#}"),
            format!("{wrapped:?}"),
        ]
    }

    /// The broken line is the one holding the key, because that is the line
    /// a TOML parse error would quote back.
    #[test]
    fn a_parse_error_names_the_line_and_never_quotes_it() {
        let err = load_error_of(&format!("{VALID_HEAD}api_key = \"sk-test\" oops\n"));
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        let display = err.to_string();
        assert!(display.contains("describer.toml: line 5: "), "{display}");
        for rendering in renderings_of(err) {
            assert!(!rendering.contains("sk-test"), "{rendering}");
        }
    }

    #[test]
    fn a_parse_error_counts_lines_without_a_trailing_newline_and_across_crlf() {
        let bare = load_error_of(&format!("{VALID_HEAD}api_key = \"sk-test\" oops"));
        assert!(bare.to_string().contains(": line 5: "), "{bare}");

        let crlf = format!("{VALID_HEAD}api_key = \"sk-test\" oops\n").replace('\n', "\r\n");
        let crlf = load_error_of(&crlf);
        assert!(crlf.to_string().contains(": line 5: "), "{crlf}");
        for rendering in renderings_of(crlf) {
            assert!(!rendering.contains("sk-test"), "{rendering}");
        }

        let first = load_error_of("api_key = \"sk-test\" oops\nmodel = \"m\"\n");
        assert!(first.to_string().contains(": line 1: "), "{first}");
    }

    /// However the key's own line is garbled, no rendering carries it.
    #[test]
    fn a_garbled_api_key_line_is_never_quoted() {
        for line in [
            "api_key = \"sk-test\" oops",
            "api_key = \"sk-test",
            "api_key = sk-test",
            "api_key \"sk-test\"",
            "api_key = 'sk-test' 'again'",
            "api_key = [\"sk-test\"]",
            "api_key = \"sk-test\"\napi_key = \"sk-test\"",
        ] {
            let err = load_error_of(&format!("{VALID_HEAD}{line}\n"));
            for rendering in renderings_of(err) {
                assert!(!rendering.contains("sk-test"), "{line}: {rendering}");
            }
        }
    }

    /// A key pasted into the wrong field: `backend` has a closed value set,
    /// and the error for a value outside it lists the set, not the value.
    #[test]
    fn a_wrong_backend_value_is_never_quoted() {
        let err = load_error_of("backend = \"sk-test\"\nbase_url = \"u\"\nmodel = \"m\"\n");
        let display = err.to_string();
        assert!(display.contains(": line 1: "), "{display}");
        assert!(
            display.contains("expected one of ollama, lm-studio, open-router"),
            "{display}"
        );
        for rendering in renderings_of(err) {
            assert!(!rendering.contains("sk-test"), "{rendering}");
        }
    }

    /// Through `load`, against the real parser: a well-formed value of the
    /// wrong type is reported as the fixed sentence. The file's key serves
    /// every backend, so an all-digit token pasted without quotes is a key
    /// like any other. Trips if a dependency bump rewords serde's type errors.
    #[test]
    fn a_wrong_typed_value_is_never_echoed() {
        let keyless = "backend = \"ollama\"\nbase_url = \"u\"\n";
        for (text, line, echoes) in [
            (format!("{VALID_HEAD}api_key = 12345\n"), 5, vec!["12345"]),
            (format!("{VALID_HEAD}api_key = 1.5\n"), 5, vec!["1.5"]),
            (format!("{VALID_HEAD}api_key = true\n"), 5, vec!["true"]),
            (
                format!("{VALID_HEAD}api_key = 123456789012345678901234\n"),
                5,
                vec!["1234567890", "901234"],
            ),
            (
                format!("{VALID_HEAD}api_key = 0xfeed42\n"),
                5,
                vec!["feed42", "16706882"],
            ),
            (format!("{keyless}model = 12345\n"), 3, vec!["12345"]),
        ] {
            let (err, dir) = load_error_in_dir(&text);
            let display = err.to_string();
            assert!(
                display.ends_with(&format!("describer.toml: line {line}: {WRONG_TYPE}")),
                "{display}"
            );
            for rendering in renderings_of(err) {
                // The temp dir's own name holds digits.
                let rendering = rendering.replace(&dir.path().display().to_string(), "<dir>");
                for echo in &echoes {
                    assert!(!rendering.contains(echo), "{text}: {rendering}");
                }
            }
        }

        let err = load_error_of("backend = \"ollama\"\nbase_url = [\"sk-test\"]\nmodel = \"m\"\n");
        for rendering in renderings_of(err) {
            assert!(!rendering.contains("sk-test"), "{rendering}");
        }
    }

    /// Replaces each field of `populated` in turn with a string, an integer,
    /// a float and a boolean, and holds every refusal to: no rendering carries
    /// the planted value or any other value of the file. The field list comes
    /// from the struct's own serialization, so a new field is covered the day
    /// it is added. `populated` must leave no field out (no `None`).
    fn assert_no_field_echoes_a_planted_value<T>(populated: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let table = toml::Table::try_from(populated).expect("a populated config is a table");
        let planted = [
            (toml::Value::String("sk-test".into()), "sk-test"),
            (toml::Value::Integer(8_675_309), "8675309"),
            (toml::Value::Float(86753.09), "86753"),
        ];
        for field in table.keys() {
            for (value, echo) in &planted {
                let mut table = table.clone();
                table.insert(field.clone(), value.clone());
                let text = toml::to_string(&table).expect("serialize");
                let Err(err) = parse::<T>(Path::new("describer.toml"), &text) else {
                    continue;
                };
                for rendering in renderings_of(err) {
                    assert!(!rendering.contains(echo), "{field} = {value}: {rendering}");
                    assert!(
                        !rendering.contains("sk-test"),
                        "{field} = {value}: {rendering}"
                    );
                }
            }
        }

        // "true" is too common a substring to look for; a refused boolean is
        // a type error, so the whole message is ours.
        for field in table.keys() {
            let mut table = table.clone();
            table.insert(field.clone(), toml::Value::Boolean(true));
            let text = toml::to_string(&table).expect("serialize");
            match parse::<T>(Path::new("describer.toml"), &text) {
                Ok(_) => {}
                Err(ConfigError::Parse { message, .. }) => assert_eq!(message, WRONG_TYPE),
                Err(other) => panic!("{field} = true: {other}"),
            }
        }
    }

    #[test]
    fn no_field_of_the_config_ever_echoes_a_planted_value() {
        let populated = keyed_config();
        assert!(populated.api_key.is_some(), "a `None` field is not covered");
        assert_no_field_echoes_a_planted_value(&populated);
    }

    /// The fields [`DescriberConfig`] does not have: a number, a float and a
    /// flag each refuse a string, and serde's refusal quotes it. This is the
    /// case the test above exists for, held here so it is proven before the
    /// first such field arrives.
    #[test]
    fn no_field_of_a_config_with_non_string_fields_would_echo_one_either() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct WithOtherKinds {
            api_key: String,
            timeout_secs: u32,
            temperature: f64,
            enabled: bool,
        }
        assert_no_field_echoes_a_planted_value(&WithOtherKinds {
            api_key: "sk-test".into(),
            timeout_secs: 30,
            temperature: 0.5,
            enabled: false,
        });
    }

    /// A missing field belongs to no line; the parser points at the table's
    /// start, which for this file is line 1.
    #[test]
    fn a_missing_field_is_reported_on_line_1_by_name() {
        let err = load_error_of("backend = \"ollama\"\nbase_url = \"u\"\n");
        let display = err.to_string();
        assert!(display.contains(": line 1: "), "{display}");
        assert!(display.contains("model"), "{display}");
    }

    /// A span is the parser's claim about `text`, so one that is absent,
    /// past the end, or inside a multi-byte character is line 1, not a panic.
    #[test]
    fn line_of_counts_newlines_before_the_span_and_falls_back_to_1() {
        let text = "a\nb\né\n";
        assert_eq!(line_of(text, Some(0..1)), 1);
        assert_eq!(line_of(text, Some(2..3)), 2);
        assert_eq!(line_of(text, Some(4..6)), 3);
        assert_eq!(line_of(text, Some(text.len()..text.len())), 4);
        assert_eq!(line_of(text, None), 1);
        assert_eq!(line_of(text, Some(5..6)), 1, "inside `é`");
        assert_eq!(line_of(text, Some(99..100)), 1, "past the end");
    }

    #[test]
    fn every_backend_round_trips_through_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        for backend in BackendKind::ALL {
            let config = DescriberConfig {
                backend,
                base_url: "u".into(),
                model: "m".into(),
                api_key: None,
            };
            config.store(&path).expect("store");
            let text = std::fs::read_to_string(&path).expect("read back");
            assert!(
                text.contains(&format!("backend = \"{}\"", backend.as_str())),
                "{text}"
            );
            assert_eq!(DescriberConfig::load(&path).expect("load"), Some(config));
        }
    }

    #[test]
    fn a_type_error_becomes_the_fixed_sentence_and_every_other_message_is_kept() {
        for echoing in [
            "invalid type: integer `12345`, expected a string",
            "invalid type: string \"sk-test\", expected u32",
            "invalid value: integer `7`, expected one of `1`, `2`",
        ] {
            assert_eq!(without_echoed_values(echoing), WRONG_TYPE);
        }
        for untouched in [
            "missing field `backend`",
            "invalid string, expected `\"`, `'`",
            "unknown backend — expected one of ollama, lm-studio, open-router",
        ] {
            assert_eq!(without_echoed_values(untouched), untouched);
        }
    }

    fn keyed_config() -> DescriberConfig {
        DescriberConfig {
            backend: BackendKind::OpenRouter,
            base_url: "https://openrouter.ai/api".into(),
            model: "m".into(),
            api_key: Some("sk-test".into()),
        }
    }

    /// A store that fails after its temp file exists must not leave it — it
    /// can hold a key. The target is a non-empty directory, which a file
    /// cannot be renamed over.
    #[test]
    fn a_failed_store_leaves_no_tmp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        std::fs::create_dir(&path).expect("mkdir");
        std::fs::write(path.join("occupant"), "x").expect("occupy");

        let err = keyed_config().store(&path).expect_err("rename must fail");

        assert!(matches!(err, ConfigError::Write { .. }), "{err}");
        assert!(!tmp_path(&path).exists());
    }

    /// A stale `<path>.tmp` that can't be removed is what the reader has to
    /// fix, so the error names it rather than the target.
    #[test]
    fn a_stale_tmp_that_cannot_be_removed_is_named_in_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        let tmp = tmp_path(&path);
        std::fs::create_dir(&tmp).expect("mkdir");
        std::fs::write(tmp.join("occupant"), "x").expect("occupy");

        let err = keyed_config()
            .store(&path)
            .expect_err("a directory is not removed");

        assert!(matches!(err, ConfigError::Write { .. }), "{err}");
        assert!(err.to_string().contains("describer.toml.tmp: "), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn store_replaces_the_file_atomically_and_leaves_no_tmp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        std::fs::write(&path, "an older file").expect("plant");
        #[cfg(unix)]
        let inode_before = file_id(&path);

        keyed_config().store(&path).expect("store");

        assert!(!tmp_path(&path).exists());
        assert_eq!(
            DescriberConfig::load(&path).expect("load"),
            Some(keyed_config())
        );
        #[cfg(unix)]
        assert_ne!(
            file_id(&path),
            inode_before,
            "the target is replaced by rename, never truncated in place"
        );
    }

    #[cfg(unix)]
    fn file_id(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).expect("meta").ino()
    }

    /// `.mode()` applies only when a file is created, so a rewrite in place
    /// would leave an older, wider file as wide as it was.
    #[cfg(unix)]
    #[test]
    fn store_over_a_world_readable_file_leaves_it_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        std::fs::write(&path, "an older file").expect("plant");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        keyed_config().store(&path).expect("store");

        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// A run killed between create and rename leaves `<path>.tmp` behind;
    /// the next store must neither trip on it nor inherit its permissions.
    #[test]
    fn store_survives_a_stale_tmp_from_a_crashed_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("describer.toml");
        let tmp = tmp_path(&path);
        std::fs::write(&tmp, "half a fi").expect("plant stale tmp");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        }

        keyed_config().store(&path).expect("store");

        assert!(!tmp.exists());
        assert_eq!(
            DescriberConfig::load(&path).expect("load"),
            Some(keyed_config())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_describer_config_never_debug_prints_the_key() {
        let rendered = format!("{:?}", keyed_config());
        assert!(!rendered.contains("sk-test"), "{rendered}");
        assert!(
            rendered.contains("api_key: Some(\"<redacted>\")"),
            "{rendered}"
        );
        let keyless = DescriberConfig {
            api_key: None,
            ..keyed_config()
        };
        assert!(format!("{keyless:?}").contains("api_key: None"));
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
