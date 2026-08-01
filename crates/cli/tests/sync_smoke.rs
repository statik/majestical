//! `maj sync` push/pull/status end to end over real temp-dir catalogs and
//! locations.
mod common;
use common::{first_asset_id, maj, maj_as};
use std::path::{Path, PathBuf};

// `#[cfg(test)]` on these helpers is required, not redundant — see
// `tests/common/mod.rs`'s `maj_as` doc comment for the full rationale.
#[cfg(test)]
fn init_catalog_as(catalog: &Path, state: &Path, machine: &str) {
    maj_as(catalog, state, machine)
        .args(["catalog", "init"])
        .assert()
        .success();
}

#[cfg(test)]
fn init_catalog(catalog: &Path, state: &Path) {
    init_catalog_as(catalog, state, "test-machine");
}

/// state/catalogs/<key>/sync.toml — exactly one catalog key exists per test.
#[cfg(test)]
fn find_sync_toml(state: &Path) -> PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("sync.toml")
}

/// state/catalogs/<key>/catalog.db — same one-key-per-test layout as
/// [`find_sync_toml`].
#[cfg(test)]
fn find_catalog_db(state: &Path) -> PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("catalog.db")
}

/// Counts rows in the local `assets` table, read directly off `db_path` —
/// bypasses the CLI entirely so a test can assert `cmd_pull` itself wrote
/// to sqlite, rather than a later `maj` invocation (which incrementally
/// syncs the catalog on every open) doing it instead.
#[cfg(test)]
fn asset_count(db_path: &Path) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open catalog.db read-only");
    conn.query_row("select count(*) from assets", [], |row| row.get(0))
        .expect("count assets")
}

/// Recursively snapshots every entry under `root` — files as
/// `Some((size, mtime))`, directories as `None` (so a new EMPTY directory,
/// e.g. an `execute`-created `tmp/` staging dir, still shows up as a map
/// diff even though it has no size or mtime of its own to compare). Two
/// snapshots taken around a command that must be read-only compare equal
/// via plain `assert_eq!` — any added/removed path, size change, or mtime
/// change (a rewrite-in-place that happens to keep the same length) shows
/// up as a diff, which is a stronger proof than spot-checking a couple of
/// paths that are "supposed" to change.
#[cfg(test)]
fn tree_snapshot(
    root: &Path,
) -> std::collections::BTreeMap<PathBuf, Option<(u64, std::time::SystemTime)>> {
    let mut snapshot = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                snapshot.insert(path.clone(), None);
                stack.push(path);
            } else {
                let mtime = meta.modified().expect("mtime");
                snapshot.insert(path, Some((meta.len(), mtime)));
            }
        }
    }
    snapshot
}

/// A `maj` catalog + state dir pair, initialized and ready for `sync`
/// commands — the common preamble every push test needs. Built from a
/// shared `root` plus a `name` so two fixtures (e.g. Task 6's two-machine
/// pull tests) can live under the same tempdir without colliding.
struct Fixture {
    catalog: PathBuf,
    state: PathBuf,
    media: PathBuf,
    machine: String,
}

/// Builds a fixture identified as `machine` — the two-machine pull tests
/// need distinct machine ids (segments land under `events/<machine>/`, and
/// a pull's report names whose events it applied), unlike every push test,
/// which only ever needs one machine and gets it via [`fixture`].
#[cfg(test)]
fn fixture_as(root: &Path, name: &str, machine: &str) -> Fixture {
    let catalog = root.join(format!("{name}-cat"));
    let state = root.join(format!("{name}-state"));
    let media = root.join(format!("{name}-media"));
    init_catalog_as(&catalog, &state, machine);
    Fixture {
        catalog,
        state,
        media,
        machine: machine.to_string(),
    }
}

#[cfg(test)]
fn fixture(root: &Path, name: &str) -> Fixture {
    fixture_as(root, name, "test-machine")
}

#[cfg(test)]
impl Fixture {
    /// A `maj` invocation scoped to this fixture's catalog, state, and
    /// machine id — the machine id no longer needs re-typing (and risking a
    /// typo that silently runs as a different machine) at every call site
    /// that already has a `Fixture` in hand.
    fn maj(&self) -> assert_cmd::Command {
        maj_as(&self.catalog, &self.state, &self.machine)
    }

    /// Scans `name` (written with `contents`) into this fixture's catalog
    /// under `volume` — the general form of [`Self::scan_one_file`], for
    /// tests that need a specific filename instead of the fixed `a.jpg`.
    fn scan_named_file(&self, name: &str, contents: &[u8], volume: &str) {
        std::fs::create_dir_all(&self.media).expect("mkdir");
        std::fs::write(self.media.join(name), contents).expect("write");
        self.maj()
            .args(["scan"])
            .arg(&self.media)
            .args(["--volume", volume])
            .assert()
            .success();
    }

