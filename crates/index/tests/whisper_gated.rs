#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

use std::path::Path;

use majestical_index::model::{self, WHISPER};
use majestical_index::transcribe::Transcriber;
use majestical_index::video;

/// Synthesizes an aiff fixture via macOS `say`.
fn say_fixture(path: &Path) {
    let status = std::process::Command::new("say")
        .arg("-o")
        .arg(path)
        .arg("The quick brown fox jumps over the lazy dog")
        .status()
        .expect("say");
    assert!(status.success());
}

/// `say` has, on rare CI runs, produced an aiff whose decoded PCM is all
/// zeros (a flake observed in CI, not reproduced locally) — this is how the
/// fallback path detects and retries it.
fn is_silent(pcm: &[f32]) -> bool {
    pcm.iter().all(|sample| sample.abs() < 1e-6)
}

#[test]
#[ignore = "needs fetched whisper model and ffmpeg + say on PATH"]
fn transcribes_spoken_fixture_with_sane_timestamps() {
    let dir = model::model_dir_for(&WHISPER).expect("dir");
    assert!(
        dir.join(majestical_index::transcribe::MODEL_FILE).is_file(),
        "run `maj model fetch --only whisper-large-v3-turbo-q5-v1` first"
    );

    // Prefer the recipe-generated fixture (`just whisper-conformance` exports
    // MAJ_AUDIO for this and whisper_conformance in the same cargo test
    // invocation) over synthesizing our own — one fewer `say` call, and reuse
    // dodges the flake below entirely. Fall back to say+extract for a
    // standalone run of just this test.
    let pcm = if let Ok(audio) = std::env::var("MAJ_AUDIO") {
        // 120_000ms cap (vs. the fallback's 10_000): the recipe fixture runs
        // ~11.5s including its 2s silent lead-in, which would be truncated
        // by the fallback's shorter cap.
        let pcm = video::extract_audio_pcm(Path::new(&audio), 120_000).expect("pcm");
        // No retry here, unlike the fallback below — this is someone else's
        // committed/generated file, not one we can resynthesize in place.
        assert!(
            !is_silent(&pcm),
            "MAJ_AUDIO fixture is silent — regenerate target/whisper-fixture.wav"
        );
        pcm
    } else {
        let tmp = tempfile::tempdir().expect("tempdir");
        let aiff = tmp.path().join("fixture.aiff");
        say_fixture(&aiff);
        let mut pcm = video::extract_audio_pcm(&aiff, 10_000).expect("pcm");
        if is_silent(&pcm) {
            say_fixture(&aiff);
            pcm = video::extract_audio_pcm(&aiff, 10_000).expect("pcm");
            assert!(!is_silent(&pcm), "say produced silent audio twice");
        }
        pcm
    };

    let transcriber = Transcriber::load(&dir).expect("load");
    let transcript = transcriber.transcribe(&pcm).expect("transcribe");
    let lower = transcript.text.to_lowercase();
    // Matches both paths: our own say fixture, and the recipe's fixture
    // (`just whisper-conformance`'s `say` text starts with the same sentence).
    assert!(lower.contains("quick brown fox"), "got: {lower}");
    assert!(!transcript.segments.is_empty());
    assert!(transcript.segments[0].end_ms > transcript.segments[0].start_ms);
}
