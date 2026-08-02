//! `crates/cli/tests/services_parity.rs` — byte-identical CLI output through
//! the services extraction, proven by diffing this build's stdout against
//! the pre-extraction reference binary (`/tmp/maj-ref`, built at each PR
//! chunk's start). Skips (with a loud message) when the reference is
//! absent — CI rebuilds it in the job.
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
        (String::from_utf8_lossy(&new.stdout), new.status.code()),
        (String::from_utf8_lossy(&old.stdout), old.status.code()),
        "stdout/exit diverged for {args:?}"
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
