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

/// `--limit 0` truncates every kind's per-pass queue to zero items
/// (`split_and_cap_items`), so this is a deterministic empty pass regardless
/// of `--kinds` defaulting to every kind — proving the engine-extraction's
/// full open-catalog/build-plan/heal/failure-report path byte-for-byte
/// against the pre-extraction binary without needing models or ffmpeg.
#[test]
fn index_run_empty_pass_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    for args in [
        ["index", "run", "--limit", "0", "--json"].as_slice(),
        ["index", "run", "--limit", "0"].as_slice(),
    ] {
        diff_against_ref(&root, &state, args);
    }
}

/// `--kinds thumbs` against a text-only catalog (`fixture_catalog`'s `.txt`
/// files are `MediaKind::Other`, never queued for any kind — see
/// `crates/index/src/work.rs`): another deterministic empty pass, this time
/// through the `--kinds`-narrowed path rather than `--limit`.
#[test]
fn index_run_kinds_thumbs_on_a_text_only_catalog_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    diff_against_ref(
        &root,
        &state,
        &["index", "run", "--kinds", "thumbs", "--limit", "1"],
    );
}

/// An unknown `--kinds` value is a pure arg-validation error — `parse_kinds`
/// rejects it before the engine ever runs, so this proves the error path
/// (message and exit code) stayed in the CLI and byte-identical.
#[test]
fn index_run_invalid_kinds_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    diff_against_ref(&root, &state, &["index", "run", "--kinds", "bogus"]);
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

/// `sync location add`'s state dir sync.toml lives at a hashed-key path
/// under `state/catalogs/<key>/` — one key per test catalog root, so the
/// first (and only) entry under `catalogs/` is always it.
#[cfg(test)]
fn find_sync_toml(state: &Path) -> std::path::PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("sync.toml")
}

#[test]
fn tags_confirm_reject_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, &state, "a.txt");
    // Confirming a tag never already on the asset is a plain TagAdd —
    // idempotent on a shared root (a re-add is a harmless OR-Set re-add,
    // same reasoning as `tag_add_output_is_byte_identical`).
    diff_against_ref(&root, &state, &["tags", "confirm", &asset, "landscape"]);
    // Reject never validates against a current suggestion, so it's a
    // harmless append-only write regardless of prior state — safe to run
    // against the shared machine-scoped rejection log twice.
    diff_against_ref(&root, &state, &["tags", "reject", &asset, "blurry"]);
}

#[test]
fn describer_set_and_test_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    // Setting the same backend/model twice (once per binary) is a plain
    // overwrite of describer.toml — idempotent, safe on a shared root.
    diff_against_ref(
        &root,
        &state,
        &[
            "describer",
            "set",
            "--backend",
            "ollama",
            "--model",
            "llava",
            "--base-url",
            "http://127.0.0.1:1",
        ],
    );
    // Nothing listens on port 1: both binaries hit the identical
    // connection-refused path — the parity case that matters here, since
    // neither this fixture nor CI has a real describer backend running.
    diff_against_ref(&root, &state, &["describer", "test"]);
}

#[test]
fn sync_location_add_rm_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let location_dir = dir.path().join("shuttle");
    std::fs::create_dir_all(&location_dir).expect("mkdir");
    let loc_str = location_dir.to_str().expect("utf8").to_string();
    let add_args = ["sync", "location", "add", "shuttle", &loc_str];

    // First application only (a second `add` of the same name refuses) —
    // `between` removes the entry `new` just added via the `ref`-independent
    // rm verb, so `ref`'s identical add call is also a genuine first
    // application against the same shared root/location path.
    diff_against_ref_with_between(&root, &state, &add_args, || {
        common::maj(&root, &state)
            .args(["sync", "location", "rm", "shuttle"])
            .assert()
            .success();
    });

    // Duplicate add: the `between` above already left "shuttle" configured
    // again (the `ref` binary's own add, run last inside
    // `diff_against_ref_with_between`), so both binaries now hit the
    // identical "already configured" refusal against the shared config.
    diff_against_ref(&root, &state, &add_args);

    // rm of an unknown name is a safe, non-mutating refusal on a shared root.
    diff_against_ref(&root, &state, &["sync", "location", "rm", "ghost"]);

    // rm success: `between` re-adds "shuttle" so `ref`'s identical rm call
    // is also a genuine first removal.
    diff_against_ref_with_between(
        &root,
        &state,
        &["sync", "location", "rm", "shuttle"],
        || {
            common::maj(&root, &state).args(add_args).assert().success();
        },
    );
}

