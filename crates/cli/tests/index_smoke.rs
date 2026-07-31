//! End-to-end: `maj index run`/`maj index status` against real scanned
//! image bytes — the queue-as-diff planner exercised through the CLI and a
//! real (auto-detected) volume identity, since the indexer needs to
//! re-resolve bytes from a stored instance path.
mod common;

use common::{maj, walkdir_find};
use predicates::str::contains;

#[cfg(test)]
fn write_test_png(path: &std::path::Path, width: u32, height: u32) {
    image::RgbImage::new(width, height)
        .save(path)
        .expect("write a real PNG for the thumbnailer to decode");
}

/// `index run` writes exactly one thumbnail blob for a freshly scanned
/// image, is idempotent on a second run (the blob already exists), and
/// self-heals if the blob is deleted out from under it — the queue is
/// always the live diff against the blob store, never a stored ledger of
/// "already done".
#[test]
fn index_run_writes_thumbs_idempotently_and_self_heals() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("photo.png"), 64, 48);
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    // No --volume: the indexer needs a real, re-resolvable mount to find
    // these bytes again.
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();

    maj(&root, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 written"));

    let thumbs = walkdir_find(&root, "thumb-320.webp");
    assert_eq!(
        thumbs.len(),
        1,
        "exactly one thumbnail blob under the sync root's blob store"
    );

    maj(&root, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("0 written"));

    std::fs::remove_file(&thumbs[0]).unwrap();

    maj(&root, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 written"));

    maj(&root, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("thumbs: 1 done"));
}

/// `index status` reports the honest reason work can't run, rather than
/// pretending everything is simply "pending": an image with no encoder
/// model installed shows `needs_model` for embeddings, and an instance
/// scanned under a volume id that never actually mounts degrades to
/// `offline` for thumbnails instead of erroring.
#[test]
fn index_status_reports_needs_model_and_offline_honestly() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("a.png"), 64, 48);
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    // An empty model cache: no encoder installed, so embeddings/keyframes
    // must honestly report needs-model rather than pretending to be pending.
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
        .stdout(contains(
            "embeddings: 0 done, 0 pending, 0 offline, 0 unsupported, 0 need ffmpeg, 1 need model",
        ));

    // A second file scanned under an explicit --volume id that is never
    // actually mounted on this machine: its instance path can never be
    // re-based to a real mount, so the planner must degrade it to offline
    // rather than treating it as pending forever.
    let media2 = tempfile::tempdir().unwrap();
    write_test_png(&media2.path().join("b.png"), 32, 32);
    maj(&root, &state)
        .args(["scan"])
        .arg(media2.path())
        .args(["--volume", "maj-test-never-mounted"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("thumbs: 0 done, 1 pending, 1 offline"));
}

/// `maj model fetch` skips every file that's already present at its exact
/// byte size without hitting the network — the presence check is size-only,
/// so pre-placing correctly-sized dummy files is enough to prove the
/// already-present path without downloading a real model.
#[test]
fn model_fetch_reports_already_present_without_network() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_root = tempfile::tempdir().unwrap();
    // model_dir() joins MAJ_MODEL_DIR with the model tag, so the pre-placed
    // files must live one level down from what we set here.
    let model_dir = model_root.path().join(majestical_index::model::MODEL_TAG);
    std::fs::create_dir_all(&model_dir).unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    for file in majestical_index::model::MODEL_FILES {
        let path = model_dir.join(file.name);
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(file.bytes).unwrap();
    }

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args([
            "model",
            "fetch",
            "--only",
            majestical_index::model::MODEL_TAG,
        ])
        .assert()
        .success()
        .stdout(contains("already present").count(3));
}

/// `--only` rejects an unrecognized tag before touching the network — the
/// check runs against the static registry ahead of any fetch, so this test
/// never needs a model cache or a real download.
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

