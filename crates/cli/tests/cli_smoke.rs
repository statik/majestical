//! End-to-end: init a catalog, scan a folder, tag by name-match, search.
mod common;

use common::{first_asset_id, maj, maj_as, walkdir_find};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::{contains, diff};

/// Reads every event this test's single machine ("test-machine") has
/// appended so far, in file order.
#[cfg(test)]
fn read_events(root: &std::path::Path) -> Vec<serde_json::Value> {
    let seg = root.join("events/test-machine/0001.jsonl");
    let contents = std::fs::read_to_string(&seg).unwrap();
    contents
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect()
}

/// Asserts the catalog's raw event log carries exactly one `AssetParaSet`
/// for the one distinct asset an ingest run placed (not one per
/// destination — a per-dest emission would mint redundant, identical
/// assignments), and exactly one `ManifestRecorded` per `dest_count`
/// destination, each carrying a `c4`-prefixed roothash (the ASC MHL chain
/// hash, per `WrittenGeneration`).
#[cfg(test)]
fn assert_ingest_event_granularity(root: &std::path::Path, dest_count: usize) {
    let events = read_events(root);
    let asset_para_set_count = events
        .iter()
        .filter(|e| e["op"]["type"] == "asset_para_set")
        .count();
    assert_eq!(
        asset_para_set_count, 1,
        "expected exactly one AssetParaSet for the one distinct placed asset, got {asset_para_set_count}"
    );
    let manifest_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["op"]["type"] == "manifest_recorded")
        .collect();
    assert_eq!(
        manifest_events.len(),
        dest_count,
        "expected one ManifestRecorded per destination, got {}",
        manifest_events.len()
    );
    for m in &manifest_events {
        let roothash = m["op"]["roothash"].as_str().unwrap();
        assert!(
            roothash.starts_with("c4"),
            "expected a c4-prefixed roothash, got {roothash}"
        );
    }
}

