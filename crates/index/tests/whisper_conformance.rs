#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! Conformance against the pinned Python `faster-whisper` reference (the
//! oracle) for whisper large-v3-turbo. Needs a golden JSON file produced by
//! `conformance/whisper/golden.py`, the fixture WAV it transcribed, plus the
//! fetched ggml model:
//!
//!   `MAJ_AUDIO`=/path/to/fixture.wav `MAJ_GOLDEN`=/path/to/golden.json \
//!     `MAJ_MODEL_DIR`=… cargo test -p majestical-index --test `whisper_conformance` -- --ignored
//!
//! `just whisper-conformance` drives the whole pipeline (fetch, `say` +
//! ffmpeg fixture synthesis, golden.py, this test) in one shot.
//!
//! Compared on two axes rather than exact text match: word error rate (WER)
//! tolerates the reference and whisper.cpp choosing different but equally
//! valid renderings of the same speech (punctuation, casing, "quarter"
//! vs "quarterly"), and boundary drift (first segment's start, last
//! segment's end) catches the two implementations disagreeing about where
//! speech starts/ends without requiring every segment split at the same
//! words.
//!
//! Both boundaries are asserted, not just one: the fixture leads with ~2s of
//! silence (see the justfile recipe), but both engines absorb that silence
//! into the first segment rather than reporting a nonzero start — so the
//! first-boundary assert only catches gross start disagreement (e.g. an
//! engine trimming or offsetting leading audio), not a uniform timestamp
//! scale bug (a dropped x10 unit conversion still lands at 0ms either way).
//! The last-segment-end assert is what actually defends against that class
//! of bug: the fixture runs long enough (~9.5s) that a x10 scale error
//! can't land within tolerance by chance.

use majestical_index::model::{self, WHISPER};
use majestical_index::transcribe::Transcriber;
use majestical_index::video;

const MAX_WER: f64 = 0.15;
const MAX_BOUNDARY_DRIFT_MS: i64 = 1_500;

fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "WER over small word counts (dozens of words, not billions)"
)]
fn word_error_rate(reference: &[String], hypothesis: &[String]) -> f64 {
    // Levenshtein distance over words.
    let rows = reference.len() + 1;
    let cols = hypothesis.len() + 1;
    let mut distance = vec![0_usize; rows * cols];
    for r in 0..rows {
        distance[r * cols] = r;
    }
    for (c, slot) in distance.iter_mut().enumerate().take(cols) {
        *slot = c;
    }
    for r in 1..rows {
        for c in 1..cols {
            let substitution = distance[(r - 1) * cols + (c - 1)]
                + usize::from(reference[r - 1] != hypothesis[c - 1]);
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
    let reference_segments = golden["segments"].as_array().expect("segments");
    assert!(
        !reference_segments.is_empty(),
        "reference produced no segments — vacuous gate"
    );
    let reference_text: String = reference_segments
        .iter()
        .map(|s| s["text"].as_str().expect("text"))
        .collect::<Vec<_>>()
        .join(" ");
    let dir = model::model_dir_for(&WHISPER).expect("dir");
    let pcm = video::extract_audio_pcm(std::path::Path::new(&audio), 120_000).expect("pcm");
    let transcript = Transcriber::load(&dir)
        .expect("load")
        .transcribe(&pcm)
        .expect("transcribe");
    assert!(
        !transcript.segments.is_empty(),
        "our transcription produced no segments"
    );

    let reference = normalize(&reference_text);
    let hypothesis = normalize(&transcript.text);
    let wer = word_error_rate(&reference, &hypothesis);
    // Sibling gates only assert against floors; this one also prints the measured values.
    report_measurement(&format!("WER {wer:.4} (max {MAX_WER})"));
    assert!(
        wer <= MAX_WER,
        "WER {wer:.3} exceeds {MAX_WER} — ref: {reference_text:?} got: {:?}",
        transcript.text
    );

    let reference_first = reference_segments[0]["start_ms"].as_i64().expect("start");
    let ours_first = i64::try_from(transcript.segments[0].start_ms).expect("fits");
    let drift = (reference_first - ours_first).abs();
    report_measurement(&format!(
        "first-boundary drift {drift}ms (max {MAX_BOUNDARY_DRIFT_MS}ms)"
    ));
    assert!(
        drift <= MAX_BOUNDARY_DRIFT_MS,
        "first-segment drift {reference_first} vs {ours_first}"
    );

    let reference_last = reference_segments.last().expect("at least one segment")["end_ms"]
        .as_i64()
        .expect("end");
    let ours_last = i64::try_from(
        transcript
            .segments
            .last()
            .expect("at least one segment")
            .end_ms,
    )
    .expect("fits");
    let end_drift = (reference_last - ours_last).abs();
    report_measurement(&format!(
        "last-boundary drift {end_drift}ms (max {MAX_BOUNDARY_DRIFT_MS}ms)"
    ));
    assert!(
        end_drift <= MAX_BOUNDARY_DRIFT_MS,
        "last-segment drift {reference_last} vs {ours_last}"
    );
}

#[expect(
    clippy::print_stdout,
    reason = "conformance gate: measured WER/drift are part of the run's evidence, \
              printed only under --ignored on an explicit conformance invocation"
)]
fn report_measurement(message: &str) {
    println!("whisper conformance: {message}");
}
