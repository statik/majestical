//! End-to-end: caption + tag-suggestion derivation through `maj index run`
//! against a mock OpenAI-compatible backend — the describer wiring proof,
//! since no real local LLM backend is guaranteed on a test machine.
mod common;

use common::{maj, walkdir_find};
use httpmock::prelude::*;
use predicates::str::contains;

#[cfg(test)]
fn write_png(path: &std::path::Path, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(64, 64, image::Rgb(rgb))
        .save(path)
        .expect("write a real PNG for the thumbnailer to decode");
}

#[cfg(test)]
fn write_red_png(path: &std::path::Path) {
    write_png(path, [255, 0, 0]);
}

#[cfg(test)]
fn caption_response() -> serde_json::Value {
    serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "a red square"}}]})
}

#[cfg(test)]
fn tags_response() -> serde_json::Value {
    serde_json::json!({"choices": [{"message": {"role": "assistant",
        "content": "{\"tags\":[{\"tag\":\"color/red\",\"confidence\":0.95}]}"}}]})
}

/// Two mocks on one server, distinguished by prompt text in the request
/// body: the caption prompt starts "Describe this image", the tag prompt
/// starts "Suggest tags".
#[cfg(test)]
fn mock_describer_backend(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Describe this image");
        then.status(200).json_body(caption_response());
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Suggest tags");
        then.status(200).json_body(tags_response());
    });
}

#[cfg(test)]
fn configure_describer(root: &std::path::Path, state: &std::path::Path, base_url: &str) {
    maj(root, state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "mock-model",
            "--base-url",
            base_url,
        ])
        .assert()
        .success();
}

/// A still image flows caption-end-to-end: `index run --kinds captions`
/// reads the thumbnail blob, captions + tag-suggests it via the configured
/// (mock) backend, writes both blobs under the describer's model tag, and a
/// second run is a no-op with status reporting the kind done.
#[test]
fn index_run_captions_via_mock_backend() {
    let server = MockServer::start();
    mock_describer_backend(&server);
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    // Empty model cache: no encoder/whisper work interferes with this test.
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();
    configure_describer(&root, &state, &server.base_url());

    // Captions ride on thumbnails: the runner reads the thumb blob, never
    // re-decodes the source.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "thumbs"])
        .assert()
        .success();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 1 written"));

    assert_eq!(
        walkdir_find(&root, "caption.json.zst").len(),
        1,
        "exactly one caption blob under the sync root's blob store"
    );
    assert_eq!(
        walkdir_find(&root, "tags.json.zst").len(),
        1,
        "exactly one tag-suggestions blob under the sync root's blob store"
    );

    // Idempotent: the caption blob is the done marker, so a second run
    // re-plans nothing.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 0 written"));

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions: 1 done"));
}

/// With no describer configured, `index status` names the exact remedy for
/// the captions gap rather than pretending the work is merely pending.
#[test]
fn caption_status_without_describer_names_the_remedy() {
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions"))
        .stdout(contains("maj describer set"));
}

/// A backend outage mid-run is a run-level success: the first failing item
/// is recorded, the remaining caption items are skipped (not hammered
/// against a dead backend), and every item re-plans next run because no
/// done-blob was written.
#[test]
fn caption_backend_outage_mid_run_skips_remaining_and_reports() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(500);
    });
    let media = tempfile::tempdir().unwrap();
    // Different pixel colors: identical bytes would dedupe to one asset.
    write_png(&media.path().join("one.png"), [255, 0, 0]);
    write_png(&media.path().join("two.png"), [0, 0, 255]);
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();
    configure_describer(&root, &state, &server.base_url());

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "thumbs"])
        .assert()
        .success();

    // One real backend failure plus one abandoned item — the skipped-reason
    // text only ever appears when the abort-after-first-failure path runs,
    // so a mutant that keeps hammering the dead backend (recording two real
    // errors instead) fails these pins.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 0 written, 2 failed"))
        .stderr(contains("500"))
        .stderr(contains("skipped after first failure"));

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions failed last run: 2"))
        .stdout(contains("captions: 0 done, 2 pending"));
}