    /// Runs `maj search <query>` and asserts stdout contains `needle` —
    /// shared by the shuttle test's repeated search-and-check steps.
    fn assert_search_finds(&self, query: &str, needle: &str, msg: &str) {
        let out = self.maj().args(["search", query]).assert().success();
        let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(stdout.contains(needle), "{msg}: {stdout}");
    }

    /// Scans a single real file into this fixture's catalog, producing one
    /// real segment (`events/<machine>/0001.jsonl`) with actual content.
    fn scan_one_file(&self) {
        std::fs::create_dir_all(&self.media).expect("mkdir");
        std::fs::write(self.media.join("a.jpg"), b"jpeg-bytes").expect("write");
        maj_as(&self.catalog, &self.state, &self.machine)
            .args(["scan"])
            .arg(&self.media)
            .args(["--volume", "vol1"])
            .assert()
            .success();
    }

    /// Writes a blob at `blobs/<rel>` under this fixture's catalog.
    fn write_blob(&self, rel: &str, contents: &[u8]) {
        let path = self.catalog.join("blobs").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    /// Registers `path` as a sync location named `name` on this fixture.
    fn add_location(&self, name: &str, path: &Path) {
        maj_as(&self.catalog, &self.state, &self.machine)
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }
}

#[test]
fn push_replicates_segments_and_blobs_to_a_location() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.scan_one_file();
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &location);

    let out = maj(&fx.catalog, &fx.state)
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
    let out = maj(&fx.catalog, &fx.state)
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
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.add_location("nas", &location);
    // Flip readonly by rewriting the config file the CLI just created.
    let config = find_sync_toml(&fx.state);
    let text = std::fs::read_to_string(&config).expect("read");
    let flipped = text.replace("readonly = false", "readonly = true");
    assert_ne!(
        flipped, text,
        "sync.toml must already contain readonly = false: {text}"
    );
    std::fs::write(&config, flipped).expect("write");
    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("readonly = true")
            && stderr.contains("sync.toml")
            && stderr.contains("set `readonly = false` there to push from this machine"),
        "refusal must name the setting, the file, and the remedy: {stderr}"
    );
}

#[test]
fn unreachable_location_is_skipped_with_a_notice_not_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let good = root.path().join("nas");
    let gone = root.path().join("shuttle");
    std::fs::create_dir_all(&good).expect("mkdir");
    std::fs::create_dir_all(&gone).expect("mkdir");
    fx.add_location("nas", &good);
    fx.add_location("shuttle", &gone);
    std::fs::remove_dir_all(&gone).expect("eject the shuttle");
    let out = maj(&fx.catalog, &fx.state)
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
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    // Two blobs; one becomes unreadable before push runs.
    fx.write_blob("ab/abcd/thumb-320.webp", b"w1");
    fx.write_blob("cd/cdef/thumb-320.webp", b"w2");
    let blocked = fx.catalog.join("blobs/ab/abcd/thumb-320.webp");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    fx.add_location("nas", &location);

    let out = maj(&fx.catalog, &fx.state)
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
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.scan_one_file();
    fx.write_blob("ab/abcd/thumb-320.webp", b"thumb");
    fx.write_blob("ab/abcd/tags.json.zst", b"tags");
    fx.add_location("nas", &location);

    let segment = location.join("events/test-machine/0001.jsonl");
    let thumb = location.join("blobs/ab/abcd/thumb-320.webp");
    let metadata = location.join("blobs/ab/abcd/tags.json.zst");

    maj(&fx.catalog, &fx.state)
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

    maj(&fx.catalog, &fx.state)
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

    maj(&fx.catalog, &fx.state)
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
    let fx = fixture(root.path(), "fx");
    let nas = root.path().join("nas");
    let usb = root.path().join("usb");
    std::fs::create_dir_all(&nas).expect("mkdir");
    std::fs::create_dir_all(&usb).expect("mkdir");
    fx.scan_one_file();
    fx.write_blob("ab/abcd/thumb-320.webp", b"thumb");
    fx.add_location("nas", &nas);
    fx.add_location("usb", &usb);

    maj(&fx.catalog, &fx.state)
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

    let out = maj(&fx.catalog, &fx.state)
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
#[cfg(unix)]
fn json_rows_pin_the_agent_facing_contract() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let nas = root.path().join("nas");
    let gone = root.path().join("gone");
    let denied = root.path().join("denied");
    std::fs::create_dir_all(&nas).expect("mkdir");
    std::fs::create_dir_all(&gone).expect("mkdir");
    std::fs::create_dir_all(&denied).expect("mkdir");
    fx.scan_one_file();
    fx.add_location("nas", &nas);
    fx.add_location("gone", &gone);
    fx.add_location("denied", &denied);
    std::fs::remove_dir_all(&gone).expect("eject the gone location");
    // No write permission on the location root itself means `execute` can't
    // create its `tmp/` staging dir — a plan/execute setup failure distinct
    // from "the mount isn't there at all".
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o555))
        .expect("chmod 555 to block tmp/ creation");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "push", "--json"])
        .assert()
        .success(); // "nas" still succeeded — exit 0
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();

    // Restore permissions so the tempdir can be cleaned up regardless of
    // what the assertions below find.
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755))
        .expect("restore perms");

    let rows: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = rows.as_array().expect("a JSON array of rows");
    assert_eq!(rows.len(), 3, "one row per requested location: {stdout}");

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
        "an unreachable row carries only {{location, skipped}}, never the outcome fields: {gone_row}"
    );
    assert!(
        gone_row["skipped"].is_string(),
        "skipped must be a string, never null: {gone_row}"
    );

    let denied_row = rows
        .iter()
        .find(|r| r["location"] == "denied")
        .expect("a row for denied");
    let keys: std::collections::BTreeSet<&str> = denied_row
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["location", "error"].into_iter().collect(),
        "a failed-transfer row carries only {{location, error}}, distinct from skipped: {denied_row}"
    );
    assert!(
        denied_row["error"].is_string(),
        "error must be a string, never null: {denied_row}"
    );
}