/// Asserts every `VerificationRecorded` carries the same path as its paired
/// `AssetSeen` — both are emitted per destination from the same local
/// `vol_rel` value in `asset_and_para_ops`, pushed as an immediately
/// adjacent pair for each destination in `dest_volumes` order, so the two
/// filtered event streams stay pairwise aligned by emission order (not
/// matched by (asset, volume), which two destinations sharing one
/// auto-detected volume id — e.g. two tempdirs both on the root volume —
/// would collapse into one, hiding a mismatch rather than catching it). A
/// regression re-basing one event but not the other would otherwise pass CI
/// silently.
#[cfg(test)]
fn assert_verification_paths_match_asset_seen_paths(root: &std::path::Path) {
    let events = read_events(root);
    let asset_seen_paths: Vec<&str> = events
        .iter()
        .filter(|e| e["op"]["type"] == "asset_seen")
        .map(|e| e["op"]["path"].as_str().unwrap())
        .collect();
    let verification_paths: Vec<&str> = events
        .iter()
        .filter(|e| e["op"]["type"] == "verification_recorded")
        .map(|e| e["op"]["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        asset_seen_paths.len(),
        verification_paths.len(),
        "expected one VerificationRecorded per AssetSeen"
    );
    for (seen_path, verified_path) in asset_seen_paths.iter().zip(&verification_paths) {
        assert_eq!(
            seen_path, verified_path,
            "VerificationRecorded's path must match its paired AssetSeen's path"
        );
    }
}

#[test]
fn init_scan_tag_search_round_trip() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("sunset.mov"), b"fake video bytes").unwrap();
    std::fs::write(media.path().join("notes.txt"), b"hello").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success()
        .stdout(contains("2 assets"));
    // Find the asset id for sunset.mov via name search (json output).
    let out = maj(&root, &state)
        .args(["search", "sunset", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    let id = first_asset_id(&out);

    maj(&root, &state)
        .args(["tag", "add", &id, "topic/drone"])
        .assert()
        .success();
    let out = maj(&root, &state)
        .args(["search", "tag:topic/drone", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    assert_eq!(hits["results"][0]["asset"], id);

    maj(&root, &state)
        .args(["tag", "rm", &id, "topic/drone"])
        .assert()
        .success();
    let out = maj(&root, &state)
        .args(["search", "tag:topic/drone", "--json"])
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();

    let seg = root.join("events/test-machine/0001.jsonl");
    let mut contents = std::fs::read_to_string(&seg).unwrap();
    contents.push_str("not valid json\n");
    std::fs::write(&seg, contents).unwrap();

    let assert = maj(&root, &state)
        .args(["search", "sunset", "--json"])
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

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
            mtime_ms: 0,
        },
    };
    let line = serde_json::to_string(&poisoned).unwrap();
    std::fs::write(peer_dir.join("0001.jsonl"), format!("{line}\n")).unwrap();

    let assert = maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj_as(&root, &state, "machine-a")
        .args(["catalog", "init"])
        .assert()
        .success();
    maj_as(&root, &state, "machine-a")
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj_as(&root, &state, "machine-a")
        .args(["search", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);
    maj_as(&root, &state, "machine-a")
        .args(["tag", "add", &id, "tag/a"])
        .assert()
        .success();

    // machine-b, in a separate process, sees machine-a's asset and tag.
    let out = maj_as(&root, &state, "machine-b")
        .args(["search", "tag:tag/a", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);
    assert_eq!(hits["results"][0]["asset"], id);
    maj_as(&root, &state, "machine-b")
        .args(["tag", "add", &id, "tag/b"])
        .assert()
        .success();

    // machine-a removes machine-b's tag, citing the add-ids it observes
    // via the merged projection.
    maj_as(&root, &state, "machine-a")
        .args(["tag", "rm", &id, "tag/b"])
        .assert()
        .success();

    for machine in ["machine-a", "machine-b"] {
        let out = maj_as(&root, &state, machine)
            .args(["search", "tag:tag/a", "--json"])
            .output()
            .unwrap();
        let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(hits["count"], 1, "{machine} should still see tag/a");

        let out = maj_as(&root, &state, machine)
            .args(["search", "tag:tag/b", "--json"])
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "maj-test-no-such-volume"])
        .assert()
        .success();

    let out = maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .assert()
        .success();

    let out = maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

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

    let out = maj(&root, &state)
        .args(["volumes", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let volumes = parsed["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0]["clock_suspect"], true);

    maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["search", "anything", "--json"])
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["search", "anything", "--json"])
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["tag", "add", "xxh3:neverseen", "some/tag"])
        .assert()
        .failure()
        .stderr(contains("unknown asset xxh3:neverseen"));

    let out = maj(&root, &state)
        .args(["search", "tag:some/tag", "--json"])
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj(&root, &state)
        .args(["search", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);

    maj(&root, &state)
        .args(["meta", "set", &id, "rating", "5"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["meta", "get", &id, "rating"])
        .assert()
        .success()
        .stdout(diff("5\n"));

    // Getting every field (no field name) lists it as `field\tvalue` lines.
    maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj(&root, &state)
        .args(["search", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);

    maj(&root, &state)
        .args(["meta", "get", &id, "rating"])
        .assert()
        .success()
        .stdout(diff("\n"));
    maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    let out = maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let node_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_is_ulid(&node_id);

    let out = maj(&root, &state)
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

    maj(&root, &state)
        .args(["para", "rename", "project/client-x", "client-y"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["para", "archive", "project/client-y"])
        .assert()
        .success();

    let out = maj(&root, &state)
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
    maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();
    maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["para", "rename", "project/nope", "x"])
        .assert()
        .failure()
        .stderr(contains("maj para list"));

    maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "client-x"])
        .assert()
        .success();

    let materialized = tempfile::tempdir().unwrap();
    let node_dir = materialized.path().join("Projects").join("client-x");
    std::fs::create_dir_all(&node_dir).unwrap();
    std::fs::write(node_dir.join("a.txt"), b"hello").unwrap();

    maj(&root, &state)
        .args(["para", "archive", "project/client-x"])
        .arg("--root")
        .arg(materialized.path())
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("would move"));
    assert!(node_dir.is_dir(), "dry run must not move the directory");

    maj(&root, &state)
        .args(["para", "archive", "project/client-x"])
        .arg("--root")
        .arg(materialized.path())
        .assert()
        .success();
    assert!(!node_dir.exists(), "source directory must be moved away");
    let archived = materialized.path().join("Archives").join("client-x");
    assert!(archived.join("a.txt").is_file());

    let out = maj(&root, &state)
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
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
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

    maj(&root, &state)
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

    let out = maj(&root, &state)
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
    let state = catalog.path().join("state");

    maj_as(&root, &state, "machine-a")
        .args(["catalog", "init"])
        .assert()
        .success();
    maj_as(&root, &state, "machine-a")
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success();
    let out = maj_as(&root, &state, "machine-a")
        .args(["search", "clip", "--json"])
        .output()
        .unwrap();
    let id = first_asset_id(&out);

    // machine-b writes first (HLC-earlier)...
    maj_as(&root, &state, "machine-b")
        .args(["meta", "set", &id, "rating", "3"])
        .assert()
        .success();
    // ...machine-a writes second (HLC-later) and must win on both machines.
    maj_as(&root, &state, "machine-a")
        .args(["meta", "set", &id, "rating", "5"])
        .assert()
        .success();

    for machine in ["machine-a", "machine-b"] {
        maj_as(&root, &state, machine)
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
    let state = catalog.path().join("state");

    let hash_list = majestical_ingest::mhl::hash_dir(media.path(), "2026-07-30T00:00:00Z").unwrap();
    majestical_ingest::mhl::write_generation(media.path(), &hash_list).unwrap();

    maj(&root, &state)
        .args(["verify"])
        .arg(media.path())
        .assert()
        .success()
        .stdout(contains("wrote generation 2"));

    std::fs::write(media.path().join("a.mov"), b"ZZZZ").unwrap();

    maj(&root, &state)
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
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    let out = maj(&root, &state)
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

    // Pin the catalog event granularity directly against the raw JSONL log
    // (not just the CLI's own summary counts) — see
    // `assert_ingest_event_granularity`.
    assert_ingest_event_granularity(&root, 2);

    // Ingest must stat its own placed files for `AssetSeen.mtime_ms`, same as
    // `scan` — not leave the phase-1 `0` placeholder in place.
    let events = read_events(&root);
    for e in events.iter().filter(|e| e["op"]["type"] == "asset_seen") {
        let mtime_ms = e["op"]["mtime_ms"].as_u64().unwrap();
        assert!(
            mtime_ms > 0,
            "expected a real mtime_ms on an ingest-placed AssetSeen event, got {e}"
        );
    }

    // A regression re-basing `AssetSeen`'s path but not
    // `VerificationRecorded`'s (or vice versa) would otherwise pass CI
    // silently — see `assert_verification_paths_match_asset_seen_paths`.
    assert_verification_paths_match_asset_seen_paths(&root);

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

    let out = maj(&root, &state)
        .args(["search", "a.mov", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hits["count"], 1);

    maj(&root, &state)
        .args(["verify"])
        .arg(d1.path())
        .assert()
        .success();
    maj(&root, &state)
        .args(["verify"])
        .arg(d2.path())
        .assert()
        .success();

    let out = maj(&root, &state)
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
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    maj(&root, &state)
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
    assert!(
        !root.join("runs").exists(),
        "dry run must not write a journal into the sync root"
    );
    let journals: Vec<_> = walkdir::WalkDir::new(&state)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .collect();
    assert!(
        journals.is_empty(),
        "dry run must not write a journal into the state dir, found: {journals:?}"
    );
}

/// `maj ingest unfinished` lists the runs `--resume` can still finish, and
/// only those. Arranged with a real, deterministic per-file failure: a
/// directory sitting at one file's final path makes its rename fail while
/// the other file places normally, so the run's journal ends with one of
/// two files placed — the two-asset shape that tells `1` apart from "all"
/// and from "none".
#[test]
fn ingest_unfinished_lists_a_run_with_files_left_and_nothing_once_it_is_done() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"AAAA").unwrap();
    std::fs::write(media.path().join("b.mov"), b"BBBB").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    // Nothing has run yet.
    let out = maj(&root, &state)
        .args(["ingest", "unfinished", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["runs"].as_array().unwrap().len(), 0);

    // A fixed template makes the destination path predictable, so a.mov's
    // final path can be occupied by a directory before the run starts.
    std::fs::create_dir_all(d1.path().join("Projects/shoot/fixed/a.mov")).unwrap();
    maj(&root, &state)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", "project/shoot", "--template", "fixed"])
        .assert()
        .failure();

    let out = maj(&root, &state)
        .args(["ingest", "unfinished", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let runs = parsed["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{parsed}");
    assert_eq!(runs[0]["placed"], 1, "{parsed}");
    assert_eq!(runs[0]["planned"], 2, "{parsed}");
    assert_eq!(
        runs[0]["source"],
        serde_json::json!(media.path().display().to_string())
    );
    assert_eq!(
        runs[0]["destinations"],
        serde_json::json!([d1.path().display().to_string()])
    );
    let run_id = runs[0]["run_id"].as_str().unwrap().to_string();

    // The human rendering names the same run, its counts, and its source.
    maj(&root, &state)
        .args(["ingest", "unfinished"])
        .assert()
        .success()
        .stdout(contains(&run_id))
        .stdout(contains("1/2 placed"));

    // Clear the obstruction and resume: with everything placed, the run
    // drops off the listing entirely.
    std::fs::remove_dir(d1.path().join("Projects/shoot/fixed/a.mov")).unwrap();
    maj(&root, &state)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args([
            "--para",
            "project/shoot",
            "--template",
            "fixed",
            "--resume",
            &run_id,
        ])
        .assert()
        .success();

    let out = maj(&root, &state)
        .args(["ingest", "unfinished", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        parsed["runs"].as_array().unwrap().len(),
        0,
        "a run that placed everything it planned is finished: {parsed}"
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
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["ingest"])
        .arg(&file)
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", "project/shoot"])
        .assert()
        .failure()
        .stderr(contains("source must be a directory"));
}

/// `--resume <id>` for a run id with no journal on disk fails loudly (a
/// typo'd or fabricated id, not a fresh run under that name) and creates
/// nothing — no journal file, no copied bytes.
#[test]
fn ingest_resume_with_an_unknown_run_id_fails_and_creates_nothing() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("a.mov"), b"AAAA").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", "project/shoot", "--resume", "nonexistent"])
        .assert()
        .failure()
        .stderr(contains(
            "no journal for run 'nonexistent' — check the id printed at the start of the original run",
        ));

    assert!(
        !root.join("runs").exists(),
        "an unknown --resume id must not create a runs/ directory"
    );
    assert!(
        !d1.path().join("ascmhl").exists(),
        "an unknown --resume id must not copy anything"
    );
    assert!(
        walkdir_find(&state, "nonexistent.jsonl").is_empty(),
        "an unknown --resume id must not create a journal in the state dir either"
    );
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
    let state = catalog.path().join("state");
    let d1 = tempfile::tempdir().unwrap();

    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "add", "project", "shoot"])
        .assert()
        .success();
    maj(&root, &state)
        .args(["para", "archive", "project/shoot"])
        .assert()
        .success();

    let out = maj(&root, &state)
        .args(["para", "list", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let node_id = parsed["nodes"][0]["id"].as_str().unwrap().to_string();

    maj(&root, &state)
        .args(["ingest"])
        .arg(media.path())
        .arg("--dest")
        .arg(d1.path())
        .args(["--para", &node_id])
        .assert()
        .failure()
        .stderr(contains("is archived"));
}

/// `catalog.db` is a disposable local projection, not shared catalog data —
/// it must live under the per-machine state dir, never in the sync root.
#[test]
fn catalog_db_lives_in_state_dir_not_sync_root() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("a.txt"), b"hello").unwrap();
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();
    maj(&catalog, &state)
        .args(["search", "a.txt"])
        .assert()
        .success();
    assert!(
        !catalog.join("catalog.db").exists(),
        "catalog.db must not be created in the sync root"
    );
    let dbs: Vec<_> = walkdir_find(&state, "catalog.db");
    assert_eq!(dbs.len(), 1, "exactly one catalog.db under the state dir");
}

/// A pre-phase-4 `catalog.db` left behind in the sync root (from before the
/// local-state split) is cleaned up automatically the next time any command
/// opens the catalog — it's disposable and gets rebuilt locally, so leaving
/// it in the shared sync root would just be stale, confusing clutter.
#[test]
fn legacy_catalog_db_in_sync_root_is_removed_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    std::fs::write(catalog.join("catalog.db"), b"legacy").unwrap();
    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();
    assert!(
        !catalog.join("catalog.db").exists(),
        "legacy db must be cleaned out of the sync root"
    );
}

/// Pre-phase-4 ingest run journals under `<catalog>/runs/` are migrated into
/// the local state dir on the next open, so `--resume` keeps working for
/// runs started before the split without leaving journals in the sync root.
#[test]
fn legacy_run_journals_move_to_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(catalog.join("runs")).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    std::fs::write(catalog.join("runs").join("01OLD.jsonl"), b"{}\n").unwrap();
    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();
    assert!(
        !catalog.join("runs").exists(),
        "legacy runs/ removed from sync root"
    );
    let moved: Vec<_> = walkdir_find(&state, "01OLD.jsonl");
    assert_eq!(moved.len(), 1, "journal moved into the state dir");
    assert!(
        walkdir_find(&state, "01OLD.jsonl.partial").is_empty(),
        "the copy-then-rename temp file must not linger after a successful migration"
    );
}