#[test]
fn sync_push_output_is_byte_identical() {
    // A real push actually copies segments/blobs, so a second push against
    // the same location reports zero — independent catalogs, one per
    // binary, each doing its own first push.
    let dir_new = tempfile::tempdir().expect("tempdir");
    let (root_new, state_new) = common::fixture_catalog(dir_new.path());
    let location_new = dir_new.path().join("shuttle");
    std::fs::create_dir_all(&location_new).expect("mkdir");
    common::maj(&root_new, &state_new)
        .args([
            "sync",
            "location",
            "add",
            "shuttle",
            location_new.to_str().expect("utf8"),
        ])
        .assert()
        .success();

    let dir_ref = tempfile::tempdir().expect("tempdir");
    let (root_ref, state_ref) = common::fixture_catalog(dir_ref.path());
    let location_ref = dir_ref.path().join("shuttle");
    std::fs::create_dir_all(&location_ref).expect("mkdir");
    common::maj(&root_ref, &state_ref)
        .args([
            "sync",
            "location",
            "add",
            "shuttle",
            location_ref.to_str().expect("utf8"),
        ])
        .assert()
        .success();

    for args in [
        ["sync", "push", "--json"].as_slice(),
        ["sync", "push"].as_slice(),
    ] {
        diff_against_ref_independent(&root_new, &state_new, &root_ref, &state_ref, args);
        // Reset both sides to "nothing pushed yet" before the next mode's
        // pass — push is otherwise a one-shot event in this test.
        std::fs::remove_dir_all(location_new.join("events")).expect("reset new location");
        std::fs::remove_dir_all(location_ref.join("events")).expect("reset ref location");
        std::fs::create_dir_all(location_new.join("events")).expect("recreate");
        std::fs::create_dir_all(location_ref.join("events")).expect("recreate");
    }
}

#[test]
fn sync_push_readonly_refusal_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let location_dir = dir.path().join("nas");
    std::fs::create_dir_all(&location_dir).expect("mkdir");
    common::maj(&root, &state)
        .args([
            "sync",
            "location",
            "add",
            "nas",
            location_dir.to_str().expect("utf8"),
        ])
        .assert()
        .success();
    let config = find_sync_toml(&state);
    let text = std::fs::read_to_string(&config).expect("read");
    let flipped = text.replace("readonly = false", "readonly = true");
    assert_ne!(
        flipped, text,
        "sync.toml must contain readonly = false: {text}"
    );
    std::fs::write(&config, flipped).expect("write");

    // Refuses before touching any location, so this never mutates and is
    // safe to run for both binaries against the shared config file — whose
    // absolute path is embedded in the refusal message and is identical
    // either way, since it's the same path on disk.
    diff_against_ref(&root, &state, &["sync", "push"]);
}

