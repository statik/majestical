//! `crates/cli/tests/services_parity.rs` — byte-identical CLI output through
//! the services extraction, proven by diffing this build's stdout, stderr,
//! and exit code against the pre-extraction reference binary (`/tmp/maj-ref`,
//! built at each PR chunk's start). Skips (with a loud message) when the
//! reference is absent — CI rebuilds it in the job.
mod common;

use assert_cmd::Command; // both arms below are assert_cmd::Command
use std::path::Path;

#[cfg(test)]
fn diff_against_ref(root: &Path, state: &Path, args: &[&str]) {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity({args:?}): /tmp/maj-ref missing — build it first");
        return;
    }
    let run = |bin: &str| {
        let mut c = if bin == "ref" {
            Command::new(reference)
        } else {
            Command::cargo_bin("maj").expect("bin")
        };
        c.env("MAJ_CATALOG", root)
            .env("MAJ_MACHINE_ID", "test-machine")
            .env("MAJ_STATE_DIR", state)
            .args(args);
        c.output().expect("run")
    };
    let (new, old) = (run("new"), run("ref"));
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for {args:?}"
    );
}

/// Runs `args` once per binary, each against its OWN catalog root/state —
/// for a mutating verb whose second-ever application legitimately behaves
/// differently from its first (e.g. `tag rm` tombstones every currently
/// observed add, so a second `tag rm` has nothing left and errors). Sharing
/// one root between the two invocations (like [`diff_against_ref`] does)
/// would make the second binary's call diverge from the first for reasons
/// that have nothing to do with an extraction bug — this keeps each
/// binary's call the first (and only) one against its own root.
#[cfg(test)]
fn diff_against_ref_independent(
    root_new: &Path,
    state_new: &Path,
    root_ref: &Path,
    state_ref: &Path,
    args: &[&str],
) {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity({args:?}): /tmp/maj-ref missing — build it first");
        return;
    }
    let new = common::maj(root_new, state_new)
        .args(args)
        .output()
        .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", root_ref)
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", state_ref)
        .args(args);
    let old = c.output().expect("run ref");
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for {args:?}"
    );
}

/// Like [`diff_against_ref`], but runs `between` after the `new` binary's
/// call and before the `ref` binary's call — for a mutating verb whose
/// output embeds an absolute path under the shared root (e.g. `para
/// archive --root <dir>`'s "moved X -> Y" line), where two independently
/// seeded roots would print two different, non-comparable paths.
/// `between` undoes the `new` binary's filesystem side effect (e.g.
/// renaming the archived directory back) so the `ref` binary's call is
/// also a genuine first application against the same paths.
#[cfg(test)]
fn diff_against_ref_with_between(root: &Path, state: &Path, args: &[&str], between: impl FnOnce()) {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity({args:?}): /tmp/maj-ref missing — build it first");
        return;
    }
    let run = |bin: &str| {
        let mut c = if bin == "ref" {
            Command::new(reference)
        } else {
            Command::cargo_bin("maj").expect("bin")
        };
        c.env("MAJ_CATALOG", root)
            .env("MAJ_MACHINE_ID", "test-machine")
            .env("MAJ_STATE_DIR", state)
            .args(args);
        c.output().expect("run")
    };
    let new = run("new");
    between();
    let old = run("ref");
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for {args:?}"
    );
}

