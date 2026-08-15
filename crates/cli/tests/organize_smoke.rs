//! End-to-end: `maj tags list`, `maj tag rename`/`merge`/`assign`, and
//! `maj para file` through the real CLI. Compute is
//! `crates/services/src/tags.rs`/`para.rs`'s own unit-test territory; this
//! suite exercises the CLI's arg parsing, rendering, and error surfacing —
//! the same split `browse_smoke.rs` draws for `maj browse`.
mod common;

use common::{asset_id_of, fixture_catalog, maj};
use predicates::str::contains;

/// `fixture_catalog` seeds `a.txt`/`b.txt` on `vol1` with `a.txt` already
/// tagged `demo`. Returns the catalog root, its state dir, and both assets'
/// ids, resolved once via `search --json` so no test hardcodes a hash.
#[cfg(test)]
fn seeded() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
    String,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, state) = fixture_catalog(tmp.path());
    let a = asset_id_of(&root, &state, "a.txt");
    let b = asset_id_of(&root, &state, "b.txt");
    (tmp, root, state, a, b)
}

#[test]
fn tags_list_json_reports_tag_count_and_last_used() {
    let (_tmp, root, state, _a, b) = seeded();
    maj(&root, &state)
        .args(["tag", "add", &b, "demo"])
        .assert()
        .success();

    let out = maj(&root, &state)
        .args(["tags", "list", "--json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let tags = parsed["tags"].as_array().expect("tags array");
    assert_eq!(tags.len(), 1, "{parsed}");
    assert_eq!(tags[0]["tag"], serde_json::json!("demo"), "{parsed}");
    assert_eq!(tags[0]["count"], serde_json::json!(2), "{parsed}");
    assert!(tags[0]["last_used_ms"].is_u64(), "{parsed}");
}

/// Pins the COUNT column, not just the tag name and a date substring: a
/// prior version of this test only checked `contains("demo")`/`contains("T"
/// )`/`contains("Z")`, which stays green even if `print_tags_table` drops
/// the count column entirely (the header text and the date still appear
/// somewhere in stdout either way). Parsing the header and the data row into
/// tokens and checking the actual count value (fixture `demo` = 1) closes
/// that gap.
#[test]
fn tags_list_human_shows_the_count_column_with_the_real_count() {
    let (_tmp, root, state, _a, _b) = seeded();
    let out = maj(&root, &state)
        .args(["tags", "list"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let mut lines = stdout.lines();
    let header = lines.next().expect("header line");
    assert!(
        header.contains("COUNT"),
        "header must name the count column: {header:?}"
    );
    let row = lines
        .find(|line| line.starts_with("demo"))
        .unwrap_or_else(|| panic!("no row for 'demo': {stdout:?}"));
    let fields: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(fields[0], "demo", "{row:?}");
    assert_eq!(
        fields[1], "1",
        "the row's second column must be the real count, not the date: {row:?}"
    );
    // last_used_ms renders through the CLI's iso8601 helper, which always
    // carries a "T" separator and a "Z" suffix — checked on the same row,
    // not just anywhere in stdout, so it can't be satisfied by the count
    // column alone.
    assert!(
        fields[2].contains('T') && fields[2].ends_with('Z'),
        "{row:?}"
    );
}

#[test]
fn tags_list_of_an_empty_catalog_is_success_with_no_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let out = maj(&root, &state)
        .args(["tags", "list", "--json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["tags"], serde_json::json!([]), "{parsed}");
}

#[test]
fn tag_rename_renders_the_summary_and_updates_the_vocabulary() {
    let (_tmp, root, state, _a, b) = seeded();
    maj(&root, &state)
        .args(["tag", "add", &b, "demo"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tag", "rename", "demo", "reviewed"])
        .assert()
        .success()
        .stdout(contains(
            "renamed 'demo' to 'reviewed' — rewrote 2 asset(s)",
        ));

    let out = maj(&root, &state)
        .args(["tags", "list", "--json"])
        .output()
        .expect("run");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let tags: Vec<&str> = parsed["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .map(|t| t["tag"].as_str().unwrap())
        .collect();
    assert_eq!(tags, vec!["reviewed"], "{parsed}");

    let out = maj(&root, &state)
        .args(["search", "tag:reviewed", "--json"])
        .output()
        .expect("run");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(hits["count"], serde_json::json!(2), "{hits}");
}

#[test]
fn tag_rename_of_an_unknown_tag_errors_with_the_service_message_verbatim() {
    let (_tmp, root, state, _a, _b) = seeded();
    maj(&root, &state)
        .args(["tag", "rename", "nope", "reviewed"])
        .assert()
        .failure()
        .stderr(contains("no tag 'nope'"))
        .stderr(contains("maj tags list"));
}

#[test]
fn tag_merge_renders_the_summary_and_unions_the_assets() {
    let (_tmp, root, state, _a, b) = seeded();
    maj(&root, &state)
        .args(["tag", "add", &b, "keeper"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tag", "merge", "demo", "keeper"])
        .assert()
        .success()
        .stdout(contains("merged 'demo' into 'keeper' — rewrote 1 asset(s)"));

    let out = maj(&root, &state)
        .args(["tags", "list", "--json"])
        .output()
        .expect("run");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let tags = parsed["tags"].as_array().expect("tags array");
    assert_eq!(tags.len(), 1, "{parsed}");
    assert_eq!(tags[0]["tag"], serde_json::json!("keeper"), "{parsed}");
    assert_eq!(tags[0]["count"], serde_json::json!(2), "{parsed}");
}

#[test]
fn tag_merge_into_a_tag_nothing_carries_errors_pointing_at_tag_rename() {
    let (_tmp, root, state, _a, _b) = seeded();
    maj(&root, &state)
        .args(["tag", "merge", "demo", "nope"])
        .assert()
        .failure()
        .stderr(contains("maj tag rename"));
}

#[test]
fn tag_assign_applies_a_bulk_tag_set_across_assets() {
    let (_tmp, root, state, a, b) = seeded();
    maj(&root, &state)
        .args(["tag", "assign", "--tag", "x", "--tag", "y", &a, &b])
        .assert()
        .success()
        .stdout(contains("applied 4 tag assignment(s)"));

    let out = maj(&root, &state)
        .args(["search", "tag:x", "--json"])
        .output()
        .expect("run");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(hits["count"], serde_json::json!(2), "{hits}");
}

#[test]
fn tag_assign_reports_an_unknown_asset_as_a_failed_line_and_still_applies_known_ones() {
    let (_tmp, root, state, a, _b) = seeded();
    let out = maj(&root, &state)
        .args([
            "tag",
            "assign",
            "--tag",
            "z",
            &a,
            "xxh3:ffffffffffffffffffffffffffffffff",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).expect("utf8");
    assert!(stdout.contains("applied 1 tag assignment(s)"), "{stdout}");
    assert!(
        stdout.contains("FAILED xxh3:ffffffffffffffffffffffffffffffff:")
            && stdout.contains("unknown asset"),
        "{stdout}"
    );
}

/// All-failed policy: when every requested asset fails, `tags_assign`
/// itself errors rather than reporting an `Ok` with `applied: 0` — so `maj
/// tag assign` exits nonzero, unlike the partial-failure case above (one
/// known asset, exit 0).
#[test]
fn tag_assign_exits_nonzero_when_every_requested_asset_fails() {
    let (_tmp, root, state, _a, _b) = seeded();
    maj(&root, &state)
        .args([
            "tag",
            "assign",
            "--tag",
            "z",
            "xxh3:ffffffffffffffffffffffffffffffff",
        ])
        .assert()
        .failure()
        .stderr(contains("every requested asset"));
}

#[test]
fn para_file_bulk_files_multiple_assets_under_one_node() {
    let (_tmp, root, state, a, b) = seeded();
    maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["para", "file", "project/client-x", &a, &b])
        .assert()
        .success()
        .stdout(contains("filed 2 asset(s) to project/client-x"));

    let out = maj(&root, &state)
        .args(["search", "para:project/client-x", "--json"])
        .output()
        .expect("run");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(hits["count"], serde_json::json!(2), "{hits}");
}

#[test]
fn para_file_reports_an_unknown_asset_as_a_failed_line_and_still_files_known_ones() {
    let (_tmp, root, state, a, _b) = seeded();
    maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    let out = maj(&root, &state)
        .args([
            "para",
            "file",
            "project/client-x",
            &a,
            "xxh3:ffffffffffffffffffffffffffffffff",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).expect("utf8");
    assert!(
        stdout.contains("filed 1 asset(s) to project/client-x"),
        "{stdout}"
    );
    assert!(
        stdout.contains("FAILED xxh3:ffffffffffffffffffffffffffffffff:")
            && stdout.contains("unknown asset"),
        "{stdout}"
    );
}

/// All-failed policy, `maj para file`'s half — same shape as
/// `tag_assign_exits_nonzero_when_every_requested_asset_fails`.
#[test]
fn para_file_exits_nonzero_when_every_requested_asset_fails() {
    let (_tmp, root, state, _a, _b) = seeded();
    maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    maj(&root, &state)
        .args([
            "para",
            "file",
            "project/client-x",
            "xxh3:ffffffffffffffffffffffffffffffff",
        ])
        .assert()
        .failure()
        .stderr(contains("every requested asset"));
}

#[test]
fn para_file_into_an_unknown_node_is_a_hard_error_and_files_nothing() {
    let (_tmp, root, state, a, _b) = seeded();
    maj(&root, &state)
        .args(["para", "file", "project/nope", &a])
        .assert()
        .failure()
        .stderr(contains("no active PARA node"));

    let out = maj(&root, &state)
        .args(["search", &a, "--json"])
        .output()
        .expect("run");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(
        hits["results"][0]["para"],
        serde_json::Value::Null,
        "an unresolvable node must not file the asset: {hits}"
    );
}