/// A legacy `runs/` dir can hold more than plain journals — a Syncthing
/// `.stversions/` subdirectory, a stray `.DS_Store`, whatever else ends up
/// next to synced files. Migration must not choke on those: it moves only
/// `*.jsonl` regular files, leaves anything else in place, and the catalog
/// stays usable (including on a second command run, since the non-journal
/// entries mean the legacy `runs/` dir can never be fully cleaned up).
#[test]
fn legacy_runs_dir_with_non_journal_entries_migrates_the_journal_and_leaves_junk() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(catalog.join("runs")).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    std::fs::write(catalog.join("runs").join("01OLD.jsonl"), b"{}\n").unwrap();
    std::fs::write(catalog.join("runs").join(".DS_Store"), b"junk").unwrap();
    std::fs::create_dir_all(catalog.join("runs").join(".stversions")).unwrap();
    std::fs::write(
        catalog.join("runs").join(".stversions").join("01OLD.jsonl"),
        b"old version\n",
    )
    .unwrap();

    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();

    let moved: Vec<_> = walkdir_find(&state, "01OLD.jsonl");
    assert_eq!(moved.len(), 1, "the journal moved into the state dir");
    assert!(
        catalog.join("runs").join(".DS_Store").is_file(),
        "non-journal junk is left in the sync root"
    );
    assert!(
        catalog
            .join("runs")
            .join(".stversions")
            .join("01OLD.jsonl")
            .is_file(),
        "a subdirectory under runs/ is left in the sync root"
    );

    // The catalog must still be usable on a second run, even though the
    // legacy runs/ dir can never be fully removed (junk remains in it).
    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();
}

