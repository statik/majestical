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
