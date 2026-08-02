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