/// If the state dir already has a journal for a run id (e.g. a second
/// machine already migrated it), migration must not clobber it with
/// whatever's in the sync root's legacy copy — the state-dir copy is the
/// one actively in use locally. The legacy source is still removed so the
/// sync root converges to having no `runs/` dir.
#[test]
fn legacy_journal_migration_does_not_overwrite_an_existing_state_dir_journal() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    // Any command opens the catalog, which creates the state dir's runs/.
    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();
    let state_runs = walkdir::WalkDir::new(&state)
        .into_iter()
        .filter_map(Result::ok)
        .find(|e| e.file_name() == "runs")
        .expect("state dir must already have a runs/ dir")
        .into_path();
    std::fs::write(state_runs.join("01OLD.jsonl"), b"state dir content\n").unwrap();

    std::fs::create_dir_all(catalog.join("runs")).unwrap();
    std::fs::write(
        catalog.join("runs").join("01OLD.jsonl"),
        b"stale legacy content\n",
    )
    .unwrap();

    maj(&catalog, &state)
        .args(["search", "nothing"])
        .assert()
        .success();

    let content = std::fs::read_to_string(state_runs.join("01OLD.jsonl")).unwrap();
    assert_eq!(
        content, "state dir content\n",
        "an existing state-dir journal must not be overwritten by the legacy copy"
    );
    assert!(
        !catalog.join("runs").join("01OLD.jsonl").exists(),
        "the legacy source is still removed so the sync root converges"
    );
}