#[test]
fn sync_push_partial_failure_output_is_byte_identical() {
    // One reachable location plus one whose directory was removed after
    // `location add` (so `push` reports it Skipped, unreachable) —
    // independent catalogs since the reachable location's push is a
    // real, first-application transfer.
    let dir_new = tempfile::tempdir().expect("tempdir");
    let (root_new, state_new) = common::fixture_catalog(dir_new.path());
    let good_new = dir_new.path().join("good");
    std::fs::create_dir_all(&good_new).expect("mkdir");
    let gone_new = dir_new.path().join("gone");
    std::fs::create_dir_all(&gone_new).expect("mkdir");
    common::maj(&root_new, &state_new)
        .args([
            "sync",
            "location",
            "add",
            "good",
            good_new.to_str().expect("utf8"),
        ])
        .assert()
        .success();
    common::maj(&root_new, &state_new)
        .args([
            "sync",
            "location",
            "add",
            "gone",
            gone_new.to_str().expect("utf8"),
        ])
        .assert()
        .success();
    std::fs::remove_dir_all(&gone_new).expect("remove");

    let dir_ref = tempfile::tempdir().expect("tempdir");
    let (root_ref, state_ref) = common::fixture_catalog(dir_ref.path());
    let good_ref = dir_ref.path().join("good");
    std::fs::create_dir_all(&good_ref).expect("mkdir");
    let gone_ref = dir_ref.path().join("gone");
    std::fs::create_dir_all(&gone_ref).expect("mkdir");
    common::maj(&root_ref, &state_ref)
        .args([
            "sync",
            "location",
            "add",
            "good",
            good_ref.to_str().expect("utf8"),
        ])
        .assert()
        .success();
    common::maj(&root_ref, &state_ref)
        .args([
            "sync",
            "location",
            "add",
            "gone",
            gone_ref.to_str().expect("utf8"),
        ])
        .assert()
        .success();
    std::fs::remove_dir_all(&gone_ref).expect("remove");

    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(sync push partial failure): /tmp/maj-ref missing — build it first");
        return;
    }
    let new = common::maj(&root_new, &state_new)
        .args(["sync", "push", "--json"])
        .output()
        .expect("run new");
    let mut c = Command::new(reference);
    c.env("MAJ_CATALOG", &root_ref)
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", &state_ref)
        .args(["sync", "push", "--json"]);
    let old = c.output().expect("run ref");

    assert_eq!(new.status.code(), old.status.code());
    assert_eq!(
        String::from_utf8_lossy(&new.stderr),
        String::from_utf8_lossy(&old.stderr)
    );
    // The "gone" row's `skipped` reason embeds each side's own independent
    // tempdir path — structurally identical (same location name, same
    // "unreachable ... — skipped" wording) but never textually equal, so
    // that one field is normalized before comparing the rest of the JSON.
    let mut new_json: serde_json::Value =
        serde_json::from_slice(&new.stdout).expect("new stdout is JSON");
    let mut old_json: serde_json::Value =
        serde_json::from_slice(&old.stdout).expect("ref stdout is JSON");
    for doc in [&mut new_json, &mut old_json] {
        if let Some(rows) = doc.as_array_mut() {
            for row in rows {
                if row.get("location") == Some(&serde_json::json!("gone")) {
                    row["skipped"] = serde_json::json!("<SKIPPED>");
                }
            }
        }
    }
    assert_eq!(
        new_json, old_json,
        "sync push JSON must match once the unreachable location's path is normalized"
    );
}

#[test]
fn sync_pull_output_is_byte_identical() {
    // Independent (source catalog, location, dest catalog) triples: a pull
    // both fetches and applies, so a second pull against the same location
    // reports nothing new.
    fn seed_and_pull(base: &std::path::Path) -> std::process::Output {
        let (source_root, source_state) = common::fixture_catalog(base);
        let location = base.join("shuttle");
        std::fs::create_dir_all(&location).expect("mkdir");
        common::maj(&source_root, &source_state)
            .args([
                "sync",
                "location",
                "add",
                "shuttle",
                location.to_str().expect("utf8"),
            ])
            .assert()
            .success();
        common::maj(&source_root, &source_state)
            .args(["sync", "push"])
            .assert()
            .success();

        let dest_root = base.join("dest");
        let dest_state = base.join("dest-state");
        common::maj(&dest_root, &dest_state)
            .args(["catalog", "init"])
            .assert()
            .success();
        common::maj(&dest_root, &dest_state)
            .args([
                "sync",
                "location",
                "add",
                "shuttle",
                location.to_str().expect("utf8"),
            ])
            .assert()
            .success();
        common::maj(&dest_root, &dest_state)
            .args(["sync", "pull", "--json"])
            .output()
            .expect("run pull")
    }

    let dir_new = tempfile::tempdir().expect("tempdir");
    let new = seed_and_pull(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let ref_out = seed_and_pull(dir_ref.path());

    assert_eq!(new.status.code(), ref_out.status.code());
    assert_eq!(
        String::from_utf8_lossy(&new.stdout),
        String::from_utf8_lossy(&ref_out.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&new.stderr),
        String::from_utf8_lossy(&ref_out.stderr)
    );
}

/// `state/catalogs/<key>/catalog.db` — one key per test catalog root, so
/// the first (and only) entry under `catalogs/` is always it. Mirrors
/// `sync_smoke.rs::find_catalog_db`.
#[cfg(test)]
fn find_catalog_db(state: &Path) -> std::path::PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("catalog.db")
}

