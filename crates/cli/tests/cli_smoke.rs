//! End-to-end: init a catalog, scan a folder, tag by name-match, search.
use assert_cmd::Command;
use predicates::str::{contains, diff};

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
    assert_eq!(hits["count"], 1);
    let id = first_asset_id(&out);

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

/// A peer's clock reporting a timestamp far past physical now must not be
/// adopted outright: the HLC clamps it (see `clock.rs`'s own tests for the
/// clamp behavior), and the CLI surfaces a single warning on stderr rather
/// than letting the poisoned event's ordering silently take over.
///
/// The poisoned event is an `AssetSeen` rather than a `TagAdd` so that
/// `xxh3:deadbeef` has a recorded instance — `tag add` validates that the
/// asset is known, and this scenario is about clock poisoning, not that
/// validation, so the asset must actually be "scanned" (here, by the
/// planted peer event rather than a real scan).
#[test]
fn far_future_peer_event_triggers_clamp_warning() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    // Hand-write a segment for a peer machine "peerbad" whose HLC is ~400
    // days ahead of physical now, bypassing the CLI entirely so no local
    // process ever produced this timestamp.
    let peer_dir = root.join("events/peerbad");
    std::fs::create_dir_all(&peer_dir).unwrap();
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let future_ms = now_ms + 400 * 24 * 60 * 60 * 1000;
    let poisoned = majestical_core::event::Event {
        id: majestical_core::event::EventId(ulid::Ulid::from_parts(1, 1)),
        hlc: majestical_core::clock::Hlc {
            wall_ms: future_ms,
            counter: 0,
            machine: majestical_core::clock::MachineId("peerbad".into()),
        },
        author: "peerbad".into(),
        op: majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId("xxh3:deadbeef".into()),
            volume: "peerbad-volume".into(),
            path: "poison.mov".into(),
            size: 1,
        },
    };
    let line = serde_json::to_string(&poisoned).unwrap();
    std::fs::write(peer_dir.join("0001.jsonl"), format!("{line}\n")).unwrap();

    let assert = maj(&root)
        .args(["tag", "add", "xxh3:deadbeef", "poison"])
        .assert()
        .success();
    let out = assert.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("warning"),
        "clamp warning leaked onto stdout: {stdout}"
    );
    assert!(
        stderr.contains("timestamps more than 24h in the future"),
        "expected clamp warning on stderr, got: {stderr}"
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

/// A scan with an explicit `--volume` shows up in `volumes list --json`
/// with that id as both id and label, the right asset count, and offline.
/// The volume name is deliberately implausible so a real mounted volume
/// on the test machine can never coincide with it and flip `online` true.
#[test]
fn volumes_list_shows_explicit_volume_with_asset_count() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"fake video bytes a").unwrap();
    std::fs::write(media.path().join("b.mov"), b"fake video bytes b").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "maj-test-no-such-volume"])
        .assert()
        .success();

    let out = maj(&root)
        .args(["volumes", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let volumes = parsed["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0]["id"], "maj-test-no-such-volume");
    assert_eq!(volumes[0]["label"], "maj-test-no-such-volume");
    assert_eq!(volumes[0]["asset_count"], 2);
    assert_eq!(volumes[0]["online"], false);
    assert_eq!(volumes[0]["clock_suspect"], false);
}

/// A scan without `--volume` auto-detects the volume's physical identity.
/// The exact id is machine-specific (a `VolumeUUID` on macOS, a mount-point
/// label elsewhere), so this only asserts the shape: non-empty, prefixed
/// `uuid:` or `label:`.
#[test]
fn volumes_list_shows_auto_detected_volume() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();

    let out = maj(&root)
        .args(["volumes", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let volumes = parsed["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    let id = volumes[0]["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert!(
        id.starts_with("uuid:") || id.starts_with("label:"),
        "expected an auto-detected id prefix, got {id}"
    );
}

/// A `VolumeSeen` whose HLC carries a timestamp far past physical now (a
/// poisoned or misconfigured peer clock) must never silently win the
/// last-seen display: `volumes list` flags it rather than showing a
/// plausible-looking date forever. The HLC clamp only bounds the *local*
/// clock's adoption of such a timestamp — it does not sanitize what's
/// already durable in the event log — so this is display-layer detection,
/// hand-written the same way `far_future_peer_event_triggers_clamp_warning`
/// bypasses the CLI to plant the poisoned event directly.
#[test]
fn volumes_list_flags_a_far_future_last_seen_as_clock_suspect() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    let peer_dir = root.join("events/peerbad");
    std::fs::create_dir_all(&peer_dir).unwrap();
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let future_ms = now_ms + 400 * 24 * 60 * 60 * 1000;
    let poisoned = majestical_core::event::Event {
        id: majestical_core::event::EventId(ulid::Ulid::from_parts(1, 1)),
        hlc: majestical_core::clock::Hlc {
            wall_ms: future_ms,
            counter: 0,
            machine: majestical_core::clock::MachineId("peerbad".into()),
        },
        author: "peerbad".into(),
        op: majestical_core::event::Op::VolumeSeen {
            volume: "card1".into(),
            label: "card1".into(),
        },
    };
    let line = serde_json::to_string(&poisoned).unwrap();
    std::fs::write(peer_dir.join("0001.jsonl"), format!("{line}\n")).unwrap();

    let out = maj(&root)
        .args(["volumes", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let volumes = parsed["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0]["clock_suspect"], true);

    maj(&root)
        .args(["volumes", "list"])
        .assert()
        .success()
        .stdout(contains("(clock suspect)"));
}

/// Any command against a catalog directory that was never initialized fails
/// fast with a clear message, rather than silently creating an empty
/// catalog on a typo'd path.
#[test]
fn commands_against_an_uninitialized_catalog_fail_with_a_clear_message() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root)
        .args(["search", "--name", "anything", "--json"])
        .assert()
        .failure()
        .stderr(contains("no catalog at"))
        .stderr(contains("maj catalog init"));
}

/// After `catalog init`, commands against that same root succeed.
#[test]
fn commands_succeed_after_catalog_init() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["search", "--name", "anything", "--json"])
        .assert()
        .success();
}

/// `tag add` on an asset id that was never scanned (no recorded instances)
/// fails, and leaves the catalog unchanged — a subsequent search for the
/// tag finds nothing.
#[test]
fn tag_add_on_an_unscanned_asset_fails_and_leaves_the_catalog_unchanged() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    maj(&root)
        .args(["tag", "add", "xxh3:neverseen", "some/tag"])
        .assert()
        .failure()
        .stderr(contains("unknown asset xxh3:neverseen"));

    let out = maj(&root)
        .args(["search", "--tag", "some/tag", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 0);
}

/// Events carry the `--author`/`MAJ_AUTHOR` identity, distinct from the
/// machine id, in the raw event log line on disk.
#[test]
fn emitted_events_carry_the_configured_author() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .env("MAJ_AUTHOR", "elliot")
        .assert()
        .success();

    let seg = root.join("events/test-machine/0001.jsonl");
    let contents = std::fs::read_to_string(&seg).unwrap();
    assert!(
        contents
            .lines()
            .all(|line| line.contains(r#""author":"elliot""#)),
        "expected every event to carry author \"elliot\", got: {contents}"
    );
}

/// The default author (no `--author`/`MAJ_AUTHOR`) is the machine id.
#[test]
fn author_defaults_to_the_machine_id() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
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
    let contents = std::fs::read_to_string(&seg).unwrap();
    assert!(
        contents
            .lines()
            .all(|line| line.contains(r#""author":"test-machine""#)),
        "expected every event to carry the default author \"test-machine\", got: {contents}"
    );
}

/// `meta set` then `meta get` round trips a single field's value.
#[test]
fn meta_set_get_round_trip() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj(&root)
        .args(["search", "--name", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);

    maj(&root)
        .args(["meta", "set", &id, "rating", "5"])
        .assert()
        .success();
    maj(&root)
        .args(["meta", "get", &id, "rating"])
        .assert()
        .success()
        .stdout(diff("5\n"));

    // Getting every field (no field name) lists it as `field\tvalue` lines.
    maj(&root)
        .args(["meta", "get", &id])
        .assert()
        .success()
        .stdout(diff("rating\t5\n"));
}

/// Getting a field that was never set prints an empty line in text mode,
/// or `{"field":null}` in JSON — "not set yet" is not an error, mirroring
/// `search`'s zero-hits style.
#[test]
fn meta_get_on_a_missing_field_is_empty_not_an_error() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("clip.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj(&root)
        .args(["search", "--name", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);

    maj(&root)
        .args(["meta", "get", &id, "rating"])
        .assert()
        .success()
        .stdout(diff("\n"));
    maj(&root)
        .args(["meta", "get", &id, "rating", "--json"])
        .assert()
        .success()
        .stdout(diff("{\"rating\":null}\n"));
}

/// `meta set` on an unscanned asset fails the same way `tag add` does.
#[test]
fn meta_set_on_an_unscanned_asset_fails() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    maj(&root)
        .args(["meta", "set", "xxh3:neverseen", "rating", "5"])
        .assert()
        .failure()
        .stderr(contains("unknown asset xxh3:neverseen"));
}

/// A minted PARA node id is a ULID: 26 characters, Crockford base32.
#[cfg(test)]
fn assert_is_ulid(s: &str) {
    assert_eq!(s.len(), 26, "expected a 26-char ULID, got: {s}");
    assert!(
        s.chars().all(|c| c.is_ascii_alphanumeric()
            && c != 'I'
            && c != 'L'
            && c != 'O'
            && c != 'U'),
        "expected Crockford base32 ULID chars, got: {s}"
    );
}

/// `para add` mints a node, `para list --json` reflects it, `para rename`
/// and `para archive` (with no `--root`, catalog-only) update it in place.
#[test]
fn para_add_list_rename_archive_round_trip() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    let out = maj(&root)
        .args(["para", "add", "project", "client-x"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let node_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_is_ulid(&node_id);

    let out = maj(&root)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], node_id);
    assert_eq!(nodes[0]["kind"], "project");
    assert_eq!(nodes[0]["name"], "client-x");
    assert_eq!(nodes[0]["archived"], false);

    maj(&root)
        .args(["para", "rename", "project/client-x", "client-y"])
        .assert()
        .success();

    maj(&root)
        .args(["para", "archive", "project/client-y"])
        .assert()
        .success();

    let out = maj(&root)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], node_id);
    assert_eq!(nodes[0]["name"], "client-y");
    assert_eq!(nodes[0]["archived"], true);

    // An archived node no longer resolves by `<kind>/<name>` — only a raw
    // node id reaches it now (see `resolve_para_node`'s non-archived filter).
    maj(&root)
        .args(["para", "rename", "project/client-y", "z"])
        .assert()
        .failure()
        .stderr(contains("no active PARA node"));
}

/// A second `para add` for the same (kind, name) while the first is still
/// active is rejected — it does not create a second, indistinguishable node.
#[test]
fn para_add_rejects_duplicate_active_name() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    maj(&root)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();
    maj(&root)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .failure()
        .stderr(contains("already exists"));
}

