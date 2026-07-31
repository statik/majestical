# Phase 5 — Describers, Transcription, OCR, PDF, Text Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Caption/tag suggestion via Ollama/LM Studio/OpenRouter behind one adapter, in-process whisper.cpp transcription, Apple Vision OCR, PDFKit extraction, and an N-way fused query layer over the new text.

**Architecture:** Local derivers (whisper, Vision, PDFKit, MiniLM) join `crates/index` beside the SigLIP encoder on the existing model-fetch + queue-as-diff machinery; a new `Describer` port in `crates/core` is implemented by a new `crates/describe` HTTP adapter. Blobs stay the exchange format; SQLite `text_fts` and a 384-d Lance `text_chunks` table are rebuildable projections of blobs. No new CRDT ops.

**Tech Stack:** whisper-rs 0.16 (Metal), objc2-vision / objc2-pdf-kit 0.3.2, tokenizers + ort (existing), ureq 3.3, httpmock 0.8 (dev), lancedb (existing), FTS5.

**Spec:** `docs/superpowers/specs/2026-07-31-phase5-describers-design.md`

---

## Planning-time discoveries (record deviations in as-built notes)

1. **`model.rs` is single-model today** (`MODEL_TAG`/`MODEL_FILES` consts, `model_dir()` hardwired). PR 2 generalizes to a `ModelSpec` registry (siglip2, whisper, minilm) and adds `maj model fetch --only <tag>` so CI jobs fetch only what they gate on.
2. **No HTTP client exists in the workspace** (model fetch shells to `curl`). `crates/describe` uses `ureq` 3.3 (blocking, matches the sync codebase); dialect tests use `httpmock` 0.8 (sync API — wiremock is async-only).
3. **whisper-rs 0.16 returns timestamps in centiseconds** (10 ms units) via the segment-object API (`full_n_segments`/`get_segment`); multiply by 10 for ms. Feature `metal`, not `coreml` (Core ML needs a separate 1 GB encoder artifact — not worth it; Metal is fast enough).
4. **MiniLM's official ONNX has no pooled output** — `last_hidden_state [1, seq, 384]` only; `text_encoder.rs` mean-pools over the attention mask, then reuses `encoder::l2_normalize`.
5. **Caption input is the existing `thumb-320.webp` blob**, not a fresh full-res decode — thumbnails precede captions in the queue priority, VLMs downscale anyway, and it makes caption cost independent of source size. Video keyframes are captioned from `video::extract_frame` output re-encoded as WebP at 512 px.
6. **`text_fts` is a projection of blobs, healed every `index run`** (mirror of `load_missing_vectors_from_blobs`): projection rebuild drops it (schema.rs DROP+CREATE), the next `index run` refills it from blobs. `SNAPSHOT_VERSION` bumps 5→6 (PR 4) and 6→7 (PR 6, media-kind change).
7. **A second Lance table `text_chunks` (384-d)** lives beside the 768-d `vectors` table in the same `<state>/lance` dir. It stores the chunk **text** as a column so semantic hits can print snippets without a second lookup.
8. **`MediaKind` gains `Audio` and `Pdf` variants** (PR 6). This also folds in the watchlist item about missing extensions (`mpg`/`mpeg`/`3gp`/`wmv`/`insv`, `jxl`/`pef`/`iiq`/`3fr`). `Op::AssetSeen` carries no kind — kind is derived from path at projection time — so the wire format is untouched.
9. **objc2 framework APIs are `unsafe fn` almost throughout.** All unsafe stays inside `ocr.rs`/`pdf.rs` behind safe functions with SAFETY comments. `objc2-pdf-kit` needs its `PDFPage` and `objc2-app-kit` features for text + thumbnail rendering.
10. **ffmpeg audio extraction ships with a duration-scaled timeout** via a std-only polling helper (`try_wait` + 100 ms sleep) — no new dependency. Existing ffmpeg calls keep their watchlist item.
11. **Failure markers live in `<state>/index-failures.json`**, rewritten each `index run`, so `index status` can say "failed last time: <reason>" and failed items re-plan next run.
12. **Upstream pins the implementer must resolve at execution time** (exact commands are in the tasks; never trust memory): the current commit sha of `ggerganov/whisper.cpp` (for the pinned model URL) and of `sentence-transformers/all-MiniLM-L6-v2`, plus sha256 for MiniLM's `onnx/model.onnx` and `tokenizer.json`. The whisper ggml sha256 was verified at planning time: `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` (574,041,195 bytes).

## Conventions for every task in this plan

- **TDD**: write the failing test, run it, watch it fail, implement, watch it pass. Test commands below name the exact test.
- **Gate**: `just check` (fmt + clippy `-D warnings`) before every commit; `cargo test -p <crate>` for the touched crate. Zero warnings — workspace lints deny `unwrap_used`, `panic`, `print_stdout`/`print_stderr` (CLI crate allows printing; library crates do not — diagnostics go through `&mut dyn FnMut(&str)` callbacks like `model::fetch`).
- **Stage only your own files** (`git add <paths>`, never `git add -A`). No Claude-Session trailers.
- **Library code never prints.** New `crates/index` modules and `crates/describe` return errors or use callbacks; only `crates/cli` prints.
- **Docs**: `/// # Errors` on every public `Result` fn (clippy pedantic).
- **Versions**: verify each new dependency's current version on crates.io at execution time before writing it into a Cargo.toml (`curl -s https://crates.io/api/v1/crates/<name> | jq -r .crate.max_stable_version`). Versions in this plan were verified 2026-07-31.
- **PRs**: branch per PR, squash-merge after green CI, via
  `git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git <branch>`.

---

# PR 1 — Describer port + `crates/describe` + `maj describer set|show|test`

### Task 1: `Describer` port in core

**Files:**
- Modify: `crates/core/src/ports.rs` (append after `CatalogStore`, ~line 125)

