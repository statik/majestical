//! `contribution.json` manifest types and the two read-only validation
//! gates `maj inbox process` runs before touching a contribution's bytes:
//! [`load_manifest`] (parse + structural/trust-boundary validation) and
//! [`check_files`] (presence/size/regular-file checks, plus the
//! unlisted-file report). Orchestration — what to do with a validated
//! manifest — lives in `crate::inbox`. Moved here (from
//! `crates/cli/src/inbox_manifest.rs`) alongside that orchestration since
//! services can't depend back on the CLI crate.
//!
//! Name comparisons (manifest entries vs. the folder listing) are raw
//! UTF-8 byte equality with no Unicode NFC/NFD normalization — an iOS
//! export that writes NFD-normalized file names can misreport a real,
//! APFS-resolvable file as "unlisted". A related case, also deferred: on
//! the default case-insensitive APFS, two manifest entries differing only
//! in case collide on the same real file. Both are watchlisted pending
//! real-world signal on which normalization strategy (if any) is worth it.
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub const MANIFEST_NAME: &str = "contribution.json";
const SUPPORTED_VERSION: u32 = 1;

/// Shared tail on every hard-refusal message below: `contribution.json` is
/// written by the share-sheet Shortcut (or a future iOS app), never
/// hand-authored in the normal flow, so a validation failure here means
/// the export broke or the file was hand-edited — either way the
/// contributor (or whoever is triaging the inbox) needs a next step, not
/// just a diagnosis.
const REMEDY: &str = " — contribution.json is machine-generated; re-export the contribution \
                       from the share sheet, or fix the manifest by hand";

