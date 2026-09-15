//! Shared helpers for the whisper gates. `mod common;` in each test file.
#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

/// True when every sample is (numerically) zero: the shape a silent `say`
/// fixture decodes to. Both whisper gates refuse such a fixture up front
/// rather than letting two models agree on a hallucination.
pub fn is_silent(pcm: &[f32]) -> bool {
    pcm.iter().all(|sample| sample.abs() < 1e-6)
}

pub const SILENT_FIXTURE_MSG: &str = "fixture is silent — regenerate with `just whisper-fixture`";
