//! Inbox contribution acceptance: real `maj inbox process` runs over real
//! temp-dir catalogs, exercising the manifested-ingest, incomplete-upload,
//! hash-mismatch, unknown-version, and manifest-less-triage flows end to
//! end.
//!
//! Steps return `Result` instead of asserting/panicking: this binary is a
//! `harness = false` integration test (see `crates/cli/tests/acceptance.rs`
//! and its doc comment for the same pattern), so it is not compiled under
//! `cfg(test)` the way `#[test]` functions are, and the workspace denies
//! `panic`/`unwrap_used` outside test code.
use assert_cmd::Command;
use cucumber::{World, given, then, when};
use std::path::{Path, PathBuf};

/// The machine every step runs `maj` as — this suite never needs more than
/// one identity.
const MACHINE: &str = "inbox-acceptance";

#[derive(Debug, World)]
#[world(init = Self::new)]
struct InboxWorld {
    /// Holds `cat/` (catalog root), `state/` (this machine's state dir),
    /// `inbox/` (the drop folder), and `dest/` (the ingest destination) for
    /// the scenario's lifetime.
    root: Option<tempfile::TempDir>,
    last_stdout: String,
    last_stderr: String,
    /// The most recently declared contribution or manifest-less folder's
    /// name — read by steps that don't repeat it in their own Gherkin text
    /// (e.g. "the file finishes uploading", "the contribution folder has
    /// moved to \".processed\"").
    last_contribution: String,
    /// File names the most recent `Given` step created, so a `Then` step
    /// like "finds both files" can check search results without
    /// re-deriving names from scenario prose.
    tracked_files: Vec<String>,
    /// Set by the "short on disk" `Given` step (path, full bytes);
    /// completed by "the file finishes uploading".
    pending_upload: Option<(PathBuf, Vec<u8>)>,
    /// Set by the hash-mismatch `Given` step: (manifest's declared hash,
    /// the file's real hash) — checked by "the report names the mismatched
    /// file and both hashes".
    hash_pair: Option<(String, String)>,
}

impl InboxWorld {
    fn new() -> Self {
        Self {
            root: None,
            last_stdout: String::new(),
            last_stderr: String::new(),
            last_contribution: String::new(),
            tracked_files: Vec::new(),
            pending_upload: None,
            hash_pair: None,
        }
    }

    fn root_path(&self) -> Result<&Path, String> {
        self.root
            .as_ref()
            .map(tempfile::TempDir::path)
            .ok_or_else(|| "no catalog set up yet".to_string())
    }

    fn catalog(&self) -> Result<PathBuf, String> {
        Ok(self.root_path()?.join("cat"))
    }

    fn state(&self) -> Result<PathBuf, String> {
        Ok(self.root_path()?.join("state"))
    }

    fn inbox(&self) -> Result<PathBuf, String> {
        Ok(self.root_path()?.join("inbox"))
    }

    fn dest(&self) -> Result<PathBuf, String> {
        Ok(self.root_path()?.join("dest"))
    }

    /// Builds a `maj` invocation with this machine's catalog/state env
    /// already set.
    fn maj(&self) -> Result<Command, String> {
        let mut cmd = Command::cargo_bin("maj").map_err(|e| e.to_string())?;
        cmd.env("MAJ_CATALOG", self.catalog()?)
            .env("MAJ_MACHINE_ID", MACHINE)
            .env("MAJ_STATE_DIR", self.state()?);
        Ok(cmd)
    }

