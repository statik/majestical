//! End-to-end: caption + tag-suggestion derivation through `maj index run`
//! against a mock OpenAI-compatible backend — the describer wiring proof,
//! since no real local LLM backend is guaranteed on a test machine.
mod common;

use common::{first_asset_id, maj, walkdir_find};
use httpmock::prelude::*;
use predicates::prelude::PredicateBooleanExt;
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

/// Fake-size model files at the exact byte sizes `model_present_for()` checks —
/// same precedent as `index_smoke.rs`. Opens the planner's vision-model gate
/// (the keyframe-manifest path needs `caps.model_tag`) without a real,
/// loadable model; nothing in these tests may reach `Encoder::load`.
#[cfg(test)]
fn seed_fake_model_files(model_root: &std::path::Path) {
    let model_dir = model_root.join(majestical_index::model::MODEL_TAG);
    std::fs::create_dir_all(&model_dir).unwrap();
    for file in majestical_index::model::MODEL_FILES {
        let f = std::fs::File::create(model_dir.join(file.name)).unwrap();
        f.set_len(file.bytes).unwrap();
    }
}

/// A corrupt existing captions blob must not wedge the video item forever:
/// the runner notes it, treats it as absent, re-describes, and overwrites
/// it — a zero-keyframe manifest keeps the re-describe path off ffmpeg, so
/// this runs on any machine.
#[test]
fn corrupt_video_captions_blob_recovers_by_redescribing() {
    let server = MockServer::start();
    mock_describer_backend(&server);
    let media = tempfile::tempdir().unwrap();
    let mov_path = media.path().join("clip.mov");
    // Never decoded: the caption path reads only the manifest and blobs.
    std::fs::write(&mov_path, b"not a real mov").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_root = tempfile::tempdir().unwrap();
    seed_fake_model_files(model_root.path());

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

    let bytes = std::fs::read(&mov_path).unwrap();
    let hex = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes));
    let blobs = majestical_index::blob::BlobStore::new(&root);
    let manifest_path = blobs.path_for(
        &hex,
        &majestical_index::blob::Derivation::KeyframeManifest {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    blobs
        .write_atomic(
            &manifest_path,
            br#"{"model_tag":"m1","detected":0,"timestamps":[]}"#,
        )
        .unwrap();
    let captions_path = blobs.path_for(
        &hex,
        &majestical_index::blob::Derivation::Captions {
            model_tag: "describe-mock-model",
        },
    );
    blobs
        .write_atomic(&captions_path, b"GARBAGE-NOT-ZSTD-JSON")
        .unwrap();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args(["index", "run", "--kinds", "captions"])
        .assert()
        .success()
        .stdout(contains("captions: 1 written"))
        .stderr(contains("re-describing"));

    // Rewritten in place: the blob decodes again (zero described rows —
    // the manifest listed no keyframes).
    let rewritten = std::fs::read(&captions_path).unwrap();
    let json = zstd::decode_all(&rewritten[..]).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["described"].as_array().unwrap().len(), 0);

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("captions: 1 done"));
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

