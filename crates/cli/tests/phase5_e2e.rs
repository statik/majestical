//! Phase 5's end-to-end proof points, gated behind real models, `ffmpeg`,
//! and macOS `say` — the thing all the unit/smoke coverage in this phase
//! adds up to. Each test drives the real CLI against real bytes; none of
//! them hand-plant a blob to skip the derivation that matters.
//!
//!     MAJ_MODEL_DIR="$PWD/.model-cache" cargo test -p majestical-cli \
//!         --test phase5_e2e -- --ignored --nocapture
mod common;

use common::{maj, walkdir_find};
use httpmock::prelude::*;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// Renders `text` centered on a white 640x360 PNG via `ImageMagick`. This
/// machine's ffmpeg has no `drawtext` support (built without freetype), the
/// same gap PR 5 hit generating `crates/index/tests/fixtures/ocr-hello.png`
/// — `ImageMagick` is the workaround both times.
#[cfg(test)]
fn render_text_png(path: &std::path::Path, text: &str) {
    let status = std::process::Command::new("magick")
        .args(["-size", "640x360", "xc:white"])
        .args(["-font", "/System/Library/Fonts/Supplemental/Arial.ttf"])
        .args(["-pointsize", "48", "-fill", "black", "-gravity", "center"])
        .args(["-annotate", "0", text])
        .arg(path)
        .status()
        .expect("running ImageMagick to render the OCR fixture");
    assert!(status.success(), "magick failed to render {text}");
}

/// Builds a 4s, yuv420p .mov clip from a still PNG — enough duration for
/// the scene detector's uniform-sampling fallback (no cuts in a static
/// frame) to still emit at least one keyframe.
#[cfg(test)]
fn build_clip_from_png(png: &std::path::Path, out: &std::path::Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-loop", "1", "-i"])
        .arg(png)
        .args(["-t", "4", "-pix_fmt", "yuv420p"])
        .arg(out)
        .status()
        .expect("running ffmpeg to build the clip");
    assert!(status.success(), "ffmpeg failed to build clip from {png:?}");
}

/// Proof point 1: a spoken phrase is found by a PARAPHRASE — the query
/// shares no vocabulary with the transcript, so only the `MiniLM` vector
/// path can produce the hit.
#[test]
#[ignore = "needs fetched whisper+minilm models, ffmpeg and say on PATH"]
fn semantic_transcript_search_resolves_paraphrase_with_timestamp() {
    assert!(majestical_index::video::ffmpeg_available());
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");

    // Speech-then-payload, not silence-then-payload: whisper folds leading
    // silence into segment 1, so a silence-led fixture still reports
    // `@0m00s` for the payload. A short speech-then-payload fixture isn't
    // enough either — verified empirically, chunking (crates/index/src/
    // chunk.rs) never splits a whisper segment, but it DOES merge adjacent
    // segments into one chunk (keyed on the first segment's start_ms) as
    // long as the merged span stays under `MAX_CHUNK_MS` (45s)/
    // `MAX_CHUNK_WORDS` (120) — so a two-segment, ~8s fixture merges into a
    // single chunk starting at 0ms even though whisper itself splits the
    // filler and payload into separate segments. ~47s of filler (6x a
    // 21-word sentence, empirically: whisper folds the repeats into 4
    // segments totaling 84 words/40900ms) pushes the running chunk just
    // past both caps once the payload segment is added, forcing a new
    // chunk that starts at the payload's own segment boundary
    // (empirically: @0m40s) instead of at 0.
    let filler = "The weather in the valley was calm and clear this week, \
        with light winds moving steadily from the west every afternoon. "
        .repeat(6);
    let aiff = tmp.path().join("speech.aiff");
    let status = std::process::Command::new("say")
        .arg("-o")
        .arg(&aiff)
        .arg(format!(
            "{filler}We spent the entire meeting reviewing the quarterly \
             budget and the cost overruns from last quarter."
        ))
        .status()
        .expect("say");
    assert!(status.success(), "say failed to synthesize the fixture");

    let wav = media.join("meeting.wav");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&aiff)
        .args(["-ar", "16000", "-ac", "1"])
        .arg(&wav)
        .status()
        .expect("ffmpeg");
    assert!(
        status.success(),
        "ffmpeg failed to convert the aiff fixture"
    );

    maj(&root, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    // Two passes: `index run --kinds transcripts` spans both the Transcribe
    // and TranscriptEmbed work kinds, but the run's plan is built once up
    // front — a transcript produced mid-run isn't re-planned for embedding
    // in the same pass. Pass 1 transcribes; pass 2 embeds the chunks.
    maj(&root, &state)
        .args(["index", "run", "--kinds", "transcripts"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["index", "run", "--kinds", "transcripts"])
        .assert()
        .success();

    // The query deliberately shares no vocabulary with the transcript
    // ("spending money" vs. "quarterly budget and cost overruns") — a bare
    // FTS/word match cannot produce this hit, only the MiniLM vector path.
    let assert = maj(&root, &state)
        .args(["search", "talking about spending money in:transcript"])
        .assert()
        .success()
        .stdout(contains("meeting.wav"));
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("quarterly budget") || stdout.contains("cost overruns"),
        "hit snippet must show payload text: {stdout}"
    );
    assert!(
        !stdout.contains("@0m00s"),
        "hit must resolve inside the speech, not at the very start: {stdout}"
    );

    // Control: point MAJ_MODEL_DIR at an empty dir (MiniLM absent, so the
    // semantic path is unavailable) and rerun the identical search. If FTS
    // alone could produce this hit, it would still appear here — it must
    // not, proving the paraphrase match above is load-bearing on the
    // semantic path, not an accidental word overlap.
    let empty_models = tempfile::tempdir().expect("tempdir");
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "talking about spending money in:transcript"])
        .assert()
        .success()
        .stdout(contains("0 results"))
        .stdout(contains("meeting.wav").not());
}

