//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app); manifest-less drops go to a triage PARA node after a
//! quiescence check. Reuses the verified-ingest pipeline end to end.
//!
//! Name comparisons (manifest entries vs. the folder listing) are raw
//! UTF-8 byte equality with no Unicode NFC/NFD normalization — an iOS
//! export that writes NFD-normalized file names can misreport a real,
//! APFS-resolvable file as "unlisted"; watchlisted, not fixed here.
#![expect(
    dead_code,
    reason = "manifest types and load/check functions are unused until `maj inbox process` \
              wires them up in Task 10; narrow or remove this once that lands"
)]
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) const MANIFEST_NAME: &str = "contribution.json";
const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ContributionManifest {
    pub version: u32,
    pub contributor: String,
    #[serde(default)]
    pub para_target: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Free-form capture context; carried for future surfacing.
    #[serde(default)]
    pub note: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ManifestFile {
    pub name: String,
    pub xxh64: String,
    pub size: u64,
}

/// `Ok(None)` when the folder has no manifest (the manifest-less path).
/// Unknown versions, degenerate names, duplicate names, and path-traversal
/// names are hard errors — a manifest we cannot fully honor must never be
/// half-honored.
///
/// # Errors
/// Returns an error if the manifest exists but cannot be read or parsed,
/// declares an unsupported `version`, or lists a file name that is empty,
/// names a directory (trailing `/`), escapes the contribution folder
/// (absolute path or a `..` component), or repeats a name already listed.
pub(crate) fn load_manifest(dir: &Path) -> Result<Option<ContributionManifest>> {
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
    let mut seen_names = std::collections::BTreeSet::new();
    for file in &manifest.files {
        anyhow::ensure!(
            !file.name.is_empty(),
            "manifest lists an entry with an empty name — refusing the whole contribution"
        );
        anyhow::ensure!(
            !file.name.ends_with('/'),
            "manifest entry '{}' names a directory, not a file — refusing the whole contribution",
            file.name
        );
        let name_path = Path::new(&file.name);
        let escapes = name_path.is_absolute()
            || name_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        anyhow::ensure!(
            !escapes,
            "manifest entry '{}' escapes the contribution folder — refusing the whole contribution",
            file.name
        );
        anyhow::ensure!(
            seen_names.insert(file.name.clone()),
            "manifest entry '{}' is listed more than once — refusing the whole contribution",
            file.name
        );
    }
    Ok(Some(manifest))
}

/// Presence/size gate ("still uploading" detection) + the unlisted-file
/// report. Hash checking is deliberately NOT here — it reads every byte,
/// so it runs once, later, only on contributions that pass this gate.
#[derive(Debug)]
pub(crate) struct FileCheck {
    /// Human-readable per-file reasons the contribution isn't ready.
    pub waiting: Vec<String>,
    /// Files in the folder the manifest doesn't list (reported, left
    /// untouched, never ingested from a manifested contribution).
    pub unlisted: Vec<String>,
}

/// # Errors
/// Returns an error if the contribution folder (or any subdirectory) can't
/// be read while walking for unlisted files, or if a manifest-listed name
/// resolves to anything other than a regular file — a symlink (which could
/// point outside the contribution folder), a directory, or other special
/// file. That case fails the whole contribution, same as a traversal name:
/// Task 10 hashes and ingests listed files sight-unseen, so "looks like the
/// right file" is not enough — it must BE the file, in place, unlinked.
pub(crate) fn check_files(dir: &Path, manifest: &ContributionManifest) -> Result<FileCheck> {
    let mut waiting = Vec::new();
    let mut listed = std::collections::BTreeSet::new();
    for file in &manifest.files {
        listed.insert(file.name.clone());
        let path = dir.join(&file.name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() => {
                anyhow::bail!(
                    "manifest entry '{}' is {}, not a regular file — refusing the whole contribution",
                    file.name,
                    describe_non_file(meta.file_type())
                );
            }
            Ok(meta) if meta.len() == file.size => {}
            Ok(meta) => waiting.push(format!(
                "{}: {} of {} bytes present — still uploading?",
                file.name,
                meta.len(),
                file.size
            )),
            Err(_) => waiting.push(format!("{}: not yet present", file.name)),
        }
    }
    let mut unlisted = Vec::new();
    collect_unlisted(dir, dir, &listed, &mut unlisted)?;
    Ok(FileCheck { waiting, unlisted })
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
fn collect_unlisted(
    root: &Path,
    dir: &Path,
    listed: &std::collections::BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading file type of {}", path.display()))?;
        if file_type.is_dir() {
            collect_unlisted(root, &path, listed, out)?;
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
    out.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_json(files: &str) -> String {
        format!(
            r#"{{"version":1,"contributor":"dana","para_target":"Projects/spring","source":"iphone","files":{files}}}"#
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
    fn a_symlinked_listed_file_is_a_hard_error() {
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
        let err = check_files(dir.path(), &manifest)
            .expect_err("a symlinked listed file must be refused, matching size or not");
        assert!(err.to_string().contains("IMG_1.HEIC"), "{err}");
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    #[test]
    fn duplicate_manifest_entries_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(
                r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4},
                    {"name":"IMG_1.HEIC","xxh64":"11111111","size":9}]"#,
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
            manifest_json(r#"[{"name":"","xxh64":"00","size":1}]"#),
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
            manifest_json(r#"[{"name":"sub/","xxh64":"00","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("trailing slash must fail");
        assert!(err.to_string().contains("sub/"), "{err}");
    }

    #[test]
    fn unknown_version_and_traversal_names_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":99,"contributor":"dana","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("unknown version must fail");
        assert!(err.to_string().contains("version 99"), "{err}");
        assert!(err.to_string().contains("supports version 1"), "{err}");

        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"../escape.mov","xxh64":"00","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("traversal must fail");
        assert!(err.to_string().contains("escape"), "{err}");
    }

    #[test]
    fn a_folder_without_a_manifest_loads_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_manifest(dir.path()).expect("load").is_none());
    }
}