/// A search query combines bare name terms (FTS ranked) with `key:value`
/// hard filters (AND'd, `-` negated) — `tag:` and `kind:` filters exercised
/// together against a scanned catalog.
#[test]
fn search_combines_terms_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("beach_day.mov"), b"aaa").unwrap();
    std::fs::write(media.join("mountain.jpg"), b"bbb").unwrap();
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();
    let out = maj(&catalog, &state)
        .args(["search", "beach", "--json"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Pin the JSON contract beyond just `results[].asset`: name, and each
    // volume's online flag (true here — the scanned tempdir is on the
    // always-mounted root volume).
    assert_eq!(hits["results"][0]["name"], "beach_day.mov");
    assert_eq!(hits["results"][0]["volumes"][0]["online"], true);
    let asset = first_asset_id(&out);
    maj(&catalog, &state)
        .args(["tag", "add", &asset, "status/select"])
        .assert()
        .success();
    maj(&catalog, &state)
        .args(["search", "beach tag:status/select"])
        .assert()
        .success()
        .stdout(contains("beach_day.mov"));
    maj(&catalog, &state)
        .args(["search", "beach -tag:status/select"])
        .assert()
        .success()
        .stdout(contains("0 results"));
    // A '-' negated filter as the query's very first character — clap must
    // not mistake the whole query for an unrecognized option.
    maj(&catalog, &state)
        .args(["search", "-tag:status/select"])
        .assert()
        .success()
        .stdout(contains("mountain.jpg"));
    maj(&catalog, &state)
        .args(["search", "kind:video"])
        .assert()
        .success()
        .stdout(contains("beach_day.mov"));
    maj(&catalog, &state)
        .args(["search", "kind:image -tag:status/select"])
        .assert()
        .success()
        .stdout(contains("mountain.jpg"));

    let vol_out = maj(&catalog, &state)
        .args(["volumes", "list", "--json"])
        .output()
        .unwrap();
    let vols: serde_json::Value = serde_json::from_slice(&vol_out.stdout).unwrap();
    let vol_label = vols["volumes"][0]["label"].as_str().unwrap();
    maj(&catalog, &state)
        .args(["search", &format!("vol:{vol_label}")])
        .assert()
        .success()
        .stdout(contains("beach_day.mov"))
        .stdout(contains("mountain.jpg"));
}

/// The search limit applies to the intersection of ranked terms and hard
/// filters, not to a pre-filter slice of the ranked list — a filter match
/// that happens to rank outside the first `limit * 4` terms must still be
/// found.
#[test]
fn search_limit_applies_after_filtering_not_before() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    for n in 0..30 {
        std::fs::write(media.join(format!("beach_{n:02}.mov")), n.to_string()).unwrap();
    }
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    let out = maj(&catalog, &state)
        .args(["search", "beach", "--json", "--limit", "100"])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = hits["results"].as_array().unwrap();
    assert_eq!(results.len(), 30);
    // Tag the 4 worst-ranked hits (the tail of the unfiltered ranked list) —
    // a `--limit 2` search must still find them via intersection, not miss
    // them because they fall outside a `limit * 4` pre-filter window.
    for r in results.iter().rev().take(4) {
        let asset = r["asset"].as_str().unwrap();
        maj(&catalog, &state)
            .args(["tag", "add", asset, "status/select"])
            .assert()
            .success();
    }

    let out = maj(&catalog, &state)
        .args([
            "search",
            "beach tag:status/select",
            "--limit",
            "2",
            "--json",
        ])
        .output()
        .unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        hits["count"], 2,
        "the 4 tagged assets rank last among 30 matches; --limit 2 must still \
         find them via intersection rather than a pre-filter slice, got: {hits}"
    );
}

