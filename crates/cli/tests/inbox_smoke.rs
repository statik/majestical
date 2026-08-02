//! `maj inbox process` end to end: real manifests, real verified ingest,
//! real ASC MHL, real provenance tags.
mod common;

use common::maj;
use std::path::Path;

#[cfg(test)]
fn xxh64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh64::xxh64(bytes, 0))
}

#[cfg(test)]
struct Setup {
    root: tempfile::TempDir,
}

#[cfg(test)]
impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let s = Self { root };
        maj(&s.catalog(), &s.state())
            .args(["catalog", "init"])
            .assert()
            .success();
        maj(&s.catalog(), &s.state())
            .args(["para", "add", "project", "spring"])
            .assert()
            .success();
        std::fs::create_dir_all(s.inbox()).expect("mkdir");
        std::fs::create_dir_all(s.dest()).expect("mkdir");
        s
    }
    fn catalog(&self) -> std::path::PathBuf {
        self.root.path().join("cat")
    }
    fn state(&self) -> std::path::PathBuf {
        self.root.path().join("state")
    }
    fn inbox(&self) -> std::path::PathBuf {
        self.root.path().join("inbox")
    }
    fn dest(&self) -> std::path::PathBuf {
        self.root.path().join("dest")
    }

    fn write_contribution(&self, folder: &str, payload: &[u8], hash: &str) {
        let dir = self.inbox().join(folder);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("clip.mov"), payload).expect("write");
        let manifest = format!(
            r#"{{"version":1,"contributor":"dana","para_target":"project/spring","source":"iphone","files":[{{"name":"clip.mov","xxh64":"{hash}","size":{}}}]}}"#,
            payload.len()
        );
        std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");
    }

    fn process(&self) -> assert_cmd::assert::Assert {
        maj(&self.catalog(), &self.state())
            .args(["inbox", "process"])
            .arg(self.inbox())
            .args(["--dest"])
            .arg(self.dest())
            .assert()
    }
}

#[test]
fn a_valid_contribution_ingests_with_provenance_and_moves_to_processed() {
    let s = Setup::new();
    let payload = b"mov-bytes-for-clip";
    s.write_contribution("drop-1", payload, &xxh64_hex(payload));
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("drop-1"),
        "report names the contribution: {stdout}"
    );
    assert!(
        s.inbox().join(".processed/drop-1/clip.mov").is_file(),
        "success moves the contribution to .processed/"
    );
    assert!(!s.inbox().join("drop-1").exists());
    // Real MHL was written at the destination.
    let ascmhl = s.dest().join("ascmhl");
    assert!(ascmhl.is_dir(), "verified ingest writes an ASC MHL history");
    // Provenance tags are searchable.
    for query in ["tag:contributor/dana", "tag:source/iphone"] {
        let out = maj(&s.catalog(), &s.state())
            .args(["search", query])
            .assert()
            .success();
        let found = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(
            found.contains("clip.mov"),
            "{query} must find the clip: {found}"
        );
    }
    // A second pass is a clean no-op: .processed/ is skipped.
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("nothing to process"), "{stdout}");
}

#[test]
fn keep_leaves_the_contribution_and_a_redrop_dedupes() {
    let s = Setup::new();
    let payload = b"mov-bytes-for-clip";
    s.write_contribution("drop-keep", payload, &xxh64_hex(payload));
    maj(&s.catalog(), &s.state())
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .args(["--keep"])
        .assert()
        .success();
    assert!(
        s.inbox().join("drop-keep/clip.mov").is_file(),
        "--keep must leave the contribution in place"
    );
    // The same content dropped again (new folder) dedupes: the planner's
    // content-hash prefilter marks it duplicate/skip, so nothing re-copies
    // but the pass still succeeds.
    s.write_contribution("drop-again", payload, &xxh64_hex(payload));
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("drop-again"), "{stdout}");
    let copies = walkdir_count(&s.dest(), "clip.mov");
    assert_eq!(copies, 1, "a re-dropped duplicate must not copy again");
}

/// Counts files named `name` under `root`, recursively.
#[cfg(test)]
fn walkdir_count(root: &Path, name: &str) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if entry.file_name().to_string_lossy() == name {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn hash_mismatch_fails_the_contribution_records_it_and_skips_next_pass() {
    let s = Setup::new();
    s.write_contribution("drop-bad", b"actual-bytes", "0000000000000000");
    let out = s.process().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("clip.mov") && stderr.contains("0000000000000000"),
        "failure must name the file and both hashes: {stderr}"
    );
    assert!(
        s.inbox().join("drop-bad/clip.mov").is_file(),
        "a failed contribution is left untouched in the inbox"
    );
    assert!(
        !s.dest().join("ascmhl").exists(),
        "nothing from a failed contribution may be ingested"
    );
    // Second pass: skipped via the recorded marker, not re-hashed; the
    // pass itself succeeds (a recorded failure is a notice, not an error).
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("drop-bad") && stdout.contains("recorded failure"),
        "second pass must skip with the recorded reason: {stdout}"
    );
}