/// `index run` always performs the blob↔Lance diff — even with no model
/// installed and zero embed items in the plan — because that diff is how a
/// teammate's synced vectors (or a lance dir just rebuilt after corruption)
/// get indexed with zero re-inference. A hand-written vector blob (as if
/// synced in) must get picked up without the encoder ever loading.
#[test]
fn embeddings_loaded_from_blobs_without_model() {
    let media = tempfile::tempdir().unwrap();
    let png_path = media.path().join("photo.png");
    write_test_png(&png_path, 8, 8);
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    // Empty model cache: the encoder must never load on this path.
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

    // Same content hash `scan` computes: a one-shot xxh3-128 digest of the
    // file's bytes (streaming vs. one-shot makes no difference to the
    // digest for a file this small).
    let bytes = std::fs::read(&png_path).unwrap();
    let hex = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes));

    let blobs = majestical_index::blob::BlobStore::new(&root);
    let vector = vec![0.1f32; majestical_index::vector_store::DIM];
    let path = blobs.path_for(
        &hex,
        &majestical_index::blob::Derivation::ImageEmbedding {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    blobs.write_vector(&path, &vector).unwrap();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_dir.path())
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 loaded from blobs"));
}

/// With no model installed, `search` degrades to name-only matching and
/// reports the specific fix on stderr — never a hard failure just because
/// the semantic layer can't run.
#[test]
fn search_without_model_degrades_with_notice() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("photo.png"), 8, 8);
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
        .args(["search", "photo"])
        .assert()
        .success()
        .stdout(contains("results"))
        .stderr(contains("maj model fetch"));
}

/// Fake-size model files at the exact byte sizes `model_present()` checks —
/// same precedent as `model_fetch_reports_already_present_without_network`.
/// Passes the (size-only) presence check without a real, loadable model, so
/// any test using this must never actually reach `Encoder::load`.
#[cfg(test)]
fn seed_fake_model_files(model_root: &std::path::Path) {
    let model_dir = model_root.join(majestical_index::model::MODEL_TAG);
    std::fs::create_dir_all(&model_dir).unwrap();
    for file in majestical_index::model::MODEL_FILES {
        let f = std::fs::File::create(model_dir.join(file.name)).unwrap();
        f.set_len(file.bytes).unwrap();
    }
}

/// `index run`'s blob↔Lance diff must run in the MODEL-PRESENT path too, not
/// just when the model is absent (`embeddings_loaded_from_blobs_without_model`
/// pins that path already). An empty catalog (zero embed items in the plan)
/// alongside a hand-written vector blob proves the diff runs even when
/// `run_embed_items` takes the "model present" branch — the branch is
/// skipped only because there's nothing to embed, not because the model is
/// unavailable, so the encoder is never asked to load the fake (unloadable)
/// model files this test seeds.
#[test]
fn embeddings_loaded_from_blobs_with_model_present_and_no_pending_items() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let model_root = tempfile::tempdir().unwrap();
    seed_fake_model_files(model_root.path());

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    // No scan at all: an empty catalog, so the plan has zero ImageEmbed
    // items.

    let hex = "deadbeefdeadbeefdeadbeefdeadbeef".to_string();
    let blobs = majestical_index::blob::BlobStore::new(&root);
    let vector = vec![0.1f32; majestical_index::vector_store::DIM];
    let path = blobs.path_for(
        &hex,
        &majestical_index::blob::Derivation::ImageEmbedding {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    blobs.write_vector(&path, &vector).unwrap();

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 loaded from blobs"));
}

/// A query-time model presence check (size-only) can pass without the model
/// actually being loadable. Searching against an index that simply has
/// nothing in it yet must resolve to "index is empty" without ever trying
/// to load that unloadable model — which would otherwise surface as a
/// crash, or the wrong (`model fetch`) notice, instead of the right one.
#[test]
fn search_reports_empty_index_without_loading_an_unloadable_model() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("photo.png"), 8, 8);
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
    // Deliberately never run `index run`: no lance store exists yet.

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args(["search", "photo"])
        .assert()
        .success()
        .stdout(contains("results"))
        .stderr(contains("semantic index is empty"));
}

