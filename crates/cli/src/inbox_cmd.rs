//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app); manifest-less drops go to a triage PARA node after a
//! quiescence check. Reuses the verified-ingest pipeline end to end.
//!
//! Name comparisons (manifest entries vs. the folder listing) are raw
//! UTF-8 byte equality with no Unicode NFC/NFD normalization — an iOS
//! export that writes NFD-normalized file names can misreport a real,
//! APFS-resolvable file as "unlisted". A related case, also deferred: on
//! the default case-insensitive APFS, two manifest entries differing only
//! in case collide on the same real file. Both are watchlisted pending
//! real-world signal on which normalization strategy (if any) is worth it.
use crate::app::FsApp;
use crate::commands::{self, ExecuteIngest};
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op};
use majestical_ingest::{hashing, plan};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub(crate) const MANIFEST_NAME: &str = "contribution.json";
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
pub(crate) struct ContributionManifest {
    pub version: u32,
    pub contributor: String,
    #[serde(default)]
    pub para_target: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Free-form capture context; carried for future surfacing.
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "surfaced in a future inbox report; not yet read anywhere"
    )]
    pub note: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ManifestFile {
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
pub(crate) struct FileCheck {
    /// Human-readable per-file reasons the contribution isn't ready yet
    /// (missing or short files). Transient — the next pass may find them
    /// resolved.
    pub waiting: Vec<String>,
    /// Files in the folder the manifest doesn't list (reported, left
    /// untouched, never ingested from a manifested contribution).
    pub unlisted: Vec<String>,
    /// Listed entries that resolve to something other than a regular file
    /// (a symlink, a directory, ...) — named, not silently followed or
    /// ingested. Unlike `waiting`, this is not transient: Task 10 must
    /// treat any non-empty `refused` as the whole contribution failing,
    /// the same policy as a hash mismatch, and never partially ingest.
    /// This is a value, not an `Err`, on purpose — it's a fact about the
    /// contribution's contents, not an operational failure of the check
    /// itself; `Err` here is reserved for real I/O failures (an unreadable
    /// directory) that the operator, not the contributor, must fix.
    pub refused: Vec<String>,
}

/// # Errors
/// Returns an error only for real I/O failures — the contribution folder
/// (or a subdirectory) can't be read while walking for unlisted files. A
/// listed entry that isn't a regular file is reported in
/// [`FileCheck::refused`], not returned as an `Err`.
pub(crate) fn check_files(dir: &Path, manifest: &ContributionManifest) -> Result<FileCheck> {
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
            Err(_) => waiting.push(format!("{}: not yet present", file.name)),
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

/// Bundles the flags within the house 5-positional-parameter limit.
pub(crate) struct InboxArgs {
    pub inbox: PathBuf,
    pub dest: Vec<PathBuf>,
    /// PARA node for manifest-less drops. Accepted and stored starting this
    /// task so the clap surface is stable; not yet read anywhere — Task 11
    /// wires manifest-less triage against it.
    #[expect(
        dead_code,
        reason = "manifest-less triage lands in Task 11; the flag is wired now so the CLI \
                  surface doesn't change again when it does"
    )]
    pub triage_target: Option<String>,
    pub keep: bool,
    pub json: bool,
}

/// Catalog root + inbox flags, threaded through every contribution —
/// bundled so `process_contribution`/`ingest_contribution` stay within the
/// house 5-positional-parameter limit.
struct InboxCtx<'a> {
    catalog: &'a Path,
    args: &'a InboxArgs,
}

/// Per-machine record of contributions that failed hash validation or were
/// refused, so a later pass skips them with a notice instead of re-checking
/// forever. Keyed by folder name; cleared automatically when the manifest
/// changes (mtime+size fingerprint) — a re-upload re-validates.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FailureMarkers {
    #[serde(default)]
    failures: std::collections::BTreeMap<String, FailureMarker>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FailureMarker {
    reason: String,
    manifest_mtime_ms: u64,
    manifest_size: u64,
}