#[test]
fn incomplete_upload_is_skipped_and_converges_when_complete() {
    let s = Setup::new();
    let payload = b"full-payload-bytes";
    let dir = s.inbox().join("drop-slow");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), &payload[..4]).expect("partial write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write");
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("still uploading") || stdout.contains("not yet present"),
        "incomplete upload skips with the reason: {stdout}"
    );
    // Upload completes; the next pass converges.
    std::fs::write(dir.join("clip.mov"), payload).expect("complete write");
    s.process().success();
    assert!(s.inbox().join(".processed/drop-slow/clip.mov").is_file());
}

/// A file present in the folder but never declared in `contribution.json`
/// must never cross the trust boundary into a catalog destination, even
/// though the rest of the contribution is valid and ingests normally.
#[test]
fn unlisted_files_in_the_folder_are_never_ingested_or_tagged() {
    let s = Setup::new();
    let payload = b"listed-clip-bytes";
    s.write_contribution("drop-unlisted", payload, &xxh64_hex(payload));
    std::fs::write(
        s.inbox().join("drop-unlisted/stray.mov"),
        b"never-declared-bytes",
    )
    .expect("write stray file");
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("drop-unlisted"), "{stdout}");
    assert_eq!(
        walkdir_count(&s.dest(), "stray.mov"),
        0,
        "an unlisted file must never be copied into a destination"
    );
    let out = maj(&s.catalog(), &s.state())
        .args(["search", "stray.mov"])
        .assert()
        .success();
    let found = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        !found.contains("stray.mov"),
        "an unlisted file must never become a searchable, tagged catalog asset: {found}"
    );
}

/// A `para_target` naming a PARA node that was never created fails only
/// that one contribution — the remedy names the exact fix — and does not
/// block a valid contribution processed in the same pass.
#[test]
fn a_nonexistent_para_target_fails_only_that_contribution() {
    let s = Setup::new();
    let bad_payload = b"ghost-node-bytes";
    let dir = s.inbox().join("drop-ghost");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), bad_payload).expect("write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/ghost","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(bad_payload),
        bad_payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");
    let good_payload = b"good-node-bytes";
    s.write_contribution("drop-good", good_payload, &xxh64_hex(good_payload));

    let out = s.process().failure();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("maj para add project ghost"),
        "the remedy must name the exact command: {stderr}"
    );
    assert!(
        stdout.contains("drop-good") && stdout.contains("ingested"),
        "a bad contribution must not block a good one in the same pass: {stdout}"
    );
    assert!(
        s.inbox().join(".processed/drop-good").exists(),
        "the good contribution must still be processed"
    );
    assert!(
        s.inbox().join("drop-ghost").exists(),
        "the bad contribution stays put for the operator to fix"
    );
}

/// `para_target` is optional in the wire format — a well-formed manifest
/// that simply hasn't been routed yet must fail only itself, never halt
/// every other contribution in the same pass.
#[test]
fn a_missing_para_target_fails_only_that_contribution() {
    let s = Setup::new();
    let untargeted_payload = b"untargeted-clip-bytes";
    let dir = s.inbox().join("drop-no-target");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), untargeted_payload).expect("write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(untargeted_payload),
        untargeted_payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");
    let good_payload = b"targeted-clip-bytes";
    s.write_contribution("drop-good-2", good_payload, &xxh64_hex(good_payload));

    let out = s.process().failure();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("drop-no-target") && stdout.contains("para_target"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("drop-no-target: FAILED — drop-no-target:"),
        "the report line already prepends the name — the message must not repeat it: {stdout}"
    );
    assert!(
        stdout.contains("drop-good-2") && stdout.contains("ingested"),
        "a manifest missing para_target must not block others: {stdout}"
    );
}

/// `--json` must emit exactly one parseable JSON document on stdout — a
/// per-contribution `maj ingest`-style blob interleaved ahead of the final
/// report would leave trailing bytes a strict parse rejects.
#[test]
fn json_output_is_a_single_parseable_document() {
    let s = Setup::new();
    let payload = b"json-clip-bytes";
    s.write_contribution("drop-json", payload, &xxh64_hex(payload));
    let out = maj(&s.catalog(), &s.state())
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .args(["--json"])
        .assert()
        .success();
    let stdout = out.get_output().stdout.clone();
    let rows: serde_json::Value =
        serde_json::from_slice(&stdout).expect("stdout must be exactly one JSON document");
    let arr = rows.as_array().expect("report is a JSON array");
    assert_eq!(arr.len(), 1, "one row per contribution, nothing extra");
    assert_eq!(arr[0]["contribution"], "drop-json");
    assert_eq!(arr[0]["status"], "ingested");
    assert_eq!(arr[0]["placed"], 1);
}