/// Node-reference resolution failures point the user at a concrete next
/// step rather than a bare "not found".
#[test]
fn para_node_reference_errors_are_actionable() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();

    maj(&root)
        .args(["para", "rename", "project/nope", "x"])
        .assert()
        .failure()
        .stderr(contains("maj para list"));

    maj(&root)
        .args(["para", "rename", "garbage", "x"])
        .assert()
        .failure()
        .stderr(contains("<kind>/<name>"));
}

/// `para archive --root <dir>` moves the node's materialized directory into
/// `Archives/`; `--dry-run` reports the move without performing it.
#[test]
fn para_archive_moves_materialized_dir_with_root() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    let materialized = tempfile::tempdir().unwrap();
    let node_dir = materialized.path().join("Projects").join("client-x");
    std::fs::create_dir_all(&node_dir).unwrap();
    std::fs::write(node_dir.join("a.txt"), b"hello").unwrap();

    maj(&root)
        .args(["para", "archive", "project/client-x"])
        .arg("--root")
        .arg(materialized.path())
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("would move"));
    assert!(node_dir.is_dir(), "dry run must not move the directory");

    maj(&root)
        .args(["para", "archive", "project/client-x"])
        .arg("--root")
        .arg(materialized.path())
        .assert()
        .success();
    assert!(!node_dir.exists(), "source directory must be moved away");
    let archived = materialized.path().join("Archives").join("client-x");
    assert!(archived.join("a.txt").is_file());

    let out = maj(&root)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["archived"], true);
}