/// `online:`/`-online:` matches against the currently-mounted volume set.
/// The scanned tempdir lives on the always-mounted root volume, so its
/// asset must show up under `online:yes`/`-online:no` and disappear under
/// `online:no`/`-online:yes`.
#[test]
fn search_online_filter_matches_currently_mounted_volumes() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("clip.mov"), b"aaa").unwrap();
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "online:yes"])
        .assert()
        .success()
        .stdout(contains("clip.mov"));
    maj(&catalog, &state)
        .args(["search", "-online:no"])
        .assert()
        .success()
        .stdout(contains("clip.mov"));
    maj(&catalog, &state)
        .args(["search", "online:no"])
        .assert()
        .success()
        .stdout(contains("0 results"));
    maj(&catalog, &state)
        .args(["search", "-online:yes"])
        .assert()
        .success()
        .stdout(contains("0 results"));
}

/// An unknown filter key fails fast, naming the keys that are actually
/// valid, rather than silently matching nothing.
#[test]
fn search_with_unknown_filter_key_lists_valid_keys() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["search", "flavor:salty"])
        .assert()
        .failure()
        .stderr(contains("tag"))
        .stderr(contains("before"));
}

/// `before:`/`after:` filters compare against an instance's real recorded
/// file mtime, not the placeholder `0` scans used to write.
#[test]
fn search_mtime_filters_use_real_file_mtimes() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("recent.mov"), b"aaa").unwrap();
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "after:1970-01-02"])
        .assert()
        .success()
        .stdout(contains("recent.mov"));
    maj(&catalog, &state)
        .args(["search", "before:1970-01-02"])
        .assert()
        .success()
        .stdout(contains("0 results"));
}