/// A corrupt local Lance store degrades `search` to FTS-only with a
/// distinct, rebuild-framed notice — and, critically, `search` never
/// repairs it itself: only `index run` writes, so only it may delete and
/// rebuild. Proven by checking the corrupt manifest bytes are byte-for-byte
/// unchanged after `search` runs.
#[test]
fn search_degrades_on_a_corrupt_lance_store_and_never_touches_it() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("photo.png"), 8, 8);
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
    // Model absent for this pass: only the blob↔Lance diff runs, which is
    // enough to materialize a real, empty lance dataset at a discoverable
    // path — the corruption recipe below needs real manifest files to
    // corrupt, not a directory that doesn't exist yet.
    let empty_model_dir = tempfile::tempdir().unwrap();
    maj(&root, &state)
        .env("MAJ_MODEL_DIR", empty_model_dir.path())
        .args(["index", "run"])
        .assert()
        .success();

    let lance_dirs = walkdir_find(&state, "lance");
    assert_eq!(
        lance_dirs.len(),
        1,
        "exactly one lance store under the state dir"
    );
    let lance_dir = &lance_dirs[0];

    // Seed a real vector directly (bypassing the CLI/encoder entirely) so
    // there's real manifest data on disk to corrupt.
    majestical_index::vector_store::VectorStore::open(lance_dir)
        .unwrap()
        .add(vec![majestical_index::vector_store::VectorRow {
            asset_hex: "aa11".into(),
            kind: "image".into(),
            ts_ms: -1,
            model_tag: majestical_index::model::MODEL_TAG.into(),
            vector: vec![0.1f32; majestical_index::vector_store::DIM],
        }])
        .unwrap();

    let versions_dir = lance_dir.join("vectors.lance/_versions");
    let manifest_paths: Vec<_> = std::fs::read_dir(&versions_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "manifest"))
        .collect();
    assert!(
        !manifest_paths.is_empty(),
        "seeded store must have manifest files"
    );
    for path in &manifest_paths {
        std::fs::write(path, b"GARBAGE-NOT-A-REAL-MANIFEST").unwrap();
    }
    let corrupted_bytes: Vec<Vec<u8>> = manifest_paths
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();

    let model_root = tempfile::tempdir().unwrap();
    seed_fake_model_files(model_root.path());

    maj(&root, &state)
        .env("MAJ_MODEL_DIR", model_root.path())
        .args(["search", "photo"])
        .assert()
        .success()
        .stdout(contains("results"))
        .stderr(contains("semantic index unreadable"));

    for (path, before) in manifest_paths.iter().zip(&corrupted_bytes) {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            before,
            "search must never modify the corrupt store — only `index run` may repair it"
        );
    }
}

/// `--limit` caps how much of the queue one pass works, so a long-running
/// index can be broken into bounded chunks: two pending thumbnails with
/// `--limit 1` take two passes, not one.
#[test]
fn index_run_limit_caps_one_pass() {
    let media = tempfile::tempdir().unwrap();
    write_test_png(&media.path().join("one.png"), 64, 48);
    write_test_png(&media.path().join("two.png"), 32, 24);
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

    maj(&root, &state)
        .args(["index", "run", "--limit", "1"])
        .assert()
        .success()
        .stdout(contains("1 written"));
    assert_eq!(
        walkdir_find(&root, "thumb-320.webp").len(),
        1,
        "--limit 1 must leave the second thumbnail for a later pass"
    );

    maj(&root, &state)
        .args(["index", "run", "--limit", "1"])
        .assert()
        .success()
        .stdout(contains("1 written"));
    assert_eq!(walkdir_find(&root, "thumb-320.webp").len(), 2);
}