/// A partial item — caption blob written, tags call failed — must re-plan
/// (done requires BOTH blobs) and, on retry, skip the already-completed
/// caption half: the caption endpoint is hit exactly once across both runs.
#[test]
fn caption_written_but_tags_failed_replans_and_completes_without_recaption() {
    let server = MockServer::start();
    let caption_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Describe this image");
        then.status(200).json_body(caption_response());
    });
    let mut tags_down = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Suggest tags");
        then.status(500);
    });
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();
    configure_describer(&root, &state, &server.base_url());
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "thumbs"])
        .assert()
        .success();

    // Run 1: caption succeeds, tags fails — the item fails but the caption
    // blob (the completed half) lands on disk.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 0 written, 1 failed"));
    assert_eq!(walkdir_find(&root, "caption.json.zst").len(), 1);
    assert_eq!(
        walkdir_find(&root, "tags.json.zst").len(),
        0,
        "no tags blob after the failed tags call"
    );

    // The half-finished item must count pending, not done.
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions: 0 done, 1 pending"));

    // Backend recovers for tags; run 2 completes the item.
    tags_down.delete();
    server.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .body_includes("Suggest tags");
        then.status(200).json_body(tags_response());
    });

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 1 written"));

    assert_eq!(walkdir_find(&root, "caption.json.zst").len(), 1);
    assert_eq!(walkdir_find(&root, "tags.json.zst").len(), 1);
    caption_mock.assert_calls(1);

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions: 1 done"));
}

/// A garbage `describer.toml` must degrade, not kill indexing: `index
/// status` still succeeds, notes the unparsable config on stderr, and
/// counts captions as needing a model (unconfigured).
#[test]
fn broken_describer_toml_degrades_to_needs_model_with_note() {
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();
    configure_describer(&root, &state, "http://127.0.0.1:1");

    let toml_paths = walkdir_find(&state, "describer.toml");
    assert_eq!(toml_paths.len(), 1, "exactly one describer config");
    std::fs::write(&toml_paths[0], "not valid toml [[[").unwrap();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains(
            "captions: 0 done, 0 pending, 0 offline, 0 unsupported, 0 need ffmpeg, 1 need model",
        ))
        .stderr(contains("describer config"));
}

/// The same pass that writes a caption blob heals it into `text_fts` under
/// the "caption" source, so captions are FTS-searchable immediately.
#[test]
fn captions_are_healed_into_text_fts() {
    let server = MockServer::start();
    mock_describer_backend(&server);
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();
    configure_describer(&root, &state, &server.base_url());

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "thumbs"])
        .assert()
        .success();
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success();

    let db_paths = walkdir_find(&state, "catalog.db");
    assert_eq!(db_paths.len(), 1, "exactly one state-dir sqlite catalog");
    let db = majestical_catalog_sqlite::SqliteCatalog::open(&db_paths[0]).unwrap();
    assert_eq!(
        db.text_assets("caption").unwrap().len(),
        1,
        "text_fts must have a caption row for the captioned asset"
    );
}

/// The video-captions heal path: a hand-written `captions.json.zst` blob
/// (as if produced on a teammate's machine with ffmpeg + a backend) gets
/// its per-keyframe rows healed into `text_fts` by any later pass — the
/// caption heal is blob-driven, same as every other text source.
#[test]
fn video_captions_blob_heals_per_keyframe_rows() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_dir = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    let hex = "beadfeedbeadfeedbeadfeedbeadfeed";
    let blobs = majestical_index::blob::BlobStore::new(&root);
    let json = serde_json::json!({
        "model_tag": "describe-mock-model",
        "detected_keyframes": 2,
        "described": [[1500, "a red barn"], [4500, "a green field"]],
    });
    let compressed = zstd::encode_all(json.to_string().as_bytes(), 3).unwrap();
    let path = blobs.path_for(
        hex,
        &majestical_index::blob::Derivation::Captions {
            model_tag: "describe-mock-model",
        },
    );
    blobs.write_atomic(&path, &compressed).unwrap();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run"])
        .assert()
        .success();

    let db_paths = walkdir_find(&state, "catalog.db");
    assert_eq!(db_paths.len(), 1, "exactly one state-dir sqlite catalog");
    let db = majestical_catalog_sqlite::SqliteCatalog::open(&db_paths[0]).unwrap();
    let covered = db.text_assets("caption").unwrap();
    assert!(
        covered.contains(&majestical_core::event::AssetId(format!("xxh3:{hex}"))),
        "video caption heal must populate text_fts: {covered:?}"
    );
}