#[test]
fn push_fails_when_every_requested_location_is_unreachable() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir");
    std::fs::create_dir_all(&b).expect("mkdir");
    fx.add_location("a", &a);
    fx.add_location("b", &b);
    std::fs::remove_dir_all(&a).expect("eject a");
    std::fs::remove_dir_all(&b).expect("eject b");

    let out = maj(&fx.catalog, &fx.state)
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
fn push_errors_name_their_remedies() {
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

#[test]
fn pull_applies_a_teammates_events_and_names_the_index_remedy() {
    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    // Machine 1: its own catalog root + state, scans a file and a blob,
    // pushes both to the shared location.
    let m1 = fixture_as(root.path(), "m1", "m1");
    m1.scan_one_file();
    m1.write_blob("ab/abcd/thumb-320.webp", b"w");
    m1.add_location("nas", &location);
    maj_as(&m1.catalog, &m1.state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    // Machine 2: a separate catalog root + state, pulling from the same
    // location.
    let m2 = fixture_as(root.path(), "m2", "m2");
    m2.add_location("nas", &location);
    let out = maj_as(&m2.catalog, &m2.state, "m2")
        .args(["sync", "pull"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("applied") && stdout.contains("m1"),
        "pull must report applied events and the machines they came from: {stdout}"
    );
    assert!(
        stdout.contains("maj index run"),
        "fetched blobs must carry the index remedy notice: {stdout}"
    );
    assert!(
        m2.catalog.join("blobs/ab/abcd/thumb-320.webp").is_file(),
        "the pulled blob must land in machine 2's own catalog"
    );

    // Pin the apply directly, on disk, BEFORE any other `maj` invocation:
    // `maj search` (below) itself opens and incrementally syncs the sqlite
    // catalog, so asserting only through search couldn't tell "`cmd_pull`
    // applied this" from "the next command happened to apply it" — deleting
    // `cmd_pull`'s own `FsApp::open`/`open_catalog` call would leave this
    // test green regardless. Read `assets` straight off `catalog.db`.
    let db = find_catalog_db(&m2.state);
    assert_eq!(
        asset_count(&db),
        1,
        "the pulled asset must already be in machine 2's sqlite catalog \
         immediately after pull, before any other command could apply it"
    );

    // The pulled events are actually in machine 2's catalog: search sees
    // the scanned asset.
    let out = maj_as(&m2.catalog, &m2.state, "m2")
        .args(["search", "a.jpg"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("a.jpg"),
        "pulled catalog must be searchable: {stdout}"
    );

    // A second pull after full convergence reports zero applied, not an
    // error — the manual round-trip check this pins automatically. Zero
    // blobs fetched this time means the index remedy notice must be gone,
    // not printed unconditionally.
    let out = maj_as(&m2.catalog, &m2.state, "m2")
        .args(["sync", "pull"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("applied 0 new event"),
        "a converged pull reports zero applied: {stdout}"
    );
    assert!(
        !stdout.contains("maj index run"),
        "a converged pull fetched no blobs — the remedy notice must not print: {stdout}"
    );

    // `--json` on that same converged pull must be exactly one parseable
    // document — see `assert_pull_json_is_one_document`'s doc comment.
    assert_pull_json_is_one_document(&m2);
}

/// Runs `maj sync pull --json` and asserts stdout is exactly ONE parseable
/// JSON document: the per-location rows folded into the summary object,
/// not the rows array printed separately followed by a second object.
/// `serde_json::from_str` is strict about trailing data, so two
/// concatenated documents fail to parse at all. Split out of
/// `pull_applies_a_teammates_events_and_names_the_index_remedy` purely to
/// stay under the house max-function-length lint.
#[cfg(test)]
fn assert_pull_json_is_one_document(m2: &Fixture) {
    let out = maj_as(&m2.catalog, &m2.state, &m2.machine)
        .args(["sync", "pull", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("pull --json must print exactly one parseable JSON document");
    let keys: std::collections::BTreeSet<&str> = doc
        .as_object()
        .expect("a JSON object, not an array of rows")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["locations", "applied_events", "machines", "blobs_fetched"]
            .into_iter()
            .collect(),
        "pull --json's top-level keys: {doc}"
    );
    let nas_row = doc["locations"]
        .as_array()
        .expect("locations is an array")
        .iter()
        .find(|r| r["location"] == "nas")
        .expect("a row for nas");
    let row_keys: std::collections::BTreeSet<&str> = nas_row
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        row_keys,
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
        "a location outcome row's keys: {nas_row}"
    );
}

#[test]
#[cfg(unix)]
fn pull_applies_events_despite_a_blob_failure_then_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    let m1 = fixture_as(root.path(), "m1", "m1");
    m1.scan_one_file();
    m1.write_blob("ab/abcd/thumb-320.webp", b"w");
    m1.add_location("nas", &location);
    maj_as(&m1.catalog, &m1.state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    // Sabotage the pulled blob AT THE LOCATION: unreadable, so `execute`'s
    // copy of that one file fails while the segment (containing the real
    // event) still lands — pinning that a per-file blob failure must never
    // block an already-transferred segment from being applied.
    let blob = location.join("blobs/ab/abcd/thumb-320.webp");
    std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let m2 = fixture_as(root.path(), "m2", "m2");
    m2.add_location("nas", &location);
    let out = maj_as(&m2.catalog, &m2.state, "m2")
        .args(["sync", "pull"])
        .assert()
        .failure();

    // Restore permissions so the tempdir can be cleaned up regardless of
    // what the assertions below find.
    std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o644)).expect("restore perms");

    let output = out.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stdout.contains("applied") && stdout.contains("m1"),
        "the event must still apply despite the blob failure: {stdout}"
    );
    assert!(
        stderr.contains("progress was kept") && stderr.contains("retries"),
        "the final error must say progress was kept and the next run retries: {stderr}"
    );

    let out = maj_as(&m2.catalog, &m2.state, "m2")
        .args(["search", "a.jpg"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("a.jpg"),
        "the applied event must be searchable despite the blob failure: {stdout}"
    );
}

#[test]
fn pull_against_an_uninitialized_directory_names_the_catalog_init_remedy() {
    let root = tempfile::tempdir().expect("tempdir");
    // mkdir'd but never `maj catalog init`ed — the directory exists, but
    // has no `events/`.
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    std::fs::create_dir_all(&catalog).expect("mkdir");

    let out = maj_as(&catalog, &state, "m1")
        .args(["sync", "pull"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no catalog at") && stderr.contains("maj catalog init"),
        "pull against an uninitialized catalog must name the remedy: {stderr}"
    );
    assert!(
        std::fs::read_dir(&catalog)
            .expect("read catalog dir")
            .next()
            .is_none(),
        "pull must not manufacture an events/ dir (or anything else) in an \
         uninitialized catalog — it must refuse before touching it"
    );
}

#[test]
fn status_counts_are_walked_not_cached() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &location);
    maj(&fx.catalog, &fx.state)
        .args(["sync", "push"])
        .assert()
        .success();

    let synced = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let synced: serde_json::Value =
        serde_json::from_slice(&synced.get_output().stdout).expect("json");
    assert_eq!(
        synced[0]["ahead"]["blobs"]["thumbs"], 0,
        "in sync after push: {synced}"
    );

    // Sabotage: delete the remote blob. Status must see it — no cache.
    std::fs::remove_file(location.join("blobs/ab/abcd/thumb-320.webp")).expect("rm");
    let after = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&after.get_output().stdout).expect("json");
    assert_eq!(
        after[0]["ahead"]["blobs"]["thumbs"], 1,
        "a deleted remote blob must reappear in ahead-counts: {after}"
    );
}

#[test]
fn status_reports_an_unreachable_location_and_exits_zero() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let gone = root.path().join("shuttle");
    std::fs::create_dir_all(&gone).expect("mkdir");
    fx.add_location("shuttle", &gone);
    let canonical_gone = gone.canonicalize().expect("canonicalize before eject");
    std::fs::remove_dir_all(&gone).expect("eject the shuttle");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    let row = &rows[0];
    assert_eq!(row["location"], "shuttle");
    assert_eq!(row["reachable"], false);
    assert_eq!(
        row["path"].as_str(),
        canonical_gone.to_str(),
        "the unreachable row: {row}"
    );
    let keys: std::collections::BTreeSet<&str> = row
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["location", "reachable", "path"].into_iter().collect(),
        "an unreachable row's keys are exactly {{location, reachable, path}}: {row}"
    );

    // Text mode also names the location and the path, and still exits 0.
    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("shuttle") && stdout.contains(canonical_gone.to_str().expect("utf8")),
        "unreachable text row must name the location and path: {stdout}"
    );
}

