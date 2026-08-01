//! `maj sync push` end to end over real temp-dir catalogs and locations.
mod common;
use common::maj;
use std::path::Path;

#[cfg(test)]
fn init_catalog(catalog: &Path, state: &Path) {
    maj(catalog, state)
        .args(["catalog", "init"])
        .assert()
        .success();
}

#[test]
fn push_replicates_segments_and_blobs_to_a_location() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    init_catalog(&catalog, &state);
    // One real event past init: scan a file to get a segment with content.
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    // A blob to carry along.
    let blob_dir = catalog.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"w").expect("write");

    maj(&catalog, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("nas"),
        "report names the location: {stdout}"
    );
    assert!(
        location.join("events/test-machine/0001.jsonl").is_file(),
        "segments replicated"
    );
    assert!(
        location.join("blobs/ab/abcd/thumb-320.webp").is_file(),
        "blobs replicated"
    );

    // A second push after full convergence must report zero copies, not an
    // error — the plan is empty, and an empty plan is still success.
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("0 segment") && stdout.contains("0 blob"),
        "converged push reports zero copies: {stdout}"
    );
}

#[test]
fn readonly_refuses_push_naming_the_config_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    init_catalog(&catalog, &state);
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    maj(&catalog, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    // Flip readonly by rewriting the config file the CLI just created.
    let config = find_sync_toml(&state);
    let text = std::fs::read_to_string(&config).expect("read");
    let flipped = text.replace("readonly = false", "readonly = true");
    assert_ne!(
        flipped, text,
        "sync.toml must already contain readonly = false: {text}"
    );
    std::fs::write(&config, flipped).expect("write");
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("readonly = true") && stderr.contains("sync.toml"),
        "refusal must name the setting and the file: {stderr}"
    );
}

/// state/catalogs/<key>/sync.toml — exactly one catalog key exists per test.
#[cfg(test)]
fn find_sync_toml(state: &Path) -> std::path::PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("sync.toml")
}

#[test]
fn unreachable_location_is_skipped_with_a_notice_not_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    init_catalog(&catalog, &state);
    let good = root.path().join("nas");
    let gone = root.path().join("shuttle");
    std::fs::create_dir_all(&good).expect("mkdir");
    std::fs::create_dir_all(&gone).expect("mkdir");
    for (name, path) in [("nas", &good), ("shuttle", &gone)] {
        maj(&catalog, &state)
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }
    std::fs::remove_dir_all(&gone).expect("eject the shuttle");
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .success(); // one location succeeded — exit 0
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("shuttle") && stdout.contains("skipped"),
        "skip notice must name the location: {stdout}"
    );
}

#[test]
#[cfg(unix)]
fn a_per_file_push_failure_still_copies_other_blobs_but_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    init_catalog(&catalog, &state);

    // Two blobs; one becomes unreadable before push runs.
    let blocked_dir = catalog.join("blobs/ab/abcd");
    let ok_dir = catalog.join("blobs/cd/cdef");
    std::fs::create_dir_all(&blocked_dir).expect("mkdir");
    std::fs::create_dir_all(&ok_dir).expect("mkdir");
    let blocked = blocked_dir.join("thumb-320.webp");
    std::fs::write(&blocked, b"w1").expect("write");
    std::fs::write(ok_dir.join("thumb-320.webp"), b"w2").expect("write");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

    maj(&catalog, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let output = out.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Restore permissions so the tempdir can be cleaned up regardless of
    // what the assertions below find.
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644))
        .expect("restore perms");

    assert!(
        location.join("blobs/cd/cdef/thumb-320.webp").is_file(),
        "the surviving blob must still be copied: {stdout}"
    );
    assert!(
        stdout.contains("1 failed"),
        "the row must append the failure count: {stdout}"
    );
    assert!(
        stderr.contains("nas") && stderr.contains("thumb-320.webp"),
        "the failure line must name the location and path: {stderr}"
    );
    assert!(
        stderr.contains("progress was kept") && stderr.contains("retries"),
        "the final error must say progress was kept and the next run retries: {stderr}"
    );
}

#[test]
fn only_flag_filters_the_transfer_plan_to_one_class() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    init_catalog(&catalog, &state);
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    let blob_dir = catalog.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"thumb").expect("write");
    std::fs::write(blob_dir.join("tags.json.zst"), b"tags").expect("write");
    maj(&catalog, &state)
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();

    let segment = location.join("events/test-machine/0001.jsonl");
    let thumb = location.join("blobs/ab/abcd/thumb-320.webp");
    let metadata = location.join("blobs/ab/abcd/tags.json.zst");

    maj(&catalog, &state)
        .args(["sync", "push", "--only", "thumbs"])
        .assert()
        .success();
    assert!(thumb.is_file(), "--only thumbs must transfer the thumb");
    assert!(
        !segment.is_file(),
        "--only thumbs must not transfer segments"
    );
    assert!(
        !metadata.is_file(),
        "--only thumbs must not transfer other blob classes"
    );

    maj(&catalog, &state)
        .args(["sync", "push", "--only", "segments"])
        .assert()
        .success();
    assert!(
        segment.is_file(),
        "--only segments must transfer the segment"
    );
    assert!(
        !metadata.is_file(),
        "--only segments must still not transfer blobs"
    );

    maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .success();
    assert!(
        metadata.is_file(),
        "an unfiltered push must transfer the remaining blob class"
    );
}