/// Pins the abort-midway case `ServiceError::SyncPullApplyFailed` exists
/// for: the transfer completes (real segment copy, reported as a `Ran`
/// row) but the subsequent local-catalog apply fails, and the CLI must
/// still render that completed row before surfacing the apply error —
/// exactly like the pre-extraction code, which printed rows before the
/// apply ran at all. Forces the apply to fail deterministically by
/// pre-creating a directory at the exact path `open_synced` will try to
/// open as a sqlite file (`Connection::open` on a directory fails on every
/// platform, no corrupted bytes needed) — independent catalogs, since the
/// transfer itself is a real, first-application push+pull.
#[test]
fn sync_pull_apply_failure_output_is_byte_identical() {
    fn seed_then_break_apply(base: &std::path::Path) -> (std::process::Output, std::path::PathBuf) {
        let (source_root, source_state) = common::fixture_catalog(base);
        let location = base.join("shuttle");
        std::fs::create_dir_all(&location).expect("mkdir");
        common::maj(&source_root, &source_state)
            .args([
                "sync",
                "location",
                "add",
                "shuttle",
                location.to_str().expect("utf8"),
            ])
            .assert()
            .success();
        common::maj(&source_root, &source_state)
            .args(["sync", "push"])
            .assert()
            .success();

        let dest_root = base.join("dest");
        let dest_state = base.join("dest-state");
        common::maj(&dest_root, &dest_state)
            .args(["catalog", "init"])
            .assert()
            .success();
        common::maj(&dest_root, &dest_state)
            .args([
                "sync",
                "location",
                "add",
                "shuttle",
                location.to_str().expect("utf8"),
            ])
            .assert()
            .success();

        // `sync location add` above already touched the state dir (writing
        // sync.toml under it), so `catalogs/<key>/` already exists — block
        // the apply before it ever runs.
        let db_path = find_catalog_db(&dest_state);
        std::fs::create_dir_all(&db_path).expect("mkdir catalog.db");

        let output = common::maj(&dest_root, &dest_state)
            .args(["sync", "pull"])
            .output()
            .expect("run pull");
        (output, db_path)
    }

    let dir_new = tempfile::tempdir().expect("tempdir");
    let (new, db_path_new) = seed_then_break_apply(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let (ref_out, db_path_ref) = seed_then_break_apply(dir_ref.path());

    assert_eq!(new.status.code(), ref_out.status.code());
    assert_ne!(
        new.status.code(),
        Some(0),
        "an apply failure must be nonzero"
    );
    assert_eq!(
        String::from_utf8_lossy(&new.stdout),
        String::from_utf8_lossy(&ref_out.stdout),
        "the completed transfer row must render identically before the apply error"
    );
    // The apply error embeds `catalog.db`'s absolute path — real, and
    // structurally identical in shape between the two sides, but never
    // textually equal across two independent tempdirs, so it's normalized
    // before comparing the rest of stderr.
    let new_stderr = String::from_utf8_lossy(&new.stderr)
        .replace(db_path_new.to_str().expect("utf8"), "<CATALOG_DB>");
    let ref_stderr = String::from_utf8_lossy(&ref_out.stderr)
        .replace(db_path_ref.to_str().expect("utf8"), "<CATALOG_DB>");
    assert_eq!(new_stderr, ref_stderr);
}

#[test]
fn inbox_process_output_is_byte_identical() {
    // A clean pass moves the contribution to `.processed/`, so a second
    // pass over the same inbox finds nothing left — independent catalogs,
    // one per binary, each with its own fresh inbox.
    fn seed_and_process(base: &std::path::Path) -> std::process::Output {
        let (root, state) = common::fixture_catalog(base);
        common::maj(&root, &state)
            .args(["para", "add", "project", "spring"])
            .assert()
            .success();
        let inbox = base.join("inbox");
        let drop = inbox.join("drop-1");
        std::fs::create_dir_all(&drop).expect("mkdir");
        std::fs::write(drop.join("clip.mov"), b"hello-world").expect("write");
        let xxh64 = format!("{:016x}", xxhash_rust::xxh64::xxh64(b"hello-world", 0));
        std::fs::write(
            drop.join("contribution.json"),
            serde_json::json!({
                "version": 1,
                "contributor": "dana",
                "para_target": "project/spring",
                "files": [{"name": "clip.mov", "xxh64": xxh64, "size": 11}]
            })
            .to_string(),
        )
        .expect("write manifest");
        let dest = base.join("dest");
        std::fs::create_dir_all(&dest).expect("mkdir");
        common::maj(&root, &state)
            .args([
                "inbox",
                "process",
                inbox.to_str().expect("utf8"),
                "--dest",
                dest.to_str().expect("utf8"),
                "--json",
            ])
            .output()
            .expect("run inbox process")
    }

    let dir_new = tempfile::tempdir().expect("tempdir");
    let new = seed_and_process(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let ref_out = seed_and_process(dir_ref.path());

    assert_eq!(new.status.code(), ref_out.status.code());
    assert_eq!(
        String::from_utf8_lossy(&new.stdout),
        String::from_utf8_lossy(&ref_out.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&new.stderr),
        String::from_utf8_lossy(&ref_out.stderr)
    );
}

/// `maj ingest --dry-run` never writes a journal or prints a run id, so a
/// shared root/source/dest is safe to diff byte for byte — unlike the real
/// run below.
#[test]
fn ingest_dry_run_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let source = dir.path().join("src");
    std::fs::create_dir_all(&source).expect("mkdir");
    std::fs::write(source.join("a.mov"), b"hello").expect("write");
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).expect("mkdir");
    let source_str = source.to_str().expect("utf8").to_string();
    let dest_str = dest.to_str().expect("utf8").to_string();
    for args in [
        vec![
            "ingest",
            &source_str,
            "--dest",
            &dest_str,
            "--para",
            "project/p1",
            "--dry-run",
            "--json",
        ],
        vec![
            "ingest",
            &source_str,
            "--dest",
            &dest_str,
            "--para",
            "project/p1",
            "--dry-run",
        ],
    ] {
        diff_against_ref(&root, &state, &args);
    }
}