    /// Runs `maj` with `args`, recording stdout/stderr, and fails the step
    /// (not panicking) on a nonzero exit — for the "setup" commands (catalog
    /// init, para add, search) whose success is always assumed.
    fn exec(&mut self, args: &[&str]) -> Result<(), String> {
        let output = self.maj()?.args(args).output().map_err(|e| e.to_string())?;
        self.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        self.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(format!(
                "`maj {}` failed: {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                output.status,
                self.last_stdout,
                self.last_stderr
            ));
        }
        Ok(())
    }

    /// Runs `maj inbox process <inbox> --dest <dest> <extra...>` with
    /// `env` additionally set, recording stdout/stderr. Does not assert on
    /// the exit status — callers decide what a given invocation's success
    /// or failure means (a fresh failure vs. a converged/recorded one).
    fn process_with_env(
        &mut self,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> Result<std::process::ExitStatus, String> {
        let inbox = self.inbox()?.to_string_lossy().into_owned();
        let dest = self.dest()?.to_string_lossy().into_owned();
        let mut cmd = self.maj()?;
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.args(["inbox", "process", &inbox, "--dest", &dest]);
        cmd.args(extra);
        let output = cmd.output().map_err(|e| e.to_string())?;
        self.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        self.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Ok(output.status)
    }

    fn process(&mut self, extra: &[&str]) -> Result<std::process::ExitStatus, String> {
        self.process_with_env(extra, &[])
    }
}

fn xxh64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh64::xxh64(bytes, 0))
}

fn init_catalog(world: &mut InboxWorld, kind: &str, name: &str) -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    world.root = Some(root);
    std::fs::create_dir_all(world.inbox()?).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(world.dest()?).map_err(|e| e.to_string())?;
    world.exec(&["catalog", "init"])?;
    world.exec(&["para", "add", kind, name])?;
    Ok(())
}

#[given(expr = "a catalog with a PARA project {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn catalog_with_project(world: &mut InboxWorld, name: String) -> Result<(), String> {
    init_catalog(world, "project", &name)
}

#[given(expr = "a catalog with a PARA resource {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn catalog_with_resource(world: &mut InboxWorld, name: String) -> Result<(), String> {
    init_catalog(world, "resource", &name)
}

/// Writes a manifested contribution folder with `count` real, correctly
/// hashed files, targeting `target` (a `<kind>/<name>` PARA node) on behalf
/// of `contributor`.
#[given(
    expr = "a contribution {string} of {int} files from contributor {string} targeting {string}"
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn contribution_of_n_files(
    world: &mut InboxWorld,
    name: String,
    count: i64,
    contributor: String,
    target: String,
) -> Result<(), String> {
    let dir = world.inbox()?.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut files_json = Vec::new();
    let mut names = Vec::new();
    for i in 0..count {
        let file_name = format!("clip-{i}.mov");
        let payload = format!("bytes-for-{name}-{i}").into_bytes();
        std::fs::write(dir.join(&file_name), &payload).map_err(|e| e.to_string())?;
        files_json.push(format!(
            r#"{{"name":"{file_name}","xxh64":"{}","size":{}}}"#,
            xxh64_hex(&payload),
            payload.len()
        ));
        names.push(file_name);
    }
    let manifest = format!(
        r#"{{"version":1,"contributor":"{contributor}","para_target":"{target}","source":"acceptance","files":[{}]}}"#,
        files_json.join(",")
    );
    std::fs::write(dir.join("contribution.json"), manifest).map_err(|e| e.to_string())?;
    world.last_contribution = name;
    world.tracked_files = names;
    Ok(())
}

/// Writes a manifest promising a file whose declared size is larger than
/// what's actually on disk yet — the "still uploading" wait path.
#[given(expr = "a contribution {string} whose manifest promises a file that is short on disk")]
fn contribution_short_on_disk(world: &mut InboxWorld, name: String) -> Result<(), String> {
    let dir = world.inbox()?.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let payload = b"full-payload-bytes-for-slow-upload".to_vec();
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(&payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).map_err(|e| e.to_string())?;
    // Only the first few bytes have landed so far — an upload in progress.
    std::fs::write(dir.join("clip.mov"), &payload[..4]).map_err(|e| e.to_string())?;
    world.last_contribution = name;
    world.tracked_files = vec!["clip.mov".to_string()];
    world.pending_upload = Some((dir.join("clip.mov"), payload));
    Ok(())
}

