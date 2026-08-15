//! End-to-end: `maj browse tree`/`maj browse list` through the real CLI —
//! folder tree counts, flatten on/off, sort (including the default
//! "captured" order and "size"), pagination, and the unknown-volume error.
//! Mirrors `crates/services/src/browse.rs`'s own unit-test fixture (three
//! files on one volume: `A/x.mov`, `A/B/y.jpg`, `C/z.pdf`) but scanned from
//! real files on disk instead of synthetic events, so this suite exercises
//! the CLI's arg parsing and rendering, not the compute already covered
//! there. The three files get distinct sizes and (backdated) distinct
//! mtimes so every `--sort` value has a real, non-degenerate order to pin.
mod common;

use common::maj;
use predicates::str::contains;
use std::path::PathBuf;

/// A volume label unlikely to collide with a real `/Volumes/<label>` mount
/// on the runner — `volume_is_online` reads any volume with a nonexistent
/// mount as offline, so this fixture's volume reads offline deterministically
/// in CI, the same trick `crates/services/src/browse.rs`'s own tests use.
#[cfg(test)]
const VOLUME: &str = "browse-e2e-vol-xyz";

/// Seeds a catalog with the shared fixture layout: `A/x.mov` (9 bytes,
/// oldest), `A/B/y.jpg` (6 bytes, middle), `C/z.pdf` (18 bytes, newest), all
/// on [`VOLUME`]. Every file gets a distinct, deterministic mtime an hour
/// apart — backdated with `filetime` (the same mechanism
/// `inbox_smoke.rs`'s quiescence tests use; std has no portable way to set
/// an mtime before Rust 1.75, and this repo's convention already leans on
/// this crate) before `scan` ever reads the directory, so `scan`'s real
/// `AssetSeen.mtime_ms` — not a synthetic value — carries the distinct
/// times. Without this, all three files would share one filesystem-clock
/// tick and the default "captured" sort would silently degenerate to the
/// asset-id tiebreak instead of exercising mtime ordering. Sizes are
/// already distinct by construction (9/6/18 bytes), which doubles as the
/// `--sort size` fixture. Returns the catalog root and its isolated state
/// dir.
#[cfg(test)]
fn seeded() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("cat");
    let state = tmp.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("A/B")).expect("mkdir A/B");
    std::fs::create_dir_all(src.join("C")).expect("mkdir C");
    let x = src.join("A/x.mov");
    let y = src.join("A/B/y.jpg");
    let z = src.join("C/z.pdf");
    std::fs::write(&x, b"hello-mov").expect("write x.mov");
    std::fs::write(&y, b"hi-jpg").expect("write y.jpg");
    std::fs::write(&z, b"pdf-content-longer").expect("write z.pdf");
    let now = std::time::SystemTime::now();
    backdate(&x, now - std::time::Duration::from_hours(3));
    backdate(&y, now - std::time::Duration::from_hours(2));
    backdate(&z, now - std::time::Duration::from_hours(1));
    maj(&root, &state)
        .args(["scan", src.to_str().expect("utf8"), "--volume", VOLUME])
        .assert()
        .success();
    (tmp, root, state)
}

#[cfg(test)]
fn backdate(path: &std::path::Path, at: std::time::SystemTime) {
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(at))
        .expect("backdate mtime");
}

#[cfg(test)]
fn folder<'a>(volume: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    volume["folders"]
        .as_array()
        .expect("folders array")
        .iter()
        .find(|f| f["path"] == serde_json::json!(path))
        .unwrap_or_else(|| panic!("no folder '{path}' in {volume}"))
}