#[test]
fn status_reports_behind_when_the_location_has_what_the_catalog_lacks() {
    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    let a = fixture_as(root.path(), "a", "m1");
    a.write_blob("ab/abcd/thumb-320.webp", b"w");
    a.add_location("nas", &location);
    maj_as(&a.catalog, &a.state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    let b = fixture_as(root.path(), "b", "m2");
    b.add_location("nas", &location);
    let out = maj_as(&b.catalog, &b.state, "m2")
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    assert_eq!(
        rows[0]["behind"]["blobs"]["thumbs"], 1,
        "b's catalog lacks what a already pushed to the shared location: {rows}"
    );
    assert_eq!(
        rows[0]["ahead"]["blobs"]["thumbs"], 0,
        "b has pushed nothing of its own: {rows}"
    );
}

#[test]
fn status_never_mutates_the_location_or_catalog_tree() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.scan_one_file();
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &location);
    // Push once first so both trees hold real content for status to walk —
    // an empty location can't distinguish "status wrote nothing" from
    // "status wrote nothing because there was nothing to diff".
    maj(&fx.catalog, &fx.state)
        .args(["sync", "push"])
        .assert()
        .success();
    // The push above leaves its own `tmp/` staging dir behind at the
    // location (by design — `execute` never cleans up its own successful
    // run, only stale leftovers from an interrupted one). Remove it before
    // snapshotting so a status regression that (re)creates a `tmp/` dir is
    // visible as a diff, not masked by one push already having left it
    // there.
    let stray_tmp = location.join("tmp");
    if stray_tmp.is_dir() {
        std::fs::remove_dir_all(&stray_tmp).expect("remove push's own tmp/ leftover");
    }

    let before_location = tree_snapshot(&location);
    let before_catalog = tree_snapshot(&fx.catalog);
    assert!(
        !before_location.is_empty() && !before_catalog.is_empty(),
        "the fixture must have produced real files in both trees before status runs"
    );

    maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();

    let after_location = tree_snapshot(&location);
    let after_catalog = tree_snapshot(&fx.catalog);
    assert_eq!(
        before_location, after_location,
        "status (json or text) must not add, remove, resize, or touch any file at the location"
    );
    assert_eq!(
        before_catalog, after_catalog,
        "status (json or text) must not add, remove, resize, or touch any file in the catalog"
    );
}