/// Writes a manifest whose declared `xxh64` does not match the file's real
/// bytes — the hash-mismatch failure path.
#[given(expr = "a contribution {string} whose manifest hash does not match the file")]
fn contribution_hash_mismatch(world: &mut InboxWorld, name: String) -> Result<(), String> {
    let dir = world.inbox()?.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let payload = b"actual-bytes-on-disk-for-clip".to_vec();
    let wrong_hash = "0000000000000000".to_string();
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{wrong_hash}","size":{}}}]}}"#,
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).map_err(|e| e.to_string())?;
    let computed_hash = xxh64_hex(&payload);
    std::fs::write(dir.join("clip.mov"), &payload).map_err(|e| e.to_string())?;
    world.last_contribution = name;
    world.tracked_files = vec!["clip.mov".to_string()];
    world.hash_pair = Some((wrong_hash, computed_hash));
    Ok(())
}

/// Writes a well-formed contribution whose `contribution.json` declares an
/// unsupported `version`.
#[given(expr = "a contribution {string} with manifest version {int}")]
fn contribution_with_version(
    world: &mut InboxWorld,
    name: String,
    version: i64,
) -> Result<(), String> {
    let dir = world.inbox()?.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let payload = b"version-mismatch-bytes".to_vec();
    let manifest = format!(
        r#"{{"version":{version},"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(&payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("clip.mov"), &payload).map_err(|e| e.to_string())?;
    world.last_contribution = name;
    Ok(())
}

/// Writes `count` manifest-less files under a bare folder — quiescence
/// itself is forced by setting `MAJ_INBOX_QUIESCENCE_MS=0` when the pass
/// runs (see "I process the inbox with triage target"), the same shortcut
/// `inbox_smoke.rs` uses rather than backdating real mtimes.
#[given(expr = "a quiescent manifest-less folder {string} holding {int} file")]
fn quiescent_manifest_less_folder(
    world: &mut InboxWorld,
    name: String,
    count: i64,
) -> Result<(), String> {
    let dir = world.inbox()?.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for i in 0..count {
        let file_name = format!("photo-{i}.heic");
        std::fs::write(dir.join(&file_name), format!("bytes-{i}").into_bytes())
            .map_err(|e| e.to_string())?;
        names.push(file_name);
    }
    world.last_contribution = name;
    world.tracked_files = names;
    Ok(())
}

#[when("I process the inbox")]
fn process_inbox(world: &mut InboxWorld) -> Result<(), String> {
    let status = world.process(&[])?;
    if !status.success() {
        return Err(format!(
            "expected `maj inbox process` to succeed, but it failed\nstdout: {}\nstderr: {}",
            world.last_stdout, world.last_stderr
        ));
    }
    Ok(())
}

#[when("I process the inbox expecting failure")]
fn process_inbox_expecting_failure(world: &mut InboxWorld) -> Result<(), String> {
    let status = world.process(&[])?;
    if status.success() {
        return Err(format!(
            "expected `maj inbox process` to fail, but it succeeded\nstdout: {}",
            world.last_stdout
        ));
    }
    Ok(())
}

#[when(expr = "I process the inbox with triage target {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn process_inbox_with_triage(world: &mut InboxWorld, target: String) -> Result<(), String> {
    let status = world.process_with_env(
        &["--triage-target", &target],
        &[("MAJ_INBOX_QUIESCENCE_MS", "0")],
    )?;
    if !status.success() {
        return Err(format!(
            "expected `maj inbox process --triage-target {target}` to succeed, but it failed\n\
             stdout: {}\nstderr: {}",
            world.last_stdout, world.last_stderr
        ));
    }
    Ok(())
}

#[when("the file finishes uploading")]
fn file_finishes_uploading(world: &mut InboxWorld) -> Result<(), String> {
    let (path, payload) = world
        .pending_upload
        .take()
        .ok_or_else(|| "no pending upload to complete".to_string())?;
    std::fs::write(&path, &payload).map_err(|e| e.to_string())
}