/// `index status` on a catalog with a scanned video reports a real keyframes
/// row — ffmpeg detection (`video::ffmpeg_available()`) is live, not the
/// placeholder `false` PR 8 replaces. Only the row's presence and command
/// success are pinned here: whether it lands in `needs_ffmpeg` or `pending`
/// depends on whether this machine actually has ffmpeg installed, which this
/// test doesn't control.
#[test]
fn index_status_reports_a_real_keyframes_row_for_a_scanned_video() {
    let media = tempfile::tempdir().unwrap();
    // Not a real decodable video: `index status` only diffs against the blob
    // store, never decodes.
    std::fs::write(media.path().join("clip.mov"), b"not a real mov").unwrap();
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

    maj(&root, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("keyframes:"));
}

/// An explicit `--kinds keyframes` is a hard ask, not a best-effort one: with
/// ffmpeg absent from `PATH`, `index run --kinds keyframes` must fail loudly
/// with install guidance, rather than silently reporting 0 done the way the
/// default all-kinds run degrades.
#[test]
fn index_run_explicit_keyframes_without_ffmpeg_hard_errors() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"not a real mov").unwrap();
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

    maj(&root, &state)
        // Stock macOS bin dirs only — ffmpeg is never preinstalled there, and
        // the maj binary itself runs by absolute path so it doesn't need
        // PATH to launch.
        .env("PATH", "/usr/bin:/bin")
        .args(["index", "run", "--kinds", "keyframes"])
        .assert()
        .failure()
        .stderr(contains("install ffmpeg"));
}

/// Builds the same 9s, 320x180 three-segment (red/green/blue) clip
/// `video_e2e.rs` uses, at `path`.
#[cfg(test)]
fn generate_three_segment_clip(path: &std::path::Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args(["-filter_complex", "[0:v][1:v][2:v]concat=n=3:v=1:a=0[outv]"])
        .args(["-map", "[outv]", "-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .expect("running ffmpeg to generate the test clip");
    assert!(status.success(), "ffmpeg clip generation failed");
}

/// Full pipeline through the real CLI, with a real fetched model and real
/// ffmpeg: scan a three-segment color clip, `index run` it (video
/// thumbnail + scene-detected keyframes + embeddings), then confirm
/// semantic search resolves each segment's color to a keyframe timestamp
/// inside that segment — the plumbing `video::detect_scenes` ->
/// `Encoder::embed_image` -> the Lance store -> `search.rs`'s `@MmSSs`
/// rendering, end to end.
///
///     MAJ_MODEL_DIR=<model cache dir> \
///         cargo test -p majestical-cli --test index_smoke -- --ignored keyframe_search
#[test]
#[ignore = "needs a fetched model and ffmpeg on PATH"]
fn keyframe_search_resolves_the_correct_segment_and_timestamp() {
    assert!(
        majestical_index::video::ffmpeg_available(),
        "ffmpeg/ffprobe must be on PATH for this test"
    );
    let model_dir = majestical_index::model::model_dir().expect("resolve model dir");
    assert!(
        majestical_index::model::model_present(&model_dir),
        "model not present at {}; run `maj model fetch`",
        model_dir.display()
    );

    let media = tempfile::tempdir().unwrap();
    generate_three_segment_clip(&media.path().join("clip.mp4"));
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

    maj(&root, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains(
            "1 videos, 3 frames embedded, 0 frame failures, 0 videos failed",
        ));

    maj(&root, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("keyframes: 1 done"));

    // The red segment spans 0-3s; its detected keyframe midpoint is 1.5s.
    maj(&root, &state)
        .args(["search", "solid red"])
        .assert()
        .success()
        .stdout(contains("clip.mp4"))
        .stdout(contains("@0m01s"));

    // The blue segment spans 6-9s; its detected keyframe midpoint is 7.5s.
    maj(&root, &state)
        .args(["search", "solid blue color"])
        .assert()
        .success()
        .stdout(contains("clip.mp4"))
        .stdout(contains("@0m07s"));
}