#[test]
fn status_groups_ahead_segments_per_machine() {
    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    // Two machines each push their own segment straight to the shared
    // location — no third fixture yet.
    let m1 = fixture_as(root.path(), "m1", "m1");
    m1.scan_one_file();
    m1.add_location("nas", &location);
    maj_as(&m1.catalog, &m1.state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    let m2 = fixture_as(root.path(), "m2", "m2");
    m2.scan_one_file();
    m2.add_location("nas", &location);
    maj_as(&m2.catalog, &m2.state, "m2")
        .args(["sync", "push"])
        .assert()
        .success();

    // A third, empty fixture is behind both machines' segments — this is
    // where per-machine grouping is observable: collapsing the two
    // machines' entries into one key (e.g. keying by segment name instead
    // of machine id) would leave only one entry here instead of two.
    let m3 = fixture_as(root.path(), "m3", "m3");
    m3.add_location("nas", &location);
    let out = maj_as(&m3.catalog, &m3.state, "m3")
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    let segments = rows[0]["behind"]["segments"]
        .as_object()
        .expect("segments object");
    let keys: std::collections::BTreeSet<&str> = segments.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["m1", "m2"].into_iter().collect(),
        "both machines' segments must appear as distinct keys, not collapsed: {segments:?}"
    );
    assert_eq!(
        segments["m1"]["files"], 1,
        "m1 pushed exactly one segment file: {segments:?}"
    );
    assert_eq!(
        segments["m2"]["files"], 1,
        "m2 pushed exactly one segment file: {segments:?}"
    );
}