#[test]
fn location_flag_restricts_push_to_the_named_location() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let nas = root.path().join("nas");
    let usb = root.path().join("usb");
    std::fs::create_dir_all(&nas).expect("mkdir");
    std::fs::create_dir_all(&usb).expect("mkdir");
    init_catalog(&catalog, &state);
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    let blob_dir = catalog.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"thumb").expect("write");
    for (name, path) in [("nas", &nas), ("usb", &usb)] {
        maj(&catalog, &state)
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }

    maj(&catalog, &state)
        .args(["sync", "push", "--location", "nas"])
        .assert()
        .success();
    assert!(
        nas.join("events/test-machine/0001.jsonl").is_file(),
        "the named location must receive the push"
    );
    assert!(
        nas.join("blobs/ab/abcd/thumb-320.webp").is_file(),
        "the named location must receive the push"
    );
    assert!(
        !usb.join("events/test-machine/0001.jsonl").is_file(),
        "an unnamed location must be left untouched"
    );
    assert!(
        !usb.join("blobs/ab/abcd/thumb-320.webp").is_file(),
        "an unnamed location must be left untouched"
    );

    let out = maj(&catalog, &state)
        .args(["sync", "push", "--location", "ghost"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no sync location named 'ghost'")
            && stderr.contains("nas")
            && stderr.contains("usb"),
        "an unknown --location must list what is configured: {stderr}"
    );
}

#[test]
fn json_rows_pin_the_agent_facing_contract() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let nas = root.path().join("nas");
    let gone = root.path().join("gone");
    std::fs::create_dir_all(&nas).expect("mkdir");
    std::fs::create_dir_all(&gone).expect("mkdir");
    init_catalog(&catalog, &state);
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&catalog, &state)
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    for (name, path) in [("nas", &nas), ("gone", &gone)] {
        maj(&catalog, &state)
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }
    std::fs::remove_dir_all(&gone).expect("eject the gone location");

    let out = maj(&catalog, &state)
        .args(["sync", "push", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let rows: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = rows.as_array().expect("a JSON array of rows");
    assert_eq!(rows.len(), 2, "one row per requested location: {stdout}");

    let nas_row = rows
        .iter()
        .find(|r| r["location"] == "nas")
        .expect("a row for nas");
    let keys: std::collections::BTreeSet<&str> = nas_row
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "location",
            "segments",
            "segment_bytes",
            "blobs",
            "blob_bytes",
            "failures"
        ]
        .into_iter()
        .collect(),
        "a succeeded row's keys are the agent-facing contract: {nas_row}"
    );
    assert!(
        nas_row["failures"].as_array().is_some_and(Vec::is_empty),
        "failures must be an (empty) array, not absent or null: {nas_row}"
    );

    let gone_row = rows
        .iter()
        .find(|r| r["location"] == "gone")
        .expect("a row for gone");
    let keys: std::collections::BTreeSet<&str> = gone_row
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["location", "skipped"].into_iter().collect(),
        "a skipped row carries only {{location, skipped}}, never the outcome fields: {gone_row}"
    );
    assert!(
        gone_row["skipped"].is_string(),
        "skipped must be a string, never null: {gone_row}"
    );
}

#[test]
fn push_fails_when_every_requested_location_is_unreachable() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir");
    std::fs::create_dir_all(&b).expect("mkdir");
    init_catalog(&catalog, &state);
    for (name, path) in [("a", &a), ("b", &b)] {
        maj(&catalog, &state)
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }
    std::fs::remove_dir_all(&a).expect("eject a");
    std::fs::remove_dir_all(&b).expect("eject b");

    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("failed for every requested location"),
        "every location unreachable must fail with that message: {stderr}"
    );
}

#[test]
fn push_ensures_name_the_catalog_and_no_locations_remedies() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat"); // never initialized
    let state = root.path().join("state");

    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no catalog at") && stderr.contains("maj catalog init"),
        "push against an uninitialized catalog must name the remedy: {stderr}"
    );

    init_catalog(&catalog, &state);
    let out = maj(&catalog, &state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no sync locations configured") && stderr.contains("maj sync location add"),
        "push with no locations configured must name the remedy: {stderr}"
    );
}