/// A query with no terms and no filters is rejected rather than silently
/// running an unbounded "everything" search.
#[test]
fn empty_query_without_filters_is_an_error() {
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&root, &state)
        .args(["search", "   "])
        .assert()
        .failure()
        .stderr(contains("terms"))
        .stderr(contains("filter"));
}

/// Text-mode output hints when a result count lands exactly on `--limit` —
/// almost always meaning more matches exist past the cutoff — so a
/// truncated list doesn't read as the complete answer.
#[test]
fn search_text_output_notes_truncation_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("beach_1.mov"), b"a").unwrap();
    std::fs::write(media.join("beach_2.mov"), b"b").unwrap();
    std::fs::write(media.join("beach_3.mov"), b"c").unwrap();
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "beach", "--limit", "2"])
        .assert()
        .success()
        .stdout(contains(
            "note: results truncated at 2; raise --limit to see more",
        ));
    let out = maj(&catalog, &state)
        .args(["search", "beach", "--limit", "10"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("note: results truncated"),
        "expected no truncation note when the match count falls short of --limit, got: {stdout}"
    );
}

/// `--save` on one machine syncs to another machine's `searches list` via
/// the shared event log; `searches rm` on the second machine likewise syncs
/// back to the first.
#[test]
fn saved_searches_sync_between_machines() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state_a = dir.path().join("state-a");
    let state_b = dir.path().join("state-b");
    std::fs::create_dir_all(&catalog).unwrap();
    maj_as(&catalog, &state_a, "machine-a")
        .args(["catalog", "init"])
        .assert()
        .success();
    maj_as(&catalog, &state_a, "machine-a")
        .args(["search", "tag:keep", "--save", "keepers"])
        .assert()
        .success();

    maj_as(&catalog, &state_b, "machine-b")
        .args(["searches", "list"])
        .assert()
        .success()
        .stdout(contains("keepers"))
        .stdout(contains("tag:keep"));

    maj_as(&catalog, &state_b, "machine-b")
        .args(["searches", "rm", "keepers"])
        .assert()
        .success();

    maj_as(&catalog, &state_a, "machine-a")
        .args(["searches", "list"])
        .assert()
        .success()
        .stdout(contains("no saved searches"));
}