#[test]
fn status_segment_bytes_are_the_destination_shortfall() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.scan_one_file();
    fx.add_location("nas", &location);
    maj(&fx.catalog, &fx.state)
        .args(["sync", "push"])
        .assert()
        .success();

    // Truncate the just-pushed remote copy to a known, small size: the
    // location is now behind by exactly `full_len - truncated_len` bytes,
    // not by the full source length (`src_len`) and not by nothing.
    let remote_segment = location.join("events/test-machine/0001.jsonl");
    let full_len = std::fs::metadata(&remote_segment).expect("meta").len();
    let truncated_len = 10u64;
    assert!(
        truncated_len < full_len,
        "the fixture's segment must be longer than the truncation target: {full_len}"
    );
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&remote_segment)
        .expect("open");
    f.set_len(truncated_len).expect("truncate");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    let entry = &rows[0]["ahead"]["segments"]["test-machine"];
    assert_eq!(
        entry["files"], 1,
        "the truncated segment is still pending: {entry}"
    );
    assert_eq!(
        entry["bytes"],
        full_len - truncated_len,
        "bytes must be the destination shortfall (full_len - truncated_len), \
         not the full source length and not zero: {entry}"
    );
}

#[test]
fn status_readonly_notice_is_text_only() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.add_location("nas", &location);
    let config = find_sync_toml(&fx.state);
    let text = std::fs::read_to_string(&config).expect("read");
    let flipped = text.replace("readonly = false", "readonly = true");
    assert_ne!(
        flipped, text,
        "sync.toml must already contain readonly = false: {text}"
    );
    std::fs::write(&config, flipped).expect("write");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("readonly = true — this machine never pushes"),
        "text status must print the readonly notice: {stdout}"
    );

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        !stdout.contains("readonly"),
        "--json status must never print the text-mode readonly notice: {stdout}"
    );
}

#[test]
fn status_against_an_uninitialized_directory_names_the_catalog_init_remedy() {
    let root = tempfile::tempdir().expect("tempdir");
    // mkdir'd but never `maj catalog init`ed — the directory exists, but
    // has no `events/`.
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    std::fs::create_dir_all(&catalog).expect("mkdir");

    let out = maj(&catalog, &state)
        .args(["sync", "status"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no catalog at") && stderr.contains("maj catalog init"),
        "status against an uninitialized catalog must name the remedy: {stderr}"
    );
}

#[test]
fn status_json_pins_the_agent_facing_contract() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.scan_one_file();
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &location);

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    let rows = rows.as_array().expect("array of rows");
    assert_eq!(rows.len(), 1, "one row per configured location: {rows:?}");
    let row = &rows[0];

    let keys: std::collections::BTreeSet<&str> = row
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["location", "reachable", "ahead", "behind"]
            .into_iter()
            .collect(),
        "a reachable row's keys: {row}"
    );

    let ahead = &row["ahead"];
    let ahead_keys: std::collections::BTreeSet<&str> = ahead
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        ahead_keys,
        ["segments", "blobs"].into_iter().collect(),
        "a direction's keys: {ahead}"
    );

    let blob_keys: std::collections::BTreeSet<&str> = ahead["blobs"]
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        blob_keys,
        ["thumbs", "metadata", "vectors", "transcripts"]
            .into_iter()
            .collect(),
        "blob class keys are always present, zero-filled: {ahead}"
    );

    let segments = ahead["segments"].as_object().expect("segments object");
    assert_eq!(
        segments.len(),
        1,
        "one machine ahead by its one pushed segment: {ahead}"
    );
    let seg = &segments["test-machine"];
    let seg_keys: std::collections::BTreeSet<&str> = seg
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        seg_keys,
        ["files", "bytes"].into_iter().collect(),
        "a per-machine segment entry's keys: {seg}"
    );
    assert_eq!(seg["files"], 1);
    assert!(seg["bytes"].as_u64().is_some_and(|b| b > 0));

    assert!(
        row["behind"]["segments"]
            .as_object()
            .expect("segments object")
            .is_empty(),
        "nothing behind yet — nothing has ever been pulled: {row}"
    );
}

