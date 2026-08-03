//! Model-capability checks and their remedy text, shared by `index status`
//! and `search`'s coverage notices so the two surfaces can't drift on either
//! the "installed" definition or the command that closes a gap. Moved
//! verbatim from `crates/cli/src/index_cmd.rs`.
use majestical_index::model::{MINILM, WHISPER};
use std::path::PathBuf;

/// The whisper cache dir, only if `model_present_for` accepts it (every
/// registry file present at its exact byte size) — the single "installed"
/// definition, not a re-hash, so this stays cheap on every invocation.
#[must_use]
pub fn whisper_model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir_for(&WHISPER).ok()?;
    majestical_index::model::model_present_for(&WHISPER, &dir).then_some(dir)
}

/// The `MiniLM` cache dir, only if `model_present_for` accepts it.
#[must_use]
pub fn minilm_model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir_for(&MINILM).ok()?;
    majestical_index::model::model_present_for(&MINILM, &dir).then_some(dir)
}

/// The captions remedy line, shared verbatim by `index status` and
/// `search`'s coverage notices so the two surfaces cannot drift.
pub const DESCRIBER_REMEDY: &str = "run `maj describer set` to configure a backend";

/// The `model fetch` remedy for the transcript pipeline, naming exactly the
/// missing models — `None` when both are installed. Shared by `index
/// status` and `search`'s coverage notices so the command they print is
/// always the same one.
#[must_use]
pub fn transcript_model_remedy(whisper: bool, text_model: bool) -> Option<String> {
    let mut fetches = Vec::new();
    if !whisper {
        fetches.push(format!("--only {}", WHISPER.tag));
    }
    if !text_model {
        fetches.push(format!("--only {}", MINILM.tag));
    }
    if fetches.is_empty() {
        None
    } else {
        Some(format!("run `maj model fetch {}`", fetches.join(" ")))
    }
}