/// `--saved` runs a stored query (saving succeeds even with zero hits);
/// an unknown `--saved` name or `searches rm` target fails with a clear
/// message; `searches list --json` renders the name/query pairs.
#[test]
fn running_and_managing_saved_searches() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "tag:nothing-yet", "--save", "empty"])
        .assert()
        .success()
        .stdout(contains("0 results"));

    maj(&catalog, &state)
        .args(["search", "--saved", "empty"])
        .assert()
        .success()
        .stdout(contains("0 results"));

    maj(&catalog, &state)
        .args(["search", "--saved", "missing"])
        .assert()
        .failure()
        .stderr(contains("no saved search"));

    maj(&catalog, &state)
        .args(["searches", "rm", "missing"])
        .assert()
        .failure()
        .stderr(contains("no saved search"));

    maj(&catalog, &state)
        .args(["searches", "list", "--json"])
        .assert()
        .success()
        .stdout(diff(
            "{\"saved\":[{\"name\":\"empty\",\"query\":\"tag:nothing-yet\"}]}\n",
        ));
}

/// A positional query and `--saved` together must be rejected by clap's
/// arg parsing (a usage error, exit code 2) — not reach `cmd_search`'s
/// query-resolution match, which used to hit an `unreachable!` there and
/// abort the process with a panic (exit code 101) instead.
#[test]
fn search_query_and_saved_together_is_a_clap_conflict_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "tag:x", "--saved", "keepers"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("cannot be used with"))
        .stderr(predicates::str::contains("panicked").not());
}

/// A `--save` on a query that fails to parse must not append anything to
/// the event log: `searches list` afterward must show no saved search, not
/// one pointing at an invalid query.
#[test]
fn save_of_an_invalid_query_does_not_persist_the_saved_search() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "bogus:x", "--save", "bad"])
        .assert()
        .failure();

    maj(&catalog, &state)
        .args(["searches", "list"])
        .assert()
        .success()
        .stdout(contains("no saved searches"));
}

/// `--json` output must be pure JSON on stdout even when `--save` is also
/// given — the "saved search 'x'" confirmation belongs on stderr, not mixed
/// into the same stream a scripted consumer parses as JSON.
#[test]
fn search_save_with_json_keeps_stdout_pure_json() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    let out = maj(&catalog, &state)
        .args(["search", "tag:a", "--save", "x", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("stdout must be parseable as JSON with --save present");
}

/// An empty saved-search name is rejected outright, for both `--save` and
/// `searches rm` — an empty `name` primary key is a foot-gun no legitimate
/// caller wants, not a name worth storing.
#[test]
fn empty_saved_search_name_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state)
        .args(["catalog", "init"])
        .assert()
        .success();

    maj(&catalog, &state)
        .args(["search", "tag:a", "--save", ""])
        .assert()
        .failure()
        .stderr(contains("empty"));

    maj(&catalog, &state)
        .args(["searches", "rm", ""])
        .assert()
        .failure()
        .stderr(contains("empty"));
}