- [ ] **Step 1: Write the failing test** (in `ports.rs`'s existing `#[cfg(test)] mod tests`, or a new one at the bottom if none exists)

```rust
#[cfg(test)]
mod describer_tests {
    use super::{Caption, TagSubject, TagSuggestion};

    #[test]
    fn tag_suggestion_serializes_round_trip() {
        let s = TagSuggestion {
            tag: "person/dana".into(),
            confidence: 0.87,
            in_vocab: true,
            model_tag: "describe-qwen3-vl-8b".into(),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: TagSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tag, "person/dana");
        assert!((back.confidence - 0.87).abs() < f64::EPSILON);
    }

    #[test]
    fn caption_serializes_round_trip() {
        let c = Caption { text: "a red barn at dusk".into(), model_tag: "describe-x".into() };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Caption = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.text, "a red barn at dusk");
    }

    #[test]
    fn tag_subject_borrows_without_clone() {
        let captions = vec!["a".to_string(), "b".to_string()];
        let subject = TagSubject::Captions(&captions);
        let TagSubject::Captions(inner) = subject else { panic!("wrong variant") };
        assert_eq!(inner.len(), 2);
    }
}
```

Note: `crates/core` needs `serde_json` as a dev-dependency if it isn't one already (check `crates/core/Cargo.toml`; `serde_json` is in workspace deps).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-core describer_tests`
Expected: compile error — `Caption`/`TagSubject`/`TagSuggestion` not found.

- [ ] **Step 3: Implement the port** (append to `crates/core/src/ports.rs`)

```rust
/// A model-produced caption for one asset (or one video keyframe).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Caption {
    pub text: String,
    /// Derivation model tag, e.g. `describe-qwen3-vl-8b`.
    pub model_tag: String,
}

/// One suggested tag, pending human confirmation. Never written to the
/// event log — confirmation emits a plain `TagAdd`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TagSuggestion {
    pub tag: String,
    /// Model-reported confidence in [0, 1].
    pub confidence: f64,
    /// True when `tag` was already in the catalog's folksonomy.
    pub in_vocab: bool,
    pub model_tag: String,
}

/// What tag suggestion looks at: the image itself for stills, the pooled
/// keyframe captions (text-only call) for video.
#[derive(Debug)]
pub enum TagSubject<'a> {
    Image(&'a [u8]),
    Captions(&'a [String]),
}

/// Captions + open-vocabulary tag suggestion via a configured backend.
/// Implementations live outside core (`crates/describe`).
pub trait Describer {
    /// Caption one image (encoded bytes, e.g. WebP).
    ///
    /// # Errors
    /// Returns `PortError` when the backend is unreachable, the model
    /// rejects the request, or the response cannot be parsed.
    fn caption(&self, image: &[u8]) -> Result<Caption, PortError>;

    /// Suggest tags for a subject, classified against `existing_vocab`.
    ///
    /// # Errors
    /// Returns `PortError` when the backend is unreachable, the model
    /// rejects the request, or the response cannot be parsed.
    fn suggest_tags(
        &self,
        subject: TagSubject<'_>,
        existing_vocab: &[String],
    ) -> Result<Vec<TagSuggestion>, PortError>;
}
```

`TagSuggestion` has `f64` so it is `PartialEq` only — do not derive `Eq`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p majestical-core describer_tests`
Expected: 3 passed.

- [ ] **Step 5: Gate and commit**

```bash
just check && cargo test -p majestical-core
git add crates/core/src/ports.rs crates/core/Cargo.toml
git commit -m "feat: Describer port in core"
```

### Task 2: `crates/describe` — config + OpenAI-compatible client

**Files:**
- Create: `crates/describe/Cargo.toml`
- Create: `crates/describe/src/lib.rs`
- Create: `crates/describe/src/config.rs`
- Create: `crates/describe/src/client.rs`
- Modify: `Cargo.toml` (workspace members + `[workspace.dependencies]`)

- [ ] **Step 1: Wire the crate skeleton**

Workspace `Cargo.toml`: add `"crates/describe"` to `members`, and to `[workspace.dependencies]` (verify current versions first, per conventions):

```toml
ureq = { version = "3.3.0", features = ["json"] }
base64 = "0.22.1"
toml = "1.1.4"
httpmock = "0.8.3"
```

`crates/describe/Cargo.toml`:

```toml
[package]
name = "majestical-describe"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
majestical-core = { path = "../core" }
ureq.workspace = true
base64.workspace = true
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
thiserror.workspace = true

[dev-dependencies]
httpmock.workspace = true
tempfile.workspace = true

[lints]
workspace = true
```

`crates/describe/src/lib.rs`:

```rust
//! HTTP describer adapter: Ollama / LM Studio / OpenRouter behind one
//! OpenAI-compatible client, implementing `majestical_core::ports::Describer`.

pub mod client;
pub mod config;

pub use client::{HttpDescriber, ProbeReport};
pub use config::{BackendKind, DescriberConfig};
```

- [ ] **Step 2: Write the failing config tests** (bottom of `config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_urls_per_backend() {
        assert_eq!(BackendKind::Ollama.default_base_url(), "http://localhost:11434");
        assert_eq!(BackendKind::LmStudio.default_base_url(), "http://localhost:1234");
        assert_eq!(BackendKind::OpenRouter.default_base_url(), "https://openrouter.ai/api");
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
        let loaded = DescriberConfig::load(&path).expect("load").expect("present");
        assert_eq!(loaded, config);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "describer.toml must be 0600");
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(DescriberConfig::load(&dir.path().join("nope.toml")).expect("ok").is_none());
    }

    #[test]
    fn env_key_wins_over_file_key() {
        let config = DescriberConfig {
            backend: BackendKind::OpenRouter,
            base_url: "u".into(),
            model: "m".into(),
            api_key: Some("file-key".into()),
        };
        assert_eq!(config.effective_api_key(Some("env-key".into())).as_deref(), Some("env-key"));
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
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p majestical-describe config`
Expected: compile error — types not defined.

- [ ] **Step 4: Implement `config.rs`**

```rust
//! Per-machine, per-catalog describer configuration (`describer.toml` in
//! the state dir). Never synced: endpoints and API keys are machine-local.

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
    /// OpenRouter key. `MAJ_OPENROUTER_KEY` (passed in by the caller as
    /// `env_key`) overrides so the file can stay keyless.
    pub api_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read {path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("write {path}: {source}")]
    Write { path: String, source: std::io::Error },
    #[error("parse {path}: {source}")]
    Parse { path: String, source: toml::de::Error },
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
                return Err(ConfigError::Read { path: path.display().to_string(), source });
            }
        };
        let config = toml::from_str(&text)
            .map_err(|source| ConfigError::Parse { path: path.display().to_string(), source })?;
        Ok(Some(config))
    }

    /// Write config to `path` with 0600 permissions (may hold an API key).
    ///
    /// # Errors
    /// Returns `ConfigError` when serialization or the write fails.
    pub fn store(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        let write = |source| ConfigError::Write { path: path.display().to_string(), source };
        std::fs::write(path, text).map_err(write)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(write)?;
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
```

- [ ] **Step 5: Run config tests**

Run: `cargo test -p majestical-describe config`
Expected: 5 passed.

- [ ] **Step 6: Write the failing client dialect tests** (bottom of `client.rs`). These are the dialect fixtures the spec demands: each asserts the exact request shape a backend's reference client produces.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendKind, DescriberConfig};
    use httpmock::prelude::*;
    use majestical_core::ports::{Describer, TagSubject};

    fn config_for(server: &MockServer, backend: BackendKind, key: Option<&str>) -> DescriberConfig {
        DescriberConfig {
            backend,
            base_url: server.base_url(),
            model: "test-model".into(),
            api_key: key.map(str::to_string),
        }
    }

    fn caption_body() -> serde_json::Value {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "a red barn at dusk"}}]
        })
    }

    #[test]
    fn ollama_caption_sends_base64_data_url_no_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .matches(|req| {
                    let body: serde_json::Value =
                        serde_json::from_slice(req.body.as_deref().unwrap_or(&[])).unwrap_or_default();
                    let url = body["messages"][0]["content"][1]["image_url"]["url"]
                        .as_str()
                        .unwrap_or_default();
                    let no_auth = !req.headers.clone().unwrap_or_default().iter().any(|(k, _)| {
                        k.eq_ignore_ascii_case("authorization")
                    });
                    url.starts_with("data:image/webp;base64,") && no_auth
                        && body["model"] == "test-model"
                });
            then.status(200).json_body(caption_body());
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::Ollama, None), None);
        let caption = describer.caption(&[1, 2, 3]).expect("caption");
        assert_eq!(caption.text, "a red barn at dusk");
        assert_eq!(caption.model_tag, "describe-test-model");
        mock.assert();
    }

    #[test]
    fn openrouter_sends_bearer_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header("authorization", "Bearer sk-test");
            then.status(200).json_body(caption_body());
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::OpenRouter, Some("sk-test")), None);
        describer.caption(&[1, 2, 3]).expect("caption");
        mock.assert();
    }

    #[test]
    fn suggest_tags_parses_strict_json_and_marks_vocab() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant",
                    "content": "{\"tags\":[{\"tag\":\"person/dana\",\"confidence\":0.9},{\"tag\":\"barn\",\"confidence\":0.6}]}"}}]
            }));
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::Ollama, None), None);
        let vocab = vec!["person/dana".to_string(), "status/select".to_string()];
        let suggestions = describer
            .suggest_tags(TagSubject::Image(&[1, 2, 3]), &vocab)
            .expect("suggest");
        assert_eq!(suggestions.len(), 2);
        assert!(suggestions[0].in_vocab);
        assert!(!suggestions[1].in_vocab);
    }

    #[test]
    fn suggest_tags_retries_once_on_malformed_json() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "Sure! Here are tags: barn"}}]
            }));
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::Ollama, None), None);
        let result = describer.suggest_tags(TagSubject::Image(&[1]), &[]);
        assert!(result.is_err(), "two malformed responses must error, not panic");
        assert_eq!(mock.hits(), 2, "exactly one retry");
    }

    #[test]
    fn captions_subject_is_text_only_no_image_part() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions").matches(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body.as_deref().unwrap_or(&[])).unwrap_or_default();
                // Text-only call: content is a plain string, no content array.
                body["messages"][0]["content"].is_string()
            });
            then.status(200).json_body(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "{\"tags\":[]}"}}]
            }));
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::Ollama, None), None);
        let captions = vec!["a barn".to_string(), "a field".to_string()];
        describer
            .suggest_tags(TagSubject::Captions(&captions), &[])
            .expect("suggest");
        mock.assert();
    }

    #[test]
    fn probe_reports_lm_studio_vision_capability() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v1/models");
            then.status(200).json_body(serde_json::json!({"data": [{"id": "test-model"}]}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/api/v1/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "test-model", "type": "llm",
                          "capabilities": {"vision": false}}]
            }));
        });
        let describer =
            HttpDescriber::new(config_for(&server, BackendKind::LmStudio, None), None);
        let report = describer.probe().expect("probe");
        assert!(report.reachable);
        assert!(report.model_listed);
        assert_eq!(report.vision, Some(false));
    }

    #[test]
    fn probe_unreachable_is_err_not_panic() {
        let config = DescriberConfig {
            backend: BackendKind::Ollama,
            base_url: "http://127.0.0.1:1".into(),
            model: "m".into(),
            api_key: None,
        };
        assert!(HttpDescriber::new(config, None).probe().is_err());
    }
}
```

- [ ] **Step 7: Run to verify failure**

Run: `cargo test -p majestical-describe client`
Expected: compile error — `HttpDescriber` not defined.

- [ ] **Step 8: Implement `client.rs`**

```rust
//! One OpenAI-compatible chat client serving all three backends, with each
//! backend's dialect quirks kept explicit and pinned by tests.

use base64::Engine as _;
use majestical_core::ports::{Caption, Describer, PortError, TagSubject, TagSuggestion};

use crate::config::{BackendKind, DescriberConfig};

const CONNECT_TIMEOUT_SECS: u64 = 10;
const REQUEST_TIMEOUT_SECS: u64 = 120;

const CAPTION_PROMPT: &str = "Describe this image in one concise sentence for a media \
    catalog. Reply with only the caption, no preamble.";

fn tags_prompt(vocab: &[String]) -> String {
    let vocab_list = if vocab.is_empty() { "(none yet)".to_string() } else { vocab.join(", ") };
    format!(
        "Suggest tags for this media. Existing catalog tags: {vocab_list}. Prefer \
         existing tags when they apply; propose new lowercase tags only when clearly \
         warranted. Reply with ONLY this JSON, no other text: \
         {{\"tags\":[{{\"tag\":\"...\",\"confidence\":0.0}}]}}"
    )
}

#[derive(Debug, thiserror::Error)]
enum DescribeHttpError {
    #[error("request to {url}: {message}")]
    Request { url: String, message: String },
    #[error("backend returned malformed JSON after retry: {snippet}")]
    Malformed { snippet: String },
    #[error("backend response missing choices[0].message.content")]
    Shape,
}

/// Result of `maj describer test`'s live probe.
#[derive(Debug)]
pub struct ProbeReport {
    pub reachable: bool,
    pub model_listed: bool,
    /// LM Studio only: whether the configured model reports vision support.
    pub vision: Option<bool>,
}

pub struct HttpDescriber {
    config: DescriberConfig,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl HttpDescriber {
    /// `env_key` is the caller-read `MAJ_OPENROUTER_KEY` (core stays free
    /// of env access; the CLI reads it).
    #[must_use]
    pub fn new(config: DescriberConfig, env_key: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)))
            .timeout_global(Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .into();
        let api_key = config.effective_api_key(env_key);
        Self { config, api_key, agent }
    }

    fn chat_url(&self) -> String {
        format!("{}/v1/chat/completions", self.config.base_url.trim_end_matches('/'))
    }

    fn post_chat(&self, content: serde_json::Value) -> Result<String, DescribeHttpError> {
        let url = self.chat_url();
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": content}],
        });
        let mut request = self.agent.post(&url);
        if let Some(key) = &self.api_key {
            request = request.header("authorization", &format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(&body)
            .map_err(|error| DescribeHttpError::Request { url: url.clone(), message: error.to_string() })?;
        let parsed: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|error| DescribeHttpError::Request { url, message: error.to_string() })?;
        parsed["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or(DescribeHttpError::Shape)
    }

    fn image_content(&self, image: &[u8], prompt: &str) -> serde_json::Value {
        // All three backends accept base64 data URLs on the OpenAI-compatible
        // endpoint; Ollama accepts ONLY data URLs (never http URLs), which is
        // why data URLs are the one shared dialect.
        let encoded = base64::engine::general_purpose::STANDARD.encode(image);
        serde_json::json!([
            {"type": "text", "text": prompt},
            {"type": "image_url", "image_url": {"url": format!("data:image/webp;base64,{encoded}")}},
        ])
    }

    fn parse_tags(&self, content: &str, vocab: &[String]) -> Option<Vec<TagSuggestion>> {
        #[derive(serde::Deserialize)]
        struct Wire {
            tags: Vec<WireTag>,
        }
        #[derive(serde::Deserialize)]
        struct WireTag {
            tag: String,
            confidence: f64,
        }
        let wire: Wire = serde_json::from_str(content.trim()).ok()?;
        let model_tag = self.config.model_tag();
        Some(
            wire.tags
                .into_iter()
                .map(|t| TagSuggestion {
                    in_vocab: vocab.contains(&t.tag),
                    tag: t.tag,
                    confidence: t.confidence.clamp(0.0, 1.0),
                    model_tag: model_tag.clone(),
                })
                .collect(),
        )
    }

    fn tags_once(
        &self,
        subject: &TagSubject<'_>,
        prompt: &str,
    ) -> Result<String, DescribeHttpError> {
        let content = match subject {
            TagSubject::Image(image) => self.image_content(image, prompt),
            TagSubject::Captions(captions) => {
                serde_json::Value::String(format!("{prompt}\n\nKeyframe captions:\n{}", captions.join("\n")))
            }
        };
        self.post_chat(content)
    }

    /// Live probe used by `maj describer test`.
    ///
    /// # Errors
    /// Returns `PortError` when the backend cannot be reached at all.
    pub fn probe(&self) -> Result<ProbeReport, PortError> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = format!("{base}/v1/models");
        let mut request = self.agent.get(&url);
        if let Some(key) = &self.api_key {
            request = request.header("authorization", &format!("Bearer {key}"));
        }
        let mut response = request.call().map_err(|error| {
            PortError::new(format!("describer probe {url}"), std::io::Error::other(error.to_string()))
        })?;
        let parsed: serde_json::Value = response.body_mut().read_json().map_err(|error| {
            PortError::new(format!("describer probe {url}"), std::io::Error::other(error.to_string()))
        })?;
        let model_listed = parsed["data"]
            .as_array()
            .is_some_and(|models| models.iter().any(|m| m["id"] == self.config.model.as_str()));
        let vision = self.lm_studio_vision(base);
        Ok(ProbeReport { reachable: true, model_listed, vision })
    }

    fn lm_studio_vision(&self, base: &str) -> Option<bool> {
        if self.config.backend != BackendKind::LmStudio {
            return None;
        }
        let url = format!("{base}/api/v1/models");
        let mut response = self.agent.get(&url).call().ok()?;
        let parsed: serde_json::Value = response.body_mut().read_json().ok()?;
        parsed["data"].as_array()?.iter().find_map(|m| {
            (m["id"] == self.config.model.as_str())
                .then(|| m["capabilities"]["vision"].as_bool())
                .flatten()
        })
    }
}

fn port_error(context: &str, error: DescribeHttpError) -> PortError {
    PortError::new(context.to_string(), std::io::Error::other(error.to_string()))
}

impl Describer for HttpDescriber {
    fn caption(&self, image: &[u8]) -> Result<Caption, PortError> {
        let content = self.image_content(image, CAPTION_PROMPT);
        let text = self
            .post_chat(content)
            .map_err(|error| port_error("describer caption", error))?;
        Ok(Caption { text: text.trim().to_string(), model_tag: self.config.model_tag() })
    }

    fn suggest_tags(
        &self,
        subject: TagSubject<'_>,
        existing_vocab: &[String],
    ) -> Result<Vec<TagSuggestion>, PortError> {
        let prompt = tags_prompt(existing_vocab);
        let first = self
            .tags_once(&subject, &prompt)
            .map_err(|error| port_error("describer tags", error))?;
        if let Some(tags) = self.parse_tags(&first, existing_vocab) {
            return Ok(tags);
        }
        // One retry with an even stricter instruction — VLMs sometimes wrap
        // JSON in prose on the first attempt.
        let strict = format!("{prompt} Reply with ONLY the JSON object.");
        let second = self
            .tags_once(&subject, &strict)
            .map_err(|error| port_error("describer tags retry", error))?;
        self.parse_tags(&second, existing_vocab).ok_or_else(|| {
            let snippet: String = second.chars().take(120).collect();
            port_error("describer tags", DescribeHttpError::Malformed { snippet })
        })
    }
}
```

Adjust ureq 3.x builder method names against the version you land (`timeout_connect`/`timeout_global` naming moved between 3.x minors — check docs.rs for the exact builder methods; the behavior required is a 10 s connect timeout and 120 s overall timeout).

- [ ] **Step 9: Run to verify pass**

Run: `cargo test -p majestical-describe`
Expected: all config + client tests pass.

- [ ] **Step 10: Gate and commit**

```bash
just check && cargo test -p majestical-describe
git add Cargo.toml crates/describe
git commit -m "feat: describe crate — config + OpenAI-compatible client"
```

### Task 3: CLI `maj describer set|show|test`

**Files:**
- Create: `crates/cli/src/describer_cmd.rs`
- Modify: `crates/cli/src/main.rs` (module list line 2-9, `Cmd` enum ~line 35, dispatch in `main`)
- Modify: `crates/cli/Cargo.toml` (add `majestical-describe` path dep)
- Test: `crates/cli/tests/describer_smoke.rs`

- [ ] **Step 1: Write the failing CLI test**

```rust
mod common;
use common::maj;
use predicates::str::contains;

#[test]
fn describer_set_show_round_trip_redacts_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();

    maj(&root, &state)
        .args([
            "describer", "set", "--backend", "open-router", "--model", "qwen/qwen3-vl-8b",
            "--api-key", "sk-secret",
        ])
        .assert()
        .success()
        .stdout(contains("open-router").and(contains("qwen/qwen3-vl-8b")));

    maj(&root, &state)
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("open-router"))
        .stdout(contains("(redacted)"))
        .stdout(contains("sk-secret").not());
}

#[test]
fn describer_show_without_config_names_the_remedy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    maj(&root, &state)
        .args(["describer", "show"])
        .assert()
        .success()
        .stdout(contains("no describer configured").and(contains("maj describer set")));
}

#[test]
fn describer_set_defaults_base_url_per_backend() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    maj(&root, &state)
        .args(["describer", "set", "--backend", "ollama", "--model", "qwen3-vl:8b"])
        .assert()
        .success()
        .stdout(contains("http://localhost:11434"));
}

#[test]
fn describer_test_against_unreachable_backend_fails_with_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    maj(&root, &state)
        .args([
            "describer", "set", "--backend", "ollama", "--model", "m",
            "--base-url", "http://127.0.0.1:1",
        ])
        .assert()
        .success();
    maj(&root, &state).args(["describer", "test"]).assert().failure().stderr(contains("127.0.0.1:1"));
}
```

Note `predicates`' `contains(...).and(...)`/`.not()` composition — already a dev-dep. Import `predicates::prelude::*` if needed for `.and`/`.not`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test describer_smoke`
Expected: FAIL — `describer` is not a `maj` subcommand yet.

- [ ] **Step 3: Implement `describer_cmd.rs`**

```rust
//! `maj describer set|show|test` — per-machine backend configuration.

use std::path::Path;

use anyhow::{Context as _, bail};
use majestical_describe::{BackendKind, DescriberConfig, HttpDescriber};

use crate::state_dir;

pub(crate) fn config_path(catalog_root: &Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(state_dir::state_dir_for(catalog_root)?.join("describer.toml"))
}

/// Load the configured describer, if any. Shared with index_cmd (PR 8).
pub(crate) fn load_config(catalog_root: &Path) -> anyhow::Result<Option<DescriberConfig>> {
    let path = config_path(catalog_root)?;
    DescriberConfig::load(&path).with_context(|| format!("load {}", path.display()))
}

pub(crate) fn env_api_key() -> Option<String> {
    std::env::var("MAJ_OPENROUTER_KEY").ok().filter(|k| !k.is_empty())
}

pub(crate) struct SetArgs {
    pub backend: BackendKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

pub(crate) fn cmd_set(catalog_root: &Path, args: &SetArgs) -> anyhow::Result<()> {
    let config = DescriberConfig {
        backend: args.backend,
        base_url: args
            .base_url
            .clone()
            .unwrap_or_else(|| args.backend.default_base_url().to_string()),
        model: args.model.clone(),
        api_key: args.api_key.clone(),
    };
    let path = config_path(catalog_root)?;
    config.store(&path).with_context(|| format!("write {}", path.display()))?;
    print_config(&config);
    Ok(())
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    match load_config(catalog_root)? {
        Some(config) => print_config(&config),
        None => println!("no describer configured — run `maj describer set --backend <ollama|lm-studio|open-router> --model <model>`"),
    }
    Ok(())
}

pub(crate) fn cmd_test(catalog_root: &Path) -> anyhow::Result<()> {
    let Some(config) = load_config(catalog_root)? else {
        bail!("no describer configured — run `maj describer set`");
    };
    let base_url = config.base_url.clone();
    let model = config.model.clone();
    let describer = HttpDescriber::new(config, env_api_key());
    let report = describer
        .probe()
        .with_context(|| format!("describer test against {base_url}"))?;
    println!("backend reachable: yes");
    println!("model {model} listed: {}", if report.model_listed { "yes" } else { "NO — check the model name" });
    match report.vision {
        Some(true) => println!("vision capability: yes"),
        Some(false) => println!("vision capability: NO — caption work will not run with this model"),
        None => println!("vision capability: unknown (reported by LM Studio only)"),
    }
    if report.model_listed && report.vision != Some(false) {
        println!("caption and tag-suggestion work will run on the next `maj index run`");
    }
    Ok(())
}

fn print_config(config: &DescriberConfig) {
    println!("backend:  {}", config.backend.as_str());
    println!("base-url: {}", config.base_url);
    println!("model:    {}", config.model);
    match &config.api_key {
        Some(_) => println!("api-key:  (redacted)"),
        None => println!("api-key:  (none)"),
    }
}
```

`state_dir::state_dir_for` is currently `pub(crate)` — no change needed since `describer_cmd` is in the same crate.

- [ ] **Step 4: Wire into `main.rs`**

Module list: add `mod describer_cmd;`. `Cmd` enum: add

```rust
/// Configure the caption/tag-suggestion backend for this machine.
Describer {
    #[command(subcommand)]
    cmd: DescriberCmd,
},
```

and the sibling enum + clap `ValueEnum` for the backend:

```rust
#[derive(clap::Subcommand)]
enum DescriberCmd {
    /// Set the backend for this catalog on this machine.
    Set {
        #[arg(long, value_enum)]
        backend: DescriberBackendArg,
        #[arg(long)]
        model: String,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Show the current configuration (key redacted).
    Show,
    /// Probe the backend: connectivity, model presence, vision capability.
    Test,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DescriberBackendArg {
    Ollama,
    LmStudio,
    OpenRouter,
}

impl From<DescriberBackendArg> for majestical_describe::BackendKind {
    fn from(arg: DescriberBackendArg) -> Self {
        match arg {
            DescriberBackendArg::Ollama => Self::Ollama,
            DescriberBackendArg::LmStudio => Self::LmStudio,
            DescriberBackendArg::OpenRouter => Self::OpenRouter,
        }
    }
}
```

Dispatch arm in `main` (no catalog open needed — the `Model` precedent at main.rs:335):

```rust
Cmd::Describer { cmd } => match cmd {
    DescriberCmd::Set { backend, model, base_url, api_key } => describer_cmd::cmd_set(
        &cli.catalog,
        &describer_cmd::SetArgs { backend: backend.into(), model, base_url, api_key },
    ),
    DescriberCmd::Show => describer_cmd::cmd_show(&cli.catalog),
    DescriberCmd::Test => describer_cmd::cmd_test(&cli.catalog),
},
```

`crates/cli/Cargo.toml`: add `majestical-describe = { path = "../describe" }`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p majestical-cli --test describer_smoke`
Expected: 4 passed.

- [ ] **Step 6: Gate, commit, open PR 1**

```bash
just check && cargo test -p majestical-cli
git add crates/cli/src/describer_cmd.rs crates/cli/src/main.rs crates/cli/Cargo.toml crates/cli/tests/describer_smoke.rs Cargo.lock
git commit -m "feat: maj describer set|show|test"
```

Open PR "feat: describer backend adapter + CLI config" with Tasks 1-3; squash-merge after CI green.

# PR 2 — Model registry + MiniLM text encoder + conformance

### Task 4: Generalize `model.rs` to a `ModelSpec` registry

**Files:**
- Modify: `crates/index/src/model.rs`
- Modify: `crates/index/src/encoder.rs` (call sites of `model_dir`)
- Modify: `crates/cli/src/index_cmd.rs:917` (`cmd_model_fetch`), `crates/cli/src/main.rs:227` (`ModelCmd::Fetch` gains `--only`)
- Modify: `justfile` (`encoder-conformance` recipe passes `--only siglip2-b16-v1`)
- Modify: `.github/workflows/ci.yml` (encoder-conformance cache key unchanged; recipe change keeps its download 1 GB, not 1.7 GB)

- [ ] **Step 1: Resolve upstream pins** (execution-time verification — do not trust memory)

```bash
# whisper.cpp model repo: current main commit sha
curl -s "https://huggingface.co/api/models/ggerganov/whisper.cpp" | jq -r .sha
# MiniLM repo: current main commit sha
curl -s "https://huggingface.co/api/models/sentence-transformers/all-MiniLM-L6-v2" | jq -r .sha
# MiniLM file sizes + sha256 (LFS oid) at that revision
curl -s "https://huggingface.co/api/models/sentence-transformers/all-MiniLM-L6-v2/tree/main/onnx" | jq '.[] | select(.path=="onnx/model.onnx")'
curl -s "https://huggingface.co/api/models/sentence-transformers/all-MiniLM-L6-v2/tree/main" | jq '.[] | select(.path=="tokenizer.json")'
```

Record the shas into the consts in Step 4. Known at planning time: `ggml-large-v3-turbo-q5_0.bin` = 574,041,195 bytes, sha256 `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2`; `onnx/model.onnx` = 90,405,214 bytes.

- [ ] **Step 2: Write the failing registry tests** (in `model.rs`'s test module)

```rust
#[test]
fn registry_contains_three_models_with_distinct_tags() {
    let tags: Vec<&str> = ALL_MODELS.iter().map(|m| m.tag).collect();
    assert_eq!(tags, vec!["siglip2-b16-v1", "whisper-large-v3-turbo-q5-v1", "minilm-l6-v2-v1"]);
}

#[test]
fn model_dir_for_appends_tag_under_maj_model_dir() {
    // MAJ_MODEL_DIR handling is exercised through the public fn; this test
    // must not mutate the process env (tests run in parallel) — call the
    // path-composition helper directly.
    let base = std::path::Path::new("/models");
    assert_eq!(
        dir_under_base(base, &WHISPER),
        std::path::PathBuf::from("/models/whisper-large-v3-turbo-q5-v1")
    );
}

#[test]
fn spec_urls_pin_repo_and_revision() {
    let url = file_url(&WHISPER, &WHISPER.files[0]);
    assert!(url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/"));
    assert!(url.contains(WHISPER.revision));
    assert!(url.ends_with("ggml-large-v3-turbo-q5_0.bin"));
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p majestical-index model`
Expected: compile error — `ALL_MODELS`, `WHISPER`, `dir_under_base`, `file_url` not defined.

- [ ] **Step 4: Implement the registry** (restructure `model.rs`; keep existing consts as the siglip spec)

```rust
/// One fetchable model: tag doubles as cache-dir leaf and blob model tag.
pub struct ModelSpec {
    pub tag: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub files: &'static [ModelFile],
}

pub const SIGLIP: ModelSpec = ModelSpec {
    tag: MODEL_TAG, // existing "siglip2-b16-v1"
    repo: "onnx-community/siglip2-base-patch16-256-ONNX",
    revision: "d1114256522a37ffa257a0a58017348ab0058db2",
    files: &MODEL_FILES, // existing 3-file const
};

pub const WHISPER: ModelSpec = ModelSpec {
    tag: "whisper-large-v3-turbo-q5-v1",
    repo: "ggerganov/whisper.cpp",
    revision: "<sha from Step 1>",
    files: &[ModelFile {
        name: "ggml-large-v3-turbo-q5_0.bin",
        repo_path: "ggml-large-v3-turbo-q5_0.bin",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        bytes: 574_041_195,
    }],
};

pub const MINILM: ModelSpec = ModelSpec {
    tag: "minilm-l6-v2-v1",
    repo: "sentence-transformers/all-MiniLM-L6-v2",
    revision: "<sha from Step 1>",
    files: &[
        ModelFile {
            name: "model.onnx",
            repo_path: "onnx/model.onnx",
            sha256: "<sha256 from Step 1>",
            bytes: 90_405_214,
        },
        ModelFile {
            name: "tokenizer.json",
            repo_path: "tokenizer.json",
            sha256: "<sha256 from Step 1>",
            bytes: <bytes from Step 1>,
        },
    ],
};

pub const ALL_MODELS: [&ModelSpec; 3] = [&SIGLIP, &WHISPER, &MINILM];

fn base_dir() -> Result<PathBuf, IndexError> {
    // the existing model_dir() body minus the tag join
}

pub(crate) fn dir_under_base(base: &Path, spec: &ModelSpec) -> PathBuf {
    base.join(spec.tag)
}

/// Cache dir for one model spec (`MAJ_MODEL_DIR` override honored).
///
/// # Errors
/// Returns `IndexError::Model` when no data dir can be resolved.
pub fn model_dir_for(spec: &ModelSpec) -> Result<PathBuf, IndexError> {
    Ok(dir_under_base(&base_dir()?, spec))
}

pub(crate) fn file_url(spec: &ModelSpec, file: &ModelFile) -> String {
    format!("https://huggingface.co/{}/resolve/{}/{}", spec.repo, spec.revision, file.repo_path)
}

/// Fetch every file of `spec` (idempotent).
///
/// # Errors
/// Propagates download and digest-verification failures.
pub fn fetch_spec(
    spec: &ModelSpec,
    verify: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<(), IndexError> {
    let dir = model_dir_for(spec)?;
    for file in spec.files {
        let url = file_url(spec, file);
        let outcome = fetch_one(&FetchSpec {
            dir: &dir, name: file.name, url: &url,
            sha256: file.sha256, bytes: file.bytes, verify,
        })?;
        match outcome {
            FetchOutcome::AlreadyPresent => progress(&format!("{}/{} already present", spec.tag, file.name)),
            FetchOutcome::Downloaded => progress(&format!("{}/{} downloaded", spec.tag, file.name)),
        }
    }
    Ok(())
}
```

Keep `model_dir()`, `fetch()`, and `model_present()` working for the siglip spec (existing callers in `encoder.rs`, `index_cmd.rs`, `search.rs`, tests): reimplement `model_dir()` as `model_dir_for(&SIGLIP)` and `fetch(dir, verify, progress)` delegating to the shared internals — do not break the call sites in this task; later tasks migrate them where needed.

- [ ] **Step 5: Extend `maj model fetch` with `--only`**

`main.rs` `ModelCmd`:

```rust
enum ModelCmd {
    /// Fetch model weights (all models unless --only narrows it).
    Fetch {
        #[arg(long)]
        verify: bool,
        /// Fetch only the named model tags (repeatable).
        #[arg(long)]
        only: Vec<String>,
    },
}
```

`index_cmd.rs` `cmd_model_fetch(verify: bool, only: &[String]) -> Result<()>`:

```rust
pub(crate) fn cmd_model_fetch(verify: bool, only: &[String]) -> Result<()> {
    let known: Vec<&str> = model::ALL_MODELS.iter().map(|m| m.tag).collect();
    for tag in only {
        anyhow::ensure!(known.contains(&tag.as_str()), "unknown model tag {tag}; known: {}", known.join(", "));
    }
    for spec in model::ALL_MODELS {
        if !only.is_empty() && !only.iter().any(|t| t == spec.tag) {
            continue;
        }
        model::fetch_spec(spec, verify, &mut |line| println!("{line}"))?;
    }
    Ok(())
}
```

CLI test (add to `crates/cli/tests/index_smoke.rs`):

```rust
#[test]
fn model_fetch_only_rejects_unknown_tag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["model", "fetch", "--only", "nonsense-v9"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown model tag nonsense-v9"));
}
```

- [ ] **Step 6: justfile — pin the encoder job to siglip only**

In the `encoder-conformance` recipe, change the fetch line to
`... model fetch --only siglip2-b16-v1` so that job does not download whisper+minilm.

- [ ] **Step 7: Run to verify pass, gate, commit**

```bash
cargo test -p majestical-index model && cargo test -p majestical-cli --test index_smoke model_fetch_only_rejects_unknown_tag
just check
git add crates/index/src/model.rs crates/cli/src/index_cmd.rs crates/cli/src/main.rs crates/cli/tests/index_smoke.rs justfile
git commit -m "feat: multi-model registry + model fetch --only"
```

### Task 5: `text_encoder.rs` — MiniLM via ort

**Files:**
- Create: `crates/index/src/text_encoder.rs`
- Modify: `crates/index/src/lib.rs` (add `pub mod text_encoder;`)
- Create: `crates/index/tests/text_encoder_gated.rs` (needs fetched model, `--ignored`)

- [ ] **Step 1: Write the failing unit tests** (bottom of `text_encoder.rs` — pooling math is testable without the model)

```rust
#[cfg(test)]
mod tests {
    use super::mean_pool;

    #[test]
    fn mean_pool_respects_attention_mask() {
        // 3 tokens, dim 2; third token masked out.
        let hidden = [1.0_f32, 2.0, 3.0, 4.0, 100.0, 100.0];
        let mask = [1_u32, 1, 0];
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled, vec![2.0, 3.0]); // mean of (1,3) and (2,4)
    }

    #[test]
    fn mean_pool_all_masked_returns_zeros_not_nan() {
        let hidden = [1.0_f32, 2.0];
        let mask = [0_u32];
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled, vec![0.0, 0.0]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index text_encoder`
Expected: compile error.

- [ ] **Step 3: Implement `text_encoder.rs`**

```rust
//! In-process text embedding for transcript chunks and text queries:
//! all-MiniLM-L6-v2 (384-d) via ort on CPU. L2-normalized at the encoder
//! (same invariant as `encoder.rs`) so Lance `Dot` distance = cosine.

use std::path::Path;

use ort::session::{Session, builder::GraphOptimizationLevel};
use tokenizers::Tokenizer;

use crate::encoder::l2_normalize;
use crate::error::IndexError;

pub const TEXT_EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 256;

pub struct TextEncoder {
    session: Session,
    tokenizer: Tokenizer,
}

impl TextEncoder {
    /// Load MiniLM from `model_dir` (files `model.onnx`, `tokenizer.json`).
    ///
    /// # Errors
    /// Returns `IndexError::Model` when files are missing or unloadable.
    pub fn load(model_dir: &Path) -> Result<Self, IndexError> {
        let model_error = |message: String| IndexError::Model(message);
        let mut tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|error| model_error(format!("minilm tokenizer: {error}")))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|error| model_error(format!("minilm truncation: {error}")))?;
        let session = Session::builder()
            .map_err(|error| model_error(format!("minilm session: {error}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|error| model_error(format!("minilm session: {error}")))?
            .commit_from_file(model_dir.join("model.onnx"))
            .map_err(|error| model_error(format!("minilm model: {error}")))?;
        Ok(Self { session, tokenizer })
    }

    /// Embed one text into a unit-norm 384-d vector.
    ///
    /// # Errors
    /// Returns `IndexError::Encoder` on tokenizer or inference failure.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>, IndexError> {
        let encoder_error = |message: String| IndexError::Encoder(message);
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|error| encoder_error(format!("minilm encode: {error}")))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
        let mask_u32: Vec<u32> = encoding.get_attention_mask().to_vec();
        let mask_i64: Vec<i64> = mask_u32.iter().map(|&m| i64::from(m)).collect();
        let type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| i64::from(t)).collect();
        let seq = ids.len();
        let inputs = ort::inputs![
            "input_ids" => ort::value::Value::from_array(([1, seq], ids))
                .map_err(|error| encoder_error(error.to_string()))?,
            "attention_mask" => ort::value::Value::from_array(([1, seq], mask_i64))
                .map_err(|error| encoder_error(error.to_string()))?,
            "token_type_ids" => ort::value::Value::from_array(([1, seq], type_ids))
                .map_err(|error| encoder_error(error.to_string()))?,
        ];
        let outputs = self
            .session
            .run(inputs)
            .map_err(|error| encoder_error(format!("minilm run: {error}")))?;
        let (_, hidden) = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(|error| encoder_error(error.to_string()))?;
        let mut pooled = mean_pool(hidden, &mask_u32, TEXT_EMBED_DIM);
        l2_normalize(&mut pooled);
        Ok(pooled)
    }
}

/// Mean-pool `[seq, dim]` hidden states over the attention mask.
/// All-masked input yields zeros (callers treat that as "no signal").
fn mean_pool(hidden: &[f32], mask: &[u32], dim: usize) -> Vec<f32> {
    let mut pooled = vec![0.0_f32; dim];
    let mut count = 0.0_f32;
    for (token_index, &m) in mask.iter().enumerate() {
        if m == 0 {
            continue;
        }
        count += 1.0;
        let row = &hidden[token_index * dim..(token_index + 1) * dim];
        for (accumulator, value) in pooled.iter_mut().zip(row) {
            *accumulator += value;
        }
    }
    if count > 0.0 {
        for value in &mut pooled {
            *value /= count;
        }
    }
    pooled
}
```

Match `encoder.rs`'s actual ort call style exactly (session building, `ort::inputs!` macro shape, tensor extraction — the pinned `ort =2.0.0-rc.13` API); adjust the snippets above to whatever `encoder.rs:147-235` does, since it is the working reference for this exact ort version. `l2_normalize` may need `pub` in `encoder.rs` (it is already `pub` per the API map).

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p majestical-index text_encoder`
Expected: 2 passed.

- [ ] **Step 5: Write the gated e2e** (`crates/index/tests/text_encoder_gated.rs`, mirroring `encoder_gated.rs`'s `require_model_dir` idiom)

```rust
use majestical_index::model::{self, MINILM};
use majestical_index::text_encoder::{TEXT_EMBED_DIM, TextEncoder};

fn require_model_dir() -> std::path::PathBuf {
    let dir = model::model_dir_for(&MINILM).expect("model dir");
    assert!(dir.join("model.onnx").is_file(), "run `maj model fetch --only minilm-l6-v2-v1` first");
    dir
}

#[test]
#[ignore = "needs fetched minilm model"]
fn related_sentences_score_higher_than_unrelated() {
    let mut encoder = TextEncoder::load(&require_model_dir()).expect("load");
    let budget = encoder.embed("we discussed the quarterly budget and costs").expect("embed");
    let money = encoder.embed("talking about spending money").expect("embed");
    let cats = encoder.embed("a fluffy cat sleeping in the sun").expect("embed");
    assert_eq!(budget.len(), TEXT_EMBED_DIM);
    let related = majestical_index::encoder::cosine(&budget, &money);
    let unrelated = majestical_index::encoder::cosine(&budget, &cats);
    assert!(related > unrelated, "related {related} must beat unrelated {unrelated}");
}
```

- [ ] **Step 6: Gate and commit**

```bash
just check && cargo test -p majestical-index
git add crates/index/src/text_encoder.rs crates/index/src/lib.rs crates/index/tests/text_encoder_gated.rs
git commit -m "feat: MiniLM text encoder (384-d, mean-pooled, L2-normalized)"
```

### Task 6: text-encoder conformance gate (CI)

**Files:**
- Create: `conformance/text-encoder/golden.py`
- Create: `crates/index/tests/text_encoder_conformance.rs`
- Modify: `justfile`, `.github/workflows/ci.yml`

- [ ] **Step 1: Write the reference script** (`conformance/text-encoder/golden.py`, uv script style matching `conformance/encoder/golden.py` — read that file first and copy its argument/output conventions)

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["sentence-transformers==5.6.1"]
# ///
"""Golden embeddings from the pinned sentence-transformers reference."""
import argparse
import json

from sentence_transformers import SentenceTransformer

FIXTURES = [
    "a red barn at dusk",
    "we discussed the quarterly budget and costs",
    "TIMECODE 01:02:03 dropped frame",
    "ümläuts and 日本語 mixed with english",
    " ".join(["repetition"] * 300),  # long input: exercises truncation
    "",  # empty string: exercises the all-special-tokens path
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model = SentenceTransformer(
        "sentence-transformers/all-MiniLM-L6-v2", revision=args.revision
    )
    vectors = model.encode(FIXTURES, normalize_embeddings=True).tolist()
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump({"fixtures": FIXTURES, "vectors": vectors}, handle)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Write the conformance test** (`crates/index/tests/text_encoder_conformance.rs`)

```rust
use majestical_index::encoder::cosine;
use majestical_index::model::{self, MINILM};
use majestical_index::text_encoder::TextEncoder;

const COSINE_FLOOR: f32 = 0.999;

#[test]
#[ignore = "needs fetched minilm model and MAJ_GOLDEN from golden.py"]
fn rust_encoder_matches_sentence_transformers_reference() {
    let golden_path = std::env::var("MAJ_GOLDEN").expect("MAJ_GOLDEN env var");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("read golden"))
            .expect("parse golden");
    let fixtures = golden["fixtures"].as_array().expect("fixtures");
    let vectors = golden["vectors"].as_array().expect("vectors");
    let dir = model::model_dir_for(&MINILM).expect("model dir");
    let mut encoder = TextEncoder::load(&dir).expect("load");
    for (fixture, reference) in fixtures.iter().zip(vectors) {
        let text = fixture.as_str().expect("fixture text");
        let reference: Vec<f32> = reference
            .as_array()
            .expect("vector")
            .iter()
            .map(|v| v.as_f64().expect("f64") as f32)
            .collect();
        let ours = encoder.embed(text).expect("embed");
        let score = cosine(&ours, &reference);
        assert!(
            score >= COSINE_FLOOR,
            "cosine {score} < {COSINE_FLOOR} for fixture {text:?}"
        );
    }
}
```

(`as f32` truncation is fine here; if clippy `cast_possible_truncation` fires under pedantic, use `f32::from` where possible or a justified inline allow — but `allow_attributes` is denied, so prefer `#[expect(clippy::cast_possible_truncation, reason = "golden vectors are f32 precision")]`.)

- [ ] **Step 3: justfile recipe + CI job**

justfile (note: pin the revision var to the sha resolved in Task 4 Step 1):

```make
MINILM_TORCH_REVISION := "<sha from Task 4>"

# Text-encoder conformance: pinned sentence-transformers reference vs our
# ort MiniLM. Downloads ~90MB of model weights on first run.
text-encoder-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only minilm-l6-v2-v1
    uv run conformance/text-encoder/golden.py \
        --revision {{MINILM_TORCH_REVISION}} --out target/text-encoder-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/text-encoder-golden.json" \
        cargo test -p majestical-index --test text_encoder_conformance --test text_encoder_gated -- --ignored
```

ci.yml: add a `text-encoder-conformance` job copied from the `encoder-conformance` job (same SHA-pinned actions, protoc install, uv setup) with cache steps:

```yaml
      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4.3.0
        with:
          path: .model-cache
          key: model-minilm-l6-v2-v1
      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4.3.0
        with:
          path: ~/.cache/huggingface
          key: hf-reference-st-${{ hashFiles('justfile') }}
      - run: brew install just
      - run: just text-encoder-conformance
```

Give this job its own `hf-reference-st-` prefix, distinct from `encoder-conformance`'s
`hf-reference-`: both jobs write to `~/.cache/huggingface` but populate different
contents (different oracle downloads), GH Actions caches are immutable per key, and
the two jobs run in parallel — an identical key means whichever job's post-job save
runs second always fails (key already exists) and that job re-downloads its oracle on
every run. Each job keeps its own key, both still keyed on `hashFiles('justfile')` so
either job's cache invalidates when its pinned revision in the justfile changes.

Run `actionlint .github/workflows/` and `uvx zizmor .github/workflows/` locally before committing.

- [ ] **Step 4: Run the gate locally, commit, open PR 2**

```bash
just text-encoder-conformance   # slow on first run (~90MB + torch reference)
just check
git add conformance/text-encoder crates/index/tests/text_encoder_conformance.rs justfile .github/workflows/ci.yml
git commit -m "test: text-encoder conformance gate vs sentence-transformers"
```

Open PR "feat: model registry + MiniLM text encoder + conformance" (Tasks 4-6).

---

# PR 3 — Transcription (whisper.cpp) + conformance

### Task 7: ffmpeg audio extraction with timeout

**Files:**
- Modify: `crates/index/src/video.rs` (append `run_with_timeout` + `extract_audio_pcm`)
- Modify: `crates/index/src/error.rs` (add `Audio { path, message }` variant if `Video` doesn't fit; reuse `Video` if it reads fine — prefer reuse)

- [ ] **Step 1: Write the failing tests** (in `video.rs`'s test module; timeout helper is testable with plain shell commands)

```rust
#[test]
fn run_with_timeout_kills_a_hung_process() {
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    let started = std::time::Instant::now();
    let result = run_with_timeout(command, std::time::Duration::from_millis(300));
    assert!(result.is_err(), "hung process must error");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "must not wait for sleep 30");
}

#[test]
fn run_with_timeout_returns_output_of_fast_process() {
    let mut command = std::process::Command::new("echo");
    command.arg("ok");
    let output = run_with_timeout(command, std::time::Duration::from_secs(5)).expect("fast");
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));
}

#[test]
fn audio_timeout_scales_with_duration() {
    assert_eq!(audio_timeout(0), std::time::Duration::from_secs(60));
    assert_eq!(audio_timeout(3_600_000), std::time::Duration::from_secs(60 + 3_600));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index video::tests::run_with_timeout`
Expected: compile error.

- [ ] **Step 3: Implement** (append to `video.rs`)

```rust
/// Run a command with a hard timeout, killing the child on expiry.
/// std-only: polls `try_wait` at 100 ms. Stdout is buffered via piped
/// output on a reader thread to avoid pipe-full deadlock.
pub(crate) fn run_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::io::Read as _;
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|error| format!("spawn: {error}"))?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut stdout);
        }
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        (stdout, stderr)
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|error| format!("wait: {error}"))? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    let (stdout, stderr) = reader.join().map_err(|_| "reader thread panicked".to_string())?;
    Ok(std::process::Output { status, stdout, stderr })
}

/// Timeout for audio extraction: a fixed floor plus 1 s per source second.
pub(crate) fn audio_timeout(duration_ms: u64) -> std::time::Duration {
    std::time::Duration::from_secs(60 + duration_ms / 1000)
}

/// Extract mono 16 kHz f32 PCM (whisper's native input) from any av file.
///
/// # Errors
/// Returns `IndexError::Video` on ffmpeg failure or timeout.
pub fn extract_audio_pcm(path: &Path, duration_ms: u64) -> Result<Vec<f32>, IndexError> {
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-ar", "16000", "-ac", "1", "-f", "f32le", "-"]);
    let output = run_with_timeout(command, audio_timeout(duration_ms)).map_err(|message| {
        IndexError::Video { path: path.to_path_buf(), message: format!("audio extract: {message}") }
    })?;
    if !output.status.success() {
        let stderr_tail: String = String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("")
            .to_string();
        return Err(IndexError::Video {
            path: path.to_path_buf(),
            message: format!("ffmpeg audio extract failed: {stderr_tail}"),
        });
    }
    let mut pcm = Vec::with_capacity(output.stdout.len() / 4);
    for chunk in output.stdout.chunks_exact(4) {
        pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(pcm)
}
```

Match the exact `IndexError::Video` field names from `error.rs:4`.

- [ ] **Step 4: Run, gate, commit**

```bash
cargo test -p majestical-index video && just check
git add crates/index/src/video.rs
git commit -m "feat: ffmpeg audio extraction with duration-scaled timeout"
```

### Task 8: `transcribe.rs` — whisper-rs

**Files:**
- Modify: `Cargo.toml` (`whisper-rs = { version = "0.16.0", features = ["metal"] }` in workspace deps — verify version)
- Modify: `crates/index/Cargo.toml` (add `whisper-rs.workspace = true`)
- Create: `crates/index/src/transcribe.rs`
- Modify: `crates/index/src/lib.rs`
- Modify: `crates/index/src/blob.rs` (new `Derivation::Transcript` variant)
- Create: `crates/index/tests/whisper_gated.rs`

- [ ] **Step 1: Write the failing blob-path test** (in `blob.rs` tests)

```rust
#[test]
fn transcript_blob_path_is_model_tagged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let path = store.path_for("aabbccdd", &Derivation::Transcript { model_tag: "whisper-large-v3-turbo-q5-v1" });
    assert!(path.ends_with("aa/aabbccdd/whisper-large-v3-turbo-q5-v1/transcript.json.zst"));
}
```

- [ ] **Step 2: Run to verify failure, then extend `Derivation`**

Run: `cargo test -p majestical-index blob` — expected: compile error (`Transcript` variant missing).

`blob.rs` `Derivation` gains:

```rust
    Transcript { model_tag: &'a str },
```

and `path_for` maps it to `<model_tag>/transcript.json.zst`. `classify_vector_file` is untouched (transcripts are not vectors).

Re-run: `cargo test -p majestical-index blob` — expected: pass.

- [ ] **Step 3: Write the failing transcript-serialization tests** (bottom of `transcribe.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_json_round_trips() {
        let transcript = Transcript {
            model_tag: WHISPER_MODEL_TAG.to_string(),
            segments: vec![TranscriptSegment { start_ms: 0, end_ms: 2_500, text: " Hello world.".into() }],
            text: "Hello world.".into(),
        };
        let bytes = transcript.to_json().expect("serialize");
        let back = Transcript::from_json(&bytes).expect("parse");
        assert_eq!(back.segments.len(), 1);
        assert_eq!(back.segments[0].end_ms, 2_500);
    }

    #[test]
    fn centiseconds_convert_to_ms() {
        assert_eq!(centis_to_ms(250), 2_500);
        assert_eq!(centis_to_ms(0), 0);
    }

    #[test]
    fn full_text_joins_trimmed_segments() {
        let segments = vec![
            TranscriptSegment { start_ms: 0, end_ms: 1, text: " Hello".into() },
            TranscriptSegment { start_ms: 1, end_ms: 2, text: " world.".into() },
        ];
        assert_eq!(full_text(&segments), "Hello world.");
    }
}
```

- [ ] **Step 4: Run to verify failure, then implement `transcribe.rs`**

```rust
//! In-process transcription: whisper.cpp via whisper-rs (Metal).
//! Timestamps come back in centiseconds from whisper-rs 0.16 — converted
//! to ms at the boundary and never exposed otherwise.

use std::path::Path;

use crate::error::IndexError;

pub const WHISPER_MODEL_TAG: &str = "whisper-large-v3-turbo-q5-v1";
pub const MODEL_FILE: &str = "ggml-large-v3-turbo-q5_0.bin";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    pub model_tag: String,
    pub segments: Vec<TranscriptSegment>,
    pub text: String,
}

impl Transcript {
    /// # Errors
    /// Serialization failure (never expected for these plain types).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self).map_err(|error| IndexError::Model(format!("transcript json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes).map_err(|error| IndexError::Model(format!("transcript parse: {error}")))
    }
}

pub(crate) fn centis_to_ms(centis: i64) -> u64 {
    u64::try_from(centis.max(0)).unwrap_or(0) * 10
}

pub(crate) fn full_text(segments: &[TranscriptSegment]) -> String {
    segments.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ").trim().to_string()
}

pub struct Transcriber {
    context: whisper_rs::WhisperContext,
}

impl Transcriber {
    /// Load the ggml model from `model_dir`.
    ///
    /// # Errors
    /// Returns `IndexError::Model` when the file is missing or unloadable.
    pub fn load(model_dir: &Path) -> Result<Self, IndexError> {
        let path = model_dir.join(MODEL_FILE);
        let path_str = path
            .to_str()
            .ok_or_else(|| IndexError::Model(format!("non-utf8 model path {}", path.display())))?;
        let context = whisper_rs::WhisperContext::new_with_params(
            path_str,
            whisper_rs::WhisperContextParameters::default(),
        )
        .map_err(|error| IndexError::Model(format!("whisper load: {error}")))?;
        Ok(Self { context })
    }

    /// Transcribe mono 16 kHz f32 PCM with auto language detection.
    ///
    /// # Errors
    /// Returns `IndexError::Encoder` on inference failure.
    pub fn transcribe(&self, pcm: &[f32]) -> Result<Transcript, IndexError> {
        let mut state = self
            .context
            .create_state()
            .map_err(|error| IndexError::Encoder(format!("whisper state: {error}")))?;
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(None); // auto-detect
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        state
            .full(params, pcm)
            .map_err(|error| IndexError::Encoder(format!("whisper full: {error}")))?;
        let mut segments = Vec::new();
        let count = state.full_n_segments();
        for index in 0..count {
            let Some(segment) = state.get_segment(index) else { continue };
            let text = segment.to_str_lossy().to_string();
            segments.push(TranscriptSegment {
                start_ms: centis_to_ms(segment.start_timestamp()),
                end_ms: centis_to_ms(segment.end_timestamp()),
                text,
            });
        }
        let text = full_text(&segments);
        Ok(Transcript { model_tag: WHISPER_MODEL_TAG.to_string(), segments, text })
    }
}
```

Check whisper-rs 0.16's exact method names against docs.rs while implementing (`full_n_segments` return type, `get_segment`, `to_str_lossy`, `start_timestamp`) — the segment-object API replaced the old `full_get_segment_*` calls in this version.

- [ ] **Step 5: Run unit tests, then write the gated e2e** (`crates/index/tests/whisper_gated.rs`)

```rust
use majestical_index::model::{self, WHISPER};
use majestical_index::transcribe::Transcriber;
use majestical_index::video;

#[test]
#[ignore = "needs fetched whisper model and ffmpeg + say on PATH"]
fn transcribes_spoken_fixture_with_sane_timestamps() {
    let dir = model::model_dir_for(&WHISPER).expect("dir");
    assert!(dir.join(majestical_index::transcribe::MODEL_FILE).is_file(),
        "run `maj model fetch --only whisper-large-v3-turbo-q5-v1` first");
    let tmp = tempfile::tempdir().expect("tempdir");
    let aiff = tmp.path().join("fixture.aiff");
    let status = std::process::Command::new("say")
        .args(["-o"]).arg(&aiff)
        .arg("The quick brown fox jumps over the lazy dog")
        .status().expect("say");
    assert!(status.success());
    let pcm = video::extract_audio_pcm(&aiff, 10_000).expect("pcm");
    let transcriber = Transcriber::load(&dir).expect("load");
    let transcript = transcriber.transcribe(&pcm).expect("transcribe");
    let lower = transcript.text.to_lowercase();
    assert!(lower.contains("quick brown fox"), "got: {lower}");
    assert!(!transcript.segments.is_empty());
    assert!(transcript.segments[0].end_ms > transcript.segments[0].start_ms);
}
```

- [ ] **Step 6: Gate and commit**

```bash
cargo test -p majestical-index && just check
cargo test -p majestical-index --test whisper_gated -- --ignored   # after a one-time model fetch
git add Cargo.toml Cargo.lock crates/index/Cargo.toml crates/index/src/transcribe.rs crates/index/src/lib.rs crates/index/src/blob.rs crates/index/tests/whisper_gated.rs
git commit -m "feat: whisper.cpp transcription with ms timestamps"
```

### Task 9: whisper-conformance gate (CI)

**Files:**
- Create: `conformance/whisper/golden.py`
- Create: `conformance/whisper/wer.rs` — no; WER lives in the test: `crates/index/tests/whisper_conformance.rs`
- Modify: `justfile`, `.github/workflows/ci.yml`

- [ ] **Step 1: Reference script** (`conformance/whisper/golden.py`)

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["faster-whisper==1.2.1"]
# ///
"""Reference transcription of a fixture WAV via pinned faster-whisper."""
import argparse
import json

from faster_whisper import WhisperModel


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--audio", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model = WhisperModel("large-v3-turbo", device="cpu", compute_type="int8")
    segments, _info = model.transcribe(args.audio)
    rows = [
        {"start_ms": int(s.start * 1000), "end_ms": int(s.end * 1000), "text": s.text}
        for s in segments
    ]
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump({"segments": rows}, handle)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Conformance test** (`crates/index/tests/whisper_conformance.rs`) — WER against the reference text, plus first/last boundary drift. Fixture audio comes from `MAJ_AUDIO` (generated in the justfile recipe with `say` + ffmpeg so the exact same file feeds both sides).

```rust
use majestical_index::model::{self, WHISPER};
use majestical_index::transcribe::Transcriber;
use majestical_index::video;

const MAX_WER: f64 = 0.15;
const MAX_BOUNDARY_DRIFT_MS: i64 = 1_500;

fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn word_error_rate(reference: &[String], hypothesis: &[String]) -> f64 {
    // Levenshtein distance over words.
    let rows = reference.len() + 1;
    let cols = hypothesis.len() + 1;
    let mut distance = vec![0_usize; rows * cols];
    for r in 0..rows {
        distance[r * cols] = r;
    }
    for c in 0..cols {
        distance[c] = c;
    }
    for r in 1..rows {
        for c in 1..cols {
            let substitution =
                distance[(r - 1) * cols + (c - 1)] + usize::from(reference[r - 1] != hypothesis[c - 1]);
            let deletion = distance[(r - 1) * cols + c] + 1;
            let insertion = distance[r * cols + (c - 1)] + 1;
            distance[r * cols + c] = substitution.min(deletion).min(insertion);
        }
    }
    let denominator = reference.len().max(1);
    distance[rows * cols - 1] as f64 / denominator as f64
}

#[test]
#[ignore = "needs fetched whisper model, MAJ_AUDIO and MAJ_GOLDEN from golden.py"]
fn whisper_rs_matches_faster_whisper_reference() {
    let audio = std::env::var("MAJ_AUDIO").expect("MAJ_AUDIO");
    let golden_path = std::env::var("MAJ_GOLDEN").expect("MAJ_GOLDEN");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("read")).expect("parse");
    let reference_text: String = golden["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|s| s["text"].as_str().expect("text"))
        .collect::<Vec<_>>()
        .join(" ");
    let dir = model::model_dir_for(&WHISPER).expect("dir");
    let pcm = video::extract_audio_pcm(std::path::Path::new(&audio), 120_000).expect("pcm");
    let transcript = Transcriber::load(&dir).expect("load").transcribe(&pcm).expect("transcribe");

    let reference = normalize(&reference_text);
    let hypothesis = normalize(&transcript.text);
    let wer = word_error_rate(&reference, &hypothesis);
    assert!(wer <= MAX_WER, "WER {wer:.3} exceeds {MAX_WER} — ref: {reference_text:?} got: {:?}", transcript.text);

    let reference_first = golden["segments"][0]["start_ms"].as_i64().expect("start");
    let ours_first = i64::try_from(transcript.segments[0].start_ms).expect("fits");
    assert!((reference_first - ours_first).abs() <= MAX_BOUNDARY_DRIFT_MS,
        "first-segment drift {reference_first} vs {ours_first}");
}
```

(Add `#[expect(clippy::cast_precision_loss, reason = "WER over small word counts")]` on `word_error_rate` if pedantic fires.)

- [ ] **Step 3: justfile recipe + CI job**

```make
# Whisper conformance: same synthesized speech through pinned faster-whisper
# (reference) and our whisper-rs, compared on WER + boundary drift.
whisper-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only whisper-large-v3-turbo-q5-v1
    say -o target/whisper-fixture.aiff "The quick brown fox jumps over the lazy dog. \
        We reviewed the quarterly budget on Tuesday and shipped the release candidate."
    ffmpeg -y -i target/whisper-fixture.aiff -ar 16000 -ac 1 target/whisper-fixture.wav
    uv run conformance/whisper/golden.py \
        --audio target/whisper-fixture.wav --out target/whisper-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_AUDIO="{{justfile_directory()}}/target/whisper-fixture.wav" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/whisper-golden.json" \
        cargo test -p majestical-index --test whisper_conformance --test whisper_gated -- --ignored
```

ci.yml: add `whisper-conformance` job cloned from `encoder-conformance` (same pinned actions), with `brew install ffmpeg` added and cache keys:

```yaml
      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4.3.0
        with:
          path: .model-cache
          key: model-whisper-large-v3-turbo-q5-v1
      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4.3.0
        with:
          path: ~/.cache/huggingface
          key: hf-whisper-reference-${{ hashFiles('justfile') }}
```

Lint workflows (`actionlint`, `uvx zizmor`) before committing.

- [ ] **Step 4: Run the gate, commit, open PR 3**

```bash
just whisper-conformance   # slow first run: 574MB model + CT2 reference download
just check
git add conformance/whisper crates/index/tests/whisper_conformance.rs justfile .github/workflows/ci.yml
git commit -m "test: whisper conformance gate vs faster-whisper"
```

Open PR "feat: whisper.cpp transcription + conformance" (Tasks 7-9).

# PR 4 — Chunking + `text_chunks` Lance table + `text_fts`

### Task 10: `chunk.rs` — transcript chunking (property-tested)

**Files:**
- Create: `crates/index/src/chunk.rs`
- Modify: `crates/index/src/lib.rs`
- Modify: `crates/index/src/blob.rs` (add `Derivation::TranscriptChunk`)
- Modify: `crates/index/Cargo.toml` (add `[dev-dependencies] proptest.workspace = true` if absent)

- [ ] **Step 1: Write the failing tests** (bottom of `chunk.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::TranscriptSegment;
    use proptest::prelude::*;

    fn segment(start_ms: u64, end_ms: u64, words: usize) -> TranscriptSegment {
        TranscriptSegment { start_ms, end_ms, text: vec!["word"; words].join(" ") }
    }

    #[test]
    fn short_transcript_is_one_chunk() {
        let segments = vec![segment(0, 10_000, 20), segment(10_000, 20_000, 20)];
        let chunks = chunk_segments(&segments);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_ms, 0);
        assert_eq!(chunks[0].end_ms, 20_000);
        assert_eq!(chunks[0].text.split_whitespace().count(), 40);
    }

    #[test]
    fn duration_cap_splits() {
        let segments = vec![segment(0, 40_000, 10), segment(40_000, 80_000, 10)];
        let chunks = chunk_segments(&segments);
        assert_eq!(chunks.len(), 2, "40s + 40s cannot merge under the 45s cap");
    }

    #[test]
    fn word_cap_splits() {
        let segments = vec![segment(0, 1_000, 100), segment(1_000, 2_000, 100)];
        let chunks = chunk_segments(&segments);
        assert_eq!(chunks.len(), 2, "100 + 100 words cannot merge under the 120-word cap");
    }

    #[test]
    fn one_oversized_segment_is_still_one_chunk() {
        // A single segment over both caps must never be split.
        let segments = vec![segment(0, 90_000, 300)];
        assert_eq!(chunk_segments(&segments).len(), 1);
    }

    #[test]
    fn empty_transcript_yields_no_chunks() {
        assert!(chunk_segments(&[]).is_empty());
    }

    proptest! {
        #[test]
        fn chunks_cover_every_segment_exactly_once_in_order(
            durations in proptest::collection::vec(1_u64..60_000, 0..40),
            words in proptest::collection::vec(1_usize..150, 0..40),
        ) {
            let count = durations.len().min(words.len());
            let mut segments = Vec::new();
            let mut clock = 0_u64;
            for i in 0..count {
                segments.push(segment(clock, clock + durations[i], words[i]));
                clock += durations[i];
            }
            let chunks = chunk_segments(&segments);
            // Coverage: total words in == total words out, order preserved.
            let words_in: usize = segments.iter().map(|s| s.text.split_whitespace().count()).sum();
            let words_out: usize = chunks.iter().map(|c| c.text.split_whitespace().count()).sum();
            prop_assert_eq!(words_in, words_out);
            // Boundaries: chunk ranges are contiguous with segment boundaries,
            // monotonically increasing, and never overlap.
            for window in chunks.windows(2) {
                prop_assert!(window[0].end_ms <= window[1].start_ms);
            }
            // Caps: any chunk holding >1 segment respects both caps.
            for chunk in &chunks {
                let chunk_words = chunk.text.split_whitespace().count();
                let single_segment = segments.iter().any(|s|
                    s.start_ms == chunk.start_ms && s.end_ms == chunk.end_ms);
                if !single_segment {
                    prop_assert!(chunk.end_ms - chunk.start_ms <= MAX_CHUNK_MS);
                    prop_assert!(chunk_words <= MAX_CHUNK_WORDS);
                }
            }
            if !segments.is_empty() {
                prop_assert_eq!(chunks[0].start_ms, segments[0].start_ms);
                prop_assert_eq!(chunks.last().unwrap().end_ms, segments.last().unwrap().end_ms);
            }
        }
    }
}
```

(The `unwrap` in proptest closures: test code — the clippy test exemptions already key on `#[cfg(test)]` per `crates/catalog-sqlite/src/apply.rs:750`'s comment; follow whatever `clippy.toml` does in this repo.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index chunk`
Expected: compile error.

- [ ] **Step 3: Implement `chunk.rs`**

```rust
//! Greedy transcript chunking for text embedding: windows of at most
//! `MAX_CHUNK_MS` and `MAX_CHUNK_WORDS`, never splitting a whisper segment
//! (an oversized single segment becomes one oversized chunk).

use crate::transcribe::TranscriptSegment;

pub const MAX_CHUNK_MS: u64 = 45_000;
pub const MAX_CHUNK_WORDS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[must_use]
pub fn chunk_segments(segments: &[TranscriptSegment]) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current: Option<(Chunk, usize)> = None;
    for segment in segments {
        let words = segment.text.split_whitespace().count();
        match current.take() {
            None => {
                current = Some((
                    Chunk {
                        start_ms: segment.start_ms,
                        end_ms: segment.end_ms,
                        text: segment.text.trim().to_string(),
                    },
                    words,
                ));
            }
            Some((mut chunk, chunk_words)) => {
                let merged_ms = segment.end_ms.saturating_sub(chunk.start_ms);
                let merged_words = chunk_words + words;
                if merged_ms <= MAX_CHUNK_MS && merged_words <= MAX_CHUNK_WORDS {
                    chunk.end_ms = segment.end_ms;
                    chunk.text.push(' ');
                    chunk.text.push_str(segment.text.trim());
                    current = Some((chunk, merged_words));
                } else {
                    chunks.push(chunk);
                    current = Some((
                        Chunk {
                            start_ms: segment.start_ms,
                            end_ms: segment.end_ms,
                            text: segment.text.trim().to_string(),
                        },
                        words,
                    ));
                }
            }
        }
    }
    if let Some((chunk, _)) = current {
        chunks.push(chunk);
    }
    chunks
}
```

`blob.rs` `Derivation` gains `TranscriptChunk { model_tag: &'a str, start_ms: u64 }` → `<model_tag>/chunk-<start_ms>.f32le.zst`, and `classify_vector_file` learns `chunk-<ms>.f32le.zst` → `("chunk", ms)` so `iter_vectors("minilm-l6-v2-v1")` finds them. Add a blob test:

```rust
#[test]
fn chunk_vector_files_classify_with_timestamp() {
    assert_eq!(classify_vector_file("chunk-45000.f32le.zst"), Some(("chunk".to_string(), 45_000)));
    assert_eq!(classify_vector_file("chunk-x.f32le.zst"), None);
}
```

(Adapt to `classify_vector_file`'s actual private signature at `blob.rs:56` — test through `iter_vectors` if it isn't directly testable.)

- [ ] **Step 4: Run, gate, commit**

```bash
cargo test -p majestical-index chunk blob && just check
git add crates/index/src/chunk.rs crates/index/src/lib.rs crates/index/src/blob.rs crates/index/Cargo.toml
git commit -m "feat: transcript chunking with property-tested invariants"
```

### Task 11: `TextVectorStore` — 384-d Lance table with chunk text

**Files:**
- Modify: `crates/index/src/vector_store.rs` (append; the 768-d `VectorStore` is untouched)

- [ ] **Step 1: Write the failing tests** (in `vector_store.rs`'s test module)

```rust
#[test]
fn text_store_add_search_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TextVectorStore::open(dir.path()).expect("open");
    let mut a = vec![0.0_f32; TEXT_DIM];
    a[0] = 1.0;
    let mut b = vec![0.0_f32; TEXT_DIM];
    b[1] = 1.0;
    store
        .add(vec![
            TextChunkRow { asset_hex: "aa11".into(), source: "transcript".into(),
                start_ms: 0, end_ms: 45_000, model_tag: "minilm-l6-v2-v1".into(),
                text: "budget discussion".into(), vector: a.clone() },
            TextChunkRow { asset_hex: "bb22".into(), source: "transcript".into(),
                start_ms: 0, end_ms: 30_000, model_tag: "minilm-l6-v2-v1".into(),
                text: "cat video".into(), vector: b },
        ])
        .expect("add");
    let hits = store.search(&a, "minilm-l6-v2-v1", 10).expect("search");
    assert_eq!(hits[0].asset_hex, "aa11");
    assert_eq!(hits[0].text, "budget discussion");
    assert_eq!(hits[0].start_ms, 0);
}

#[test]
fn text_store_rejects_wrong_dim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TextVectorStore::open(dir.path()).expect("open");
    let row = TextChunkRow { asset_hex: "aa".into(), source: "transcript".into(),
        start_ms: 0, end_ms: 1, model_tag: "m".into(), text: "t".into(),
        vector: vec![0.0; 3] };
    assert!(store.add(vec![row]).is_err());
}

#[test]
fn text_store_existing_keys_and_distinct_assets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TextVectorStore::open(dir.path()).expect("open");
    store.add(vec![TextChunkRow { asset_hex: "aa11".into(), source: "transcript".into(),
        start_ms: 5, end_ms: 6, model_tag: "m1".into(), text: "t".into(),
        vector: vec![0.0; TEXT_DIM] }]).expect("add");
    let keys = store.existing_keys("m1").expect("keys");
    assert!(keys.contains(&("aa11".to_string(), 5)));
    assert!(store.existing_keys("other").expect("keys").is_empty());
    assert_eq!(store.distinct_assets("m1").expect("assets").len(), 1);
}

#[test]
fn text_store_coexists_with_image_store_in_same_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let image_store = VectorStore::open(dir.path()).expect("image open");
    let text_store = TextVectorStore::open(dir.path()).expect("text open");
    drop(image_store);
    drop(text_store);
    let reopened = TextVectorStore::open_existing(dir.path()).expect("reopen");
    assert!(reopened.is_some());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index text_store`
Expected: compile error.

- [ ] **Step 3: Implement** (append to `vector_store.rs`, cloning the `VectorStore` structure — same private tokio runtime, `connect_local`, batch/scan helpers; new consts and schema)

```rust
pub const TEXT_DIM: usize = 384;
const TEXT_TABLE_NAME: &str = "text_chunks";

#[derive(Debug, Clone, PartialEq)]
pub struct TextChunkRow {
    pub asset_hex: String,
    pub source: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub model_tag: String,
    pub text: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextChunkHit {
    pub asset_hex: String,
    pub source: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub distance: f32,
}

pub struct TextVectorStore { rt: tokio::runtime::Runtime, table: Table }

impl TextVectorStore {
    /// Open (creating an empty table if absent) in the same lance dir as
    /// the image store — a different table name, so they coexist.
    ///
    /// # Errors
    /// Returns `IndexError::VectorStore` on connection/schema failure.
    pub fn open(dir: &Path) -> Result<Self, IndexError> { /* mirror VectorStore::open with text_schema() */ }

    /// # Errors
    /// As `open`; `Ok(None)` when the dir or table does not exist.
    pub fn open_existing(dir: &Path) -> Result<Option<Self>, IndexError> { /* mirror */ }

    /// # Errors
    /// Rejects any row whose vector length differs from `TEXT_DIM`.
    pub fn add(&self, rows: Vec<TextChunkRow>) -> Result<(), IndexError> { /* mirror, validate TEXT_DIM */ }

    /// Nearest chunks by Dot distance (== cosine on unit vectors), filtered
    /// to `model_tag`, selecting all metadata columns including `text`.
    ///
    /// # Errors
    /// Returns `IndexError::VectorStore` on query failure.
    pub fn search(&self, vector: &[f32], model_tag: &str, limit: usize) -> Result<Vec<TextChunkHit>, IndexError> { /* mirror */ }

    /// (asset_hex, start_ms) pairs present for `model_tag` — the diff key
    /// for blob↔Lance healing.
    ///
    /// # Errors
    /// Returns `IndexError::VectorStore` on scan failure.
    pub fn existing_keys(&self, model_tag: &str) -> Result<BTreeSet<(String, i64)>, IndexError> { /* mirror */ }

    /// # Errors
    /// Returns `IndexError::VectorStore` on scan failure.
    pub fn distinct_assets(&self, model_tag: &str) -> Result<BTreeSet<String>, IndexError> { /* mirror */ }
}

fn text_schema() -> SchemaRef {
    std::sync::Arc::new(Schema::new(vec![
        Field::new("asset_hex", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("start_ms", DataType::Int64, false),
        Field::new("end_ms", DataType::Int64, false),
        Field::new("model_tag", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Float32, true)),
                TEXT_DIM as i32,
            ),
            true,
        ),
    ]))
}
```

"mirror" = copy the corresponding `VectorStore` method body (`vector_store.rs:70-260`) and swap table name, schema, row/hit structs, and dim const. This is deliberate duplication over premature abstraction: two stores, two shapes, and the 768-d store's `catch_corruption` docs stay specific to it. `catch_corruption` wraps this store's open+probe at the CLI call sites exactly as it does the image store's.

- [ ] **Step 4: Run, gate, commit**

```bash
cargo test -p majestical-index vector_store && just check
git add crates/index/src/vector_store.rs
git commit -m "feat: 384-d text_chunks Lance table with chunk text column"
```

### Task 12: `text_fts` in catalog-sqlite

**Files:**
- Modify: `crates/catalog-sqlite/src/lib.rs` (`SNAPSHOT_VERSION` 5 → 6)
- Modify: `crates/catalog-sqlite/src/schema.rs` (DROP + CREATE `text_fts`)
- Modify: `crates/catalog-sqlite/src/query.rs` (new methods + tests)
- Modify: `crates/catalog-sqlite/src/apply.rs` (`debug_dump` table list)

- [ ] **Step 1: Write the failing tests** (in `query.rs`'s test module, following its existing in-memory-catalog test setup — read the module's helpers first and reuse them)

```rust
#[test]
fn text_rows_upsert_and_search_ranked() {
    let mut db = test_catalog(); // the module's existing helper for an open catalog
    db.upsert_text_rows(
        &AssetId("xxh3:aa11".into()),
        "transcript",
        &[(0, "we discussed the quarterly budget"), (45_000, "then we talked about cats")],
    )
    .expect("upsert");
    db.upsert_text_rows(&AssetId("xxh3:bb22".into()), "caption", &[(-1, "a red barn at dusk")])
        .expect("upsert");
    let hits = db
        .search_text_ranked(&["budget".into()], None, 10)
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].asset.0, "xxh3:aa11");
    assert_eq!(hits[0].source, "transcript");
    assert_eq!(hits[0].locator, 0);
    assert!(hits[0].snippet.contains("budget"));
}

#[test]
fn text_search_filters_by_source() {
    let mut db = test_catalog();
    db.upsert_text_rows(&AssetId("xxh3:aa".into()), "transcript", &[(0, "barn")]).expect("upsert");
    db.upsert_text_rows(&AssetId("xxh3:bb".into()), "caption", &[(-1, "barn")]).expect("upsert");
    let sources = std::collections::BTreeSet::from(["caption".to_string()]);
    let hits = db.search_text_ranked(&["barn".into()], Some(&sources), 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].asset.0, "xxh3:bb");
}

#[test]
fn upsert_replaces_rows_for_same_asset_and_source() {
    let mut db = test_catalog();
    let asset = AssetId("xxh3:aa".into());
    db.upsert_text_rows(&asset, "transcript", &[(0, "old words")]).expect("upsert");
    db.upsert_text_rows(&asset, "transcript", &[(0, "new words")]).expect("upsert");
    assert!(db.search_text_ranked(&["old".into()], None, 10).expect("s").is_empty());
    assert_eq!(db.search_text_ranked(&["new".into()], None, 10).expect("s").len(), 1);
}

#[test]
fn text_assets_reports_per_source_coverage() {
    let mut db = test_catalog();
    db.upsert_text_rows(&AssetId("xxh3:aa".into()), "transcript", &[(0, "x")]).expect("upsert");
    let covered = db.text_assets("transcript").expect("covered");
    assert!(covered.contains(&AssetId("xxh3:aa".into())));
    assert!(db.text_assets("ocr").expect("covered").is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-catalog-sqlite text`
Expected: compile error.

- [ ] **Step 3: Implement**

`schema.rs` `create_tables` batch gains (alongside the existing `names_fts` block):

```sql
DROP TABLE IF EXISTS text_fts;
CREATE VIRTUAL TABLE text_fts USING fts5(
    content, asset UNINDEXED, source UNINDEXED, locator UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
```

`lib.rs`: `SNAPSHOT_VERSION` 5 → 6 (forces one full rebuild on first open — the established migration mechanism).

`query.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct TextHit {
    pub asset: AssetId,
    pub score: f64,
    pub source: String,
    /// ms timestamp for transcript/OCR rows, page number for PDF rows,
    /// -1 when no locator applies (captions, still-image OCR).
    pub locator: i64,
    pub snippet: String,
}

impl SqliteCatalog {
    /// Replace all `source` rows for `asset` with `rows` = (locator, content).
    ///
    /// # Errors
    /// Returns `CatalogError::Sqlite` on statement failure.
    pub fn upsert_text_rows(
        &mut self,
        asset: &AssetId,
        source: &str,
        rows: &[(i64, &str)],
    ) -> Result<(), CatalogError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM text_fts WHERE asset = ?1 AND source = ?2", (&asset.0, source))?;
        for (locator, content) in rows {
            tx.execute(
                "INSERT INTO text_fts (content, asset, source, locator) VALUES (?1, ?2, ?3, ?4)",
                (content, &asset.0, source, locator),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ranked FTS over text sources; best row per asset wins. `sources`
    /// restricts which text sources participate (None = all).
    ///
    /// # Errors
    /// Returns `CatalogError::Sqlite` on query failure.
    pub fn search_text_ranked(
        &self,
        terms: &[String],
        sources: Option<&BTreeSet<String>>,
        limit: usize,
    ) -> Result<Vec<TextHit>, CatalogError> {
        // Reuse the match-expression builder used by search_names_ranked
        // (query.rs:30-34): quoted prefix terms, embedded quotes doubled.
        let match_expr = fts_match_expr(terms); // extract the existing builder into this shared helper
        let mut sql = String::from(
            "SELECT asset, source, locator, min(rank) AS best,
                    snippet(text_fts, 0, '', '', '…', 12)
             FROM text_fts WHERE text_fts MATCH ?1",
        );
        if sources.is_some() {
            sql.push_str(" AND source IN (SELECT value FROM json_each(?3))");
        }
        sql.push_str(" GROUP BY asset ORDER BY best LIMIT ?2");
        // bind: ?1 match_expr, ?2 limit, ?3 (when present) JSON array of sources
        // map rows: score = -rank (bm25 rank is negative-better, same
        // convention search_names_ranked already returns)
        /* full statement/query_map implementation following
           search_names_ranked's body at query.rs:19-56 */
    }

    /// Distinct assets with any `source` rows — real-row coverage counting.
    ///
    /// # Errors
    /// Returns `CatalogError::Sqlite` on query failure.
    pub fn text_assets(&self, source: &str) -> Result<BTreeSet<AssetId>, CatalogError> {
        let mut statement =
            self.conn.prepare("SELECT DISTINCT asset FROM text_fts WHERE source = ?1")?;
        let rows = statement.query_map([source], |row| row.get::<_, String>(0))?;
        let mut assets = BTreeSet::new();
        for row in rows {
            assets.insert(AssetId(row?));
        }
        Ok(assets)
    }
}
```

Follow `search_names_ranked`'s exact row-mapping and score conventions (`query.rs:19-56`); extract its match-expression builder into a shared `fts_match_expr(terms)` helper used by both rather than duplicating it. `upsert_text_rows` needs `&mut self` for the transaction — if `self.conn.transaction()` conflicts with the struct's field privacy from `query.rs`, put the method in the same impl block style the file already uses (all files are `impl SqliteCatalog` on the same struct).

`apply.rs` `debug_dump` (:424): add `("text_fts", "asset, source, locator, content")` to the table list so dumps show it.

Also update the incremental-apply path: `apply_touched`'s `Touched::Asset` arm deletes `names_fts` rows for the asset; it must NOT delete `text_fts` rows (they are blob-projected, not event-projected — an event about an asset does not invalidate its transcript). Add a regression test:

```rust
#[test]
fn incremental_apply_preserves_text_fts_rows() {
    // open_synced-style flow: insert a text row, apply a TagAdd event for
    // the same asset incrementally, assert the text row survives.
    // (Build on incremental.rs's existing helpers.)
}
```

Write that test in `crates/catalog-sqlite/tests/incremental.rs` using its existing `ev()` helper and preamble pattern; assert `search_text_ranked` still finds the row after `apply_touched`.

- [ ] **Step 4: Run, gate, commit, open PR 4**

```bash
cargo test -p majestical-catalog-sqlite && just check
git add crates/catalog-sqlite/src crates/catalog-sqlite/tests
git commit -m "feat: text_fts table + ranked text search with snippets"
```

Open PR "feat: transcript chunking + text vector/FTS storage" (Tasks 10-12).

---

# PR 5 — OCR (Apple Vision)

### Task 13: `ocr.rs` — Vision wrapper + goldens

**Files:**
- Modify: `Cargo.toml` (workspace deps: `objc2 = "0.6.4"`, `objc2-vision = "0.3.2"`, `objc2-foundation = "0.3.2"`, `objc2-core-foundation = "0.3.2"` — verify versions and exact feature needs while implementing)
- Modify: `crates/index/Cargo.toml`
- Create: `crates/index/src/ocr.rs`
- Modify: `crates/index/src/lib.rs`
- Modify: `crates/index/src/blob.rs` (add `Derivation::OcrImage` / `Derivation::OcrKeyframe`)
- Create: `crates/index/tests/fixtures/ocr-hello.png` (generated once, committed)
- Create: `crates/index/tests/ocr_golden.rs`

- [ ] **Step 1: Generate and commit the fixture** (one-time; requires ffmpeg locally)

```bash
mkdir -p crates/index/tests/fixtures
ffmpeg -y -f lavfi -i "color=white:size=640x360:duration=1" \
  -vf "drawtext=text='MAJESTICAL CATALOG 42':fontcolor=black:fontsize=48:x=(w-text_w)/2:y=(h-text_h)/2" \
  -frames:v 1 crates/index/tests/fixtures/ocr-hello.png
```

Inspect the PNG (open it) to confirm the text rendered before committing.

- [ ] **Step 2: Write the failing blob + golden tests**

Blob test (`blob.rs` tests):

```rust
#[test]
fn ocr_blob_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let image = store.path_for("aabb", &Derivation::OcrImage { model_tag: "applevision-r3-v1" });
    assert!(image.ends_with("aa/aabb/applevision-r3-v1/image.json.zst"));
    let kf = store.path_for("aabb", &Derivation::OcrKeyframe { model_tag: "applevision-r3-v1", timestamp_ms: 7_000 });
    assert!(kf.ends_with("aa/aabb/applevision-r3-v1/kf-7000.json.zst"));
}
```

Golden test (`crates/index/tests/ocr_golden.rs`) — NOT `#[ignore]`: Vision ships with macOS, needs no model fetch:

```rust
use majestical_index::ocr;

#[test]
fn recognizes_rendered_text_in_fixture() {
    let image = image::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ocr-hello.png"))
        .expect("fixture")
        .to_rgb8();
    let result = ocr::recognize_text(&image).expect("ocr");
    let joined = result.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ").to_uppercase();
    assert!(joined.contains("MAJESTICAL"), "got: {joined}");
    assert!(joined.contains("42"), "got: {joined}");
    assert!(result.lines.iter().all(|l| l.confidence > 0.0));
}

#[test]
fn blank_image_yields_empty_lines_not_error() {
    let blank = image::RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
    let result = ocr::recognize_text(&blank).expect("ocr");
    assert!(result.lines.is_empty(), "blank image must produce zero lines");
}

#[test]
fn ocr_result_serializes_round_trip() {
    let result = ocr::OcrResult {
        revision: 3,
        lines: vec![ocr::OcrLine { text: "HELLO".into(), confidence: 0.98, bbox: [0.1, 0.2, 0.5, 0.1] }],
    };
    let bytes = result.to_json().expect("serialize");
    let back = ocr::OcrResult::from_json(&bytes).expect("parse");
    assert_eq!(back.lines[0].text, "HELLO");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p majestical-index --test ocr_golden`
Expected: compile error — module missing.

- [ ] **Step 4: Implement `ocr.rs`**

```rust
//! On-device OCR via Apple Vision (`VNRecognizeTextRequest`, accurate
//! mode). All objc2 unsafety is confined to this module behind safe fns.
//! The "model version" is Vision's request revision, pinned in the tag.

use crate::error::IndexError;

pub const OCR_MODEL_TAG: &str = "applevision-r3-v1";
const REQUEST_REVISION: usize = 3;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f64,
    /// Normalized [x, y, width, height], Vision's bottom-left origin.
    pub bbox: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrResult {
    pub revision: u32,
    pub lines: Vec<OcrLine>,
}

impl OcrResult {
    /// # Errors
    /// Serialization failure (not expected for plain types).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self).map_err(|error| IndexError::Model(format!("ocr json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes).map_err(|error| IndexError::Model(format!("ocr parse: {error}")))
    }
}

/// Recognize text in an image. Empty `lines` is a valid answer ("no text")
/// and is stored as such — otherwise the planner would retry forever.
///
/// # Errors
/// Returns `IndexError::Encoder` when Vision itself fails (not when it
/// simply finds nothing).
pub fn recognize_text(image: &image::RgbImage) -> Result<OcrResult, IndexError> {
    // Encode to PNG in memory: VNImageRequestHandler initWithData accepts
    // encoded image bytes, sidestepping CGImage construction entirely.
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| IndexError::Encoder(format!("ocr png encode: {error}")))?;
    recognize_png(&png)
}

fn recognize_png(png: &[u8]) -> Result<OcrResult, IndexError> {
    use objc2::rc::Retained;
    use objc2_foundation::{NSData, NSDictionary};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
        VNRequestTextRecognitionLevel,
    };
    // SAFETY: NSData copies the byte slice; the handler and request are
    // used on this thread only and dropped before return; performRequests
    // is documented synchronous. All pointers originate from objc2 Retained
    // allocations above.
    unsafe {
        let data = NSData::with_bytes(png);
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &NSDictionary::new(),
        );
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setRevision(REQUEST_REVISION);
        let requests = [Retained::into_super(Retained::into_super(request.clone()))];
        handler
            .performRequests_error(&objc2_foundation::NSArray::from_retained_slice(&requests))
            .map_err(|error| IndexError::Encoder(format!("vision perform: {error:?}")))?;
        let mut lines = Vec::new();
        if let Some(results) = request.results() {
            for observation in results.iter() {
                let Some(candidate) = observation.topCandidates(1).firstObject() else { continue };
                let bounding = observation.boundingBox();
                lines.push(OcrLine {
                    text: candidate.string().to_string(),
                    confidence: f64::from(candidate.confidence()),
                    bbox: [
                        bounding.origin.x,
                        bounding.origin.y,
                        bounding.size.width,
                        bounding.size.height,
                    ],
                });
            }
        }
        Ok(OcrResult { revision: u32::try_from(REQUEST_REVISION).unwrap_or(0), lines })
    }
}
```

The objc2-vision call shapes above are directionally right but MUST be adjusted against the crate's generated signatures at the version you land (`docs.rs/objc2-vision/0.3.2`): exact init/perform method names, `NSArray` construction, the `results()` element type, and whether `setRevision` takes `NSUInteger`. Budget real time here — this is generated-bindings archaeology, and it is the reason all of it is quarantined in this one module. If `performRequests_error`'s error type isn't `Debug`-printable, format via its `localizedDescription`.

`blob.rs`: `Derivation::OcrImage { model_tag }` → `<model_tag>/image.json.zst`; `Derivation::OcrKeyframe { model_tag, timestamp_ms }` → `<model_tag>/kf-<ts>.json.zst`. (`classify_vector_file` ignores `.json.zst` files already — confirm with the existing test for `keyframes.json`.)

- [ ] **Step 5: Run, gate, commit, open PR 5**

```bash
cargo test -p majestical-index --test ocr_golden && cargo test -p majestical-index blob
just check
git add Cargo.toml Cargo.lock crates/index/Cargo.toml crates/index/src/ocr.rs crates/index/src/lib.rs crates/index/src/blob.rs crates/index/tests/ocr_golden.rs crates/index/tests/fixtures/ocr-hello.png
git commit -m "feat: Apple Vision OCR with committed golden fixture"
```

Open PR "feat: Vision OCR" (Task 13).

---

# PR 6 — PDF (PDFKit) + media kinds

### Task 14: `MediaKind::{Audio, Pdf}` + extension-table fix

**Files:**
- Modify: `crates/core/src/media_kind.rs`
- Modify: `crates/catalog-sqlite/src/lib.rs` (`SNAPSHOT_VERSION` 6 → 7)
- Modify: `crates/index/src/work.rs` (audio/pdf gating comes in PR 7; here only compile fixes for exhaustive matches)
- Modify: any other exhaustive `MediaKind` matches the compiler flags (the no-wildcard-match house rule makes the compiler list them all)

- [ ] **Step 1: Write the failing tests** (in `media_kind.rs`'s test module)

```rust
#[test]
fn audio_and_pdf_kinds_classify() {
    assert_eq!(media_kind("voice-memo.m4a"), MediaKind::Audio);
    assert_eq!(media_kind("PODCAST.WAV"), MediaKind::Audio);
    assert_eq!(media_kind("brief.pdf"), MediaKind::Pdf);
    assert_eq!(media_kind("shot.mpg"), MediaKind::Video);
    assert_eq!(media_kind("frame.jxl"), MediaKind::Image);
    assert_eq!(media_kind("notes.txt"), MediaKind::Other);
}

#[test]
fn all_lists_every_kind() {
    assert_eq!(MediaKind::ALL.len(), 5);
}
```

- [ ] **Step 2: Run to verify failure, implement**

Run: `cargo test -p majestical-core media_kind` — expect failure.

`media_kind.rs`: single extension table (this closes the watchlist's "one-place extension table" item):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Pdf,
    Other,
}

impl MediaKind {
    pub const ALL: [MediaKind; 5] =
        [MediaKind::Image, MediaKind::Video, MediaKind::Audio, MediaKind::Pdf, MediaKind::Other];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Pdf => "pdf",
            MediaKind::Other => "other",
        }
    }
}

const EXTENSIONS: &[(&str, MediaKind)] = &[
    // image — existing list plus jxl, pef, iiq, 3fr (watchlist)
    /* carry over every existing image extension, then: */
    ("jxl", MediaKind::Image), ("pef", MediaKind::Image),
    ("iiq", MediaKind::Image), ("3fr", MediaKind::Image),
    // video — existing list plus mpg, mpeg, 3gp, wmv, insv (watchlist)
    /* carry over every existing video extension, then: */
    ("mpg", MediaKind::Video), ("mpeg", MediaKind::Video), ("3gp", MediaKind::Video),
    ("wmv", MediaKind::Video), ("insv", MediaKind::Video),
    // audio — new
    ("wav", MediaKind::Audio), ("mp3", MediaKind::Audio), ("m4a", MediaKind::Audio),
    ("aac", MediaKind::Audio), ("flac", MediaKind::Audio), ("aif", MediaKind::Audio),
    ("aiff", MediaKind::Audio), ("caf", MediaKind::Audio), ("ogg", MediaKind::Audio),
    // pdf — new
    ("pdf", MediaKind::Pdf),
];

#[must_use]
pub fn media_kind(path: &str) -> MediaKind {
    let Some((_, extension)) = path.rsplit_once('.') else { return MediaKind::Other };
    let lower = extension.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .find(|(candidate, _)| *candidate == lower)
        .map_or(MediaKind::Other, |(_, kind)| *kind)
}
```

Carry over the complete existing image/video lists from `media_kind.rs:12-18` — do not drop any current extension. Keep the existing public fn signature (`media_kind(path: &str)`; check whether it currently takes the whole path or just matches extension case-insensitively, and preserve its exact matching semantics for existing kinds).

Then chase compile errors across the workspace: every exhaustive `match` on `MediaKind` (the no-wildcard rule means the compiler finds them all — `work.rs` planners, CLI `kind:` filter validation, thumbnail decode). For this task, map the new kinds conservatively: `plan_thumb` treats `Audio` like `Other` (no thumb) and `Pdf` as pending-capable only after PR 7 (for now: `Audio` and `Pdf` → skip, preserving current behavior for existing kinds). `SNAPSHOT_VERSION` 6 → 7 (instance `kind` column values change for pre-existing audio/pdf files on rebuild).

- [ ] **Step 3: Run the workspace tests, gate, commit**

```bash
cargo test --workspace && just check
git add crates/core/src/media_kind.rs crates/catalog-sqlite/src/lib.rs crates/index/src/work.rs <any other files the compiler forced>
git commit -m "feat: Audio and Pdf media kinds + one-place extension table"
```

### Task 15: `pdf.rs` — PDFKit text + first-page render

**Files:**
- Modify: `Cargo.toml` (workspace deps: `objc2-pdf-kit = "0.3.2"` with features for `PDFDocument`/`PDFPage` + `objc2-app-kit`; `objc2-app-kit = "0.3.2"` — verify)
- Modify: `crates/index/Cargo.toml`
- Create: `crates/index/src/pdf.rs`
- Modify: `crates/index/src/lib.rs`, `crates/index/src/blob.rs` (`Derivation::PdfText`)
- Create: `crates/index/tests/fixtures/fixture.pdf` (generated once, committed)
- Create: `crates/index/tests/pdf_golden.rs`

- [ ] **Step 1: Generate and commit the fixture PDF**

```bash
# macOS: textutil + cupsfilter ship with the OS
printf 'Majestical fixture document.\nInvoice 7734 for the barn shoot.\n' > /tmp/fixture.txt
textutil -convert html /tmp/fixture.txt -output /tmp/fixture.html
cupsfilter /tmp/fixture.html > crates/index/tests/fixtures/fixture.pdf 2>/dev/null
```

Open the PDF to confirm both lines render, then commit. (Any generation route is fine — Pages export, `ps2pdf` — as long as the text layer contains the two known strings.)

- [ ] **Step 2: Write the failing tests** (`crates/index/tests/pdf_golden.rs`)

```rust
use majestical_index::pdf;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fixture.pdf");

#[test]
fn extracts_per_page_text() {
    let content = pdf::extract_text(std::path::Path::new(FIXTURE)).expect("extract");
    assert!(!content.pages.is_empty());
    let all = content.pages.join(" ");
    assert!(all.contains("Majestical fixture"), "got: {all}");
    assert!(all.contains("7734"), "got: {all}");
}

#[test]
fn renders_first_page_to_rgb() {
    let rendered = pdf::render_first_page(std::path::Path::new(FIXTURE), 1024).expect("render");
    assert_eq!(rendered.width().max(rendered.height()), 1024);
    // A rendered text page is mostly white but not uniform.
    let first = *rendered.get_pixel(0, 0);
    assert!(rendered.pixels().any(|p| *p != first), "render must not be a flat color");
}

#[test]
fn missing_file_is_a_decode_error() {
    assert!(pdf::extract_text(std::path::Path::new("/nonexistent.pdf")).is_err());
}
```

Blob test (`blob.rs` tests):

```rust
#[test]
fn pdf_text_blob_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let path = store.path_for("aabb", &Derivation::PdfText { model_tag: "pdfkit-v1" });
    assert!(path.ends_with("aa/aabb/pdfkit-v1/text.json.zst"));
}
```

- [ ] **Step 3: Run to verify failure, implement `pdf.rs`**

```rust
//! PDF text extraction + first-page rendering via PDFKit. All objc2
//! unsafety is confined here behind safe fns (same policy as `ocr.rs`).

use std::path::Path;

use crate::error::IndexError;

pub const PDF_MODEL_TAG: &str = "pdfkit-v1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PdfContent {
    /// Extracted text per page, index 0 = page 1. Pages with no text
    /// layer are empty strings (an answer, not an error).
    pub pages: Vec<String>,
}

impl PdfContent {
    /// # Errors
    /// Serialization failure (not expected).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self).map_err(|error| IndexError::Model(format!("pdf json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes).map_err(|error| IndexError::Model(format!("pdf parse: {error}")))
    }
}

/// Per-page text via `PDFPage.string`.
///
/// # Errors
/// Returns `IndexError::Decode` when the file cannot be opened as a PDF.
pub fn extract_text(path: &Path) -> Result<PdfContent, IndexError> {
    // SAFETY: document/page objects are created, read, and dropped on this
    // thread; NSURL is built from a checked absolute path.
    unsafe {
        let document = open_document(path)?;
        let count = document.pageCount();
        let mut pages = Vec::with_capacity(count);
        for index in 0..count {
            let text = document
                .pageAtIndex(index)
                .and_then(|page| page.string())
                .map(|s| s.to_string())
                .unwrap_or_default();
            pages.push(text);
        }
        Ok(PdfContent { pages })
    }
}

/// Render page 1 with its longest edge at `edge` px, as RGB — feeds the
/// existing thumbnail + SigLIP embedding path so PDFs join visual search.
///
/// # Errors
/// Returns `IndexError::Decode` on open/render failure.
pub fn render_first_page(path: &Path, edge: u32) -> Result<image::RgbImage, IndexError> {
    // SAFETY: as above; thumbnailOfSize:forBox: returns an autoreleased
    // NSImage consumed via its TIFF representation before return.
    unsafe {
        let document = open_document(path)?;
        let page = document.pageAtIndex(0).ok_or_else(|| IndexError::Decode {
            path: path.to_path_buf(),
            message: "pdf has no pages".into(),
        })?;
        /* bounds = page.boundsForBox(MediaBox); scale so the longest edge
           == edge; nsimage = page.thumbnailOfSize_forBox(size, MediaBox);
           tiff = nsimage.TIFFRepresentation(); decode via
           image::load_from_memory(tiff).to_rgb8() */
    }
}

unsafe fn open_document(path: &Path) -> Result<Retained<PDFDocument>, IndexError> {
    /* canonicalize path; NSURL::fileURLWithPath; PDFDocument::initWithURL;
       None → IndexError::Decode { path, message: "not a readable PDF" } */
}
```

As with `ocr.rs`: the exact objc2-pdf-kit signatures (`boundsForBox`, `thumbnailOfSize_forBox`, `TIFFRepresentation` via objc2-app-kit's `NSImage`) must be checked against docs.rs for the landed version; going through the TIFF representation and `image::load_from_memory` keeps CGImage handling out of scope. Fill both stubbed bodies completely — the tests in Step 2 define done.

- [ ] **Step 4: Run, gate, commit, open PR 6**

```bash
cargo test -p majestical-index --test pdf_golden && cargo test -p majestical-index blob
just check
git add Cargo.toml Cargo.lock crates/index/Cargo.toml crates/index/src/pdf.rs crates/index/src/lib.rs crates/index/src/blob.rs crates/index/tests/pdf_golden.rs crates/index/tests/fixtures/fixture.pdf
git commit -m "feat: PDFKit text extraction + first-page render"
```

Open PR "feat: PDF support + media kinds" (Tasks 14-15).

# PR 7 — Queue integration

### Task 16: `work.rs` — new derivation kinds, capabilities, statuses

**Files:**
- Modify: `crates/index/src/work.rs`

- [ ] **Step 1: Write the failing planner tests** (in `work.rs`'s test module, following its existing test setup — read it first; tests there build `AssetSource` lists + a temp `BlobStore` and assert on the plan)

```rust
#[test]
fn transcript_planned_for_video_and_audio_with_whisper_and_ffmpeg() {
    let (blobs, _dir) = test_blobs();
    let sources = vec![
        source("xxh3:aa", MediaKind::Video, Some("/v.mov")),
        source("xxh3:bb", MediaKind::Audio, Some("/a.m4a")),
        source("xxh3:cc", MediaKind::Image, Some("/i.jpg")),
    ];
    let caps = Capabilities {
        model_tag: Some("siglip2-b16-v1".into()),
        ffmpeg: true,
        whisper: true,
        text_model: true,
        describer_tag: None,
    };
    let plan = plan_work(&sources, &blobs, &caps);
    let transcripts: Vec<_> =
        plan.items.iter().filter(|i| i.kind == WorkKind::Transcribe).collect();
    assert_eq!(transcripts.len(), 2, "video + audio, never image");
}

#[test]
fn transcript_needs_model_without_whisper() {
    let (blobs, _dir) = test_blobs();
    let sources = vec![source("xxh3:aa", MediaKind::Video, Some("/v.mov"))];
    let caps = Capabilities { model_tag: None, ffmpeg: true, whisper: false, text_model: false, describer_tag: None };
    let plan = plan_work(&sources, &blobs, &caps);
    assert_eq!(plan.transcripts.needs_model, 1);
    assert!(plan.items.iter().all(|i| i.kind != WorkKind::Transcribe));
}

#[test]
fn transcript_embed_planned_when_transcript_blob_exists_but_chunks_missing() {
    let (blobs, _dir) = test_blobs();
    // Simulate a teammate-synced transcript blob with no local chunk vectors.
    let path = blobs.path_for("aa11", &Derivation::Transcript { model_tag: "whisper-large-v3-turbo-q5-v1" });
    blobs.write_atomic(&path, b"{}").expect("write");
    let sources = vec![source("xxh3:aa11", MediaKind::Video, Some("/v.mov"))];
    let caps = Capabilities { model_tag: None, ffmpeg: false, whisper: false, text_model: true, describer_tag: None };
    let plan = plan_work(&sources, &blobs, &caps);
    assert!(plan.items.iter().any(|i| i.kind == WorkKind::TranscriptEmbed),
        "chunk embedding needs only the transcript blob + minilm, not ffmpeg/whisper");
}

#[test]
fn ocr_planned_for_stills_and_pdf_text_for_pdfs() {
    let (blobs, _dir) = test_blobs();
    let sources = vec![
        source("xxh3:aa", MediaKind::Image, Some("/i.jpg")),
        source("xxh3:bb", MediaKind::Pdf, Some("/d.pdf")),
    ];
    let caps = Capabilities { model_tag: None, ffmpeg: false, whisper: false, text_model: false, describer_tag: None };
    let plan = plan_work(&sources, &blobs, &caps);
    assert!(plan.items.iter().any(|i| i.kind == WorkKind::OcrImage));
    assert!(plan.items.iter().any(|i| i.kind == WorkKind::PdfText));
}

#[test]
fn captions_planned_only_with_describer_configured() {
    let (blobs, _dir) = test_blobs();
    let sources = vec![source("xxh3:aa", MediaKind::Image, Some("/i.jpg"))];
    let without = Capabilities { model_tag: None, ffmpeg: false, whisper: false, text_model: false, describer_tag: None };
    assert_eq!(plan_work(&sources, &blobs, &without).captions.needs_model, 1);
    let with = Capabilities { describer_tag: Some("describe-m".into()), ..without };
    assert!(plan_work(&sources, &blobs, &with).items.iter().any(|i| i.kind == WorkKind::Caption));
}

#[test]
fn priority_order_is_thumbs_embeds_transcripts_ocr_pdf_captions() {
    // One asset of each need; assert the global item ordering groups by
    // kind in the spec's priority order.
    /* build sources covering all kinds with full caps; collect
       plan.items kinds into a Vec; assert the first occurrence index of
       each kind is monotonically: Thumb < ImageEmbed/Keyframes <
       Transcribe < OcrImage/OcrKeyframes/PdfText < Caption */
}
```

Write `priority_order_...` out fully when implementing (build the sources, map `plan.items` to kinds, compare first-occurrence indices) — the assertion strategy is fixed here, the fixture list is mechanical.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index work`
Expected: compile errors — new `WorkKind` variants and `Capabilities` fields missing.

- [ ] **Step 3: Implement**

`work.rs` extensions:

```rust
pub enum WorkKind {
    Thumb,
    ImageEmbed,
    Keyframes,
    Transcribe,       // video+audio → transcript blob (needs ffmpeg + whisper)
    TranscriptEmbed,  // transcript blob → chunk vector blobs (needs minilm)
    OcrImage,         // stills (no capability needed — Vision ships with macOS)
    OcrKeyframes,     // keyframe manifest → per-frame OCR blobs (needs ffmpeg)
    PdfText,          // pdf → text blob (PDFKit, no capability needed)
    Caption,          // stills + videos → caption/tags blobs (needs describer)
}

pub struct Capabilities {
    pub model_tag: Option<String>,
    pub ffmpeg: bool,
    pub whisper: bool,
    pub text_model: bool,
    /// Model tag of the configured describer, when one is configured and
    /// its blobs should be planned (e.g. `describe-qwen3-vl-8b`).
    pub describer_tag: Option<String>,
}

pub struct WorkPlan {
    pub items: Vec<WorkItem>,
    pub thumbs: KindStatus,
    pub embeddings: KindStatus,
    pub keyframes: KindStatus,
    pub transcripts: KindStatus,
    pub ocr: KindStatus,
    pub pdf: KindStatus,
    pub captions: KindStatus,
}
```

New per-kind planners, each following `plan_image_embed`'s precedence template (`work.rs:160`): capability-missing → `needs_model` (or `needs_ffmpeg`), blob exists → `done`, no path → `offline`, else `pending` + item:

- `plan_transcribe`: `MediaKind::Video | MediaKind::Audio`; done-marker = `Derivation::Transcript` blob; needs `caps.whisper` (→`needs_model`) and `caps.ffmpeg` (→`needs_ffmpeg`).
- `plan_transcript_embed`: any asset whose `Transcript` blob exists; done-marker = at least one `chunk-*.f32le.zst` for the minilm tag (use `path_for(..., &Derivation::TranscriptChunk { model_tag, start_ms: 0 })`'s parent dir listing — add a small `BlobStore::has_any_chunk(asset_hex, model_tag) -> bool` helper in `blob.rs` for this, with its own unit test). Needs `caps.text_model`. An empty transcript (no segments) writes a zero-chunk marker — see Task 17 — so it doesn't replan forever; `has_any_chunk` also returns true when the marker `chunks-empty.json` exists.
- `plan_ocr`: `MediaKind::Image` → done-marker `Derivation::OcrImage`; `MediaKind::Video` with keyframe manifest present → done-marker: OCR blob for **every** timestamp in the manifest (needs a manifest reader — Task 17 adds one; the planner emits one `OcrKeyframes` item per video, the runner diffs per-timestamp). Video OCR needs `caps.ffmpeg`.
- `plan_pdf_text`: `MediaKind::Pdf`; done-marker `Derivation::PdfText`.
- `plan_caption`: `MediaKind::Image | MediaKind::Video` (video requires keyframe manifest present); `caps.describer_tag` `None` → `needs_model`; done-marker = `Derivation::Caption { model_tag }` blob (stills) / `Derivation::Captions { model_tag }` (video, Task 19).

Order the passes: thumbs → image embeds → keyframes → transcribe → transcript-embed → ocr → pdf → captions.

Also in `blob.rs` (this task): `Derivation::Caption { model_tag }` → `<model_tag>/caption.json.zst`, `Derivation::Captions { model_tag }` → `<model_tag>/captions.json.zst`, `Derivation::Tags { model_tag }` → `<model_tag>/tags.json.zst`, plus the `has_any_chunk` helper. Blob-path tests for each (same pattern as Task 8 Step 1).

- [ ] **Step 4: Run, gate, commit**

```bash
cargo test -p majestical-index work blob && just check
git add crates/index/src/work.rs crates/index/src/blob.rs
git commit -m "feat: work planner covers transcripts, OCR, PDF, captions"
```

### Task 17: `index run` executes the new kinds; failure markers; `index status`

**Files:**
- Modify: `crates/cli/src/index_cmd.rs`
- Modify: `crates/cli/src/main.rs` (`--kinds` docs)
- Test: `crates/cli/tests/index_smoke.rs` (extend)

- [ ] **Step 1: Write the failing CLI tests**

```rust
#[test]
fn index_status_reports_new_kinds_with_named_gaps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("clip.wav"), b"RIFFfake").expect("write");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    // Empty MAJ_MODEL_DIR → whisper absent → the gap is named with a remedy.
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("transcripts"))
        .stdout(contains("needs model"));
}

#[test]
fn index_run_kinds_accepts_new_names_and_rejects_unknown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    maj(&root, &state).args(["index", "run", "--kinds", "transcripts,ocr,pdf,captions"]).assert().success();
    maj(&root, &state)
        .args(["index", "run", "--kinds", "bogus"])
        .assert()
        .failure()
        .stderr(contains("bogus"));
}

#[test]
fn failed_derivations_are_reported_and_replanned() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    // A .pdf that is not a PDF: PdfText will fail, and must be visible.
    std::fs::write(media.join("broken.pdf"), b"not a pdf").expect("write");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    maj(&root, &state).args(["index", "run", "--kinds", "pdf"]).assert().success();
    maj(&root, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("failed last run: 1").or(contains("failed: 1")));
    // Second run re-attempts (still fails, still visible) — never silently dropped.
    maj(&root, &state).args(["index", "run", "--kinds", "pdf", "--json"]).assert().success()
        .stdout(contains("\"pdf\""));
}
```

Fix the exact status wording when implementing `print_kind_status` — the test asserts the substrings above; keep them.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test index_smoke index_status_reports_new_kinds`
Expected: FAIL — status doesn't know `transcripts`.

- [ ] **Step 3: Implement in `index_cmd.rs`**

1. `VALID_KINDS` → `&["thumbs", "embeddings", "keyframes", "transcripts", "ocr", "pdf", "captions"]` (CLI kind `transcripts` covers both `Transcribe` and `TranscriptEmbed` work).
2. `capabilities()` (:35) grows:

```rust
fn capabilities() -> Capabilities {
    let whisper = model::model_dir_for(&model::WHISPER)
        .map(|dir| dir.join(transcribe::MODEL_FILE).is_file())
        .unwrap_or(false);
    let text_model = model::model_dir_for(&model::MINILM)
        .map(|dir| dir.join("model.onnx").is_file())
        .unwrap_or(false);
    // describer_tag is filled by run_once from describer_cmd::load_config
    Capabilities { model_tag: /* existing siglip check */, ffmpeg: video::ffmpeg_available(), whisper, text_model, describer_tag: None }
}
```

3. New runner fns, each following `run_thumb_items`' scoped-thread/cursor pattern (`:131`) for parallel-safe kinds and plain loops for model-bound kinds (whisper and the describer are one-at-a-time):
   - `run_transcribe_items`: per item — `video::probe` (video) or ffprobe duration via the same probe (audio also works through ffprobe), `video::extract_audio_pcm`, `Transcriber::load` once outside the loop, `transcribe`, `blobs.write_atomic(transcript_path, &zstd::encode_all(transcript.to_json()?, 3)?)`. Transcript blobs are zstd like all `.zst` blobs — reuse `write_atomic` after manual zstd encode (mirror how `write_vector` composes zstd + write_atomic).
   - `run_transcript_embed_items`: read transcript blob (zstd decode + `Transcript::from_json`), `chunk_segments`, `TextEncoder::load` once, embed each chunk → `blobs.write_vector(chunk_path, &vector)` + `TextVectorStore.add` rows (source `"transcript"`, chunk text included). Zero chunks (empty/silent audio) → write the `chunks-empty.json` marker via `write_atomic` so the planner sees done.
   - `run_ocr_items`: stills — decode via `thumbs::decode_image`, `ocr::recognize_text`, write `OcrImage` blob (zstd JSON). Videos — read the keyframe manifest (add `keyframes_manifest_read(bytes) -> Result<(String, usize, Vec<u64>)>` in `index_cmd.rs` beside `keyframes_manifest_json` at `:649`, with a round-trip unit test in the `:926` test module), diff timestamps against existing OCR blobs, `video::extract_frame` per missing timestamp, OCR, write `kf-<ts>.json.zst`.
   - `run_pdf_text_items`: `pdf::extract_text`, write `PdfText` blob. (The PDF *preview* path — thumb + embedding — comes from teaching `decode_thumb_source` (:99) and `embed_one` (:363) to route `MediaKind::Pdf` through `pdf::render_first_page(path, 1024)`; do that here too, so PDFs flow through the existing Thumb/ImageEmbed kinds without new work kinds.)
   - Captions are Task 19 (PR 8) — `run_once` skips `WorkKind::Caption` items with a "captions: not yet implemented" JSON count of 0 until then; simpler: don't plan captions until PR 8 wires the describer into `capabilities()` (leave `describer_tag: None` here).
4. **text_fts heal** (mirror of `load_missing_vectors_from_blobs`, always runs): enumerate transcript/ocr/pdf blobs via `BlobStore` walks; for each asset+source missing from `db.text_assets(source)`, decode the blob and `db.upsert_text_rows(...)`. Transcript rows = one per chunk (locator = chunk start_ms, content = chunk text — chunk at heal time with `chunk_segments`, deterministic); OCR rows = one per keyframe (locator = ts) or one for stills (locator -1, lines joined with spaces); PDF rows = one per page (locator = 1-based page number). This needs the catalog db: `run_once` already has `open_catalog` access via its `FsApp` — follow how `cmd_index_run` obtains the projection today (`gather_sources`) and open the `SqliteCatalog` alongside.
5. **Failure markers**: after each run, write `<state>/index-failures.json`:

```rust
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct FailureReport {
    /// kind name → [(path, reason)]
    failures: std::collections::BTreeMap<String, Vec<(String, String)>>,
}
```

written via serde_json to the state dir (plain `std::fs::write`; it's per-machine scratch, not a blob). `cmd_index_status` reads it and appends `failed last run: N (<first reason>)` per kind. Failures do NOT write done-markers, so the next run re-plans them (the test in Step 1 pins this).
6. `print_run_result`/`print_kind_status`/JSON: add the four new kinds' outcomes and statuses (JSON keys: `transcripts`, `ocr`, `pdf`, `captions`).

Keep `run_once` under the 100-line/complexity-8 limits by extracting per-kind helpers — the existing file's per-kind `run_*_items` layout is the template.

- [ ] **Step 4: Run, gate, commit, open PR 7**

```bash
cargo test -p majestical-cli --test index_smoke && cargo test -p majestical-index && just check
git add crates/cli/src/index_cmd.rs crates/cli/src/main.rs crates/cli/tests/index_smoke.rs
git commit -m "feat: index run/status cover transcripts, OCR, PDF with failure markers"
```

Open PR "feat: queue integration for new derivations" (Tasks 16-17). Also run the gated e2e locally before the PR:
`MAJ_MODEL_DIR=.model-cache cargo test -p majestical-cli --test index_smoke -- --ignored` (existing gated tests must still pass).

---

# PR 8 — Captions + tag suggestions

### Task 18: caption/tags derivation in `index run`

**Files:**
- Modify: `crates/cli/src/index_cmd.rs`
- Modify: `crates/cli/src/describer_cmd.rs` (expose `load_config` — done in Task 3)
- Test: `crates/cli/tests/caption_smoke.rs` (uses httpmock as a fake backend — add `httpmock.workspace = true` to `crates/cli` dev-deps)

- [ ] **Step 1: Write the failing test** (`caption_smoke.rs`; a real end-to-end through the CLI against a mock backend — no model weights needed since captions ride on thumbs)

```rust
mod common;
use common::maj;
use httpmock::prelude::*;
use predicates::str::contains;

fn caption_response() -> serde_json::Value {
    serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "a red square"}}]})
}

fn tags_response() -> serde_json::Value {
    serde_json::json!({"choices": [{"message": {"role": "assistant",
        "content": "{\"tags\":[{\"tag\":\"color/red\",\"confidence\":0.95}]}"}}]})
}

#[test]
fn index_run_captions_via_mock_backend_and_search_finds_them() {
    let server = MockServer::start();
    // First call captions, second call suggests tags; a body matcher on the
    // prompt text distinguishes them.
    server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions")
            .body_includes("Describe this image");
        then.status(200).json_body(caption_response());
    });
    server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions")
            .body_includes("Suggest tags");
        then.status(200).json_body(tags_response());
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    let png = media.join("red.png");
    image::RgbImage::from_pixel(64, 64, image::Rgb([255, 0, 0])).save(&png).expect("png");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    maj(&root, &state)
        .args(["describer", "set", "--backend", "ollama", "--model", "mock-model",
               "--base-url", &server.base_url()])
        .assert().success();

    // thumbs first (captions ride on thumbs), then captions.
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "run", "--kinds", "thumbs"]).assert().success();
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "run", "--kinds", "captions"]).assert().success()
        .stdout(contains("captions"));

    // The caption is now searchable text (text_fts heal ran in the same pass).
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "red square"])
        .assert().success()
        .stdout(contains("red.png"));
}

