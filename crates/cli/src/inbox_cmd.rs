//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app); manifest-less drops go to a triage PARA node after a
//! quiescence check. Reuses the verified-ingest pipeline end to end.
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
/// Unknown versions and path-traversal names are hard errors — a manifest
/// we cannot fully honor must never be half-honored.
///
/// # Errors
/// Returns an error if the manifest exists but cannot be read or parsed,
/// declares an unsupported `version`, or lists a file name that escapes
/// the contribution folder (absolute path or a `..` component).
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
    for file in &manifest.files {
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
    }
    Ok(Some(manifest))
}

/// Presence/size gate ("still uploading" detection) + the unlisted-file
/// report. Hash checking is deliberately NOT here — it reads every byte,
/// so it runs once, later, only on contributions that pass this gate.
pub(crate) struct FileCheck {
    /// Human-readable per-file reasons the contribution isn't ready.
    pub waiting: Vec<String>,
    /// Files in the folder the manifest doesn't list (reported, left
    /// untouched, never ingested from a manifested contribution).
    pub unlisted: Vec<String>,
}

/// # Errors
/// Returns an error if the contribution folder (or any subdirectory) can't
/// be read while walking for unlisted files.
pub(crate) fn check_files(dir: &Path, manifest: &ContributionManifest) -> Result<FileCheck> {
    let mut waiting = Vec::new();
    let mut listed = std::collections::BTreeSet::new();
    for file in &manifest.files {
        listed.insert(file.name.clone());
        let path = dir.join(&file.name);
        match std::fs::metadata(&path) {
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

fn collect_unlisted(
    root: &Path,
    dir: &Path,
    listed: &std::collections::BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
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
