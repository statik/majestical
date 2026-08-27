//! End-to-end: `maj doctor` through the real CLI — the JSON shape, the
//! human rendering's stable "no catalog" line, exit-code polarity (0 even
//! when every row warns), and that passing `--catalog` actually changes the
//! catalog-dependent rows. Doctor's own compute (which checks run, what each
//! one reports) is `crates/services/src/doctor.rs`'s own unit-test
//! fixture's concern, not this suite's — this exercises the CLI's arg
//! parsing and rendering only.
mod common;

use common::maj;
use predicates::str::contains;

/// A `maj doctor` invocation built through `common::maj`'s helper, purely to
/// reuse its `Command`-building convenience. `maj()`'s env vars
/// (`MAJ_CATALOG`/`MAJ_MACHINE_ID`/`MAJ_STATE_DIR`) are set but ignored here:
/// doctor takes its own local, optional `--catalog`, independent of those
/// top-level ones, and no test in this file passes it — nothing below
/// actually reads what `maj()` set. For the case where those vars are
/// genuinely absent rather than merely set-and-ignored, see
/// `doctor_runs_with_neither_catalog_nor_machine_id_configured_at_all`.
fn maj_doctor() -> assert_cmd::Command {
    use std::path::Path;
    let mut cmd = maj(
        Path::new("/never/touched/catalog"),
        Path::new("/never/touched/state"),
    );
    cmd.arg("doctor");
    cmd
}

#[test]
fn doctor_json_prints_a_nonempty_checks_array_with_the_wire_fields() {
    let out = maj_doctor().arg("--json").output().expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let checks = parsed["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty(), "{parsed}");
    for check in checks {
        assert!(check["name"].is_string(), "{check}");
        assert!(check["status"].is_string(), "{check}");
        assert!(check["detail"].is_string(), "{check}");
    }
}

#[test]
fn doctor_human_shows_a_stable_warn_line_for_no_catalog() {
    maj_doctor()
        .assert()
        .success()
        .stdout(contains("catalog"))
        .stdout(contains("WARN"))
        .stdout(contains("remedy:"));
}

/// A completely bare invocation — neither `--catalog`/`--machine-id` nor
/// their `MAJ_CATALOG`/`MAJ_MACHINE_ID` env vars set at all — is exactly
/// what a fresh install with no configured catalog looks like. Doctor's
/// whole purpose is diagnosing that state, so unlike every other verb (see
/// `volumes_list_without_catalog_or_machine_id_names_the_remedy` in
/// `cli_smoke.rs`), it must still run rather than fail at dispatch.
#[test]
fn doctor_runs_with_neither_catalog_nor_machine_id_configured_at_all() {
    let out = assert_cmd::Command::cargo_bin("maj")
        .expect("bin")
        .env_remove("MAJ_CATALOG")
        .env_remove("MAJ_MACHINE_ID")
        .arg("doctor")
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("WARN"),
        "{out:?}"
    );

    assert_cmd::Command::cargo_bin("maj")
        .expect("bin")
        .env_remove("MAJ_CATALOG")
        .env_remove("MAJ_MACHINE_ID")
        .args(["doctor", "--json"])
        .assert()
        .success();
}

/// Exit-code polarity pin: every row can warn (or fail) and the process
/// still exits 0 — findings are rows, not CLI errors. Covers both `--json`
/// and human rendering, since each goes through a separate `println!` path
/// in `commands::cmd_doctor`.
#[test]
fn doctor_exits_zero_even_though_the_catalog_row_warns() {
    maj_doctor().assert().success();
    maj_doctor().arg("--json").assert().success();
}

#[test]
fn doctor_catalog_flag_reports_ok_for_a_real_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (root, state) = common::fixture_catalog(dir.path());
    let out = maj(&root, &state)
        .args([
            "doctor",
            "--catalog",
            root.to_str().expect("utf8"),
            "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let checks = parsed["checks"].as_array().expect("checks array");
    let catalog_row = checks
        .iter()
        .find(|c| c["name"] == serde_json::json!("catalog"))
        .unwrap_or_else(|| panic!("no `catalog` row in {parsed}"));
    assert_eq!(
        catalog_row["status"],
        serde_json::json!("ok"),
        "{catalog_row}"
    );
}