/// A real ingest run: independent catalogs/sources/dests, one per binary.
#[cfg(test)]
fn run_real_ingest(base: &std::path::Path) -> std::process::Output {
    let (root, state) = common::fixture_catalog(base);
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let source = base.join("src");
    std::fs::create_dir_all(&source).expect("mkdir");
    std::fs::write(source.join("a.mov"), b"hello").expect("write");
    let dest = base.join("dest");
    std::fs::create_dir_all(&dest).expect("mkdir");
    common::maj(&root, &state)
        .args([
            "ingest",
            source.to_str().expect("utf8"),
            "--dest",
            dest.to_str().expect("utf8"),
            "--para",
            "project/p1",
            "--json",
        ])
        .output()
        .expect("run ingest")
}

/// `maj ingest` prints a fresh run-id ULID (in `--json`'s `"run"` field, and
/// on stderr's `run {id} — resume with: --resume {id}` line) — like `para
/// add`, this can never byte-match across two independent invocations, so
/// this checks structure instead: each binary's own ULID shape, the rest of
/// the JSON body (`"run"` blanked out) structurally equal, and stderr equal
/// after substituting each side's own run id for a shared placeholder.
#[test]
fn ingest_real_run_output_is_byte_identical() {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(ingest real run): /tmp/maj-ref missing — build it first");
        return;
    }

    let dir_new = tempfile::tempdir().expect("tempdir");
    let new = run_real_ingest(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let old = run_real_ingest(dir_ref.path());

    assert_eq!(new.status.code(), old.status.code());
    let is_ulid_char = |c: char| c.is_ascii_alphanumeric();
    let extract_run_id = |stderr: &str| -> String {
        stderr
            .strip_prefix("run ")
            .and_then(|s| s.split(' ').next())
            .filter(|token| token.len() == 26 && token.chars().all(is_ulid_char))
            .unwrap_or_default()
            .to_string()
    };
    let new_stderr = String::from_utf8_lossy(&new.stderr).into_owned();
    let old_stderr = String::from_utf8_lossy(&old.stderr).into_owned();
    let new_run_id = extract_run_id(&new_stderr);
    let old_run_id = extract_run_id(&old_stderr);
    assert_eq!(
        new_run_id.len(),
        26,
        "expected a ULID in stderr: {new_stderr:?}"
    );
    assert_eq!(
        old_run_id.len(),
        26,
        "expected a ULID in stderr: {old_stderr:?}"
    );
    assert_eq!(
        new_stderr.replacen(&new_run_id, "<ULID>", 2),
        old_stderr.replacen(&old_run_id, "<ULID>", 2),
        "stderr must match once each side's own run id is normalized"
    );

    let mut new_json: serde_json::Value =
        serde_json::from_slice(&new.stdout).expect("new stdout is JSON");
    let mut old_json: serde_json::Value =
        serde_json::from_slice(&old.stdout).expect("ref stdout is JSON");
    assert_eq!(new_json["run"].as_str(), Some(new_run_id.as_str()));
    assert_eq!(old_json["run"].as_str(), Some(old_run_id.as_str()));
    new_json["run"] = serde_json::json!("<ULID>");
    old_json["run"] = serde_json::json!("<ULID>");
    // `generations[].root` is an absolute path under each side's own
    // independent tempdir — structurally equal in shape (one entry, same
    // generation number) but never textually equal, so it's normalized too.
    for doc in [&mut new_json, &mut old_json] {
        if let Some(generations) = doc["generations"].as_array_mut() {
            for g in generations {
                g["root"] = serde_json::json!("<DEST_ROOT>");
            }
        }
    }
    assert_eq!(
        new_json, old_json,
        "ingest JSON must match once run id and dest roots are normalized"
    );
}