#[test]
fn search_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    for args in [
        ["search", "a.txt", "--json"].as_slice(),
        ["search", "a.txt"].as_slice(),
        ["search", "tag:demo", "--json"].as_slice(),
        ["search", "nomatch", "--json"].as_slice(),
        ["searches", "list", "--json"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

#[test]
fn volumes_list_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    for args in [
        ["volumes", "list", "--json"].as_slice(),
        ["volumes", "list"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

#[test]
fn meta_get_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    common::maj(&root, &state)
        .args(["meta", "set", &asset, "rating", "5"])
        .assert()
        .success();
    for args in [
        vec!["meta", "get", &asset, "rating", "--json"],
        vec!["meta", "get", &asset, "rating"],
        vec!["meta", "get", &asset, "missing-field", "--json"],
        vec!["meta", "get", &asset, "missing-field"],
        vec!["meta", "get", &asset, "--json"],
        vec!["meta", "get", &asset],
    ] {
        diff_against_ref(&root, &state, &args);
    }
}

#[test]
fn para_list_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();
    for args in [
        ["para", "list", "--json"].as_slice(),
        ["para", "list"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

#[test]
fn tags_suggestions_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    diff_against_ref(&root, &state, &["tags", "suggestions"]);

    // A fresh catalog with no assets at all — the empty-catalog output must
    // match too, not just "assets scanned but no suggestion blobs".
    let empty_dir = tempfile::tempdir().expect("tempdir");
    let empty_root = empty_dir.path().join("cat");
    let empty_state = empty_dir.path().join("state");
    common::maj(&empty_root, &empty_state)
        .args(["catalog", "init"])
        .assert()
        .success();
    diff_against_ref(&empty_root, &empty_state, &["tags", "suggestions"]);
}

#[test]
fn describer_show_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    diff_against_ref(&root, &state, &["describer", "show"]);

    common::maj(&root, &state)
        .args([
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "llava",
        ])
        .assert()
        .success();
    diff_against_ref(&root, &state, &["describer", "show"]);
}

#[test]
fn sync_status_and_location_list_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());

    // Zero locations configured: `status` errors naming the remedy, and
    // `location list` prints the same hint — both must match byte for byte.
    diff_against_ref(&root, &state, &["sync", "status", "--json"]);
    diff_against_ref(&root, &state, &["sync", "status"]);
    diff_against_ref(&root, &state, &["sync", "location", "list", "--json"]);

    let location_dir = dir.path().join("shuttle");
    std::fs::create_dir_all(&location_dir).expect("mkdir");
    common::maj(&root, &state)
        .args([
            "sync",
            "location",
            "add",
            "shuttle",
            location_dir.to_str().expect("utf8"),
        ])
        .assert()
        .success();

    for args in [
        ["sync", "status", "--json"].as_slice(),
        ["sync", "status"].as_slice(),
        ["sync", "location", "list", "--json"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

#[test]
fn index_status_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    for args in [
        ["index", "status", "--json"].as_slice(),
        ["index", "status"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

#[test]
fn no_catalog_error_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("nonexistent");
    let state = dir.path().join("state");
    diff_against_ref(&root, &state, &["volumes", "list"]);
}

#[test]
fn catalog_init_on_an_existing_catalog_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // `catalog init` is idempotent (`FileEventLog::init` uses
    // `create_dir_all`), so both binaries below run against an
    // already-initialized catalog and must succeed identically rather than
    // erroring.
    diff_against_ref(&root, &state, &["catalog", "init"]);
}

#[test]
fn tag_add_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    // Adding a tag twice (once per binary) is a harmless OR-Set re-add, so
    // this is safe against a shared root.
    diff_against_ref(&root, &state, &["tag", "add", &asset, "x"]);
}

#[test]
fn tag_rm_refusal_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    // Never set — both binaries hit the same "nothing to remove" refusal,
    // which doesn't mutate the log, so a shared root is safe here too.
    diff_against_ref(&root, &state, &["tag", "rm", &asset, "never-set"]);
}

#[test]
fn tag_rm_after_add_output_is_byte_identical() {
    // `tag rm` tombstones every currently observed add-event for the tag —
    // its second application has nothing left to remove and errors, so
    // (unlike `tag_add`/`tag_rm_refusal` above) this needs two
    // independently seeded catalogs, one per binary.
    let dir_new = tempfile::tempdir().expect("tempdir");
    let (root_new, state_new) = common::fixture_catalog(dir_new.path());
    let asset = common::asset_id_of(&root_new, &state_new, "a.txt");
    common::maj(&root_new, &state_new)
        .args(["tag", "add", &asset, "x"])
        .assert()
        .success();

    let dir_ref = tempfile::tempdir().expect("tempdir");
    let (root_ref, state_ref) = common::fixture_catalog(dir_ref.path());
    common::maj(&root_ref, &state_ref)
        .args(["tag", "add", &asset, "x"])
        .assert()
        .success();

    diff_against_ref_independent(
        &root_new,
        &state_new,
        &root_ref,
        &state_ref,
        &["tag", "rm", &asset, "x"],
    );
}

#[test]
fn meta_set_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    // Setting the same field to the same value twice (once per binary) is
    // an idempotent LWW write, so a shared root is safe.
    diff_against_ref(&root, &state, &["meta", "set", &asset, "f", "v"]);
}

#[test]
fn scan_rescan_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // `scan`'s printed count comes straight from walking `dir` on this
    // call, not from anything derived from prior catalog history, so
    // re-scanning the same directory (once per binary) is safe against a
    // shared root.
    let src2 = dir.path().join("src2");
    std::fs::create_dir_all(&src2).expect("mkdir");
    std::fs::write(src2.join("c.txt"), b"gamma").expect("write");
    let src2_str = src2.to_str().expect("utf8");
    diff_against_ref(&root, &state, &["scan", src2_str, "--volume", "v2"]);
}