#[test]
fn browse_tree_json_reports_exact_folder_structure() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args(["browse", "tree", "--json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let volumes = parsed["volumes"].as_array().expect("volumes array");
    let v = volumes
        .iter()
        .find(|v| v["id"] == serde_json::json!(VOLUME))
        .expect("fixture volume present");
    assert_eq!(v["online"], serde_json::json!(false), "{v}");

    assert_eq!(folder(v, "")["recursive_count"], serde_json::json!(3));
    assert_eq!(
        folder(v, "")["children"],
        serde_json::json!(["A", "C"]),
        "{v}"
    );
    assert_eq!(folder(v, "A")["recursive_count"], serde_json::json!(2));
    assert_eq!(folder(v, "A")["children"], serde_json::json!(["B"]), "{v}");
    assert_eq!(folder(v, "A/B")["recursive_count"], serde_json::json!(1));
    assert_eq!(folder(v, "C")["recursive_count"], serde_json::json!(1));
}

#[test]
fn browse_tree_human_shows_a_stable_folder_line_with_its_count() {
    let (_tmp, root, state) = seeded();
    maj(&root, &state)
        .args(["browse", "tree"])
        .assert()
        .success()
        .stdout(contains(VOLUME))
        .stdout(contains("offline"))
        .stdout(contains("A/B  1"))
        .stdout(contains("C  1"));
}

/// Flatten's scope concern only — count and `folder_count`, and that both
/// `A/x.mov` and `A/B/y.jpg` are present, order-agnostic. `--sort name`'s
/// own ordering (and the exact per-row fields it doesn't disturb) is
/// `browse_list_sort_name_is_ascending_with_exact_row_fields`'s concern,
/// not this test's — keeping the two apart means a broken `--sort` can't
/// hide behind "the flatten test still passed" the way it could when both
/// concerns lived in one assertion block.
#[test]
fn browse_list_json_flatten_scopes_the_whole_subtree() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--path", "A", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(2), "{parsed}");
    assert_eq!(parsed["folder_count"], serde_json::json!(2), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    let mut names: Vec<&str> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["x.mov", "y.jpg"],
        "flatten includes both A/x.mov and A/B/y.jpg: {parsed}"
    );
}

/// `--sort name`: ascending by filename, with each row's other fields
/// (`size`/`kind`/`mtime_ms`) pinned exactly — the CLI's own `--sort` arg wiring,
/// split out from the flatten-scoping concern above so the two can't mask
/// each other's regressions.
#[test]
fn browse_list_sort_name_is_ascending_with_exact_row_fields() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--path", "A", "--sort", "name", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let results = parsed["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2, "{parsed}");
    // --sort name sorts by filename ascending: "x.mov" < "y.jpg".
    let x = &results[0];
    assert_eq!(x["name"], serde_json::json!("x.mov"), "{parsed}");
    assert_eq!(x["size"], serde_json::json!(9), "{parsed}");
    assert_eq!(x["kind"], serde_json::json!("video"), "{parsed}");
    assert!(x["mtime_ms"].is_u64(), "{parsed}");
    let y = &results[1];
    assert_eq!(y["name"], serde_json::json!("y.jpg"), "{parsed}");
    assert_eq!(y["size"], serde_json::json!(6), "{parsed}");
    assert_eq!(y["kind"], serde_json::json!("image"), "{parsed}");
    assert!(y["mtime_ms"].is_u64(), "{parsed}");
}

/// Default sort ("captured", i.e. no `--sort` at all) at the volume root:
/// newest `mtime` first. The fixture's three files are backdated an hour
/// apart specifically so this order is unambiguous — z.pdf (1h old) before
/// y.jpg (2h old) before x.mov (3h old) — rather than falling back to
/// whatever tiebreak the id happens to produce.
#[test]
fn browse_list_default_sort_is_captured_newest_first() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args(["browse", "list", "--volume", VOLUME, "--json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(3), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    let names: Vec<&str> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["z.pdf", "y.jpg", "x.mov"],
        "newest mtime first with no --sort given: {parsed}"
    );
}

/// `--sort size`: largest first. The fixture's three files have distinct
/// sizes by construction (18/9/6 bytes), so this order is unambiguous too.
#[test]
fn browse_list_sort_size_is_largest_first() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--sort", "size", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(3), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    let names: Vec<&str> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["z.pdf", "x.mov", "y.jpg"],
        "largest size (18, 9, 6 bytes) first: {parsed}"
    );
}