/// `contribution.json` — the manifest at the root of a contribution folder.
/// Wire format: `files[].name` is a `/`-separated path relative to the
/// contribution folder; `files[].xxh64` is the xxHash64 of the file's
/// bytes (seed 0) as 16 lowercase hex digits; `files[].size` is in bytes.
#[derive(Debug, serde::Deserialize)]
pub struct ContributionManifest {
    pub version: u32,
    pub contributor: String,
    #[serde(default)]
    pub para_target: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Free-form capture context; carried for future surfacing. `pub` (not
    /// `pub(crate)`, as it was pre-extraction) means rustc no longer treats
    /// it as dead code even though nothing in this crate reads it yet — an
    /// external head (MCP, GUI) is free to.
    #[serde(default)]
    pub note: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ManifestFile {
    /// `/`-separated path relative to the contribution folder.
    pub name: String,
    /// xxHash64 of the file's bytes, seed 0, as 16 lowercase hex digits.
    pub xxh64: String,
    /// Size in bytes.
    pub size: u64,
}

/// `Ok(None)` when the folder has no manifest (the manifest-less path).
/// Unknown versions, degenerate or escaping names (files, contributor,
/// source, `para_target`), duplicate file entries, and malformed hashes are
/// all hard errors — a manifest we cannot fully honor must never be
/// half-honored. Every one of these strings is contributor-controlled;
/// `contributor` and `source` are spliced straight into tag names, and
/// `para_target` is used to look up an existing PARA node (never
/// interpolated into a path itself), so this is the trust boundary.
///
/// # Errors
/// Returns an error if the manifest exists but cannot be read or parsed;
/// declares an unsupported `version`; or has a `contributor`, `source`, or
/// `para_target` that is empty or escapes the contribution folder (absolute
/// path or a `..` component; `para_target` may additionally contain at most
/// one interior `/`, e.g. `project/spring`). Also errors if any
/// `files[]` entry has a name that is empty, names a directory (trailing
/// `/`), escapes the contribution folder, repeats a name already listed, or
/// has an `xxh64` that isn't exactly 16 lowercase hex characters.
pub fn load_manifest(dir: &Path) -> Result<Option<ContributionManifest>> {
    let path = dir.join(MANIFEST_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let manifest: ContributionManifest =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    anyhow::ensure!(
        manifest.version == SUPPORTED_VERSION,
        "{} is version {} — this maj supports version {SUPPORTED_VERSION}; upgrade maj or re-export the contribution",
        path.display(),
        manifest.version
    );
    validate_identity_field("contributor", &manifest.contributor)?;
    if let Some(source) = &manifest.source {
        validate_identity_field("source", source)?;
    }
    if let Some(target) = &manifest.para_target {
        validate_identity_field("para_target", target)?;
        anyhow::ensure!(
            target.matches('/').count() <= 1,
            "manifest 'para_target' '{target}' has more than one '/' — expected \
             <kind>/<name> form (kind is project, area, resource, or archive; \
             e.g. project/spring){REMEDY}"
        );
    }
    let mut seen_names = BTreeSet::new();
    for file in &manifest.files {
        anyhow::ensure!(
            !file.name.is_empty(),
            "manifest lists an entry with an empty name — refusing the whole contribution{REMEDY}"
        );
        anyhow::ensure!(
            !file.name.ends_with('/'),
            "manifest entry '{}' names a directory, not a file — refusing the whole contribution{REMEDY}",
            file.name
        );
        anyhow::ensure!(
            !escapes_folder(&file.name),
            "manifest entry '{}' escapes the contribution folder — refusing the whole contribution{REMEDY}",
            file.name
        );
        anyhow::ensure!(
            is_lower_hex16(&file.xxh64),
            "manifest entry '{}' has xxh64 '{}' — expected exactly 16 lowercase hex characters — refusing the whole contribution{REMEDY}",
            file.name,
            file.xxh64
        );
        anyhow::ensure!(
            seen_names.insert(file.name.clone()),
            "manifest entry '{}' is listed more than once — refusing the whole contribution{REMEDY}",
            file.name
        );
    }
    Ok(Some(manifest))
}

/// `contributor`/`source`/`para_target` share this check: non-empty, and
/// doesn't escape the contribution folder. `Path::join` with an absolute
/// component discards everything before it, so an unchecked `contributor`
/// of e.g. `/tmp/evil` would silently redirect wherever it's later joined
/// onto a destination path — this is what stands between that and a hard,
/// named refusal.
fn validate_identity_field(field: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "manifest '{field}' is empty{REMEDY}");
    anyhow::ensure!(
        !escapes_folder(value),
        "manifest '{field}' '{value}' escapes the contribution folder{REMEDY}"
    );
    Ok(())
}

/// True if `name`, joined onto some base path, could resolve outside it:
/// an absolute path (which `Path::join` lets override the base entirely)
/// or any `..` component.
fn escapes_folder(name: &str) -> bool {
    let name_path = Path::new(name);
    name_path.is_absolute()
        || name_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

fn is_lower_hex16(hash: &str) -> bool {
    hash.len() == 16
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Presence/size gate ("still uploading" detection), the not-a-regular-file
/// refusal, and the unlisted-file report. Hash checking is deliberately NOT
/// here — it reads every byte, so it runs once, later, only on
/// contributions that pass this gate.
#[derive(Debug)]
pub struct FileCheck {
    /// Human-readable per-file reasons the contribution isn't ready yet
    /// (missing or short files). Transient — the next pass may find them
    /// resolved.
    pub waiting: Vec<String>,
    /// Files in the folder the manifest doesn't list (reported, left
    /// untouched, never ingested from a manifested contribution).
    pub unlisted: Vec<String>,
    /// Listed entries that resolve to something other than a regular file
    /// (a symlink, a directory, ...) — named, not silently followed or
    /// ingested. Unlike `waiting`, this is not transient: the whole
    /// contribution must be treated as failing, the same policy as a hash
    /// mismatch, and never partially ingest. This is a value, not an
    /// `Err`, on purpose — it's a fact about the contribution's contents,
    /// not an operational failure of the check itself; `Err` here is
    /// reserved for real I/O failures (an unreadable directory, a stat
    /// failure that isn't "not found") that the operator, not the
    /// contributor, must fix.
    pub refused: Vec<String>,
}

/// # Errors
/// Returns an error for real I/O failures: the contribution folder (or a
/// subdirectory) can't be read while walking for unlisted files, or
/// stat'ing a listed file fails for a reason other than "not found" (e.g.
/// permission denied) — surfaced loudly rather than reported as
/// [`FileCheck::waiting`] forever, which "not found" alone would wrongly
/// suggest is just an in-progress upload. A listed entry that isn't a
/// regular file is reported in [`FileCheck::refused`], not returned as an
/// `Err`.
pub fn check_files(dir: &Path, manifest: &ContributionManifest) -> Result<FileCheck> {
    let mut waiting = Vec::new();
    let mut refused = Vec::new();
    let mut listed = BTreeSet::new();
    for file in &manifest.files {
        listed.insert(file.name.clone());
        let path = dir.join(&file.name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() => {
                refused.push(format!(
                    "{}: {}, not a regular file — refusing the whole contribution{REMEDY}",
                    file.name,
                    describe_non_file(meta.file_type())
                ));
            }
            Ok(meta) if meta.len() == file.size => {}
            Ok(meta) => waiting.push(format!(
                "{}: {} of {} bytes present — still uploading?",
                file.name,
                meta.len(),
                file.size
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                waiting.push(format!("{}: not yet present", file.name));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("checking {}", path.display()));
            }
        }
    }
    let mut unlisted = Vec::new();
    collect_unlisted(dir, dir, &listed, &mut unlisted)?;
    Ok(FileCheck {
        waiting,
        unlisted,
        refused,
    })
}

fn describe_non_file(file_type: std::fs::FileType) -> &'static str {
    if file_type.is_symlink() {
        "a symlink"
    } else if file_type.is_dir() {
        "a directory"
    } else {
        "a special file"
    }
}

/// Walks for files the manifest doesn't list. Symlinks are never followed:
/// a symlinked directory is reported as a single unlisted leaf and not
/// descended into (its contents, and whatever they point at, are none of
/// this contribution's business); a symlinked file is likewise reported by
/// its own name in the contribution folder, never through the link.
/// `entry.file_type()` (unlike `Path::is_dir`/`is_file`) reports the entry
/// itself and does not follow symlinks, which is what makes this safe.
///
/// Iterative (explicit stack), not recursive: this walks a
/// contributor-controlled tree, and unbounded recursion on an attacker- or
/// tool-crafted deep directory tree is a stack overflow (SIGSEGV), not a
/// catchable error.
fn collect_unlisted(
    root: &Path,
    start: &Path,
    listed: &BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("reading file type of {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if rel != MANIFEST_NAME && !listed.contains(&rel) {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_json(files: &str) -> String {
        format!(
            r#"{{"version":1,"contributor":"dana","para_target":"project/spring","source":"iphone","files":{files}}}"#
        )
    }

    #[test]
    fn a_complete_contribution_is_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"abcd").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        assert_eq!(manifest.contributor, "dana");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert!(check.waiting.is_empty());
        assert!(check.unlisted.is_empty());
        assert!(check.refused.is_empty());
    }

    #[test]
    fn short_and_missing_files_mean_still_uploading() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"ab").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(
                r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4},
                    {"name":"IMG_2.HEIC","xxh64":"deadbeef00000001","size":9}]"#,
            ),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(
            check.waiting.len(),
            2,
            "one short, one missing: {:?}",
            check.waiting
        );
    }

    #[test]
    fn unlisted_files_are_reported_never_absorbed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"abcd").expect("write");
        std::fs::write(dir.path().join("stray.mov"), b"x").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(check.unlisted, vec!["stray.mov".to_string()]);
    }

    #[test]
    fn unlisted_files_are_collected_recursively_and_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/sub")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("a")).expect("mkdir");
        // Written out of lexicographic order on purpose — the result must
        // still come back fully sorted, not just sorted within one directory.
        std::fs::write(dir.path().join("z.txt"), b"z").expect("write");
        std::fs::write(dir.path().join("b/sub/nested.txt"), b"n").expect("write");
        std::fs::write(dir.path().join("a/one.txt"), b"1").expect("write");
        std::fs::write(dir.path().join("b/two.txt"), b"2").expect("write");
        std::fs::write(dir.path().join("contribution.json"), manifest_json("[]")).expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(
            check.unlisted,
            vec![
                "a/one.txt".to_string(),
                "b/sub/nested.txt".to_string(),
                "b/two.txt".to_string(),
                "z.txt".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_directory_is_reported_but_not_descended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("secret.txt"), b"s").expect("write");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).expect("symlink dir");
        std::fs::write(dir.path().join("contribution.json"), manifest_json("[]")).expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(
            check.unlisted,
            vec!["linked".to_string()],
            "the symlink itself is reported, but nothing inside it is walked: {:?}",
            check.unlisted
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_listed_file_is_refused_not_erred() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let real = outside.path().join("real.HEIC");
        std::fs::write(&real, b"abcd").expect("write real");
        std::os::unix::fs::symlink(&real, dir.path().join("IMG_1.HEIC")).expect("symlink file");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest)
            .expect("a symlinked listed file is a refusal, not an I/O error");
        assert_eq!(check.refused.len(), 1, "{:?}", check.refused);
        assert!(
            check.refused[0].contains("IMG_1.HEIC"),
            "{:?}",
            check.refused
        );
        assert!(check.refused[0].contains("symlink"), "{:?}", check.refused);
    }

    #[test]
    fn a_listed_name_that_is_a_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("subdir")).expect("mkdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"subdir","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(check.refused.len(), 1, "{:?}", check.refused);
        assert!(check.refused[0].contains("subdir"), "{:?}", check.refused);
        assert!(
            check.refused[0].contains("directory"),
            "{:?}",
            check.refused
        );
    }

    #[test]
    fn duplicate_manifest_entries_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(
                r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4},
                    {"name":"IMG_1.HEIC","xxh64":"1111111111111111","size":9}]"#,
            ),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("duplicate entry must fail");
        assert!(err.to_string().contains("IMG_1.HEIC"), "{err}");
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn an_empty_manifest_entry_name_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"","xxh64":"deadbeef00000000","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("empty name must fail");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn a_trailing_slash_manifest_entry_name_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"sub/","xxh64":"deadbeef00000000","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("trailing slash must fail");
        assert!(err.to_string().contains("sub/"), "{err}");
    }

    #[test]
    fn a_malformed_xxh64_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"DEADBEEF00000000","size":4}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("uppercase hex must fail");
        assert!(err.to_string().contains("xxh64"), "{err}");
        assert!(err.to_string().contains("16 lowercase hex"), "{err}");
    }

    #[test]
    fn an_absolute_contributor_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":1,"contributor":"/etc/passwd","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("absolute contributor must fail");
        assert!(err.to_string().contains("contributor"), "{err}");
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[test]
    fn a_parent_dir_contributor_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":1,"contributor":"../evil","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("parent-dir contributor must fail");
        assert!(err.to_string().contains("contributor"), "{err}");
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[test]
    fn an_empty_para_target_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":1,"contributor":"dana","para_target":"","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("empty para_target must fail");
        assert!(err.to_string().contains("para_target"), "{err}");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn a_multi_slash_para_target_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":1,"contributor":"dana","para_target":"a/b/c","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("multi-slash para_target must fail");
        assert!(err.to_string().contains("para_target"), "{err}");
        assert!(err.to_string().contains("more than one"), "{err}");
    }

    #[test]
    fn an_unknown_version_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":99,"contributor":"dana","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("unknown version must fail");
        assert!(err.to_string().contains("version 99"), "{err}");
        assert!(err.to_string().contains("supports version 1"), "{err}");
    }

    #[test]
    fn a_traversal_file_name_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"../escape.mov","xxh64":"deadbeef00000000","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("traversal must fail");
        assert!(err.to_string().contains("escape"), "{err}");
    }

    #[test]
    fn malformed_json_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("contribution.json"), b"{not valid json").expect("write");
        let err = load_manifest(dir.path()).expect_err("malformed JSON must fail");
        assert!(err.to_string().contains("contribution.json"), "{err}");
    }

    #[test]
    fn a_folder_without_a_manifest_loads_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_manifest(dir.path()).expect("load").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn a_permission_denied_manifest_is_a_hard_error_not_treated_as_absent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join(MANIFEST_NAME);
        std::fs::write(&manifest_path, manifest_json("[]")).expect("write");
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");

        // Same reasoning as `check_files`'s permission test: verify the OS
        // actually enforces the block independently of `load_manifest`'s
        // own result, so a mutant that folds every io error into "absent"
        // can't hide behind an environment that doesn't enforce mode 000.
        let os_enforces_the_block = std::fs::read_to_string(&manifest_path)
            .err()
            .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);

        let result = load_manifest(dir.path());

        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms");

        if !os_enforces_the_block {
            #[expect(
                clippy::print_stderr,
                reason = "test-only environment hedge notice, verbatim from cli"
            )]
            {
                eprintln!("skipping: this environment does not enforce a mode-000 file");
            }
            return;
        }
        assert!(
            result.is_err(),
            "a permission-denied manifest must be a hard error, not Ok(None): {result:?}"
        );
    }

    /// A stat failure other than "not found" (permission denied) on a
    /// listed file must surface loudly, not sit in `waiting` forever —
    /// "not yet present" would tell the operator to just wait, which is
    /// never going to resolve a permissions problem. This is deliberately
    /// NOT tested by denying execute on the contribution folder (chmod
    /// 000): that approach is structurally unable to fail — `Ok` triggers
    /// an environment-hedge skip and `Err` satisfies the assertion, so a
    /// mutant that silently folds every io error into `waiting` also
    /// returns `Ok` and vacuously skips instead of failing the test.
    /// Listing a file *through* a path component that is itself a plain
    /// file (`plain-file/IMG_1.HEIC`) makes `symlink_metadata` fail with
    /// `NotADirectory`/`Other`, never `NotFound`, deterministically and
    /// without relying on permission enforcement at all — and
    /// `collect_unlisted`'s separate walk never descends into `plain-file`
    /// (it isn't a directory), so this is the ONLY error source in the
    /// call, isolating the guard completely.
    #[test]
    fn a_listed_path_through_a_non_directory_component_is_a_hard_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plain-file"), b"not a directory").expect("write");
        let manifest = ContributionManifest {
            version: 1,
            contributor: "dana".to_string(),
            para_target: None,
            source: None,
            note: None,
            files: vec![ManifestFile {
                name: "plain-file/IMG_1.HEIC".to_string(),
                xxh64: "deadbeef00000000".to_string(),
                size: 4,
            }],
        };
        let result = check_files(dir.path(), &manifest);
        assert!(
            result.is_err(),
            "a path through a non-directory component must be a hard error, \
             not silently waiting: {result:?}"
        );
    }
}