/// `para add` mints a fresh ULID on every call, so its stdout differs
/// between the two binaries by construction — a byte-diff would be a false
/// failure, not a real divergence. Compares stderr and exit code (which
/// must match) and checks each binary's own stdout independently for the
/// expected shape: one line, a 26-char Crockford base32 ULID.
#[test]
fn para_add_output_shape_is_consistent_between_binaries() {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(para add): /tmp/maj-ref missing — build it first");
        return;
    }
    let dir_new = tempfile::tempdir().expect("tempdir");
    let (root_new, state_new) = common::fixture_catalog(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let (root_ref, state_ref) = common::fixture_catalog(dir_ref.path());

    let new = common::maj(&root_new, &state_new)
        .args(["para", "add", "project", "p1"])
        .output()
        .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", &root_ref)
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", &state_ref)
        .args(["para", "add", "project", "p1"]);
    let old = c.output().expect("run ref");

    assert_eq!(new.status.code(), old.status.code());
    assert_eq!(
        String::from_utf8_lossy(&new.stderr),
        String::from_utf8_lossy(&old.stderr)
    );
    for stdout in [&new.stdout, &old.stdout] {
        let text = String::from_utf8_lossy(stdout);
        let line = text.trim_end_matches('\n');
        assert_eq!(
            line.len(),
            26,
            "expected a 26-char ULID line, got: {line:?}"
        );
        assert!(
            line.chars().all(|c| c.is_ascii_alphanumeric()),
            "expected Crockford base32 ULID chars, got: {line:?}"
        );
    }
}

#[test]
fn para_rename_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let out = common::maj(&root, &state)
        .args(["para", "list", "--json"])
        .output()
        .expect("run");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let node_id = parsed["nodes"][0]["id"].as_str().expect("id").to_string();
    // Renamed by raw node id (not `<kind>/<name>`) to the same target name
    // both times — renaming twice by `<kind>/<name>` would have the second
    // call fail to resolve, since the first call's rename already changed
    // the name the reference addresses.
    diff_against_ref(&root, &state, &["para", "rename", &node_id, "p1-renamed"]);
}

#[test]
fn para_archive_dry_run_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let materialized = tempfile::tempdir().expect("tempdir");
    let node_dir = materialized.path().join("Projects").join("p1");
    std::fs::create_dir_all(&node_dir).expect("mkdir");
    let root_arg = materialized.path().to_str().expect("utf8");
    // Dry run never touches the filesystem, so re-running it (once per
    // binary) against the same materialized dir is safe.
    diff_against_ref(
        &root,
        &state,
        &[
            "para",
            "archive",
            "project/p1",
            "--root",
            root_arg,
            "--dry-run",
        ],
    );
}

