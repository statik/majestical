//! Ingest planning: walk the source, decide per file, hash only when the
//! size prefilter says a duplicate is possible.
use crate::IngestError;
use majestical_core::event::AssetId;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// What the catalog already knows: content hashes grouped by file size, so
/// the planner can skip hashing sources whose size matches nothing.
#[derive(Debug, Default)]
pub struct KnownAssets {
    by_size: BTreeMap<u64, BTreeSet<String>>,
}

impl KnownAssets {
    /// `pairs` are bare xxh3-128 hex digests (no `xxh3:` prefix) paired with
    /// the byte size the catalog recorded for that content.
    #[must_use]
    pub fn from_pairs(pairs: Vec<(String, u64)>) -> Self {
        let mut by_size: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for (hash, size) in pairs {
            by_size.entry(size).or_default().insert(hash);
        }
        Self { by_size }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupeMode {
    Skip,
    CopyAnyway,
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// New content: copy and verify.
    Copy,
    /// Content hash already in the catalog; `action` is the run's mode.
    Duplicate { asset: AssetId, action: DedupeMode },
    /// Not ingestable; the run continues without it.
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedFile {
    pub source: PathBuf,
    /// Path relative to the source root, `/`-separated.
    pub rel: String,
    pub size: u64,
    /// xxh3-128 hex computed during planning, only when the size prefilter
    /// matched (dedupe confirmation); the engine reuses it when present.
    pub prehash: Option<String>,
    pub decision: Decision,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestPlan {
    pub files: Vec<PlannedFile>,
}

/// Streams an xxh3-128 hash over `path`. Media files are large and read
/// sequentially, so a bigger buffer than `cmd_scan`'s 64 KiB pays off here.
fn hash_file(path: &Path) -> Result<String, IngestError> {
    let file = std::fs::File::open(path).map_err(|source| IngestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut reader = std::io::BufReader::new(file);
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader.read(&mut buf).map_err(|source| IngestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.digest128()))
}

#[cfg(test)]
pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:032x}", xxhash_rust::xxh3::xxh3_128(bytes))
}

/// The un-ingestable-name detail `rel_utf8` reports: a human reason plus a
/// best-effort lossy relative path, since the true bytes may not be
/// representable as a `String` at all.
struct NonUtf8Rel {
    reason: String,
    lossy_rel: String,
}

/// Reduces `path` to a `/`-separated string relative to `source`. Pure and
/// filesystem-free so the non-UTF-8 rejection path is unit-testable without
/// depending on whether the host filesystem permits invalid-UTF-8 names.
///
/// # Errors
/// Returns `Err(NonUtf8Rel)` when `path`'s relative component is not valid
/// UTF-8; callers get both the reason and a lossy rel in one pass, rather
/// than recomputing the lossy conversion themselves.
fn rel_utf8(source: &Path, path: &Path) -> Result<String, NonUtf8Rel> {
    let rel_path = path.strip_prefix(source).unwrap_or(path);
    match rel_path.to_str() {
        Some(s) => Ok(s.replace('\\', "/")),
        None => Err(NonUtf8Rel {
            reason: IngestError::NonUtf8Path {
                path: path.to_path_buf(),
            }
            .to_string(),
            lossy_rel: rel_path.to_string_lossy().replace('\\', "/"),
        }),
    }
}

/// Builds the `Rejected` record for a non-UTF-8-named source file. Pure and
/// filesystem-free — this is the single place that decides how such a file
/// is represented, so tests exercise the same construction `plan_source`
/// uses instead of a hand-rolled copy of it.
fn rejected_non_utf8(path: &Path, size: u64, err: NonUtf8Rel) -> PlannedFile {
    PlannedFile {
        // Lossy, not `path.to_path_buf()`: serde's `Path` serialization
        // errors on invalid UTF-8, and this file is Rejected — it is never
        // reopened, so losing the exact raw bytes here is harmless.
        source: PathBuf::from(path.to_string_lossy().into_owned()),
        rel: err.lossy_rel,
        size,
        prehash: None,
        decision: Decision::Rejected { reason: err.reason },
    }
}

/// Walks `source` and decides, per file, whether it is new content, a
/// confirmed duplicate of something already in the catalog, or rejected.
///
/// # Errors
/// Returns `IngestError::Walk` if the directory walk itself fails (e.g. a
/// permission error reading an entry's metadata); per-file problems such as
/// non-UTF-8 names or zero-byte files are recorded as `Decision::Rejected`
/// rather than aborting the whole plan.
pub fn plan_source(
    source: &Path,
    known: &KnownAssets,
    mode: DedupeMode,
) -> Result<IngestPlan, IngestError> {
    plan_source_filtered(source, known, mode, &|_| true)
}

/// [`plan_source`], but skips any entry — and never descends into any
/// directory — for which `filter` returns `false`. `filter` is consulted
/// before the walk enters each entry under `source` (the walk root itself
/// is always visited regardless of what `filter` says about it — `filter`
/// only prunes things found underneath it); when it rejects a directory,
/// that directory's contents are never read, stat'd, or hashed at all, not
/// merely discarded from the result afterward. A caller that only wants a
/// known set of top-level files (e.g. `maj inbox process`'s loose-file
/// triage, which must not re-walk an inbox's ever-growing `.processed/`
/// archive on every pass) gets that by excluding every other subtree
/// structurally, rather than planning everything and filtering the plan.
///
/// # Errors
/// Same as [`plan_source`].
pub fn plan_source_filtered(
    source: &Path,
    known: &KnownAssets,
    mode: DedupeMode,
    filter: &dyn Fn(&Path) -> bool,
) -> Result<IngestPlan, IngestError> {
    let mut files = Vec::new();
    let walk = walkdir::WalkDir::new(source)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || filter(entry.path()));
    for entry in walk {
        let entry = entry.map_err(|source_err| IngestError::Walk {
            path: source_err
                .path()
                .map_or_else(|| source.to_path_buf(), Path::to_path_buf),
            source: source_err,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // Read the size before the UTF-8 check: a non-UTF-8-named file is
        // still rejected, but it must be rejected with its real size, not
        // a fabricated 0 indistinguishable from a genuine empty file.
        let size = entry
            .metadata()
            .map_err(|source_err| IngestError::Walk {
                path: path.to_path_buf(),
                source: source_err,
            })?
            .len();

        let rel = match rel_utf8(source, path) {
            Ok(rel) => rel,
            Err(err) => {
                files.push(rejected_non_utf8(path, size, err));
                continue;
            }
        };

        if size == 0 {
            files.push(PlannedFile {
                source: path.to_path_buf(),
                rel,
                size,
                prehash: None,
                decision: Decision::Rejected {
                    reason: "0-byte file — nothing to verify; ingest refuses it".to_string(),
                },
            });
            continue;
        }

        let (prehash, decision) = match known.by_size.get(&size) {
            None => (None, Decision::Copy),
            Some(candidates) => {
                let hash = hash_file(path)?;
                if candidates.contains(&hash) {
                    (
                        Some(hash.clone()),
                        Decision::Duplicate {
                            asset: AssetId(format!("xxh3:{hash}")),
                            action: mode,
                        },
                    )
                } else {
                    (Some(hash), Decision::Copy)
                }
            }
        };

        files.push(PlannedFile {
            source: path.to_path_buf(),
            rel,
            size,
            prehash,
            decision,
        });
    }
    Ok(IngestPlan { files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &std::path::Path, rel: &str, bytes: &[u8]) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, bytes).expect("write");
    }

    #[test]
    fn plans_new_files_and_confirms_duplicates_by_content_hash() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "clips/a.mov", b"AAAA");
        write(src.path(), "clips/b.mov", b"BBBBBB");
        // Known catalog: an asset with a's exact bytes (size 4) and an
        // unrelated same-size-as-b asset whose hash won't match b.
        let known =
            KnownAssets::from_pairs(vec![(hash_bytes(b"AAAA"), 4), (hash_bytes(b"XXXXXX"), 6)]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        let by_rel: std::collections::BTreeMap<_, _> =
            plan.files.iter().map(|f| (f.rel.clone(), f)).collect();
        // a: size matched AND pre-hash confirmed -> duplicate, skipped.
        match &by_rel["clips/a.mov"].decision {
            Decision::Duplicate { asset, action } => {
                assert_eq!(asset.0, format!("xxh3:{}", hash_bytes(b"AAAA")));
                assert_eq!(*action, DedupeMode::Skip);
            }
            other => panic!("expected duplicate, got {other:?}"),
        }
        // b: size matched but hash differs -> copies (prehash retained).
        assert!(matches!(by_rel["clips/b.mov"].decision, Decision::Copy));
        assert!(by_rel["clips/b.mov"].prehash.is_some());
    }

    #[test]
    fn size_prefilter_avoids_hashing_unmatched_sizes() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "c.mov", b"CCCCCCCC");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert!(matches!(plan.files[0].decision, Decision::Copy));
        assert!(
            plan.files[0].prehash.is_none(),
            "no known asset of size 8 — planner must not have hashed the source"
        );
    }

    #[test]
    fn zero_byte_file_is_flagged() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "empty.bin", b"");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert_eq!(plan.files.len(), 1);
        assert!(matches!(plan.files[0].decision, Decision::Rejected { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn rel_utf8_rejects_a_non_utf8_name_without_touching_the_filesystem() {
        use std::os::unix::ffi::OsStrExt;
        let source = Path::new("/src");
        let path = source.join(std::ffi::OsStr::from_bytes(b"bad\xFFname"));
        let err = rel_utf8(source, &path).expect_err("non-UTF-8 name must be rejected");
        assert!(err.reason.contains("non-UTF-8"), "got: {}", err.reason);
        assert!(
            err.lossy_rel.contains("bad"),
            "lossy_rel should still carry the recoverable prefix: {}",
            err.lossy_rel
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejected_non_utf8_file_serializes_with_a_lossy_source_path() {
        // Exercises the production `rejected_non_utf8` helper directly —
        // the same construction plan_source's rejection arm calls — rather
        // than hand-rolling the lossy conversion in the test. No filesystem
        // writes, so this runs even on filesystems (e.g. APFS) that refuse
        // to create files with raw-byte names.
        use std::os::unix::ffi::OsStrExt;
        let source = Path::new("/src");
        let path = source.join(std::ffi::OsStr::from_bytes(b"bad\xFFname"));
        let err = rel_utf8(source, &path).expect_err("non-UTF-8 name must be rejected");
        let planned = rejected_non_utf8(&path, 3, err);
        assert_eq!(
            planned.size, 3,
            "real size must be preserved, not fabricated as 0"
        );
        let json = serde_json::to_string(&planned).expect("serialize a lossy-path PlannedFile");
        let back: PlannedFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(planned, back);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_name_is_rejected_per_file_not_fatally() {
        use std::os::unix::ffi::OsStrExt;
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "ok.mov", b"OK");
        let bad = src.path().join(std::ffi::OsStr::from_bytes(b"bad\xFFname"));
        if fs::write(&bad, b"BAD").is_err() {
            // APFS refuses invalid-UTF-8 names; nothing to test on this fs.
            return;
        }
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert_eq!(plan.files.len(), 2);
        let rejected: Vec<_> = plan
            .files
            .iter()
            .filter(|f| matches!(f.decision, Decision::Rejected { .. }))
            .collect();
        assert_eq!(rejected.len(), 1, "only the raw-byte name is rejected");
        assert_eq!(
            rejected[0].size, 3,
            "rejection must record the real size, not a fabricated 0"
        );
        // Dormant on APFS (which refuses the write above), but free
        // end-to-end coverage of the lossy-path fix if a Linux runner
        // (ext4/tmpfs, which allow raw-byte names) ever runs this suite.
        serde_json::to_string(&plan).expect("a plan with a non-UTF-8 rejection must serialize");
    }

    #[test]
    fn plan_source_delegates_to_plan_source_filtered_with_an_always_true_filter() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "a.txt", b"a");
        write(src.path(), "sub/b.txt", b"b");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        let mut rels: Vec<&str> = plan.files.iter().map(|f| f.rel.as_str()).collect();
        rels.sort_unstable();
        assert_eq!(rels, vec!["a.txt", "sub/b.txt"]);
    }

    #[test]
    fn a_filtered_out_directorys_contents_are_absent_from_the_plan() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "keep.txt", b"keep");
        write(src.path(), "skip/nested.txt", b"nested");
        let known = KnownAssets::from_pairs(vec![]);
        let filter = |path: &Path| path.file_name().and_then(|n| n.to_str()) != Some("skip");
        let plan = plan_source_filtered(src.path(), &known, DedupeMode::Skip, &filter)
            .expect("filtered plan");
        let rels: Vec<&str> = plan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec!["keep.txt"],
            "the excluded directory's contents must never appear in the plan"
        );
    }

    /// Stronger proof that the filter is consulted before descending, not
    /// after: an excluded directory that is unreadable must never surface a
    /// `Walk` error, because the walk must never call `read_dir` on it in
    /// the first place. If a filtered-out directory were merely dropped
    /// from the result afterward (rather than never entered), this would
    /// fail with `IngestError::Walk`.
    #[test]
    #[cfg(unix)]
    fn a_filtered_out_directory_is_never_entered_even_when_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "keep.txt", b"keep");
        let locked = src.path().join("locked");
        fs::create_dir_all(&locked).expect("mkdir");
        fs::write(locked.join("secret.txt"), b"secret").expect("write");
        let mut perms = fs::metadata(&locked).expect("meta").permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&locked, perms).expect("chmod 000");

        // Some environments (root, certain containers/filesystems) don't
        // enforce this restriction — skip rather than false-fail there.
        // Checked BEFORE restoring permissions below: this is the actual
        // condition under test.
        if fs::read_dir(&locked).is_ok() {
            let mut perms = fs::metadata(&locked).expect("meta").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&locked, perms).expect("chmod restore");
            // ingest's Cargo.toml inherits the workspace's print_stderr =
            // "deny" (unlike cli's, which allows it crate-wide since CLI
            // diagnostics are the product) — `#[expect]` documents the
            // exception locally instead of weakening the lint crate-wide.
            #[expect(
                clippy::print_stderr,
                reason = "environment-detection notice for a permission test that doesn't apply \
                          when running as root or on a filesystem that ignores mode bits"
            )]
            {
                eprintln!(
                    "skipping: this environment does not enforce a mode-000 directory (likely root)"
                );
            }
            return;
        }

        let known = KnownAssets::from_pairs(vec![]);
        let filter = |path: &Path| path.file_name().and_then(|n| n.to_str()) != Some("locked");
        let result = plan_source_filtered(src.path(), &known, DedupeMode::Skip, &filter);

        // Restore permissions unconditionally so the tempdir can be cleaned
        // up even if the assertion below panics.
        let mut perms = fs::metadata(&locked).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&locked, perms).expect("chmod restore");

        let plan = result.expect(
            "a filtered-out directory must never be entered, so an unreadable one must never \
             surface a Walk error",
        );
        let rels: Vec<&str> = plan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["keep.txt"]);
    }
}