fn markers_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("inbox-failures.json"))
}

fn load_markers(catalog: &Path) -> Result<FailureMarkers> {
    let path = markers_path(catalog)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FailureMarkers::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn store_markers(catalog: &Path, markers: &FailureMarkers) -> Result<()> {
    let path = markers_path(catalog)?;
    std::fs::write(&path, serde_json::to_string_pretty(markers)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// `(mtime_ms, size)` of a contribution's manifest — cheap enough to check
/// on every pass, and changes whenever the manifest is re-exported, which is
/// exactly when a recorded failure should be re-validated instead of
/// trusted forever. A missing manifest fingerprints as `(0, 0)`, which can
/// never match a real marker (every recorded marker was written from a
/// manifest that existed at record time).
fn manifest_fingerprint(dir: &Path) -> (u64, u64) {
    std::fs::metadata(dir.join(MANIFEST_NAME))
        .map_or((0, 0), |m| (commands::mtime_ms_of(&m), m.len()))
}

fn record_failure(markers: &mut FailureMarkers, name: &str, reason: &str, fingerprint: (u64, u64)) {
    markers.failures.insert(
        name.to_string(),
        FailureMarker {
            reason: reason.to_string(),
            manifest_mtime_ms: fingerprint.0,
            manifest_size: fingerprint.1,
        },
    );
}

/// One contribution's outcome for the pass report.
enum ContribOutcome {
    Ingested { files: usize },
    Waiting { reasons: Vec<String> },
    RecordedFailure { reason: String },
    Failed { reason: String },
}

/// One converging pass over `args.inbox`: every manifested subfolder is
/// validated, verified-ingested, tagged with provenance, and (unless
/// `--keep`) moved to `.processed/`. Manifest-less entries (a subfolder
/// with no `contribution.json`, or a bare file) are collected and silently
/// ignored this task — `Task 11` adds the quiescence-gated triage flow that
/// consumes `args.triage_target`.
///
/// # Errors
/// Returns an error if `inbox` isn't a directory, if reading it or the
/// failure-marker store fails, or if any contribution froze the pass with a
/// pass-fatal I/O error. After a full pass, also returns an error if any
/// contribution freshly failed this run (a previously recorded failure is
/// only a notice, not an error — see `print_report`).
pub(crate) fn cmd_inbox_process(app: &mut FsApp, catalog: &Path, args: &InboxArgs) -> Result<()> {
    anyhow::ensure!(
        args.inbox.is_dir(),
        "inbox must be a directory: {}",
        args.inbox.display()
    );
    let mut markers = load_markers(catalog)?;
    let mut report: Vec<(String, ContribOutcome)> = Vec::new();
    let ctx = InboxCtx { catalog, args };
    for path in list_contribution_dirs(&args.inbox)? {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load_manifest(&path) {
            Ok(Some(manifest)) => {
                let outcome = process_contribution(app, &ctx, &path, &manifest, &mut markers)?;
                report.push((name, outcome));
            }
            Ok(None) => {} // manifest-less: Task 11
            Err(e) => report.push((
                name,
                ContribOutcome::Failed {
                    reason: format!("{e:#}"),
                },
            )),
        }
    }
    store_markers(catalog, &markers)?;
    print_report(&report, args.json)
}

/// Sorted, non-dot, directory-only entries directly under `inbox`. Bare
/// files are Task 11's manifest-less flow; `.processed/`, `.DS_Store`, and
/// any other dot-entry (including a sync tool's droppings) are skipped so a
/// completed pass is never re-walked.
fn list_contribution_dirs(inbox: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(inbox).with_context(|| format!("reading {}", inbox.display()))? {
        let entry = entry.with_context(|| format!("reading {}", inbox.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !path.is_dir() {
            continue;
        }
        entries.push(path);
    }
    entries.sort();
    Ok(entries)
}

/// Validates, hash-gates, and ingests one manifested contribution. A marker
/// recorded on an earlier pass short-circuits everything below the
/// fingerprint check — a recorded failure is never re-hashed.
fn process_contribution(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
    markers: &mut FailureMarkers,
) -> Result<ContribOutcome> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fingerprint = manifest_fingerprint(dir);
    if let Some(marker) = markers.failures.get(&name) {
        if (marker.manifest_mtime_ms, marker.manifest_size) == fingerprint {
            return Ok(ContribOutcome::RecordedFailure {
                reason: marker.reason.clone(),
            });
        }
        markers.failures.remove(&name); // manifest changed — re-validate
    }
    let check = check_files(dir, manifest)?;
    if !check.waiting.is_empty() {
        return Ok(ContribOutcome::Waiting {
            reasons: check.waiting,
        });
    }
    for unlisted in &check.unlisted {
        eprintln!("note: {name}/{unlisted} is not in the manifest — left untouched, not ingested");
    }
    if !check.refused.is_empty() {
        let reason = check.refused.join("; ");
        record_failure(markers, &name, &reason, fingerprint);
        return Ok(ContribOutcome::Failed { reason });
    }
    if let Some(reason) = hash_mismatch_reason(dir, manifest)? {
        record_failure(markers, &name, &reason, fingerprint);
        return Ok(ContribOutcome::Failed { reason });
    }
    let para = manifest.para_target.as_deref().with_context(|| {
        format!(
            "{name}: manifest has no para_target and no default exists — add one or use the \
             manifest-less triage path"
        )
    })?;
    ingest_contribution(app, ctx, dir, manifest, para)?;
    if !ctx.args.keep {
        move_to_processed(&ctx.args.inbox, dir)?;
    }
    Ok(ContribOutcome::Ingested {
        files: manifest.files.len(),
    })
}

/// End-to-end hash gate: the contributor's client-side xxh64 against a
/// fresh read of what actually arrived. Runs once, only on contributions
/// that already passed [`check_files`]'s presence/size gate — every listed
/// file is read in full here, so it must never run twice for nothing.
/// Returns the first mismatch's reason; a mismatch fails the WHOLE
/// contribution before a single byte is copied.
fn hash_mismatch_reason(dir: &Path, manifest: &ContributionManifest) -> Result<Option<String>> {
    for file in &manifest.files {
        let path = dir.join(&file.name);
        let computed =
            hashing::xxh64_file(&path).with_context(|| format!("hashing {}", path.display()))?;
        if computed != file.xxh64 {
            return Ok(Some(format!(
                "{}: manifest says xxh64 {} but the file hashes to {computed} — corrupt in \
                 transit or a stale manifest; re-upload it or remove the folder",
                file.name, file.xxh64
            )));
        }
    }
    Ok(None)
}

/// Plans, verified-ingests, and provenance-tags one contribution's files.
/// `load_manifest` has already validated `manifest.contributor`/`source`/
/// `para_target` (no absolute path, no `..`) — this function must not
/// re-validate them, only consume them.
fn ingest_contribution(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
    para: &str,
) -> Result<()> {
    let projection = app.projection()?;
    let (node_id, kind, name) = commands::resolve_ingest_node(&projection, para)?;
    let known = commands::known_assets_from_projection(&projection);
    let mut ingest_plan = plan::plan_source(dir, &known, plan::DedupeMode::Skip)
        .with_context(|| format!("planning contribution {}", dir.display()))?;
    // contribution.json describes the contribution; it must never itself
    // become a placed, verified, or tagged catalog asset.
    ingest_plan.files.retain(|f| f.rel != MANIFEST_NAME);
    let (vol_id, vol_label) = commands::resolve_volume(dir, None);
    // Same default layout `maj ingest` uses — the contributor lands as a
    // tag below, not a subdirectory, so a manifested drop and a manual
    // ingest of the same PARA node share one layout.
    let subdir = commands::render_ingest_subdir(kind, &name, "{date}/{source-label}", &vol_label)?;
    let outcome = commands::run_ingest(
        app,
        ctx.catalog,
        &ExecuteIngest {
            plan: &ingest_plan,
            dest: &ctx.args.dest,
            subdir: &subdir,
            node_id: &node_id,
            source_volume: (&vol_id, &vol_label),
            jobs: None,
            resume: None,
            json: ctx.args.json,
        },
    )?;
    // Provenance: contributor + optional source, as plain TagAdds on every
    // distinct placed asset — no new op variants this phase.
    let mut ops = Vec::new();
    let mut seen = BTreeSet::new();
    for placed in &outcome.placed {
        let asset = AssetId(format!("xxh3:{}", placed.xxh3));
        if !seen.insert(asset.clone()) {
            continue;
        }
        ops.push(Op::TagAdd {
            asset: asset.clone(),
            tag: format!("contributor/{}", manifest.contributor),
        });
        if let Some(source) = &manifest.source {
            ops.push(Op::TagAdd {
                asset,
                tag: format!("source/{source}"),
            });
        }
    }
    app.emit(ops)?;
    Ok(())
}

/// Atomic rename into `.processed/`, numeric suffix on collision.
fn move_to_processed(inbox: &Path, dir: &Path) -> Result<()> {
    let processed = inbox.join(".processed");
    std::fs::create_dir_all(&processed)
        .with_context(|| format!("creating {}", processed.display()))?;
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut target = processed.join(&name);
    let mut suffix = 2u32;
    while target.exists() {
        target = processed.join(format!("{name}-{suffix}"));
        suffix += 1;
    }
    std::fs::rename(dir, &target)
        .with_context(|| format!("moving {} to {}", dir.display(), target.display()))
}

/// Prints the pass report, then applies the exit policy: a FRESH failure
/// (this run) fails the pass; a previously RECORDED failure is a notice
/// only. Every fresh failure's detail also goes to stderr unconditionally
/// (not just in text mode) — with `--json`, stdout carries only the JSON
/// blob, so this is the only place that detail reaches the operator.
fn print_report(report: &[(String, ContribOutcome)], json: bool) -> Result<()> {
    if json {
        print_report_json(report)?;
    } else if report.is_empty() {
        println!("nothing to process");
    } else {
        print_report_text(report);
    }
    let mut any_failed = false;
    for (name, outcome) in report {
        if let ContribOutcome::Failed { reason } = outcome {
            eprintln!("{name}: {reason}");
            any_failed = true;
        }
    }
    anyhow::ensure!(
        !any_failed,
        "one or more contributions failed — see the report above"
    );
    Ok(())
}

fn print_report_json(report: &[(String, ContribOutcome)]) -> Result<()> {
    let rows: Vec<serde_json::Value> = report
        .iter()
        .map(|(name, outcome)| match outcome {
            ContribOutcome::Ingested { files } => {
                serde_json::json!({"contribution": name, "status": "ingested", "files": files})
            }
            ContribOutcome::Waiting { reasons } => {
                serde_json::json!({"contribution": name, "status": "waiting", "reasons": reasons})
            }
            ContribOutcome::RecordedFailure { reason } => {
                serde_json::json!({
                    "contribution": name, "status": "recorded_failure", "reason": reason
                })
            }
            ContribOutcome::Failed { reason } => {
                serde_json::json!({"contribution": name, "status": "failed", "reason": reason})
            }
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}

fn print_report_text(report: &[(String, ContribOutcome)]) {
    for (name, outcome) in report {
        match outcome {
            ContribOutcome::Ingested { files } => println!("{name}: ingested {files} file(s)"),
            ContribOutcome::Waiting { reasons } => {
                println!("{name}: waiting — {}", reasons.join("; "));
            }
            ContribOutcome::RecordedFailure { reason } => {
                println!("{name}: skipped (recorded failure) — {reason}");
            }
            ContribOutcome::Failed { reason } => println!("{name}: FAILED — {reason}"),
        }
    }
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
}