/// A first ingest (default `--dedupe skip`), then a second ingest of the
/// identical bytes into a fresh destination with `--dedupe copy` — for
/// [`ingest_dedupe_copy_output_is_byte_identical`]. `fixture_catalog`
/// already scanned `base/src`, so this uses its own source directory to
/// avoid colliding with those two fixture files.
#[cfg(test)]
fn run_dedupe_copy_reingest(base: &std::path::Path) -> std::process::Output {
    let (root, state) = common::fixture_catalog(base);
    common::maj(&root, &state)
        .args(["para", "add", "project", "p1"])
        .assert()
        .success();
    let source = base.join("dedupe-src");
    std::fs::create_dir_all(&source).expect("mkdir");
    std::fs::write(source.join("a.mov"), b"duplicate-me").expect("write");
    let dest1 = base.join("dest1");
    std::fs::create_dir_all(&dest1).expect("mkdir");
    common::maj(&root, &state)
        .args([
            "ingest",
            source.to_str().expect("utf8"),
            "--dest",
            dest1.to_str().expect("utf8"),
            "--para",
            "project/p1",
        ])
        .assert()
        .success();

    let dest2 = base.join("dest2");
    std::fs::create_dir_all(&dest2).expect("mkdir");
    common::maj(&root, &state)
        .args([
            "ingest",
            source.to_str().expect("utf8"),
            "--dest",
            dest2.to_str().expect("utf8"),
            "--para",
            "project/p1",
            "--dedupe",
            "copy",
            "--json",
        ])
        .output()
        .expect("run second ingest")
}

/// `--dedupe copy` is untested by the rest of the workspace suite — a
/// sabotage that treated `DedupeMode::CopyAnyway` the same as `Skip` would
/// still pass every other test. Ingests a file, then re-ingests the
/// identical bytes into a second destination with `--dedupe copy`: the
/// planner sees a known duplicate but must still place it (`placed: 1`),
/// not skip it (`skipped_duplicates: 1`) — proven identical between the two
/// binaries, modulo the second run's own fresh ULID and destination-root
/// path (same normalization as `ingest_real_run_output_is_byte_identical`).
#[test]
fn ingest_dedupe_copy_output_is_byte_identical() {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity(ingest --dedupe copy): /tmp/maj-ref missing — build it first");
        return;
    }

    let dir_new = tempfile::tempdir().expect("tempdir");
    let new = run_dedupe_copy_reingest(dir_new.path());
    let dir_ref = tempfile::tempdir().expect("tempdir");
    let old = run_dedupe_copy_reingest(dir_ref.path());

    assert_eq!(new.status.code(), Some(0), "new binary: {new:?}");
    assert_eq!(new.status.code(), old.status.code());

    let mut new_json: serde_json::Value =
        serde_json::from_slice(&new.stdout).expect("new stdout is JSON");
    let mut old_json: serde_json::Value =
        serde_json::from_slice(&old.stdout).expect("ref stdout is JSON");
    assert_eq!(
        new_json["placed"],
        serde_json::json!(1),
        "dedupe copy must place the duplicate, not skip it: {new_json}"
    );
    assert_eq!(new_json["skipped_duplicates"], serde_json::json!(0));
    assert_eq!(old_json["placed"], serde_json::json!(1));
    assert_eq!(old_json["skipped_duplicates"], serde_json::json!(0));

    new_json["run"] = serde_json::json!("<ULID>");
    old_json["run"] = serde_json::json!("<ULID>");
    for doc in [&mut new_json, &mut old_json] {
        if let Some(generations) = doc["generations"].as_array_mut() {
            for g in generations {
                g["root"] = serde_json::json!("<DEST_ROOT>");
            }
        }
    }
    assert_eq!(
        new_json, old_json,
        "ingest --dedupe copy JSON must match once run id and dest roots are normalized"
    );
}
