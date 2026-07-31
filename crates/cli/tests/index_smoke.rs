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
        .args(["model", "fetch"])
        .assert()
        .success()
        .stdout(contains("already present").count(3));
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
