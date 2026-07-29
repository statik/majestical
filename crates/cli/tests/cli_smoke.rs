//! End-to-end: init a catalog, scan a folder, tag by name-match, search.
use assert_cmd::Command;
use predicates::str::contains;

#[cfg(test)]
fn maj_as(catalog: &std::path::Path, machine_id: &str) -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", machine_id);
    c
}

#[cfg(test)]
fn maj(catalog: &std::path::Path) -> Command {
    maj_as(catalog, "test-machine")
}

/// Parses a `search --json` asset id out of the first result.
#[cfg(test)]
fn first_asset_id(out: &std::process::Output) -> String {
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    hits["results"][0]["asset"].as_str().unwrap().to_string()
}

#[test]
fn init_scan_tag_search_round_trip() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("sunset.mov"), b"fake video bytes").unwrap();
    std::fs::write(media.path().join("notes.txt"), b"hello").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success()
        .stdout(contains("2 assets"));
    // Find the asset id for sunset.mov via name search (json output).
    let out = maj(&root)
        .args(["search", "--name", "sunset", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = hits["results"][0]["asset"].as_str().unwrap().to_string();
    assert_eq!(hits["count"], 1);

    maj(&root)
        .args(["tag", "add", &id, "topic/drone"])
        .assert()
        .success();
    let out = maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    assert_eq!(hits["results"][0]["asset"], id);

    maj(&root)
        .args(["tag", "rm", &id, "topic/drone"])
        .assert()
        .success();
    let out = maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 0);
    assert_eq!(hits["results"].as_array().unwrap().len(), 0);
}

/// A torn write or damaged transport must not take down the catalog: the
/// corrupt line is skipped, the rest of the data still comes back correct
/// on stdout, and the user is warned on stderr rather than losing the fact
/// silently.
#[test]
fn corrupt_log_line_is_skipped_and_reported_on_stderr() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("sunset.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();

    let seg = root.join("events/test-machine/0001.jsonl");
    let mut contents = std::fs::read_to_string(&seg).unwrap();
    contents.push_str("not valid json\n");
    std::fs::write(&seg, contents).unwrap();

    let assert = maj(&root)
        .args(["search", "--name", "sunset", "--json"])
        .assert()
        .success();
    let out = assert.get_output();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: skipped 1 corrupt event log line(s)"),
        "expected corrupt-line warning on stderr, got: {stderr}"
    );
}

/// Two machines sharing one catalog root, exercised through the real CLI
/// and filesystem event log (not the in-memory cucumber harness): each
/// machine's edits become visible to the other through independent
/// invocations, and a cross-machine tag remove converges correctly.
#[test]
fn two_machines_converge_through_shared_catalog_root() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj_as(&root, "machine-a")
        .args(["catalog", "init"])
        .assert()
        .success();
    maj_as(&root, "machine-a")
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj_as(&root, "machine-a")
        .args(["search", "--name", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);
    maj_as(&root, "machine-a")
        .args(["tag", "add", &id, "tag/a"])
        .assert()
        .success();

    // machine-b, in a separate process, sees machine-a's asset and tag.
    let out = maj_as(&root, "machine-b")
        .args(["search", "--tag", "tag/a", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    assert_eq!(hits["results"][0]["asset"], id);
    maj_as(&root, "machine-b")
        .args(["tag", "add", &id, "tag/b"])
        .assert()
        .success();

    // machine-a removes machine-b's tag, citing the add-ids it observes
    // via the merged projection.
    maj_as(&root, "machine-a")
        .args(["tag", "rm", &id, "tag/b"])
        .assert()
        .success();

    for machine in ["machine-a", "machine-b"] {
        let out = maj_as(&root, machine)
            .args(["search", "--tag", "tag/a", "--json"])
            .output()
            .unwrap();
        let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(hits["count"], 1, "{machine} should still see tag/a");

        let out = maj_as(&root, machine)
            .args(["search", "--tag", "tag/b", "--json"])
            .output()
            .unwrap();
        let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(hits["count"], 0, "{machine} should see tag/b removed");
    }

    let events_dir = root.join("events");
    assert!(events_dir.join("machine-a").is_dir());
    assert!(events_dir.join("machine-b").is_dir());
}