#[test]
fn caption_run_without_describer_names_the_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "status"])
        .assert().success()
        .stdout(contains("captions").and(contains("maj describer set")));
}
```

(Note: `search "red square"` finding a caption hit end-to-end depends on PR 9's text search being merged; until then assert instead that the caption blob exists on disk — `walkdir_find(&root.join("blobs"), "caption.json.zst")` from `common` — and move the search assertion into PR 9's tests. Choose based on merge order at execution time; if PR 9 lands first, keep the search assertion.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test caption_smoke`
Expected: FAIL — captions kind runs nothing.

- [ ] **Step 3: Implement in `index_cmd.rs`**

1. `run_once` fills `capabilities().describer_tag` from `describer_cmd::load_config(catalog_root)?.map(|c| c.model_tag())`.
2. `run_caption_items(app, blobs, db, items, config)`:
   - Build `HttpDescriber::new(config, describer_cmd::env_api_key())` once.
   - Collect the catalog's tag vocabulary once: iterate `projection.assets()`, union `projection.tags(asset)` into a sorted `Vec<String>`.
   - Stills (`MediaKind::Image` and `MediaKind::Pdf`): read the thumb blob bytes (`Derivation::Thumb`); skip with a counted failure if the thumb is missing (thumbs precede captions in priority; a missing thumb means thumbs failed). `describer.caption(&thumb_bytes)` → write `Caption` blob (zstd JSON of the `Caption` struct). Then `describer.suggest_tags(TagSubject::Image(&thumb_bytes), &vocab)` → write `Tags` blob (zstd JSON of `Vec<TagSuggestion>`).
   - Videos: read the keyframe manifest; take up to `MAX_DESCRIBED_KEYFRAMES: usize = 12` evenly spaced timestamps (`timestamps.iter().step_by(max(1, len/12)).take(12)`); per timestamp `video::extract_frame` → `thumbs::thumbnail_webp` at 512 px (add an edge parameter or reuse 320 — decide by what `thumbnail_webp` exposes; 320 is acceptable, record as as-built note if used) → caption each; write `Captions` blob: zstd JSON of

     ```rust
     #[derive(serde::Serialize, serde::Deserialize)]
     struct VideoCaptions {
         model_tag: String,
         detected_keyframes: usize,
         described: Vec<(u64, String)>, // (ts_ms, caption) — the auditable cap
     }
     ```

     Then `describer.suggest_tags(TagSubject::Captions(&texts), &vocab)` → `Tags` blob.
   - A mid-run backend error aborts the remaining caption items (not the whole run), recording the skipped count in the failure report — degradation with a named gap, per spec.