/// A multi-root archive run must converge on re-run rather than failing
/// forever on a root an earlier partial run already moved. Simulates that
/// partial progress directly (root1 already has its directory under
/// `Archives/` and nothing left under `Projects/`) alongside a second root
/// that is still materialized and needs an actual move; a single command
/// covering both roots must skip the already-archived one, move the other,
/// and still emit the archive event.
#[test]
fn para_archive_with_multiple_roots_skips_a_root_already_archived() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    let root1 = tempfile::tempdir().unwrap();
    let root1_archived = root1.path().join("Archives").join("client-x");
    std::fs::create_dir_all(&root1_archived).unwrap();
    std::fs::write(root1_archived.join("a.txt"), b"hello").unwrap();

    let root2 = tempfile::tempdir().unwrap();
    let root2_source = root2.path().join("Projects").join("client-x");
    std::fs::create_dir_all(&root2_source).unwrap();
    std::fs::write(root2_source.join("b.txt"), b"world").unwrap();

    maj(&root)
        .args(["para", "archive", "project/client-x"])
        .arg("--root")
        .arg(root1.path())
        .arg("--root")
        .arg(root2.path())
        .assert()
        .success()
        .stdout(contains("already archived"));

    assert!(!root2_source.exists(), "root2 source must be moved");
    let root2_archived = root2.path().join("Archives").join("client-x");
    assert!(root2_archived.join("b.txt").is_file());
    // root1's already-archived directory is untouched by the skip.
    assert!(root1_archived.join("a.txt").is_file());

    let out = maj(&root)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["archived"], true);
}