#[test]
fn para_archive_executed_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let out = common::maj(&root, &state)
        .args(["para", "list", "--json"])
        .output()
        .expect("run");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let node_id = parsed["nodes"][0]["id"].as_str().expect("id").to_string();
    let materialized = tempfile::tempdir().expect("tempdir");
    let node_dir = materialized.path().join("Projects").join("p1");
    let archived_dir = materialized.path().join("Archives").join("p1");
    std::fs::create_dir_all(&node_dir).expect("mkdir");
    std::fs::write(node_dir.join("a.txt"), b"hello").expect("write");
    let root_arg = materialized.path().to_str().expect("utf8");

    // Addressed by raw node id, not `<kind>/<name>`: the first binary's
    // call archives the node, and `<kind>/<name>` resolution requires a
    // non-archived node — the second call would fail to resolve it at all
    // otherwise. The executed move also only prints "moved X -> Y" on a
    // genuine first application; a second application against the same
    // directory would instead hit the "already archived" skip path.
    // Restoring the directory between the two binaries' calls (rather than
    // seeding two separate materialized dirs) keeps both calls a genuine
    // first application of the SAME absolute path — two separate tempdirs
    // would print different, non-comparable paths in "moved X -> Y".
    diff_against_ref_with_between(
        &root,
        &state,
        &["para", "archive", &node_id, "--root", root_arg],
        || {
            if archived_dir.is_dir() && !node_dir.is_dir() {
                std::fs::rename(&archived_dir, &node_dir).expect("restore for second binary's run");
            }
        },
    );
}

/// Pins the partial-failure shape: a multi-root run failing on the SECOND
/// root must still report the FIRST root's completed move on stdout before
/// the error reaches stderr — the old (pre-extraction) binary interleaved
/// its `moved X -> Y` println with the loop, so root1's real filesystem
/// mutation was already visible before root2's failure. The buffered
/// services extraction must reproduce this exactly (see
/// `ServiceError::ParaArchivePartial`), not silently drop it.
#[test]
fn para_archive_partial_failure_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let root1 = tempfile::tempdir().expect("tempdir");
    let root1_source = root1.path().join("Projects").join("p1");
    let root1_archived = root1.path().join("Archives").join("p1");
    std::fs::create_dir_all(&root1_source).expect("mkdir");
    std::fs::write(root1_source.join("a.txt"), b"hello").expect("write");
    // root2 has no materialized directory at all, so its move fails. The
    // run never reaches the archive-event emit, so the node stays active
    // in the catalog after each binary's call — both calls can address it
    // by `<kind>/<name>` without needing the raw-node-id workaround the
    // executed (successful) archive test above needs.
    let root2 = tempfile::tempdir().expect("tempdir");
    let root1_arg = root1.path().to_str().expect("utf8");
    let root2_arg = root2.path().to_str().expect("utf8");

    // Restoring root1's directory between the two binaries' calls (rather
    // than seeding two separate root1 dirs) keeps both calls a genuine
    // first application of the SAME absolute paths — two separate tempdirs
    // would print different, non-comparable paths in "moved X -> Y".
    diff_against_ref_with_between(
        &root,
        &state,
        &[
            "para",
            "archive",
            "project/p1",
            "--root",
            root1_arg,
            "--root",
            root2_arg,
        ],
        || {
            if root1_archived.is_dir() && !root1_source.is_dir() {
                std::fs::rename(&root1_archived, &root1_source)
                    .expect("restore root1 for second binary's run");
            }
        },
    );
}

/// Builds a directory with one file and a fresh ASC MHL generation-1
/// baseline — `verify`'s starting point before its own run (which writes
/// generation 2). No `--catalog`/`--state` is needed: `maj verify` neither
/// opens nor touches the catalog.
#[cfg(test)]
fn seed_verify_baseline() -> tempfile::TempDir {
    let media = tempfile::tempdir().expect("tempdir");
    std::fs::write(media.path().join("a.mov"), b"hello").expect("write");
    let hash_list =
        majestical_ingest::mhl::hash_dir(media.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
    majestical_ingest::mhl::write_generation(media.path(), &hash_list).expect("write_generation");
    media
}

#[test]
fn verify_json_output_is_byte_identical() {
    // `verify` writes a new generation on every call, so re-running it
    // against a shared history would legitimately report a different
    // generation number the second time — two independently seeded
    // baselines, each verified exactly once, sidestep that; `verify`
    // doesn't need a real catalog, so any unused directory works there.
    let media_new = seed_verify_baseline();
    let media_ref = seed_verify_baseline();
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(verify json): /tmp/maj-ref missing — build it first");
        return;
    }
    let unused = tempfile::tempdir().expect("tempdir");
    let new = common::maj(
        &unused.path().join("cat-new"),
        &unused.path().join("state-new"),
    )
    .args(["verify", media_new.path().to_str().expect("utf8"), "--json"])
    .output()
    .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", unused.path().join("cat-ref"))
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", unused.path().join("state-ref"))
        .args(["verify", media_ref.path().to_str().expect("utf8"), "--json"]);
    let old = c.output().expect("run ref");
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for verify json"
    );
}