3. text_fts heal covers captions: source `"caption"`, locator -1 (stills) / ts_ms per described keyframe (video), content = caption text.
4. `index status` caption gap line when unconfigured: `captions: no describer configured (run maj describer set)` (the planner's `needs_model` count for captions renders with this remedy text).

- [ ] **Step 4: Run, gate, commit**

```bash
cargo test -p majestical-cli --test caption_smoke && just check
git add crates/cli/src/index_cmd.rs crates/cli/Cargo.toml crates/cli/tests/caption_smoke.rs
git commit -m "feat: caption + tag-suggestion derivation via configured describer"
```

### Task 19: `maj tags suggestions|confirm|reject`

**Files:**
- Modify: `crates/cli/src/commands.rs` (`cmd_tag` grows the new subcommands, or a new `tags_cmd.rs` module if `commands.rs` is crowded — prefer the new module)
- Create: `crates/cli/src/tags_cmd.rs`
- Modify: `crates/cli/src/main.rs` (`TagCmd` gains `Suggestions`, `Confirm`, `Reject`)
- Modify: `crates/index/src/blob.rs` (add `iter_named(file_name) -> Vec<(asset_hex, model_tag, PathBuf)>` helper + test)
- Test: extend `crates/cli/tests/caption_smoke.rs`

- [ ] **Step 1: Write the failing CLI test** (append to `caption_smoke.rs`)

```rust
#[test]
fn suggestions_list_confirm_reject_flow() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    let png = media.join("red.png");
    image::RgbImage::from_pixel(64, 64, image::Rgb([255, 0, 0])).save(&png).expect("png");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();

    // Plant a suggestions blob directly (unit-of-work isolation from the
    // describer path, which caption_smoke already covers).
    let output = maj(&root, &state).args(["search", "red"]).output().expect("search");
    let asset = common::first_asset_from(&output); // see note below
    let hex = asset.strip_prefix("xxh3:").expect("hex").to_string();
    let blobs = majestical_index::blob::BlobStore::new(&root);
    let path = blobs.path_for(&hex, &majestical_index::blob::Derivation::Tags { model_tag: "describe-m" });
    let suggestions = vec![majestical_core::ports::TagSuggestion {
        tag: "color/red".into(), confidence: 0.95, in_vocab: false, model_tag: "describe-m".into(),
    }];
    let json = serde_json::to_vec(&suggestions).expect("json");
    blobs.write_atomic(&path, &zstd::encode_all(json.as_slice(), 3).expect("zstd")).expect("write");

    maj(&root, &state).args(["tags", "suggestions"]).assert().success()
        .stdout(contains("color/red").and(contains("0.95")).and(contains("describe-m")));

    maj(&root, &state).args(["tags", "confirm", &asset, "color/red"]).assert().success();
    // Confirmed → a plain TagAdd → visible in tag search, gone from pending.
    maj(&root, &state).args(["search", "tag:color/red"]).assert().success().stdout(contains("red.png"));
    maj(&root, &state).args(["tags", "suggestions"]).assert().success()
        .stdout(contains("color/red").not());

    // Reject flow: plant a second suggestion, reject it, list excludes it.
    let more = vec![majestical_core::ports::TagSuggestion {
        tag: "shape/square".into(), confidence: 0.5, in_vocab: false, model_tag: "describe-m".into(),
    }];
    let json = serde_json::to_vec(&more).expect("json");
    blobs.write_atomic(&path, &zstd::encode_all(json.as_slice(), 3).expect("zstd")).expect("write");
    maj(&root, &state).args(["tags", "reject", &asset, "shape/square"]).assert().success();
    maj(&root, &state).args(["tags", "suggestions"]).assert().success()
        .stdout(contains("shape/square").not());
}
```

(`common::first_asset_from` — if no such helper exists, use `cli_smoke.rs`'s `first_asset_id` pattern: copy it into `common/mod.rs` as a shared helper and migrate `cli_smoke`'s local copy to it. `crates/cli` needs `zstd` and `majestical-index`/`majestical-core` already present as deps; add `zstd.workspace = true` to its dev-deps.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test caption_smoke suggestions_list`
Expected: FAIL — `tags suggestions` unknown.

- [ ] **Step 3: Implement**

`blob.rs` — enumeration helper (with a unit test writing two assets' tags blobs and asserting both are found):

```rust
/// All blobs named `file_name` across assets: (asset_hex, model_tag, path).
///
/// # Errors
/// Propagates directory-walk failures; missing dirs yield empty.
pub fn iter_named(&self, file_name: &str) -> Result<Vec<(String, String, PathBuf)>, IndexError>
```

(Same two-level walk as `iter_vectors_under_prefix` at `blob.rs:203`, matching on the file name inside each `<model_tag>/` subdir.)

`tags_cmd.rs`:

```rust
//! Suggestion review: list pending, confirm into the folksonomy, reject
//! into a per-machine jsonl (never synced, survives projection rebuilds).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context as _;
use majestical_core::ports::TagSuggestion;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;

use crate::app::FsApp;
use crate::state_dir;

#[derive(serde::Serialize, serde::Deserialize)]
struct Rejection {
    asset: String,
    tag: String,
}

fn rejections_path(catalog_root: &Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(state_dir::state_dir_for(catalog_root)?.join("tag-rejections.jsonl"))
}

fn load_rejections(catalog_root: &Path) -> anyhow::Result<BTreeSet<(String, String)>> {
    let path = rejections_path(catalog_root)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut rejections = BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rejection: Rejection =
            serde_json::from_str(line).with_context(|| format!("parse rejection line: {line}"))?;
        rejections.insert((rejection.asset, rejection.tag));
    }
    Ok(rejections)
}

/// One pending suggestion joined against catalog state.
struct Pending {
    asset: String,
    suggestion: TagSuggestion,
}

fn pending_suggestions(
    catalog_root: &Path,
    projection: &Projection,
) -> anyhow::Result<Vec<Pending>> {
    let blobs = BlobStore::new(catalog_root);
    let rejections = load_rejections(catalog_root)?;
    let mut pending = Vec::new();
    for (asset_hex, _model_tag, path) in blobs.iter_named("tags.json.zst")? {
        let asset = format!("xxh3:{asset_hex}");
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let json = zstd::decode_all(bytes.as_slice()).context("zstd decode tags blob")?;
        let suggestions: Vec<TagSuggestion> =
            serde_json::from_slice(&json).context("parse tags blob")?;
        let asset_id = majestical_core::event::AssetId(asset.clone());
        let existing: BTreeSet<&str> = projection.tags(&asset_id).iter().map(String::as_str).collect();
        for suggestion in suggestions {
            let already_tagged = existing.contains(suggestion.tag.as_str());
            let rejected = rejections.contains(&(asset.clone(), suggestion.tag.clone()));
            if !already_tagged && !rejected {
                pending.push(Pending { asset: asset.clone(), suggestion });
            }
        }
    }
    Ok(pending)
}

pub(crate) fn cmd_suggestions(app: &FsApp, catalog_root: &Path) -> anyhow::Result<()> {
    let projection = app.projection()?;
    let pending = pending_suggestions(catalog_root, &projection)?;
    if pending.is_empty() {
        println!("no pending suggestions — captions/tags derive during `maj index run` with a describer configured");
        return Ok(());
    }
    for entry in &pending {
        let vocab_marker = if entry.suggestion.in_vocab { "known" } else { "new" };
        println!(
            "{}  {}  {:.2}  {}  {}",
            entry.asset, entry.suggestion.tag, entry.suggestion.confidence,
            vocab_marker, entry.suggestion.model_tag,
        );
    }
    println!("{} pending", pending.len());
    println!("confirm: maj tags confirm <asset> <tag>…   reject: maj tags reject <asset> <tag>…");
    Ok(())
}

pub(crate) fn cmd_confirm(app: &mut FsApp, asset: &str, tags: &[String]) -> anyhow::Result<()> {
    // Reuse the existing TagCmd::Add path so a confirmed tag is exactly a
    // hand-added one: same op, same validation.
    for tag in tags {
        crate::commands::cmd_tag(
            app,
            crate::main_types_or_wherever::TagCmd::Add { asset: asset.to_string(), tag: tag.clone() },
        )?;
    }
    println!("confirmed {} tag(s) on {asset}", tags.len());
    Ok(())
}

pub(crate) fn cmd_reject(catalog_root: &Path, asset: &str, tags: &[String]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let path = rejections_path(catalog_root)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    for tag in tags {
        let line = serde_json::to_string(&Rejection { asset: asset.to_string(), tag: tag.clone() })?;
        writeln!(file, "{line}")?;
    }
    println!("rejected {} tag(s) on {asset} (this machine only)", tags.len());
    Ok(())
}
```

Fix the `cmd_confirm` call shape to how `TagCmd` is actually reachable (`TagCmd` lives in `main.rs`; either move the enum into `commands.rs` where `cmd_tag` lives, or have `cmd_confirm` emit the op the way `cmd_tag` does — read `commands.rs:158` and do exactly what it does: `ensure_asset_known` + `app.emit(vec![Op::TagAdd { .. }])`). The requirement pinned by the test: confirm produces a tag indistinguishable from `maj tag add`.

`main.rs` `TagCmd` gains:

```rust
    /// List pending AI tag suggestions.
    Suggestions,
    /// Confirm suggestions into the folksonomy (emits plain TagAdd).
    Confirm { asset: String, #[arg(required = true)] tags: Vec<String> },
    /// Reject suggestions on this machine (never synced).
    Reject { asset: String, #[arg(required = true)] tags: Vec<String> },
```

- [ ] **Step 4: Run, gate, commit, open PR 8**

```bash
cargo test -p majestical-cli --test caption_smoke && cargo test -p majestical-index blob && just check
git add crates/cli/src/tags_cmd.rs crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/tests/caption_smoke.rs crates/cli/tests/common/mod.rs crates/cli/Cargo.toml crates/index/src/blob.rs
git commit -m "feat: maj tags suggestions|confirm|reject"
```

Open PR "feat: captions + tag suggestion review flow" (Tasks 18-19).

# PR 9 — Query layer: N-way fusion, `in:` filter, snippets

### Task 20: query parser — `in:` source filter

**Files:**
- Modify: `crates/cli/src/query.rs`
- Modify: `crates/cli/src/search.rs` (`FILTER_KEYS` const at `:30`)

- [ ] **Step 1: Write the failing parser tests** (in `query.rs`'s test module, matching its existing test style)

```rust
#[test]
fn in_filter_parses_sources() {
    let parsed = parse_query("barn in:transcript in:ocr").expect("parse");
    assert_eq!(parsed.terms, vec!["barn"]);
    let sources: Vec<_> = parsed
        .filters
        .iter()
        .filter(|f| f.key == "in")
        .map(|f| f.value.as_str())
        .collect();
    assert_eq!(sources, vec!["transcript", "ocr"]);
}

#[test]
fn negated_in_filter_is_rejected_at_resolve_time_not_parse_time() {
    // The parser stays generic (RawFilter); rejection happens in
    // resolve_filters like before:/after: negation does.
    let parsed = parse_query("-in:ocr").expect("parse");
    assert!(parsed.filters[0].negated);
}
```

- [ ] **Step 2: Run, verify failure only if the parser hardcodes a key list**

Run: `cargo test -p majestical-cli query::tests::in_filter`
If `parse_query` already passes unknown keys through as `RawFilter` (check `query.rs` — the phase 4 design suggests it does), the first test may pass immediately; the real work is Task 21's resolution. Either way, land the tests.

- [ ] **Step 3: Implement (if needed), run, commit**

If the parser validates keys against a list, add `"in"`. Update `FILTER_KEYS` (search.rs:30) to `"tag, vol/volume, para, kind, online, before, after, in"`.

```bash
cargo test -p majestical-cli query && just check
git add crates/cli/src/query.rs crates/cli/src/search.rs
git commit -m "feat: in: source filter parses"
```

### Task 21: N-way fusion + text search in `maj search`

**Files:**
- Modify: `crates/cli/src/search.rs`
- Test: unit tests in `search.rs` + CLI tests in `crates/cli/tests/search_text_smoke.rs`

- [ ] **Step 1: Write the failing fusion unit tests** (in `search.rs`'s test module — it already tests `fuse_ranked`; extend alongside)

```rust
#[test]
fn fuse_n_hard_filters_every_list() {
    let allowed: BTreeSet<AssetId> = [AssetId("xxh3:aa".into())].into();
    let name_fts = vec![(AssetId("xxh3:aa".into()), -1.0), (AssetId("xxh3:zz".into()), -2.0)];
    let text_fts = vec![(AssetId("xxh3:zz".into()), -3.0)];
    let semantic_lists = vec![
        vec![AssetId("xxh3:zz".into())],
        vec![AssetId("xxh3:zz".into()), AssetId("xxh3:aa".into())],
    ];
    let fused = fuse_ranked_n(&FuseInputs {
        name_fts, text_fts, semantic: semantic_lists,
        allowed: Some(&allowed), limit: 10,
    });
    assert!(fused.iter().all(|(asset, _)| asset.0 == "xxh3:aa"),
        "the phase-4 BLOCKER: zz is filtered out of EVERY list, ranked or semantic");
}

#[test]
fn fuse_n_reduces_to_bm25_when_only_name_fts_has_results() {
    let name_fts = vec![(AssetId("xxh3:aa".into()), -1.5), (AssetId("xxh3:bb".into()), -0.5)];
    let fused = fuse_ranked_n(&FuseInputs {
        name_fts: name_fts.clone(), text_fts: vec![], semantic: vec![], allowed: None, limit: 10,
    });
    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].0.0, "xxh3:aa", "bm25 order preserved (phase-4 behavior at N=1)");
}

#[test]
fn fuse_n_rrf_merges_all_nonempty_lists() {
    // An asset ranked #1 in three lists beats one ranked #1 in a single list.
    let name_fts = vec![(AssetId("xxh3:aa".into()), -1.0)];
    let text_fts = vec![(AssetId("xxh3:bb".into()), -1.0), (AssetId("xxh3:aa".into()), -0.5)];
    let semantic = vec![vec![AssetId("xxh3:aa".into()), AssetId("xxh3:bb".into())]];
    let fused = fuse_ranked_n(&FuseInputs { name_fts, text_fts, semantic, allowed: None, limit: 10 });
    assert_eq!(fused[0].0.0, "xxh3:aa");
}

#[test]
fn fuse_n_limit_truncates() {
    let name_fts: Vec<_> =
        (0..20).map(|i| (AssetId(format!("xxh3:{i:02}")), -f64::from(i))).collect();
    let fused = fuse_ranked_n(&FuseInputs {
        name_fts, text_fts: vec![], semantic: vec![], allowed: None, limit: 5,
    });
    assert_eq!(fused.len(), 5);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli fuse_n`
Expected: compile error.

- [ ] **Step 3: Implement fusion + the search flow**

```rust
struct FuseInputs<'a> {
    /// bm25-scored (score = -rank, higher better) name hits.
    name_fts: Vec<(AssetId, f64)>,
    /// bm25-scored text hits (best row per asset).
    text_fts: Vec<(AssetId, f64)>,
    /// Rank-ordered semantic lists (image vectors, text vectors).
    semantic: Vec<Vec<AssetId>>,
    allowed: Option<&'a BTreeSet<AssetId>>,
    limit: usize,
}

/// N-way reciprocal-rank fusion with the hard-filter intersection applied
/// to EVERY input list (the phase-4 filter-leak fix, generalized).
fn fuse_ranked_n(inputs: &FuseInputs<'_>) -> Vec<(AssetId, f64)> {
    let keep = |asset: &AssetId| inputs.allowed.is_none_or(|allowed| allowed.contains(asset));
    let name: Vec<(AssetId, f64)> =
        inputs.name_fts.iter().filter(|(a, _)| keep(a)).cloned().collect();
    let text: Vec<(AssetId, f64)> =
        inputs.text_fts.iter().filter(|(a, _)| keep(a)).cloned().collect();
    let semantic: Vec<Vec<AssetId>> = inputs
        .semantic
        .iter()
        .map(|list| list.iter().filter(|a| keep(a)).cloned().collect::<Vec<_>>())
        .filter(|list: &Vec<AssetId>| !list.is_empty())
        .collect();
    let text_empty = text.is_empty();
    if text_empty && semantic.is_empty() {
        // Phase-4 behavior preserved: bm25 scores/order, truncated.
        let mut ranked = name;
        ranked.truncate(inputs.limit);
        return ranked;
    }
    let mut lists: Vec<Vec<AssetId>> = Vec::new();
    if !name.is_empty() {
        lists.push(name.iter().map(|(a, _)| a.clone()).collect());
    }
    if !text_empty {
        lists.push(text.iter().map(|(a, _)| a.clone()).collect());
    }
    lists.extend(semantic);
    rrf_merge(&lists, inputs.limit)
}
```

Rework `fuse_ranked`'s call site in `term_search` (`search.rs:172-210`) to build `FuseInputs`; delete the old two-list `fuse_ranked` outright (replace, don't deprecate) and migrate its existing tests onto `fuse_ranked_n` (they must keep passing at N=2 — that is the regression suite for phase-4 behavior).

`term_search` additions (respecting the existing prefetch-width rules at `:188-193` for all fetches):

1. **Source set** from `in:` filters, resolved in `resolve_filters`' caller: collect `RawFilter { key: "in", .. }` before `resolve_filters` sees them (it errors on unknown keys — route `in:` around the catalog-filter path). Negated `in:` → `anyhow::bail!("in: does not support negation")`. Valid values: `transcript|caption|ocr|pdf|name`; anything else errors listing the valid set.
2. **Text FTS**: unless sources exclude all of transcript/caption/ocr/pdf, call `db.search_text_ranked(terms, text_sources, fts_limit)` → best-per-asset `(AssetId, f64)` plus a `HashMap<AssetId, TextHitMeta>` (`source`, `locator`, `snippet`) for printing.
3. **Semantic text**: unless `in:` restricts to non-transcript sources — load MiniLM (`model_dir_for(&MINILM)` + `TextEncoder::load`), embed the query, `TextVectorStore::open_existing` under `catch_corruption` (mirror `open_semantic_index` at `:324` including its `SemanticMiss` notes — new variants of the miss messages name minilm: "transcript search unavailable — run `maj model fetch --only minilm-l6-v2-v1`"), search, dedupe to best chunk per asset (keep `start_ms` + chunk text for printing).
4. **`in:name`** restricts to the name-FTS list only (no text FTS, no semantic text; image-semantic stays — it matches names' modality? No: image semantic matches *content*. Decision, pinned by test: `in:name` disables text FTS + text semantic AND image semantic — "name" means names).
5. Print: text hits append ` @0m07s "…snippet…"` (transcript/keyframe-ocr with ms locator, reusing `format_ts` at `:435`), ` p3 "…snippet…"` for PDF (locator = page), ` "…snippet…"` for caption/still-OCR (locator -1). Extend `PrintOptions` with `text_meta: &HashMap<AssetId, TextHitMeta>`; JSON output gains `source`, `locator`, `snippet` fields on hits that have them.
6. Coverage notices (stdout, after results — extending the `:650` pattern), computed from real counts:
   - `transcripts: {covered} of {eligible} video/audio assets` (covered = `db.text_assets("transcript").len()` intersected with eligible; eligible from the projection's media kinds)
   - equivalent lines for captions, ocr, pdf — each printed only when `covered < eligible`, each naming its remedy (`maj index run`, `maj describer set`, `maj model fetch --only …`) — reuse the exact remedy strings from `index status` (extract them into consts shared by both call sites so they cannot drift).

- [ ] **Step 4: Write the failing CLI tests** (`crates/cli/tests/search_text_smoke.rs`)

```rust
mod common;
use common::maj;
use predicates::prelude::*;
use predicates::str::contains;

/// Seed: catalog with one wav asset + a hand-written transcript blob, no
/// models anywhere — text FTS must work with zero models fetched.
fn seeded_catalog() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("standup.wav"), b"RIFFfake").expect("write");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    let output = maj(&root, &state).args(["search", "standup"]).output().expect("search");
    let asset = common::first_asset_from(&output);
    let hex = asset.strip_prefix("xxh3:").expect("hex");
    let blobs = majestical_index::blob::BlobStore::new(&root);
    let transcript = majestical_index::transcribe::Transcript {
        model_tag: "whisper-large-v3-turbo-q5-v1".into(),
        segments: vec![majestical_index::transcribe::TranscriptSegment {
            start_ms: 5_000, end_ms: 12_000,
            text: "we walked through the quarterly budget line by line".into(),
        }],
        text: "we walked through the quarterly budget line by line".into(),
    };
    let path = blobs.path_for(hex, &majestical_index::blob::Derivation::Transcript {
        model_tag: "whisper-large-v3-turbo-q5-v1" });
    let json = transcript.to_json().expect("json");
    blobs.write_atomic(&path, &zstd::encode_all(json.as_slice(), 3).expect("zstd")).expect("write");
    (tmp, root, state)
}

