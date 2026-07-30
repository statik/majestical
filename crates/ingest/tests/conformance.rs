//! Two-way ASC MHL conformance against the Python reference implementation
//! (`ascmhl` on `PyPI`). Both tests are `#[ignore]`d by default — they need
//! the reference tooling installed, which `just conformance` does into a
//! throwaway venv before running `cargo test ... -- --ignored`.
//!
//! Divergence from a plain single-binary setup: the main `ascmhl` console
//! script (installed by `pip install ascmhl`) has no `verify` subcommand —
//! only `create`/`diff`/`flatten`/`info`. Hash-vs-history verification
//! without writing a new generation lives on a *separate* console script,
//! `ascmhl-debug`, installed alongside it (`ascmhl-debug verify`). So this
//! file uses two env vars: `ASCMHL_BIN` (default `ascmhl`, used to create a
//! reference history) and `ASCMHL_DEBUG_BIN` (default `ascmhl-debug`, used
//! to verify one).
use majestical_ingest::mhl::{HashAction, hash_dir, verify_dir, write_generation};
use std::path::Path;
use std::process::{Command, Output};

#[cfg(test)]
fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("clips")).expect("mkdir clips");
    std::fs::write(root.join("clips/a.mov"), b"AAAA").expect("write a.mov");
    std::fs::write(root.join("b space.wav"), b"BBBBBB").expect("write b space.wav");
}

#[cfg(test)]
fn ascmhl_bin() -> String {
    std::env::var("ASCMHL_BIN").unwrap_or_else(|_| "ascmhl".to_string())
}

#[cfg(test)]
fn ascmhl_debug_bin() -> String {
    std::env::var("ASCMHL_DEBUG_BIN").unwrap_or_else(|_| "ascmhl-debug".to_string())
}

#[cfg(test)]
fn describe(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// We write a generation; the reference tool must accept it as a valid,
/// unmodified ASC MHL history.
#[test]
#[ignore = "needs python ascmhl on PATH (CI: just conformance)"]
fn our_manifest_passes_reference_verify() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());

    let hash_list = hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
    write_generation(dir.path(), &hash_list).expect("write_generation");

    let output = Command::new(ascmhl_debug_bin())
        .arg("verify")
        .arg("-v")
        .arg(dir.path())
        .output()
        .expect("running ascmhl-debug verify — is it on PATH?");

    assert!(
        output.status.success(),
        "reference tool rejected our manifest:\n{}",
        describe(&output)
    );
}

/// The reference tool creates a generation; our `verify_dir` must read it,
/// find both files unchanged, and record them as verified.
#[test]
#[ignore = "needs python ascmhl on PATH (CI: just conformance)"]
fn reference_manifest_passes_our_verify() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());

    let output = Command::new(ascmhl_bin())
        .arg("create")
        .arg("-h")
        .arg("xxh64")
        .arg(dir.path())
        .output()
        .expect("running ascmhl create — is it on PATH?");
    assert!(
        output.status.success(),
        "reference tool failed to create a history:\n{}",
        describe(&output)
    );

    let report = verify_dir(dir.path(), "2026-07-30T00:01:00Z").expect("verify_dir");
    assert!(
        report.altered.is_empty(),
        "unexpected altered files: {:?}",
        report.altered
    );
    assert!(
        report.missing.is_empty(),
        "unexpected missing files: {:?}",
        report.missing
    );
    assert!(
        report.new_files.is_empty(),
        "unexpected new files: {:?}",
        report.new_files
    );
    let mut verified = report.verified.clone();
    verified.sort_unstable();
    assert_eq!(
        verified,
        vec!["b space.wav".to_string(), "clips/a.mov".to_string()]
    );

    // Sanity: our own written generation reads back with the same action.
    let written =
        majestical_ingest::mhl::read_generation(&report.written.path).expect("read_generation");
    for entry in &written.entries {
        assert_eq!(entry.action, HashAction::Verified);
    }
}