/// Two machines sharing a catalog root each set the same field; the write
/// with the later HLC (here, the one issued second in wall-clock time) wins
/// on both machines once they re-read the merged log — LWW convergence for
/// `FieldSet`, exercised end to end through the real CLI and filesystem
/// event log rather than the in-memory cucumber harness.
#[test]
fn meta_set_later_write_wins_across_machines() {
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

    // machine-b writes first (HLC-earlier)...
    maj_as(&root, "machine-b")
        .args(["meta", "set", &id, "rating", "3"])
        .assert()
        .success();
    // ...machine-a writes second (HLC-later) and must win on both machines.
    maj_as(&root, "machine-a")
        .args(["meta", "set", &id, "rating", "5"])
        .assert()
        .success();

    for machine in ["machine-a", "machine-b"] {
        maj_as(&root, machine)
            .args(["meta", "get", &id, "rating"])
            .assert()
            .success()
            .stdout(contains("5"));
    }
}

/// `maj verify` re-checks a destination against its own ASC MHL history:
/// clean the first time (nothing altered, a new generation is written),
/// and reports + fails once a file has been altered underneath it. Verify
/// needs no catalog — the history lives entirely under `<dir>/ascmhl` — an
/// arbitrary, never-initialized catalog root is passed only because
/// `--catalog`/`--machine-id` are still required, non-`Option` global args
/// (see the dispatch comment on `Cmd::Verify` in `main.rs`).
#[test]
fn maj_verify_reports_altered_file_on_second_run() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"AAAA").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    let hash_list = majestical_ingest::mhl::hash_dir(media.path(), "2026-07-30T00:00:00Z").unwrap();
    majestical_ingest::mhl::write_generation(media.path(), &hash_list).unwrap();

    maj(&root)
        .args(["verify"])
        .arg(media.path())
        .assert()
        .success()
        .stdout(contains("wrote generation 2"));

    std::fs::write(media.path().join("a.mov"), b"ZZZZ").unwrap();

    maj(&root)
        .args(["verify"])
        .arg(media.path())
        .assert()
        .failure()
        .stdout(contains("ALTERED a.mov"));
}