#[test]
fn transcript_text_is_searchable_with_timestamp_and_snippet() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    // heal pass projects the blob into text_fts (no models needed for that)
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "run", "--kinds", "transcripts"]).assert().success();
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly budget"])
        .assert().success()
        .stdout(contains("standup.wav"))
        .stdout(contains("@0m05s"))
        .stdout(contains("quarterly budget"));
}

#[test]
fn in_filter_restricts_sources() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "run", "--kinds", "transcripts"]).assert().success();
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:ocr"])
        .assert().success()
        .stdout(contains("standup.wav").not());
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:transcript"])
        .assert().success()
        .stdout(contains("standup.wav"));
}

#[test]
fn hard_filters_intersect_text_results() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["index", "run", "--kinds", "transcripts"]).assert().success();
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly tag:nonexistent"])
        .assert().success()
        .stdout(contains("standup.wav").not())
        .stdout(contains("0 results"));
}

#[test]
fn degradation_names_the_transcript_gap_when_model_missing() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    // No index run: transcript blob exists but text_fts is cold, and no
    // minilm model → both gaps named, search still succeeds.
    maj(&root, &state).env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly budget"])
        .assert().success()
        .stderr(contains("model fetch --only minilm-l6-v2-v1"));
}

#[test]
fn negated_in_errors() {
    let (_tmp, root, state) = seeded_catalog();
    maj(&root, &state).args(["search", "budget -in:ocr"]).assert().failure()
        .stderr(contains("negation"));
}
```

- [ ] **Step 5: Run all of it, gate, commit, open PR 9**

```bash
cargo test -p majestical-cli && just check
git add crates/cli/src/search.rs crates/cli/src/query.rs crates/cli/tests/search_text_smoke.rs crates/cli/tests/common/mod.rs crates/cli/Cargo.toml
git commit -m "feat: N-way fusion with text FTS, transcript vectors, in: filter"
```

Open PR "feat: layered text search" (Tasks 20-21). Before the PR, also run the full gated e2e suite locally (`--ignored` tests across crates with a fetched model + ffmpeg) — this PR touches the fusion path the phase-4 gated tests pin.

---

# PR 10 — Closing: e2e, acceptance, mutants, docs

### Task 22: end-to-end proof points (gated)

**Files:**
- Create: `crates/cli/tests/phase5_e2e.rs`

- [ ] **Step 1: Write the gated e2e tests** (the two spec proof points; idioms from `index_smoke.rs:579`)

```rust
mod common;
use common::maj;
use predicates::str::contains;