#[test]
#[cfg(unix)]
fn status_reports_a_failed_location_alongside_a_healthy_one() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let healthy = root.path().join("nas");
    let broken = root.path().join("denied");
    std::fs::create_dir_all(&healthy).expect("mkdir");
    std::fs::create_dir_all(&broken).expect("mkdir");
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &healthy);
    fx.add_location("denied", &broken);
    // The location root itself stays listable (it must, or it would just
    // read as unreachable), but its `events/` subdirectory becomes
    // unreadable — a permission error `plan_transfer` hits mid-walk, not a
    // missing mount.
    std::fs::set_permissions(
        broken.join("events"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("chmod 000 the events dir");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status", "--json"])
        .assert()
        .success(); // one location failing must not abort the report, and must not fail the exit code

    // Text mode names the location and says status failed, on its own
    // line, without aborting the rest of the report. Run this WHILE
    // permissions are still broken — restoring them before this second
    // invocation would make "denied" healthy again and defeat the point.
    let text_out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&text_out.get_output().stdout).into_owned();

    // Restore permissions so the tempdir can be cleaned up regardless of
    // what the assertions below find.
    std::fs::set_permissions(
        broken.join("events"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("restore perms");

    assert!(
        stdout.contains("denied: status failed"),
        "the text report must name the failed location: {stdout}"
    );
    assert!(
        stdout.contains("nas:"),
        "the healthy location must still be reported in text mode: {stdout}"
    );

    let rows: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    let rows = rows.as_array().expect("array of rows");
    assert_eq!(
        rows.len(),
        2,
        "both locations must still get a row: {rows:?}"
    );

    let healthy_row = rows
        .iter()
        .find(|r| r["location"] == "nas")
        .expect("a row for nas");
    assert_eq!(
        healthy_row["reachable"], true,
        "the healthy location's row must be unaffected by the other's failure: {healthy_row}"
    );

    let failed_row = rows
        .iter()
        .find(|r| r["location"] == "denied")
        .expect("a row for denied");
    let keys: std::collections::BTreeSet<&str> = failed_row
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["location", "error"].into_iter().collect(),
        "a failed row carries only {{location, error}}, distinct from reachable/unreachable: {failed_row}"
    );
    assert!(
        failed_row["error"].is_string()
            && !failed_row["error"].as_str().unwrap_or_default().is_empty(),
        "error must be a non-empty string: {failed_row}"
    );
}

#[test]
fn status_text_mode_collapses_in_sync_and_headers_reachable_rows() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    fx.write_blob("ab/abcd/thumb-320.webp", b"w");
    fx.add_location("nas", &location);

    // Before any push: the catalog is ahead by one blob — not converged,
    // so the report must be the `<name>:` header plus indented direction
    // lines, never the old `{name}: {label}:` prefix repeated per line.
    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("nas:\n"),
        "an out-of-sync location gets its own header line: {stdout}"
    );
    assert!(
        stdout.contains("  ahead (push would send):")
            && stdout.contains("  behind (pull would fetch):"),
        "each direction is one indented line under the header: {stdout}"
    );
    assert!(
        !stdout.contains("nas: ahead") && !stdout.contains("nas: behind"),
        "the old per-line '{{name}}: {{label}}:' prefix must be gone: {stdout}"
    );
    assert!(
        !stdout.contains("in sync"),
        "an out-of-sync location must not also print the collapsed line: {stdout}"
    );

    // After a push, both directions are empty: the report collapses to one
    // line.
    maj(&fx.catalog, &fx.state)
        .args(["sync", "push"])
        .assert()
        .success();
    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("nas: in sync"),
        "a fully converged location collapses to a single line: {stdout}"
    );
    assert!(
        !stdout.contains("ahead") && !stdout.contains("behind"),
        "a converged location must not also print the direction lines: {stdout}"
    );
}