#[test]
fn browse_list_no_flatten_changes_the_count() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse",
            "list",
            "--volume",
            VOLUME,
            "--path",
            "A",
            "--no-flatten",
            "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(
        parsed["count"],
        serde_json::json!(1),
        "--no-flatten excludes A/B/y.jpg, unlike the flattened count of 2: {parsed}"
    );
    let results = parsed["results"].as_array().expect("results array");
    assert_eq!(results[0]["name"], serde_json::json!("x.mov"), "{parsed}");
}

#[test]
fn browse_list_human_shows_count_line_and_rows() {
    let (_tmp, root, state) = seeded();
    maj(&root, &state)
        .args(["browse", "list", "--volume", VOLUME, "--path", "C"])
        .assert()
        .success()
        .stdout(contains("1 items across 1 folders"))
        // The row leads with the asset id (matching search.rs's row style)
        // so a browsed row alone is enough to drive `maj meta`/`maj tag`/
        // `maj para` — every asset id in this catalog is xxh3-prefixed.
        .stdout(contains("xxh3:"))
        .stdout(contains("z.pdf"))
        .stdout(contains("pdf"));
}

/// In `--json` mode, `outcome.notices` appears in BOTH channels: the JSON
/// payload's own `notices` field (browse's as-is JSON policy — see
/// `cmd_browse_tree`'s doc in `commands.rs`) and stderr (the house
/// `print_notices` convention every read verb follows regardless of
/// `--json`). Pinning both means a future "de-duplication" that moves
/// `print_notices` inside the `else` branch — silencing stderr specifically
/// in JSON mode — fails this test instead of shipping quietly.
#[test]
fn browse_list_json_offline_notice_appears_on_stderr_and_in_payload() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args(["browse", "list", "--volume", VOLUME, "--json"])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains(VOLUME) && stderr.contains("offline"),
        "stderr must carry the offline notice: {stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let notices = parsed["notices"]
        .as_array()
        .expect("notices array present in the JSON payload");
    assert!(
        notices.iter().any(|n| n
            .as_str()
            .is_some_and(|s| s.contains(VOLUME) && s.contains("offline"))),
        "the same offline notice must also ride the JSON payload: {parsed}"
    );
}

#[test]
fn browse_list_unknown_volume_names_it_and_the_remedy() {
    let (_tmp, root, state) = seeded();
    maj(&root, &state)
        .args(["browse", "list", "--volume", "no-such-volume"])
        .assert()
        .failure()
        .stderr(contains("no-such-volume"))
        .stderr(contains("maj volumes list"));
}

#[test]
fn browse_list_kind_filter_matches_only_that_kind() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--kind", "pdf", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(1), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    assert_eq!(results[0]["name"], serde_json::json!("z.pdf"), "{parsed}");
}

/// `--limit 1 --offset 1` against the default (captured, newest-first)
/// order — z.pdf, y.jpg, x.mov — must return exactly the second row
/// (y.jpg), and `count` must stay pre-pagination (3), not shrink to the
/// page size. A swapped skip/take (offset used as the take count, or vice
/// versa) would return x.mov or the wrong-length page here.
#[test]
fn browse_list_limit_and_offset_paginate_after_sorting() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--limit", "1", "--offset", "1", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(3), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "{parsed}");
    assert_eq!(results[0]["name"], serde_json::json!("y.jpg"), "{parsed}");
}

/// `--limit 0` is a valid (if useless) page size, not an error: it must
/// exit zero with an empty page and the real, pre-pagination count still
/// intact — never treated as "no limit" by an off-by-one in the take/skip
/// wiring.
#[test]
fn browse_list_limit_zero_is_success_with_an_empty_page() {
    let (_tmp, root, state) = seeded();
    let out = maj(&root, &state)
        .args([
            "browse", "list", "--volume", VOLUME, "--limit", "0", "--json",
        ])
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["count"], serde_json::json!(3), "{parsed}");
    let results = parsed["results"].as_array().expect("results array");
    assert!(results.is_empty(), "{parsed}");
}