/// The marker fingerprint must fold in the listed file's own mtime/size,
/// not just the manifest's — otherwise fixing the corrupt file (the exact
/// remedy the hash-mismatch message recommends) without touching
/// `contribution.json` would never re-validate, and the contribution would
/// stay recorded-failed forever.
#[test]
fn fixing_only_the_file_after_a_hash_mismatch_reconverges() {
    let s = Setup::new();
    let correct = b"correct-bytes-here!";
    let corrupt = b"CORRUPT-BYTES-HERE!";
    assert_eq!(
        correct.len(),
        corrupt.len(),
        "same declared size, different content"
    );
    let dir = s.inbox().join("drop-fix");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), corrupt).expect("write corrupt");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(correct),
        correct.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");

    s.process().failure();
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("recorded failure"),
        "an unchanged corrupt file must stay a notice, not re-hash: {stdout}"
    );

    // Fix ONLY the file's bytes — mtime granularity is milliseconds, so a
    // fast back-to-back write needs a real gap to guarantee a different
    // mtime_ms and thus a different fingerprint.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(dir.join("clip.mov"), correct).expect("write correct");
    s.process().success();
    assert!(
        s.inbox().join(".processed/drop-fix/clip.mov").is_file(),
        "fixing only the file must reconverge to ingested, not stay stuck"
    );
}

/// A second contributor re-dropping bytes someone else already ingested
/// produces no new copy (content-addressed dedupe), but must still be
/// tagged with their own `contributor/` provenance — the asset is real and
/// this contribution genuinely vouches for it too.
#[test]
fn a_redrop_under_a_different_contributor_still_gets_tagged() {
    let s = Setup::new();
    let payload = b"shared-clip-bytes";
    s.write_contribution("drop-dana", payload, &xxh64_hex(payload));
    s.process().success();

    let dir = s.inbox().join("drop-sam");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), payload).expect("write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"sam","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");
    s.process().success();

    for query in ["tag:contributor/dana", "tag:contributor/sam"] {
        let out = maj(&s.catalog(), &s.state())
            .args(["search", query])
            .assert()
            .success();
        let found = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(
            found.contains("clip.mov"),
            "{query} must find the clip: {found}"
        );
    }
}

/// A `para_target` naming a node that exists but is archived must never get
/// the "does not exist yet — `maj para add`" remedy: following that advice
/// creates a duplicate, indistinguishable node. It gets its own message
/// naming un-archiving instead.
#[test]
fn an_archived_para_target_names_unarchive_not_add() {
    let s = Setup::new();
    maj(&s.catalog(), &s.state())
        .args(["para", "archive", "project/spring"])
        .assert()
        .success();
    let payload = b"archived-target-bytes";
    s.write_contribution("drop-archived", payload, &xxh64_hex(payload));

    let out = s.process().failure();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("exists but is archived") && stdout.contains("un-archive"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("maj para add"),
        "an archived target must not suggest creating a duplicate node: {stdout}"
    );
}

/// state/catalogs/<key>/inbox-failures.json — exactly one catalog key
/// exists per test. Mirrors `sync_smoke.rs`'s `find_sync_toml` pattern.
#[cfg(test)]
#[cfg(unix)]
fn find_inbox_markers(state: &Path) -> std::path::PathBuf {
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("inbox-failures.json")
}

/// A later-sorting contribution whose folder contains an unreadable
/// subdirectory makes the whole pass fatal (a real I/O failure walking for
/// unlisted files, inside `check_files`) — but an earlier contribution's
/// freshly recorded failure marker must survive that fatal exit: markers
/// are stored before any pass-fatal error propagates, never only at a
/// clean end of the loop.
#[test]
#[cfg(unix)]
fn markers_persist_even_when_a_later_contribution_is_pass_fatal() {
    use std::os::unix::fs::PermissionsExt;

    let s = Setup::new();
    // "a-bad" sorts before "b-fatal" — its hash-mismatch failure must be
    // recorded even though b-fatal blows up the rest of the pass.
    s.write_contribution("a-bad", b"actual-bytes", "0000000000000000");

    let dir = s.inbox().join("b-fatal");
    let locked = dir.join("locked");
    std::fs::create_dir_all(&locked).expect("mkdir");
    let mut perms = std::fs::metadata(&locked).expect("meta").permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&locked, perms).expect("chmod 000");

    // Some environments (root, certain containers/filesystems) don't
    // enforce this restriction — skip rather than false-fail there.
    if std::fs::read_dir(&locked).is_ok() {
        let mut perms = std::fs::metadata(&locked).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&locked, perms).expect("chmod restore");
        eprintln!("skipping: this environment does not enforce a mode-000 directory (likely root)");
        return;
    }

    let payload = b"clip-bytes-for-fatal";
    std::fs::write(dir.join("clip.mov"), payload).expect("write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");

    s.process().failure();

    // Restore permissions so the tempdir can be cleaned up afterward.
    let mut perms = std::fs::metadata(&locked).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&locked, perms).expect("chmod restore");

    let markers_path = find_inbox_markers(&s.state());
    let text = std::fs::read_to_string(&markers_path).expect("read inbox-failures.json");
    assert!(
        text.contains("a-bad"),
        "the earlier contribution's marker must survive a later pass-fatal error: {text}"
    );
}