#[test]
fn status_fails_when_no_locations_are_configured() {
    let root = tempfile::tempdir().expect("tempdir");
    let fx = fixture(root.path(), "fx");

    let out = maj(&fx.catalog, &fx.state)
        .args(["sync", "status"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no sync locations configured") && stderr.contains("maj sync location add"),
        "status with no locations configured must name the remedy: {stderr}"
    );
}

#[test]
fn a_readonly_member_can_still_pull() {
    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");

    let m1 = fixture_as(root.path(), "m1", "m1");
    m1.scan_one_file();
    m1.add_location("nas", &location);
    maj_as(&m1.catalog, &m1.state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();

    // Machine 2 is a read-only sync member — push refuses this (pinned by
    // `readonly_refuses_push_naming_the_config_file`), but pull must not.
    let m2 = fixture_as(root.path(), "m2", "m2");
    m2.add_location("nas", &location);
    let config = find_sync_toml(&m2.state);
    let text = std::fs::read_to_string(&config).expect("read");
    let flipped = text.replace("readonly = false", "readonly = true");
    assert_ne!(
        flipped, text,
        "sync.toml must already contain readonly = false: {text}"
    );
    std::fs::write(&config, flipped).expect("write");

    maj_as(&m2.catalog, &m2.state, "m2")
        .args(["sync", "pull"])
        .assert()
        .success();
}

/// The spec's `§Testing` topology: site A is TWO machines (A1, A2) sharing a
/// local NAS location dir, plus a `shuttle` location that carries state to
/// site B, a single machine that never touches the NAS. Two sites that
/// never share a live network connection still converge purely through the
/// traveling shuttle drive — both A1 and B register the SAME directory as
/// their `shuttle` location (standing in for a physical drive that visits
/// each site in turn) — and the gossip hop through A1 proves out: A2's
/// asset, known to A1 only via A1's own NAS pull, still reaches site B
/// through A1's shuttle push, which must carry every machine dir under
/// A1's catalog root, not just A1's own. Finally, site B's tag on A1's
/// asset travels back on the same drive, proving the round trip in both
/// directions. A1 then relays that tag on to A2 via the NAS, so both of
/// site A's machines converge with site B, not just A1.
#[test]
fn a_shuttle_drive_converges_two_sites_that_never_meet() {
    let root = tempfile::tempdir().expect("tempdir");
    let shuttle = root.path().join("shuttle");
    let nas = root.path().join("nas");
    std::fs::create_dir_all(&shuttle).expect("mkdir");
    std::fs::create_dir_all(&nas).expect("mkdir");

    let site_a1 = fixture_as(root.path(), "site-a1", "site-a1");
    let site_a2 = fixture_as(root.path(), "site-a2", "site-a2");
    let site_b = fixture_as(root.path(), "site-b", "site-b");
    site_a1.add_location("nas", &nas);
    site_a1.add_location("shuttle", &shuttle);
    site_a2.add_location("nas", &nas);
    site_b.add_location("shuttle", &shuttle);

    // A1 catalogs its own file, but does not push yet — its push below
    // must carry this alongside whatever it gossips in from A2 via NAS. A2
    // catalogs a second, distinct file and pushes it to the local NAS only
    // — A1 and B never see this push directly.
    site_a1.scan_named_file("interview.mov", b"mov-bytes", "vol-a1");
    site_a2.scan_named_file("b-roll.mov", b"broll-bytes", "vol-a2");
    site_a2.maj().args(["sync", "push"]).assert().success();

    // The gossip hop: A1 pulls from the NAS, learning A2's asset, THEN
    // pushes to the shuttle. That push's events/ tree now holds both A1's
    // and A2's machine dirs, so it carries A2's segment onward even though
    // A2 never touched the shuttle itself.
    site_a1
        .maj()
        .args(["sync", "pull", "--location", "nas"])
        .assert()
        .success();
    site_a1
        .maj()
        .args(["sync", "push", "--location", "shuttle"])
        .assert()
        .success();

    // The drive travels. Site B pulls the shuttle ONLY — it never touches
    // the NAS — and must see BOTH A1's and A2's assets: finding only A1's
    // would mean the gossip hop above didn't actually carry A2's segment.
    site_b.maj().args(["sync", "pull"]).assert().success();
    site_b.assert_search_finds(
        "interview",
        "interview.mov",
        "site B must see A1's asset after the shuttle round trip",
    );
    site_b.assert_search_finds(
        "b-roll",
        "b-roll.mov",
        "site B must see A2's asset gossiped through A1's NAS pull + shuttle push",
    );

    // Site B tags A1's asset and pushes its own change back to the shuttle.
    let out = site_b
        .maj()
        .args(["search", "interview", "--json"])
        .output()
        .expect("run search --json");
    let asset = first_asset_id(&out);
    site_b
        .maj()
        .args(["tag", "add", &asset, "status/select"])
        .assert()
        .success();
    site_b.maj().args(["sync", "push"]).assert().success();

    // The drive travels back. A1 pulls and sees site B's tag — a
    // tag-filter search proves the round trip, not just a raw event count.
    site_a1
        .maj()
        .args(["sync", "pull", "--location", "shuttle"])
        .assert()
        .success();
    site_a1.assert_search_finds(
        "tag:status/select",
        "interview.mov",
        "A1 must see site B's tag after the shuttle round trip",
    );

    // Close A2's loop: A1 relays the tag onward to the NAS, and A2 pulls it
    // from there — both of site A's machines converge with site B, not
    // just the one that happened to carry the shuttle.
    site_a1
        .maj()
        .args(["sync", "push", "--location", "nas"])
        .assert()
        .success();
    site_a2.maj().args(["sync", "pull"]).assert().success();
    site_a2.assert_search_finds(
        "tag:status/select",
        "interview.mov",
        "A2 must see site B's tag after A1 relays it through the NAS",
    );
}
