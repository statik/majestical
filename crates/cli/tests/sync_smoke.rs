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
