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

/// Recorded for every caption item in a pass when `OpenRouter` is configured
/// but no key is available from the config file or the environment —
/// before any request is made. Transient: operator-fixable, not the item's
/// fault, so the ledger never remembers it.
///
/// It names [`majestical_describe::config::OPENROUTER_KEY_ENV`] in prose and
/// so cannot be built from it at const time; `the_no_key_reason_names_the_key_env_var`
/// keeps the two from drifting.
pub const OPENROUTER_KEY_MISSING_REASON: &str = "OpenRouter needs an API key — save one in \
    Settings → Captions, or set it with `maj describer set --api-key` or MAJ_OPENROUTER_KEY";

/// Recorded when `OpenRouter` answers 401. Transient: the operator replaces
/// the key and the same items succeed, so the ledger never remembers it.
pub const OPENROUTER_KEY_REJECTED_REASON: &str = "OpenRouter rejected the API key (HTTP 401) — \
    save a new one in Settings → Captions or with `maj describer set --api-key`";

/// Recorded when `OpenRouter` answers 402. Transient for the same reason.
pub const OPENROUTER_OUT_OF_CREDIT_REASON: &str = "OpenRouter reports the account is out of \
    credit (HTTP 402) — add credit at openrouter.ai and captions resume on their own";

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

#[cfg(test)]
mod tests {
    use super::OPENROUTER_KEY_MISSING_REASON;

    /// The reason text names the environment variable in prose, so it cannot
    /// be built from [`majestical_describe::config::OPENROUTER_KEY_ENV`] at
    /// const time. This is what keeps the two from drifting apart.
    #[test]
    fn the_no_key_reason_names_the_key_env_var() {
        assert!(
            OPENROUTER_KEY_MISSING_REASON.contains(majestical_describe::config::OPENROUTER_KEY_ENV),
            "{OPENROUTER_KEY_MISSING_REASON}"
        );
    }
}
