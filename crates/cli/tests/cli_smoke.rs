//! End-to-end: init a catalog, scan a folder, tag by name-match, search.
use assert_cmd::Command;
use predicates::str::contains;

#[cfg(test)]
fn maj(catalog: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", "test-machine");
    c
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
    maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .assert()
        .success()
        .stdout(contains(&id));
    maj(&root)
        .args(["tag", "rm", &id, "topic/drone"])
        .assert()
        .success();
    maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .assert()
        .success()
        .stdout(contains("\"count\":0"));
}