/// Spec proof point 1: spoken phrase → semantic transcript search resolves
/// the right asset + timestamp via the MiniLM path (paraphrase, not
/// word match — "spending money" never appears in the audio).
#[test]
#[ignore = "needs fetched whisper+minilm models, ffmpeg and say on PATH"]
fn semantic_transcript_search_resolves_paraphrase_with_timestamp() {
    assert!(majestical_index::video::ffmpeg_available());
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    // ~8s of silence, then the payload sentence — the hit must carry a
    // timestamp inside the speech, not at zero.
    let aiff = tmp.path().join("speech.aiff");
    std::process::Command::new("say").args(["-o"]).arg(&aiff)
        .arg("[[slnc 8000]] we spent the whole meeting reviewing the quarterly budget and costs")
        .status().expect("say");
    let wav = media.join("meeting.wav");
    std::process::Command::new("ffmpeg").args(["-y", "-i"]).arg(&aiff)
        .args(["-ar", "16000", "-ac", "1"]).arg(&wav).status().expect("ffmpeg");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    maj(&root, &state).args(["index", "run", "--kinds", "transcripts"]).assert().success();
    let assert = maj(&root, &state)
        .args(["search", "talking about spending money", "in:transcript"])
        .assert().success()
        .stdout(contains("meeting.wav"));
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    // The timestamp printed must be at/after the 8s silence, not @0m00s.
    assert!(!stdout.contains("@0m00s"), "hit must resolve inside the speech: {stdout}");
}