#[then(expr = "the report says {string} was ingested with {int} files")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn report_ingested(world: &mut InboxWorld, name: String, count: i64) -> Result<(), String> {
    let needle = format!("{name}: ingested {count} file(s)");
    if !world.last_stdout.contains(&needle) {
        return Err(format!(
            "expected stdout to contain {needle:?}, got: {}",
            world.last_stdout
        ));
    }
    Ok(())
}

#[then(expr = "the report says {string} is waiting")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn report_waiting(world: &mut InboxWorld, name: String) -> Result<(), String> {
    let needle = format!("{name}: waiting");
    if !world.last_stdout.contains(&needle) {
        return Err(format!(
            "expected stdout to contain {needle:?}, got: {}",
            world.last_stdout
        ));
    }
    Ok(())
}

#[then(expr = "the report says {string} was skipped with a recorded failure")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn report_recorded_failure(world: &mut InboxWorld, name: String) -> Result<(), String> {
    let needle = format!("{name}: skipped (recorded failure)");
    if !world.last_stdout.contains(&needle) {
        return Err(format!(
            "expected stdout to contain {needle:?}, got: {}",
            world.last_stdout
        ));
    }
    Ok(())
}

#[then("the report names the mismatched file and both hashes")]
fn report_names_mismatch(world: &mut InboxWorld) -> Result<(), String> {
    let (manifest_hash, computed_hash) = world
        .hash_pair
        .clone()
        .ok_or_else(|| "no hash pair recorded by a prior Given step".to_string())?;
    let combined = format!("{}\n{}", world.last_stdout, world.last_stderr);
    for needle in ["clip.mov", manifest_hash.as_str(), computed_hash.as_str()] {
        if !combined.contains(needle) {
            return Err(format!(
                "expected the report to name {needle:?}, got:\n{combined}"
            ));
        }
    }
    Ok(())
}

#[then("the report names version 99 and the supported version 1")]
fn report_names_version(world: &mut InboxWorld) -> Result<(), String> {
    let combined = format!("{}\n{}", world.last_stdout, world.last_stderr);
    for needle in ["version 99", "supports version 1"] {
        if !combined.contains(needle) {
            return Err(format!(
                "expected the report to mention {needle:?}, got:\n{combined}"
            ));
        }
    }
    Ok(())
}

#[then("the contribution folder has moved to \".processed\"")]
fn contribution_moved(world: &mut InboxWorld) -> Result<(), String> {
    let processed = world
        .inbox()?
        .join(".processed")
        .join(&world.last_contribution);
    if !processed.is_dir() {
        return Err(format!("expected {} to exist", processed.display()));
    }
    let original = world.inbox()?.join(&world.last_contribution);
    if original.exists() {
        return Err(format!(
            "expected {} to no longer exist",
            original.display()
        ));
    }
    Ok(())
}

/// Runs `maj search <query>` and confirms every file name tracked by the
/// most recent `Given` step appears in the (text-mode) results — shared by
/// both "finds both files" and "finds the file", which differ only in
/// scenario prose, not in what they check.
fn search_finds_every_tracked_file(world: &mut InboxWorld, query: &str) -> Result<(), String> {
    world.exec(&["search", query])?;
    let names = world.tracked_files.clone();
    if names.is_empty() {
        return Err("no tracked files to verify against search results".to_string());
    }
    for name in &names {
        if !world.last_stdout.contains(name.as_str()) {
            return Err(format!(
                "expected `search {query}` to find {name:?}, got: {}",
                world.last_stdout
            ));
        }
    }
    Ok(())
}

#[then(expr = "searching {string} finds both files")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn searching_finds_both_files(world: &mut InboxWorld, query: String) -> Result<(), String> {
    search_finds_every_tracked_file(world, &query)
}

#[then(expr = "searching {string} finds the file")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn searching_finds_the_file(world: &mut InboxWorld, query: String) -> Result<(), String> {
    search_finds_every_tracked_file(world, &query)
}

fn main() {
    futures::executor::block_on(
        InboxWorld::cucumber()
            .fail_on_skipped()
            .run_and_exit("tests/features/inbox.feature"),
    );
}
