//! End-to-end: layered text search through the real CLI — transcript text
//! reachable via FTS with timestamps and snippets, `in:` source scoping,
//! hard-filter intersection over text hits, and the degradation/coverage
//! notices. No models are fetched anywhere here: text FTS must work from a
//! healed transcript blob alone.
mod common;

use common::{first_asset_id, maj};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::{Path, PathBuf};

/// Writes a hand-written transcript blob (single segment starting at
/// `start_ms`) for `asset_hex`, as if `index run` had transcribed it.
#[cfg(test)]
fn write_transcript_blob(root: &Path, asset_hex: &str, text: &str, start_ms: u64) {
    let blobs = majestical_index::blob::BlobStore::new(root);
    let transcript = majestical_index::transcribe::Transcript {
        model_tag: majestical_index::transcribe::WHISPER_MODEL_TAG.to_string(),
        segments: vec![majestical_index::transcribe::TranscriptSegment {
            start_ms,
            end_ms: start_ms + 7_000,
            text: text.to_string(),
        }],
        text: text.to_string(),
    };
    let json = transcript.to_json().expect("transcript json");
    let compressed = zstd::encode_all(&json[..], 3).expect("zstd");
    let path = blobs.path_for(
        asset_hex,
        &majestical_index::blob::Derivation::Transcript {
            model_tag: majestical_index::transcribe::WHISPER_MODEL_TAG,
        },
    );
    blobs.write_atomic(&path, &compressed).expect("write blob");
}

/// The bare asset hex for the first `search --json` hit on `term`.
#[cfg(test)]
fn asset_hex_for(root: &Path, state: &Path, term: &str) -> String {
    let output = maj(root, state)
        .args(["search", term, "--json"])
        .output()
        .expect("search");
    let asset = first_asset_id(&output);
    asset
        .strip_prefix("xxh3:")
        .expect("asset id is xxh3-prefixed")
        .to_string()
}

/// Seed: a catalog with one scanned wav asset plus a hand-written
/// transcript blob saying "…quarterly budget…" at 5s. Nothing is indexed
/// into `text_fts` yet — tests that need that run
/// `index run --kinds transcripts` themselves (the heal pass needs no
/// models).
#[cfg(test)]
fn seeded_catalog() -> (tempfile::TempDir, PathBuf, PathBuf) {
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
    std::fs::write(media.join("standup.wav"), b"RIFFfake").expect("write wav");
    maj(&root, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();
    let hex = asset_hex_for(&root, &state, "standup");
    write_transcript_blob(
        &root,
        &hex,
        "we walked through the quarterly budget line by line",
        5_000,
    );
    (tmp, root, state)
}

/// Heals the transcript blob into `text_fts` — no models involved.
#[cfg(test)]
fn heal_text_index(root: &Path, state: &Path, empty_models: &Path) {
    maj(root, state)
        .env("MAJ_MODEL_DIR", empty_models)
        .args(["index", "run", "--kinds", "transcripts"])
        .assert()
        .success();
}

#[test]
fn transcript_text_is_searchable_with_timestamp_and_snippet() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    heal_text_index(&root, &state, empty_models.path());
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly budget"])
        .assert()
        .success()
        .stdout(contains("standup.wav"))
        .stdout(contains("@0m05s"))
        .stdout(contains("quarterly budget"));
}

#[test]
fn in_filter_restricts_sources() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    heal_text_index(&root, &state, empty_models.path());
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:ocr"])
        .assert()
        .success()
        .stdout(contains("standup.wav").not())
        .stdout(contains("0 results"));
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:transcript"])
        .assert()
        .success()
        .stdout(contains("standup.wav"));
}

/// Pins the `in:name` decision: "name means names". A term that matches one
/// asset's FILENAME and a different asset's transcript must, under
/// `in:name`, surface only the filename match — text FTS, transcript
/// vectors, and image vectors are all off.
#[test]
fn in_name_means_names_only() {
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
    // FILENAME matches "quarterly"; its (absent) transcript does not.
    std::fs::write(media.join("quarterly_notes.wav"), b"RIFFnotes").expect("write wav");
    // Filename does NOT match; its transcript does.
    std::fs::write(media.join("standup.wav"), b"RIFFfake").expect("write wav");
    maj(&root, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();
    let hex = asset_hex_for(&root, &state, "standup");
    write_transcript_blob(
        &root,
        &hex,
        "we walked through the quarterly budget line by line",
        5_000,
    );
    let empty_models = tempfile::tempdir().expect("tempdir");
    heal_text_index(&root, &state, empty_models.path());

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:name"])
        .assert()
        .success()
        .stdout(contains("quarterly_notes.wav"))
        .stdout(contains("standup.wav").not());
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly in:transcript"])
        .assert()
        .success()
        .stdout(contains("standup.wav"))
        .stdout(contains("quarterly_notes.wav").not());
}

/// THE phase-4 BLOCKER regression, now over text hits: a hard filter must
/// intersect every fused list, so a transcript match outside the filter set
/// yields zero results — never a leaked hit.
#[test]
fn hard_filters_intersect_text_results() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    heal_text_index(&root, &state, empty_models.path());
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly tag:nonexistent"])
        .assert()
        .success()
        .stdout(contains("standup.wav").not())
        .stdout(contains("0 results"));
}

/// With no `MiniLM` fetched, search still succeeds and stderr names the
/// exact fetch command for the transcript-semantic gap.
#[test]
fn degradation_names_the_transcript_gap_when_model_missing() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    // No index run: transcript blob exists but text_fts is cold, and no
    // minilm model — search must degrade, not fail.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "quarterly budget"])
        .assert()
        .success()
        .stderr(contains("model fetch --only minilm-l6-v2-v1"));
}

#[test]
fn negated_in_errors() {
    let (_tmp, root, state) = seeded_catalog();
    maj(&root, &state)
        .args(["search", "budget -in:ocr"])
        .assert()
        .failure()
        .stderr(contains("negation"));
}

/// A transcript-eligible asset with no `text_fts` rows yields a stdout
/// coverage notice naming transcripts, the real counts, and a remedy — here
/// the shared `model fetch` command, since neither whisper nor minilm is
/// installed.
#[test]
fn coverage_notice_names_uncovered_transcripts() {
    let (_tmp, root, state) = seeded_catalog();
    let empty_models = tempfile::tempdir().expect("tempdir");
    // No index run: the blob was never healed into text_fts.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_models.path())
        .args(["search", "standup"])
        .assert()
        .success()
        .stdout(contains("transcripts: 0 of 1 video/audio assets"))
        .stdout(contains("maj model fetch"));
}