/// Spec proof point 2: rendered on-screen text found via in:ocr.
#[test]
#[ignore = "needs ffmpeg on PATH"]
fn keyframe_ocr_text_found_via_in_ocr() {
    assert!(majestical_index::video::ffmpeg_available());
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    let clip = media.join("slate.mov");
    std::process::Command::new("ffmpeg")
        .args(["-y", "-f", "lavfi", "-i", "color=white:size=640x360:duration=4:rate=25"])
        .args(["-vf", "drawtext=text='SCENE 42 TAKE 7':fontcolor=black:fontsize=48:x=(w-text_w)/2:y=(h-text_h)/2"])
        .arg(&clip).status().expect("ffmpeg");
    maj(&root, &state).args(["scan"]).arg(&media).args(["--volume", "vol-1"]).assert().success();
    // keyframes need the siglip model; run everything available.
    maj(&root, &state).args(["index", "run"]).assert().success();
    maj(&root, &state)
        .args(["search", "scene 42 in:ocr"])
        .assert().success()
        .stdout(contains("slate.mov"));
}
```

- [ ] **Step 2: Run them for real** (this is the phase's acceptance evidence — capture the output)

```bash
MAJ_MODEL_DIR=.model-cache cargo run -p majestical-cli --bin maj -- --catalog . --machine-id t model fetch
MAJ_MODEL_DIR=.model-cache cargo test -p majestical-cli --test phase5_e2e -- --ignored --nocapture
```

Expected: both pass on a real machine. Fix what they surface — these two tests are the definition of phase-5-works.

- [ ] **Step 3: Commit**

```bash
git add crates/cli/tests/phase5_e2e.rs
git commit -m "test: phase 5 e2e proof points — semantic transcript + OCR search"
```

### Task 23: cucumber acceptance + mutants triage + docs

**Files:**
- Modify: `crates/cli/tests/features/` (new `.feature` file + step defs in the existing `acceptance` harness — read `crates/cli/tests/acceptance.rs` and the existing features first, follow their exact step style)
- Modify: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`
- Modify: `docs/superpowers/specs/2026-07-31-phase5-describers-design.md` (as-built deviations section)
- Create: `docs/superpowers/HANDOFF-phase6.md`