#[test]
fn verify_text_output_is_byte_identical() {
    let media_new = seed_verify_baseline();
    let media_ref = seed_verify_baseline();
    let unused = tempfile::tempdir().expect("tempdir");
    let new = common::maj(
        &unused.path().join("cat-new"),
        &unused.path().join("state-new"),
    )
    .args(["verify", media_new.path().to_str().expect("utf8")])
    .output()
    .expect("run new");
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(verify text): /tmp/maj-ref missing — build it first");
        return;
    }
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", unused.path().join("cat-ref"))
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", unused.path().join("state-ref"))
        .args(["verify", media_ref.path().to_str().expect("utf8")]);
    let old = c.output().expect("run ref");
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for verify text"
    );
}

#[test]
fn verify_tampered_output_is_byte_identical() {
    let media_new = seed_verify_baseline();
    std::fs::write(media_new.path().join("a.mov"), b"TAMPERED").expect("tamper");
    let media_ref = seed_verify_baseline();
    std::fs::write(media_ref.path().join("a.mov"), b"TAMPERED").expect("tamper");

    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(verify tampered): /tmp/maj-ref missing — build it first");
        return;
    }
    let unused = tempfile::tempdir().expect("tempdir");
    let new = common::maj(
        &unused.path().join("cat-new"),
        &unused.path().join("state-new"),
    )
    .args(["verify", media_new.path().to_str().expect("utf8"), "--json"])
    .output()
    .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", unused.path().join("cat-ref"))
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", unused.path().join("state-ref"))
        .args(["verify", media_ref.path().to_str().expect("utf8"), "--json"]);
    let old = c.output().expect("run ref");
    assert_eq!(
        (
            String::from_utf8_lossy(&new.stdout),
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for verify tampered"
    );
}

/// Pins `new_files` end to end through the CLI: a `NEW <name>` line plus
/// the "1 new" count in the summary must survive extraction — a sabotage
/// hardcoding `new_files: Vec::new()` in the service would pass every
/// other verify parity row (none of them add a file after the baseline)
/// but fail this one.
#[test]
fn verify_new_file_output_is_byte_identical() {
    let media_new = seed_verify_baseline();
    std::fs::write(media_new.path().join("b.mov"), b"world").expect("write new file");
    let media_ref = seed_verify_baseline();
    std::fs::write(media_ref.path().join("b.mov"), b"world").expect("write new file");

    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(verify new file): /tmp/maj-ref missing — build it first");
        return;
    }
    let unused = tempfile::tempdir().expect("tempdir");
    let new = common::maj(
        &unused.path().join("cat-new"),
        &unused.path().join("state-new"),
    )
    .args(["verify", media_new.path().to_str().expect("utf8")])
    .output()
    .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", unused.path().join("cat-ref"))
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", unused.path().join("state-ref"))
        .args(["verify", media_ref.path().to_str().expect("utf8")]);
    let old = c.output().expect("run ref");
    let new_stdout = String::from_utf8_lossy(&new.stdout);
    assert!(
        new_stdout.contains("NEW b.mov") && new_stdout.contains("1 new"),
        "expected a NEW b.mov line and a '1 new' count, got: {new_stdout}"
    );
    assert_eq!(
        (
            &new_stdout,
            String::from_utf8_lossy(&new.stderr),
            new.status.code()
        ),
        (
            &String::from_utf8_lossy(&old.stdout),
            String::from_utf8_lossy(&old.stderr),
            old.status.code()
        ),
        "stdout/stderr/exit diverged for verify new file"
    );
}