#[cfg(test)]
fn walkdir_contains(root: &std::path::Path, filename: &str) -> bool {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy() == filename)
}

/// `maj ingest` end to end: a card with one file, ingested to two
/// destination roots under a PARA project node. Both roots get a verified
/// copy, an ASC MHL history (`ascmhl/` present), and the catalog can find
/// the asset by name; `maj verify` passes clean on both roots afterward.
/// Re-ingesting the identical card copies nothing (content already known)
/// but still reports the duplicate.
#[test]
fn ingest_places_verified_copies_with_mhl_and_catalog_events() {
    let media = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(media.path().join("clips")).unwrap();
    std::fs::write(media.path().join("clips/a.mov"), b"fake video bytes").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    let out = maj(&root)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .arg("--dest")
        .arg(d2.path())
        .args(["--para", "project/shoot", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["placed"], 1);
    assert_eq!(parsed["failed"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["generations"].as_array().unwrap().len(), 2);

    for dest in [d1.path(), d2.path()] {
        assert!(
            walkdir_contains(dest, "a.mov"),
            "expected a.mov placed somewhere under {}",
            dest.display()
        );
        assert!(
            dest.join("ascmhl").is_dir(),
            "expected ascmhl/ under {}",
            dest.display()
        );
    }

    let out = maj(&root)
        .args(["search", "--name", "a.mov", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);

    maj(&root)
        .args(["verify"])
        .arg(d1.path())
        .assert()
        .success();
    maj(&root)
        .args(["verify"])
        .arg(d2.path())
        .assert()
        .success();

    let out = maj(&root)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .arg("--dest")
        .arg(d2.path())
        .args(["--para", "project/shoot", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["placed"], 0);
    assert_eq!(parsed["skipped_duplicates"], 1);
    assert_eq!(
        parsed["generations"].as_array().unwrap().len(),
        0,
        "a dedupe-only run must not write a new (empty) MHL generation"
    );
}

/// `--dry-run` prints the plan without copying anything, creating a
/// journal, or touching the catalog.
#[test]
fn ingest_dry_run_places_nothing_and_writes_no_journal() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"AAAA").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    maj(&root)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", "project/shoot", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("COPY a.mov"));

    assert!(
        !d1.path().join("ascmhl").exists(),
        "dry run must not copy anything"
    );
    let runs_dir = root.join("runs");
    assert!(
        !runs_dir.is_dir() || std::fs::read_dir(&runs_dir).unwrap().next().is_none(),
        "dry run must not write a journal"
    );
}

/// A source that isn't a directory is rejected up front, before any
/// planning, copying, or journal writes.
#[test]
fn ingest_rejects_a_non_directory_source() {
    let media = tempfile::tempdir().unwrap();
    let file = media.path().join("not-a-dir.mov");
    std::fs::write(&file, b"AAAA").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    maj(&root)
        .args(["ingest"])
        .arg(&file)
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", "project/shoot"])
        .assert()
        .failure()
        .stderr(contains("source must be a directory"));
}

/// An archived PARA node is rejected as an ingest target, even when
/// addressed by raw node id (the one case `resolve_para_node` otherwise
/// still allows, so a rename can still reach an archived node).
#[test]
fn ingest_rejects_an_archived_para_target() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"AAAA").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();
    maj(&root)
        .args(["para", "archive", "project/shoot"])
        .assert()
        .success();

    let out = maj(&root)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let node_id = parsed["nodes"][0]["id"].as_str().unwrap().to_string();

    maj(&root)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", &node_id])
        .assert()
        .failure()
        .stderr(contains("is archived"));
}