- [ ] **Step 1: Cucumber scenarios for the layered search flows** (`features/text_search.feature`, adapted to the existing Given/When/Then vocabulary)

```gherkin
Feature: Layered text search
  Scenario: Transcript text is searchable after indexing
    Given a catalog with a scanned audio file
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search for "quarterly budget"
    Then the results include the audio file
    And the hit shows a timestamp and a snippet

  Scenario: Source filter restricts where terms match
    Given a catalog with a scanned audio file
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search for "quarterly in:ocr"
    Then the results are empty

  Scenario: Hard filters intersect text hits
    Given a catalog with a scanned audio file
    And a transcript blob containing "quarterly budget review"
    When I run index with kinds "transcripts"
    And I search for "quarterly tag:missing"
    Then the results are empty

  Scenario: Missing models degrade with a named gap
    Given a catalog with a scanned audio file
    When I search for "anything"
    Then the search succeeds
    And the notices name the missing transcript model
```

Implement the step definitions against the real CLI the way the existing acceptance steps do.

- [ ] **Step 2: cargo-mutants triage**

```bash
cargo mutants -p majestical-describe -p majestical-index --in-diff <(git diff main...HEAD) 2>/dev/null || cargo mutants -p majestical-describe
```

(Use the invocation style of the phase-4 triage — see the watchlist's "cargo-mutants triage (phase 4)" section for the format of the recorded results.) Triage every surviving mutant: fix the test gap when cheap, otherwise record it in the watchlist under a new "cargo-mutants triage (phase 5)" section with the same category breakdown format.

- [ ] **Step 3: Watchlist + spec as-built + handoff**

- Watchlist: add "Done in phase 5" (moved items: ffmpeg-timeout *for audio extraction*, media-kind extension table, plus whatever else this phase closed), and a "Phase 5 deferrals" section (reviewer-attributed items accumulated during the PRs, plus the spec's deferred list: hosted embeddings, synced rejections, Keychain, PSD/Sketch, caption/OCR/PDF vectors, diarization).
- Spec: append "As-built deviations" recording every place execution diverged from this plan (there will be several — objc2 signatures, ureq builder names, exact status wording).
- Write `docs/superpowers/HANDOFF-phase6.md` following `HANDOFF-phase5.md`'s structure: state at handoff, architecture deltas, e2e proof points (the two Task 22 tests), backlog pointer, phase 6 recommendation (parent spec build order step 6: sync transport hardening — re-read the parent spec §5 before writing it), process conventions carried forward.

- [ ] **Step 4: Gate everything, commit, open PR 10**

```bash
just ci
just conformance && just encoder-conformance && just text-encoder-conformance && just whisper-conformance   # full oracle sweep
git add crates/cli/tests docs/superpowers
git commit -m "test: phase 5 closing — acceptance, mutants triage, as-built docs"
```

Open PR "test: phase 5 closing" (Tasks 22-23). After merge: phase 5 is done.

---

## Execution notes for the orchestrator

- **Merge order**: PRs 1-6 are independent of each other except: PR 3 needs PR 2 (model registry), PR 4 needs PR 3 (Transcript type). PR 7 needs 2-6. PR 8 needs 1 and 7. PR 9 needs 4 and 7 (and reads types from 2-3). PR 10 needs everything. Parallelize only with worktrees (user mandate #5).
- **Reviewer discipline** (user mandate #1, phase-4 lesson #4): every task gets a fresh implementer subagent, then an adversarial spec-compliance reviewer (probes empirically — runs the tests, mutation-tests claims, checks blob paths on disk), then a code-quality reviewer, inline on the same diff, fix rounds until APPROVED.
- **objc2 tasks (13, 15) carry the highest signature-drift risk** — instruct implementers to open docs.rs for the landed versions before writing the unsafe blocks, and to expect a fix round.
- **Slow gates**: whisper-conformance downloads 574 MB + a CTranslate2 reference on first CI run; text-encoder-conformance ~90 MB + torch. Cold-cache CI runs will be slow — that is expected, not a hang (handoff mandate #10).