/// End-to-end suggestion review: a hand-planted `tags.json.zst` blob (unit
/// isolation from the describer path, which the tests above already cover)
/// flows through `tags suggestions` -> `tags confirm` -> `search tag:` (a
/// confirmed suggestion is a plain `TagAdd`, indistinguishable from `maj tag
/// add`) and separately through `tags suggestions` -> `tags reject`, whose
/// rejection is recorded in the per-machine state dir rather than the
/// synced catalog.
#[test]
fn suggestions_list_confirm_reject_flow() {
    let media = tempfile::tempdir().unwrap();
    write_red_png(&media.path().join("red.png"));
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();

    let output = maj(&root, &state)
        .args(["search", "red", "--json"])
        .output()
        .unwrap();
    let asset = first_asset_id(&output);
    let hex = asset.strip_prefix("xxh3:").unwrap().to_string();

    let blobs = majestical_index::blob::BlobStore::new(&root);
    let tags_path = blobs.path_for(
        &hex,
        &majestical_index::blob::Derivation::Tags {
            model_tag: "describe-m",
        },
    );
    let suggestions = vec![majestical_core::ports::TagSuggestion {
        tag: "color/red".to_string(),
        confidence: 0.95,
        in_vocab: false,
        model_tag: "describe-m".to_string(),
    }];
    let json = serde_json::to_vec(&suggestions).unwrap();
    let compressed = zstd::encode_all(json.as_slice(), 3).unwrap();
    blobs.write_atomic(&tags_path, &compressed).unwrap();

    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success()
        .stdout(
            contains(asset.as_str())
                .and(contains("color/red"))
                .and(contains("0.95"))
                .and(contains("describe-m")),
        );

    maj(&root, &state)
        .args(["tags", "confirm", &asset, "color/red"])
        .assert()
        .success();

    // Confirm == a plain TagAdd: `search tag:` must find it exactly as it
    // would a hand-added tag.
    maj(&root, &state)
        .args(["search", "tag:color/red"])
        .assert()
        .success()
        .stdout(contains("red.png"));

    // Already tagged: no longer pending.
    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success()
        .stdout(contains("color/red").not());

    // A second suggestion, rejected instead of confirmed.
    let more = vec![majestical_core::ports::TagSuggestion {
        tag: "shape/square".to_string(),
        confidence: 0.5,
        in_vocab: false,
        model_tag: "describe-m".to_string(),
    }];
    let json = serde_json::to_vec(&more).unwrap();
    let compressed = zstd::encode_all(json.as_slice(), 3).unwrap();
    blobs.write_atomic(&tags_path, &compressed).unwrap();

    maj(&root, &state)
        .args(["tags", "reject", &asset, "shape/square"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success()
        .stdout(contains("shape/square").not());

    // Rejections are per-machine state, not a catalog artifact: the log
    // lives under the state dir and must survive a projection rebuild
    // (nothing about `tags suggestions`/`tags reject` above touched the
    // sqlite projection directly, so its mere presence here already proves
    // that; this assertion pins the file's existence and location).
    assert_eq!(
        walkdir_find(&state, "tag-rejections.jsonl").len(),
        1,
        "exactly one per-machine rejections log"
    );
}

/// A fresh catalog with no suggestion blobs at all: `tags suggestions`
/// succeeds (an empty pending list is not an error) and names the exact
/// remedy, mirroring `caption_status_without_describer_names_the_remedy`.
#[test]
fn suggestions_empty_state_prints_guidance() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success()
        .stdout(contains("maj index run"));
}

/// Confirming a tag on an asset that was never scanned must fail loudly
/// with the same remedy `maj tag add` gives — a typo'd asset id must never
/// silently create a phantom catalog entry.
#[test]
fn confirm_unknown_asset_errors_actionably() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tags", "confirm", "xxh3:doesnotexist", "some/tag"])
        .assert()
        .failure()
        .stderr(contains("unknown asset").and(contains("scan its volume first")));
}

/// A corrupt rejections log must fail loudly, not silently drop the bad
/// line: this file is append-only and owned entirely by `tags reject`, so a
/// line that doesn't parse means something else corrupted it, and quietly
/// skipping it would silently resurface a tag the user already rejected.
/// The error must name the file, the 1-based line number, and the garbage
/// content, so the fix is obvious without re-deriving it.
#[test]
fn corrupt_rejections_line_fails_loudly_with_path_and_line() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    // A first run with no rejections file yet forces the state dir (and
    // its `runs/` subdir) into existence, so the rejections file's exact
    // path can be located without hard-coding the per-catalog hash
    // directory `state_dir_for` derives.
    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success();
    let runs_dirs = walkdir_find(&state, "runs");
    assert_eq!(runs_dirs.len(), 1, "exactly one state dir's runs/ subdir");
    let state_dir = runs_dirs[0].parent().unwrap();
    let rejections_path = state_dir.join("tag-rejections.jsonl");

    std::fs::write(
        &rejections_path,
        "{\"asset\":\"xxh3:aaaa\",\"tag\":\"color/red\"}\nGARBAGE-NOT-JSON\n",
    )
    .unwrap();

    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .failure()
        .stderr(
            contains(rejections_path.display().to_string())
                .and(contains(":2:"))
                .and(contains("GARBAGE-NOT-JSON")),
        );
}

/// An unreadable `tags.json.zst` blob (garbage, not real zstd) must not
/// wedge `tags suggestions` for every other asset: it's skipped with a
/// stderr note — it may be mid-write by another process — and a second,
/// valid blob for a different asset still lists normally.
#[test]
fn unreadable_tags_blob_skips_with_a_note_and_others_still_list() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    let blobs = majestical_index::blob::BlobStore::new(&root);

    let garbage_hex = "deadbeefdeadbeefdeadbeefdeadbeef";
    let garbage_path = blobs.path_for(
        garbage_hex,
        &majestical_index::blob::Derivation::Tags {
            model_tag: "describe-m",
        },
    );
    blobs
        .write_atomic(&garbage_path, b"not zstd at all")
        .unwrap();

    let valid_hex = "cafebabecafebabecafebabecafebabe";
    let valid_path = blobs.path_for(
        valid_hex,
        &majestical_index::blob::Derivation::Tags {
            model_tag: "describe-m",
        },
    );
    let suggestions = vec![majestical_core::ports::TagSuggestion {
        tag: "topic/valid".to_string(),
        confidence: 0.7,
        in_vocab: false,
        model_tag: "describe-m".to_string(),
    }];
    let json = serde_json::to_vec(&suggestions).unwrap();
    let compressed = zstd::encode_all(json.as_slice(), 3).unwrap();
    blobs.write_atomic(&valid_path, &compressed).unwrap();

    maj(&root, &state)
        .args(["tags", "suggestions"])
        .assert()
        .success()
        .stderr(contains("skipping unreadable"))
        .stdout(contains("topic/valid"));
}