/// Proof point 2: on-screen text rendered into a clip is found via
/// `in:ocr`, through real scene detection and Vision OCR — no hand-planted
/// manifests or blobs.
#[test]
#[ignore = "needs ffmpeg on PATH"]
fn keyframe_ocr_text_found_via_in_ocr() {
    assert!(majestical_index::video::ffmpeg_available());
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");

    let png = tmp.path().join("frame.png");
    render_text_png(&png, "SCENE 42 TAKE 7");
    let clip = media.join("slate.mov");
    build_clip_from_png(&png, &clip);

    maj(&root, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    // Pass 1: keyframes (needs the siglip model — from `.model-cache`).
    // OCR-on-keyframes is only planned once a `KeyframeManifest` blob
    // exists on disk, and the run's plan is built once up front, so a
    // manifest produced mid-run isn't picked up in the same pass.
    maj(&root, &state)
        .args(["index", "run", "--kinds", "keyframes"])
        .assert()
        .success();
    // Pass 2: OCR the extracted keyframes via Vision.
    maj(&root, &state)
        .args(["index", "run", "--kinds", "ocr"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["search", "scene 42 in:ocr"])
        .assert()
        .success()
        .stdout(contains("slate.mov"));
}

/// Deferred from PR 8: the video caption path end to end — real scene
/// detection and real keyframe extraction/re-extraction (both via ffmpeg),
/// captioned through a mock describer backend (no real local LLM backend
/// is guaranteed on a test machine). PR 8's own coverage only exercised
/// this against a hand-planted `KeyframeManifest`/`Captions` blob; this is
/// the real-keyframes closure of that gap.
#[test]
#[ignore = "needs fetched siglip model and ffmpeg on PATH"]
fn video_captions_describe_real_keyframes() {
    assert!(majestical_index::video::ffmpeg_available());
    let server = MockServer::start();
    let caption_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Describe this image");
        then.status(200).json_body(serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "a red clapperboard"}}]
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Suggest tags");
        then.status(200).json_body(serde_json::json!({
            "choices": [{"message": {"role": "assistant",
                "content": "{\"tags\":[{\"tag\":\"scene/slate\",\"confidence\":0.9}]}"}}]
        }));
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&root).expect("mkdir");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = root.join("media");
    std::fs::create_dir_all(&media).expect("mkdir");

    let png = tmp.path().join("frame.png");
    render_text_png(&png, "SCENE 42 TAKE 7");
    let clip = media.join("slate.mov");
    build_clip_from_png(&png, &clip);

    maj(&root, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();
    maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "mock",
            "--base-url",
        ])
        .arg(server.base_url())
        .assert()
        .success();

    // Real keyframes: the siglip model must be present for the manifest to
    // be planned/written at all.
    maj(&root, &state)
        .args(["index", "run", "--kinds", "keyframes"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 1 written"));

    let captions_blobs = walkdir_find(&root, "captions.json.zst");
    assert_eq!(
        captions_blobs.len(),
        1,
        "exactly one video Captions blob under the sync root's blob store"
    );
    let bytes = std::fs::read(&captions_blobs[0]).expect("read captions blob");
    let json = zstd::decode_all(&bytes[..]).expect("decode captions blob");
    let value: serde_json::Value = serde_json::from_slice(&json).expect("parse captions blob");
    let described = value["described"]
        .as_array()
        .expect("described is an array");
    assert!(
        !described.is_empty(),
        "real keyframe extraction must yield at least one described row: {value}"
    );
    for row in described {
        let text = row[1].as_str().expect("described row text");
        assert_eq!(text, "a red clapperboard");
    }

    // Proves the mock backend received at least one real caption call —
    // the keyframes were genuinely re-extracted and sent, not read from a
    // stub.
    assert!(
        caption_mock.calls() >= 1,
        "mock describer backend must have received at least one caption call"
    );
}
