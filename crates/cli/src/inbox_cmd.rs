//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app). Manifest-less entries (a subfolder with no manifest, or a
//! bare top-level file) triage instead: once quiescent (no contained mtime
//! younger than [`quiescence_ms`]), they ingest to `--triage-target`,
//! tagged `source/inbox`, no contributor identity claimed. Reuses the
//! verified-ingest pipeline end to end. Manifest parsing and the read-only
//! validation gates live in `inbox_manifest.rs`; this module is the
//! orchestration: the pass loop, hash gate, routing, ingest, quiescence,
//! and the failure-marker store.
use crate::commands::{self, IngestReport};
use crate::inbox_manifest::{ContributionManifest, MANIFEST_NAME, check_files, load_manifest};
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op, ParaKind};
use majestical_core::projection::Projection;
use majestical_ingest::{engine, hashing, plan};
use majestical_services::app::FsApp;
use majestical_services::ingest::ExecuteIngest;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::xxh3_128;

/// Bundles the flags within the house 5-positional-parameter limit.
pub(crate) struct InboxArgs {
    pub inbox: PathBuf,
    pub dest: Vec<PathBuf>,
    /// PARA node for manifest-less drops; required once any quiescent
    /// manifest-less item is present — see [`process_manifest_less`].
    pub triage_target: Option<String>,
    pub keep: bool,
    pub json: bool,
}

/// Tag every manifest-less asset gets, in place of a contributor identity
/// that was never claimed.
const TRIAGE_TAG: &str = "source/inbox";

/// The report row name every triaged loose file shares — used both as the
/// `IngestSite::row_name` [`triage_loose_files_ingest`] passes down (so a
/// bad loose file's stderr line is attributed consistently) and as the row
/// key [`process_manifest_less`] pushes onto the report, so the two can't
/// drift apart.
const LOOSE_FILES_ROW: &str = "(loose files)";

/// Catalog root + inbox flags, threaded through every contribution —
/// bundled so `process_contribution`/`ingest_contribution` stay within the
/// house 5-positional-parameter limit. `inbox_key` is computed once here
/// (not re-derived per contribution) so every marker lookup and record in
/// this pass agrees on the same identity.
struct InboxCtx<'a> {
    catalog: &'a Path,
    args: &'a InboxArgs,
    inbox_key: String,
}

/// xxh3-128 of the canonicalized inbox path, hex-encoded — the same
/// pattern `state_dir.rs` uses to key a catalog's local state dir. Two
/// different inboxes sharing one catalog (a common shared-drive setup: one
/// inbox per contributor group) must never collide on this key even when a
/// folder inside each happens to share a name — see `FailureMarkers`.
///
/// # Errors
/// Returns an error if `inbox` can't be canonicalized (it was just checked
/// to be a directory by the caller, so this only fails on a genuine,
/// unusual I/O problem).
fn inbox_key(inbox: &Path) -> Result<String> {
    let canonical = inbox
        .canonicalize()
        .with_context(|| format!("canonicalizing inbox {}", inbox.display()))?;
    Ok(format!(
        "{:032x}",
        xxh3_128(canonical.as_os_str().as_encoded_bytes())
    ))
}

/// Per-machine record of contributions that failed validation, so a later
/// pass skips them with a notice instead of re-checking forever. Keyed by
/// inbox identity ([`inbox_key`]) and then by folder name within it — a bare
/// folder-name key would let two different inboxes sharing this catalog
/// evict each other's markers whenever they happen to have a same-named
/// contribution folder, oscillating fresh-fail/exit-nonzero forever instead
/// of each converging independently. Cleared automatically when the
/// fingerprint changes — the manifest OR any listed file's mtime/size — so
/// both a re-export and fixing just the corrupt file re-validate.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FailureMarkers {
    #[serde(default)]
    inboxes: std::collections::BTreeMap<String, std::collections::BTreeMap<String, FailureMarker>>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FailureMarker {
    reason: String,
    fingerprint: String,
}

fn markers_path(catalog: &Path) -> Result<PathBuf> {
    Ok(majestical_services::state_dir::state_dir_for(catalog)?.join("inbox-failures.json"))
}

/// A missing store is empty (nothing has ever failed); an unparsable one
/// is noted on stderr and treated as empty too. The store is a skip-cache
/// only — every fact it holds is re-derivable by re-checking the
/// contribution — so losing it costs one extra hash/check next pass, never
/// correctness, and it must never turn a corrupt cache file into a hard
/// failure of the whole pass.
fn load_markers(catalog: &Path) -> Result<FailureMarkers> {
    let path = markers_path(catalog)?;
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(FailureMarkers::default());
    };
    if let Ok(markers) = serde_json::from_slice(&bytes) {
        return Ok(markers);
    }
    eprintln!(
        "note: ignoring unparsable inbox failure store at {} — treating as empty",
        path.display()
    );
    Ok(FailureMarkers::default())
}

/// Writes via a same-directory temp file + rename so a pass killed
/// mid-write (or a concurrent reader — `maj inbox process` has no lock
/// against a second copy of itself) never observes a truncated or
/// half-written store, matching `sync_cmd.rs`'s `SyncConfig::store`.
fn store_markers(catalog: &Path, markers: &FailureMarkers) -> Result<()> {
    let path = markers_path(catalog)?;
    let text = serde_json::to_string_pretty(markers).context("serializing inbox failure store")?;
    let file_name = path.file_name().map_or_else(
        || "inbox-failures.json".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("finalizing {}", path.display()))
}

/// Combined fingerprint over the manifest file's own `(mtime_ms, size)` and,
/// when `manifest` is available, every listed file's `(mtime_ms, size)` too.
/// Folding in the listed files matters: a fingerprint of the manifest alone
/// would make "re-upload it" (the hash-mismatch remedy) a lie, since fixing
/// only the corrupt file — never touching `contribution.json` — would then
/// never re-validate. `manifest: None` (the manifest itself failed to
/// parse) fingerprints on the manifest file alone; there is no listed-file
/// set to enumerate yet. A missing file's metadata reads as `(0, 0)`, same
/// convention as a missing manifest.
fn contribution_fingerprint(dir: &Path, manifest: Option<&ContributionManifest>) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::new();
    // symlink_metadata, not metadata: parity with check_files, which never
    // follows a listed name through a symlink either.
    let (mtime, size) = std::fs::symlink_metadata(dir.join(MANIFEST_NAME)).map_or((0, 0), |m| {
        (majestical_services::scan::mtime_ms_of(&m), m.len())
    });
    let _ = write!(encoded, "m:{mtime}:{size}");
    if let Some(manifest) = manifest {
        for file in &manifest.files {
            let (mtime, size) = std::fs::symlink_metadata(dir.join(&file.name))
                .map_or((0, 0), |m| {
                    (majestical_services::scan::mtime_ms_of(&m), m.len())
                });
            let _ = write!(encoded, "|{}:{mtime}:{size}", file.name);
        }
    }
    format!("{:032x}", xxh3_128(encoded.as_bytes()))
}

/// A marker recorded from exactly this fingerprint means nothing about this
/// contribution needs re-checking this pass — returns the notice-only
/// outcome to reuse, without re-hashing or re-attempting anything. A marker
/// whose fingerprint no longer matches (the manifest or a listed file
/// changed since it was recorded) is stale and removed here, so the caller
/// falls through to a fresh check instead of trusting it.
fn recorded_failure(
    markers: &mut FailureMarkers,
    inbox_key: &str,
    name: &str,
    fingerprint: &str,
) -> Option<ContribOutcome> {
    let bucket = markers.inboxes.get_mut(inbox_key)?;
    let marker = bucket.get(name)?;
    if marker.fingerprint == fingerprint {
        return Some(ContribOutcome::RecordedFailure {
            reason: marker.reason.clone(),
        });
    }
    bucket.remove(name);
    None
}

fn record_failure(
    markers: &mut FailureMarkers,
    inbox_key: &str,
    name: &str,
    reason: &str,
    fingerprint: &str,
) {
    markers
        .inboxes
        .entry(inbox_key.to_string())
        .or_default()
        .insert(
            name.to_string(),
            FailureMarker {
                reason: reason.to_string(),
                fingerprint: fingerprint.to_string(),
            },
        );
}

/// One contribution's outcome for the pass report.
enum ContribOutcome {
    Ingested {
        placed: usize,
        skipped_duplicates: usize,
    },
    /// The `(loose files)` group only: some files placed, some failed or
    /// were rejected (e.g. 0 bytes). Unlike a contribution (manifested or a
    /// manifest-less folder), a loose-files group shares nothing but having
    /// landed in the inbox root at once — see [`process_triage_loose_files`]
    /// for why one bad file must never wedge the good ones in the same
    /// group forever. A fresh failure here still fails the pass (per-file
    /// reasons already reached stderr by the time this is constructed).
    PartlyIngested {
        placed: usize,
        skipped_duplicates: usize,
        failed: usize,
    },
    Waiting {
        reasons: Vec<String>,
    },
    RecordedFailure {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

/// Default quiescence window for manifest-less drops: nothing with a
/// contained mtime younger than this is touched, so a mid-upload file (or
/// folder still receiving files) is never grabbed. `MAJ_INBOX_QUIESCENCE_MS`
/// overrides it — tests force `0`; an impatient operator can shorten it.
const QUIESCENCE_MS: u64 = 5 * 60 * 1000;

/// Reads `MAJ_INBOX_QUIESCENCE_MS`, falling back to [`QUIESCENCE_MS`] when
/// unset. Called once per pass ([`process_manifest_less`] threads the
/// result through) rather than once per item, so every item in one pass is
/// judged against the same window even if the env var were somehow read at
/// slightly different instants. A value that's set but doesn't parse as a
/// `u64` is a likely operator typo — warned loudly, naming the bad value,
/// rather than silently falling back to a default that would otherwise
/// look like a working override doing nothing.
fn quiescence_ms() -> u64 {
    let Ok(value) = std::env::var("MAJ_INBOX_QUIESCENCE_MS") else {
        return QUIESCENCE_MS;
    };
    value.parse().unwrap_or_else(|_| {
        eprintln!(
            "warning: MAJ_INBOX_QUIESCENCE_MS={value:?} is not a valid number of milliseconds \
             — falling back to the default {QUIESCENCE_MS}ms"
        );
        QUIESCENCE_MS
    })
}

/// Renders a quiescence window for a human-facing message: milliseconds
/// when sub-second (tests force small or zero windows, where "0s" would be
/// misleading), whole seconds otherwise.
fn format_window(window_ms: u64) -> String {
    if window_ms < 1000 {
        format!("{window_ms}ms")
    } else {
        format!("{}s", window_ms / 1000)
    }
}

/// Newest mtime under `path`: a file's own mtime, or the max across
/// everything nested inside a folder. `None` on any unreadable entry —
/// unreadable means "not ready to judge", never "ready" — including a
/// subdirectory that becomes unreadable partway through the walk.
///
/// Iterative (explicit stack), not recursive: this walks a
/// contributor-controlled tree, same as `inbox_manifest::collect_unlisted`,
/// and unbounded recursion on a deep or adversarial directory tree is a
/// stack overflow (SIGSEGV), not a catchable error. Uses `symlink_metadata`
/// throughout (never follows a symlink into its target), so a symlinked
/// directory is judged by its own mtime and not descended into.
fn newest_mtime_ms(path: &Path) -> Option<u64> {
    let top = std::fs::symlink_metadata(path).ok()?;
    if !top.is_dir() {
        return Some(majestical_services::scan::mtime_ms_of(&top));
    }
    let mut newest = majestical_services::scan::mtime_ms_of(&top);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries {
            let entry = entry.ok()?;
            // `DirEntry::metadata` does not follow symlinks, matching
            // `symlink_metadata` above.
            let meta = entry.metadata().ok()?;
            newest = newest.max(majestical_services::scan::mtime_ms_of(&meta));
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Some(newest)
}

/// Whether `path` (a manifest-less folder or a loose file) has had no
/// activity for at least `window_ms`. `false` on any unreadable entry
/// underneath it — see [`newest_mtime_ms`].
fn is_quiescent(path: &Path, window_ms: u64) -> bool {
    let Some(newest) = newest_mtime_ms(path) else {
        return false;
    };
    majestical_services::app::physical_now_ms().saturating_sub(newest) >= window_ms
}

/// A path's final component as a display string — used everywhere a report
/// row needs the bare name of a contribution folder or loose file.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One converging pass over `args.inbox`: every manifested subfolder is
/// validated, verified-ingested, tagged with provenance, and (unless
/// `--keep`) moved to `.processed/`. Manifest-less entries (a subfolder
/// with no `contribution.json`, or a bare top-level file) triage instead —
/// see [`process_manifest_less`].
///
/// # Errors
/// Returns an error if `inbox` isn't a directory or can't be canonicalized,
/// or if loading or storing the failure-marker store fails. Reading the
/// inbox directory itself is the one per-contribution-loop failure that
/// still aborts the pass (an operator-facing I/O problem, not any one
/// contribution's fault) — markers recorded by contributions processed
/// earlier in the same pass are always persisted first, so that failure
/// never discards them. Also returns an error, after the report prints, if
/// any contribution freshly failed this run — a previously recorded
/// failure is only a notice, not an error (see `print_report`).
pub(crate) fn cmd_inbox_process(app: &mut FsApp, catalog: &Path, args: &InboxArgs) -> Result<()> {
    anyhow::ensure!(
        args.inbox.is_dir(),
        "inbox must be a directory: {} — check the path, or create it first",
        args.inbox.display()
    );
    let inbox_key = inbox_key(&args.inbox)?;
    let mut markers = load_markers(catalog)?;
    let ctx = InboxCtx {
        catalog,
        args,
        inbox_key,
    };
    let result = run_pass(app, &ctx, &mut markers);
    store_markers(catalog, &markers)?;
    print_report(&result?, args.json)
}

/// The per-contribution loop, split out of `cmd_inbox_process` purely so
/// that function can store markers before propagating any error this
/// returns — a pass-fatal I/O error must never discard markers other
/// contributions already recorded earlier in the same pass. A manifest that
/// fails to load (malformed JSON, a validation refusal) is itself recorded
/// under a manifest-only fingerprint, same as any other failure, so a
/// hand-broken manifest converges to a notice on the next pass instead of
/// failing forever.
fn run_pass(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    markers: &mut FailureMarkers,
) -> Result<Vec<(String, ContribOutcome)>> {
    let mut report = Vec::new();
    let mut triage_dirs = Vec::new();
    for path in list_contribution_dirs(&ctx.args.inbox)? {
        let name = display_name(&path);
        match load_manifest(&path) {
            Ok(Some(manifest)) => {
                let outcome = process_contribution(app, ctx, &path, &manifest, markers)?;
                report.push((name, outcome));
            }
            Ok(None) => triage_dirs.push(path),
            Err(e) => {
                let fingerprint = contribution_fingerprint(&path, None);
                let outcome = if let Some(outcome) =
                    recorded_failure(markers, &ctx.inbox_key, &name, &fingerprint)
                {
                    outcome
                } else {
                    let reason = format!("{e:#}");
                    record_failure(markers, &ctx.inbox_key, &name, &reason, &fingerprint);
                    ContribOutcome::Failed { reason }
                };
                report.push((name, outcome));
            }
        }
    }
    let loose_files = list_loose_files(&ctx.args.inbox)?;
    process_manifest_less(app, ctx, &triage_dirs, &loose_files, &mut report)?;
    Ok(report)
}

/// Sorted, non-dot, directory-only entries directly under `inbox`.
/// `.processed/`, `.DS_Store`, and any other dot-entry (including a sync
/// tool's droppings) are skipped so a completed pass is never re-walked.
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

/// Sorted, non-dot, regular-file entries directly under `inbox` — the
/// manifest-less "bare file" shape: a contributor drops files straight
/// into the inbox root instead of a `contribution.json` folder.
///
/// Candidacy is decided by `entry.file_type()`, which — like
/// `symlink_metadata` — does not follow symlinks: a symlinked file is never
/// a loose-file candidate at all. This matters because a symlink pointing
/// at a real file used to pass a `Path::is_file()` check (which follows
/// symlinks) here while `plan_source`'s walk (which also never follows
/// symlinks) silently excluded it from the actual ingest plan — the net
/// effect was a symlinked loose file getting swept into `.processed/`
/// without ever having been cataloged. Excluding it from candidacy up
/// front means it simply sits inert in the inbox instead.
fn list_loose_files(inbox: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(inbox).with_context(|| format!("reading {}", inbox.display()))? {
        let entry = entry.with_context(|| format!("reading {}", inbox.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_regular_file = entry
            .file_type()
            .with_context(|| format!("reading file type of {}", path.display()))?
            .is_file();
        if !is_regular_file {
            continue;
        }
        entries.push(path);
    }
    entries.sort();
    Ok(entries)
}

/// Validates and hash-gates one manifested contribution, then delegates the
/// routing + ingest tail to [`validate_and_ingest`]. A marker recorded on
/// an earlier pass short-circuits everything below the fingerprint check —
/// a recorded failure is never re-hashed. Past that point, only a real I/O
/// failure walking the contribution folder (inside [`check_files`]) stays
/// pass-fatal; every other problem here is scoped to this one contribution
/// and reported as a `Failed` row instead of aborting the rest of the pass.
fn process_contribution(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
    markers: &mut FailureMarkers,
) -> Result<ContribOutcome> {
    let name = display_name(dir);
    let fingerprint = contribution_fingerprint(dir, Some(manifest));
    if let Some(outcome) = recorded_failure(markers, &ctx.inbox_key, &name, &fingerprint) {
        return Ok(outcome);
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
        record_failure(markers, &ctx.inbox_key, &name, &reason, &fingerprint);
        return Ok(ContribOutcome::Failed { reason });
    }
    // Route before hashing: resolving the target is a cheap in-memory
    // projection lookup, while the hash gate below reads every listed byte.
    // A contribution parked on a typo'd or archived target must fail here,
    // not after re-reading a multi-hundred-gigabyte drop on every pass. The
    // resolved node is threaded straight into ingestion below rather than
    // re-resolved there.
    let node = match route_contribution(app, manifest) {
        Ok(node) => node,
        Err(e) => {
            return Ok(ContribOutcome::Failed {
                reason: format!("{e:#}"),
            });
        }
    };
    match hash_mismatch_reason(dir, manifest) {
        Ok(Some(reason)) => {
            record_failure(markers, &ctx.inbox_key, &name, &reason, &fingerprint);
            return Ok(ContribOutcome::Failed { reason });
        }
        Ok(None) => {}
        // A genuine I/O failure reading a file that already passed
        // check_files' presence/size gate (permissions, a mid-read fault):
        // not recorded, since nothing about the contribution itself is
        // wrong — the next pass should simply try again.
        Err(e) => {
            return Ok(ContribOutcome::Failed {
                reason: format!("{e:#}"),
            });
        }
    }
    // Everything past this point (planning, the verified copy itself) is
    // likewise contribution-scoped and never recorded: a transient copy
    // failure isn't fixed by touching the manifest, so recording it against
    // the fingerprint would leave it stuck even after the operator fixes
    // the real cause.
    match validate_and_ingest(app, ctx, dir, manifest, node) {
        Ok(outcome) => Ok(outcome),
        Err(e) => Ok(ContribOutcome::Failed {
            reason: format!("{e:#}"),
        }),
    }
}

/// A cheap, no-file-I/O check that resolves this contribution's ingest
/// target, returning it so the caller can thread it straight into
/// ingestion without a second resolution. Called before
/// [`hash_mismatch_reason`] purely to fail fast on a routing problem
/// (missing `para_target`, a typo'd or archived node) before that gate
/// re-reads every listed byte.
fn route_contribution(
    app: &mut FsApp,
    manifest: &ContributionManifest,
) -> Result<(String, ParaKind, String)> {
    let Some(para) = manifest.para_target.as_deref() else {
        anyhow::bail!("manifest has no para_target — add a para_target to contribution.json");
    };
    let projection = app.projection()?;
    resolve_contribution_node(&projection, para)
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

/// The verified-ingest + `.processed/` move tail of [`process_contribution`],
/// split out purely to stay under the house function-length limit. `node`
/// is already resolved (by `route_contribution`, earlier in
/// `process_contribution`) — this function trusts it rather than
/// re-resolving. Every error here is contribution-scoped — the caller
/// converts it into a `Failed` row rather than propagating it.
fn validate_and_ingest(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
    node: (String, ParaKind, String),
) -> Result<ContribOutcome> {
    let outcome = ingest_contribution(app, ctx, dir, manifest, node)?;
    let contrib = require_clean_outcome(&outcome)?;
    if !ctx.args.keep {
        move_into_processed(&ctx.args.inbox, dir)?;
    }
    Ok(contrib)
}

/// Converts a raw engine outcome into the report row, or an error naming
/// how many files failed/were rejected/produced a diagnostic — shared by
/// every ingest call site (manifested, triage folder, triage loose files)
/// so the "what counts as a clean placement" policy can't drift between
/// them.
fn require_clean_outcome(outcome: &engine::Outcome) -> Result<ContribOutcome> {
    anyhow::ensure!(
        outcome.failed.is_empty() && outcome.rejected.is_empty() && outcome.diagnostics.is_empty(),
        "{} failed, {} rejected, {} diagnostic(s) placing files — see stderr",
        outcome.failed.len(),
        outcome.rejected.len(),
        outcome.diagnostics.len()
    );
    Ok(ContribOutcome::Ingested {
        placed: outcome.placed.len(),
        skipped_duplicates: outcome.skipped_duplicates.len(),
    })
}

/// Resolves `para` (validated by `load_manifest` to at most one interior
/// `/`) to an active ingest target. When it's in `<kind>/<name>` form and
/// doesn't resolve to an active node, this names the exact fix instead of
/// [`majestical_services::ingest::resolve_ingest_node`]'s generic "see `maj para list`" (which
/// doesn't tell the operator what to do next) — distinguishing the two ways
/// "not active" happens matters: a target that was never created needs
/// `maj para add`, but a target that exists and is merely archived would
/// get a DUPLICATE node from that same advice, so it gets its own message
/// naming un-archiving instead.
fn resolve_contribution_node(
    projection: &Projection,
    para: &str,
) -> Result<(String, ParaKind, String)> {
    if let Some((kind_str, name)) = para.split_once('/')
        && let Ok(kind) = majestical_services::para::parse_kind(kind_str)
    {
        let active = projection
            .para_nodes()
            .any(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name));
        if !active {
            let archived = projection
                .para_nodes()
                .any(|(_, st)| st.archived() && st.kind() == Some(kind) && st.name() == Some(name));
            anyhow::ensure!(
                !archived,
                "PARA target '{para}' exists but is archived — un-archive it or target another \
                 node; see `maj para list`"
            );
            anyhow::bail!(
                "PARA target '{para}' does not exist yet — create it first: \
                 `maj para add {kind_str} {name}`"
            );
        }
    }
    majestical_services::ingest::resolve_ingest_node(projection, para)
}

/// Plans and verified-ingests one contribution's files, then tags every
/// touched asset with contributor + optional source provenance.
/// `load_manifest` has already validated `manifest.contributor`/`source`/
/// `para_target` (no absolute path, no `..`) — this function must not
/// re-validate them, only consume them. `node` is `route_contribution`'s
/// already-resolved target; nothing emits events between that resolution
/// and this call within the same contribution, so re-resolving here would
/// only cost a second projection scan for no benefit.
fn ingest_contribution(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
    node: (String, ParaKind, String),
) -> Result<engine::Outcome> {
    let projection = app.projection()?;
    let known = majestical_services::ingest::known_assets_from_projection(&projection);
    let mut ingest_plan = plan::plan_source(dir, &known, plan::DedupeMode::Skip)
        .with_context(|| format!("planning contribution {}", dir.display()))?;
    // Only files the manifest actually lists may ever be placed, verified,
    // or provenance-tagged — an unlisted file (a stray drop alongside the
    // real ones, or something added after the manifest was written) must
    // never cross the trust boundary just because it happened to sit in the
    // same folder. This also drops contribution.json itself, which is never
    // a listed entry.
    let listed: BTreeSet<&str> = manifest.files.iter().map(|f| f.name.as_str()).collect();
    ingest_plan
        .files
        .retain(|f| listed.contains(f.rel.as_str()));
    let mut tags = vec![format!("contributor/{}", manifest.contributor)];
    if let Some(source) = &manifest.source {
        tags.push(format!("source/{source}"));
    }
    let row_name = display_name(dir);
    run_shared_ingest(
        app,
        ctx,
        &ingest_plan,
        node,
        IngestSite {
            probe_dir: dir,
            row_name: &row_name,
            tags: &tags,
        },
    )
}

/// Plans and verified-ingests one manifest-less folder's files (the whole
/// folder — there is no manifest to filter against), tagged only
/// [`TRIAGE_TAG`].
fn triage_folder_ingest(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    node: (String, ParaKind, String),
) -> Result<engine::Outcome> {
    let projection = app.projection()?;
    let known = majestical_services::ingest::known_assets_from_projection(&projection);
    let ingest_plan = plan::plan_source(dir, &known, plan::DedupeMode::Skip)
        .with_context(|| format!("planning manifest-less folder {}", dir.display()))?;
    let row_name = display_name(dir);
    run_shared_ingest(
        app,
        ctx,
        &ingest_plan,
        node,
        IngestSite {
            probe_dir: dir,
            row_name: &row_name,
            tags: &[TRIAGE_TAG.to_string()],
        },
    )
}

/// Plans and verified-ingests exactly `files` — quiescent, manifest-less,
/// top-level loose files — tagged only [`TRIAGE_TAG`]. `plan_source` has no
/// single-file entry point, so this plans the inbox root but with a filter
/// (`plan_source_filtered`) accepting only these exact paths: the walk
/// never enters `.processed/`, a manifested contribution's folder, another
/// manifest-less folder, or any dot-entry — it doesn't merely discard their
/// results afterward, it never reads, stats, or hashes anything under them
/// at all. This matters in practice: without it, every pass re-walks and
/// re-hashes the inbox's ever-growing `.processed/` archive, which measured
/// a 5x slowdown at 300 MB and only gets worse as the archive grows. No
/// post-hoc `retain` is needed on top — the filter's exact-path membership
/// check is already the complete, precise criterion.
fn triage_loose_files_ingest(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    files: &[PathBuf],
    node: (String, ParaKind, String),
) -> Result<engine::Outcome> {
    let projection = app.projection()?;
    let known = majestical_services::ingest::known_assets_from_projection(&projection);
    let wanted: BTreeSet<PathBuf> = files.iter().cloned().collect();
    let filter = move |path: &Path| wanted.contains(path);
    let ingest_plan =
        plan::plan_source_filtered(&ctx.args.inbox, &known, plan::DedupeMode::Skip, &filter)
            .with_context(|| format!("planning inbox {}", ctx.args.inbox.display()))?;
    run_shared_ingest(
        app,
        ctx,
        &ingest_plan,
        node,
        IngestSite {
            probe_dir: &ctx.args.inbox,
            row_name: LOOSE_FILES_ROW,
            tags: &[TRIAGE_TAG.to_string()],
        },
    )
}

/// Where an ingest run reads from and how its output is attributed —
/// bundled so [`run_shared_ingest`] stays within the house
/// 5-positional-parameter limit. `probe_dir` is what `resolve_volume`
/// auto-detects the source volume from; `row_name` is the report row this
/// run belongs to (a contribution/folder name, or [`LOOSE_FILES_ROW`]),
/// used to prefix each per-file failure line in [`print_failure_detail`] so
/// two folders that each have a bad `clip.mov` print two attributed lines,
/// not two identical, unattributed ones.
#[derive(Clone, Copy)]
struct IngestSite<'a> {
    probe_dir: &'a Path,
    row_name: &'a str,
    tags: &'a [String],
}

/// The ingest-orchestration body shared by every call site above: run the
/// verified copy at `node`'s target, then tag every touched asset with
/// `site.tags`. Parameterizing on the tag list — rather than three
/// near-copies of "plan, resolve subdir, `run_ingest`, tag" — is what keeps
/// a manifested contribution, a triage folder, and triage loose files from
/// diverging in how they place files, only in what they tag them with.
fn run_shared_ingest(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    ingest_plan: &plan::IngestPlan,
    node: (String, ParaKind, String),
    site: IngestSite<'_>,
) -> Result<engine::Outcome> {
    let (node_id, kind, name) = node;
    let (vol_id, vol_label) = majestical_services::scan::resolve_volume(site.probe_dir, None);
    // Same default layout `maj ingest` uses — the contributor (or, for
    // triage, nothing) lands as a tag, not a subdirectory, so a manifested
    // drop, a triaged drop, and a manual ingest of the same PARA node share
    // one layout.
    let subdir = majestical_services::ingest::render_ingest_subdir(
        kind,
        &name,
        "{date}/{source-label}",
        &vol_label,
    )?;
    let run = majestical_services::ingest::run_ingest(
        app,
        ctx.catalog,
        &ExecuteIngest {
            plan: ingest_plan,
            dest: &ctx.args.dest,
            subdir: &subdir,
            node_id: &node_id,
            source_volume: (&vol_id, &vol_label),
            jobs: None,
            resume: None,
        },
        // No-op: `maj inbox process` runs this once per contribution, so
        // the `--resume` advice line would print once per contribution
        // too — and `--resume` isn't a flag this command even accepts.
        &mut |_line: &str| {},
    )?;
    // Silent in both text and JSON mode: the pass-level report this module
    // prints already carries the outcome, and stderr carries any failure
    // detail (via `print_failure_detail` below) — a per-contribution engine
    // summary interleaved ahead of it is preamble noise in text mode, and
    // would break `--json`'s one-document guarantee. Diagnostics still
    // print regardless of the `Silent` variant — see
    // `commands::print_ingest_outcome`'s doc.
    commands::print_ingest_outcome(
        &run.run_id,
        &run.outcome,
        &run.generations,
        IngestReport::Silent,
    );
    print_failure_detail(site.row_name, &run.outcome);
    tag_assets(app, ingest_plan, &run.outcome, site.tags)?;
    Ok(run.outcome)
}

/// Per-file failure/rejection detail, printed to stderr — prefixed with
/// `row_name` so two rows that each have a bad `clip.mov` don't print two
/// identical, unattributed lines — from the one place every ingest call
/// site (manifested, triage folder, triage loose files) shares.
/// `run_shared_ingest` always runs with `IngestReport::Silent`, so without
/// this nothing ever prints WHICH file failed and why —
/// [`require_clean_outcome`]'s error only reports counts, which would
/// otherwise be a circular "see stderr" pointing at a line that just
/// repeats the same counts back. Diagnostics are deliberately NOT printed
/// here: `commands::print_ingest_outcome` (called just above, right after
/// `majestical_services::ingest::run_ingest` returns) already prints every
/// diagnostic regardless of `IngestReport` variant — printing them again
/// here would be two functions claiming the same responsibility.
fn print_failure_detail(row_name: &str, outcome: &engine::Outcome) {
    for bad in outcome.failed.iter().chain(&outcome.rejected) {
        eprintln!("{row_name}: {}: {}", bad.rel, bad.reason);
    }
}

/// Every distinct asset this run touched — both newly placed files AND
/// files the planner found already known (`Decision::Duplicate`) — gets
/// every tag in `tags`. Skipping duplicates would silently drop provenance
/// whenever a second contributor (or a second triage pass) re-drops content
/// someone else already ingested: the asset is real and this run genuinely
/// vouches for it too, even though nothing new was copied.
fn tag_assets(
    app: &mut FsApp,
    ingest_plan: &plan::IngestPlan,
    outcome: &engine::Outcome,
    tags: &[String],
) -> Result<()> {
    let mut ops = Vec::new();
    let mut seen = BTreeSet::new();
    for placed in &outcome.placed {
        push_tags(
            &mut ops,
            &mut seen,
            &AssetId(format!("xxh3:{}", placed.xxh3)),
            tags,
        );
    }
    for file in &ingest_plan.files {
        if let plan::Decision::Duplicate { asset, .. } = &file.decision {
            push_tags(&mut ops, &mut seen, asset, tags);
        }
    }
    if ops.is_empty() {
        // Nothing touched this pass (every listed file was rejected out of
        // the plan, or the retained plan was empty) — an empty emit would
        // still fold the whole event log into the HLC for no new events.
        return Ok(());
    }
    app.emit(ops)?;
    Ok(())
}

fn push_tags(ops: &mut Vec<Op>, seen: &mut BTreeSet<AssetId>, asset: &AssetId, tags: &[String]) {
    if !seen.insert(asset.clone()) {
        return;
    }
    for tag in tags {
        ops.push(Op::TagAdd {
            asset: asset.clone(),
            tag: tag.clone(),
        });
    }
}

/// Manifest-less entries — folders `run_pass` found via `load_manifest`
/// returning `Ok(None)`, and bare top-level files from [`list_loose_files`]
/// — quiesce before they're touched, then triage to `ctx.args.triage_target`
/// tagged [`TRIAGE_TAG`]. Rows are appended onto `report` directly, rather
/// than returned, so the missing-`--triage-target` case (an operator-side
/// fault, same class as a nonexistent PARA target in the manifested flow)
/// can add exactly one row and return without disturbing anything the
/// manifested loop already recorded earlier in this pass — and, crucially,
/// without using `?` to propagate it, which would abort the whole pass.
fn process_manifest_less(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    triage_dirs: &[PathBuf],
    loose_files: &[PathBuf],
    report: &mut Vec<(String, ContribOutcome)>,
) -> Result<()> {
    let window_ms = quiescence_ms();
    let (quiescent_dirs, waiting_dirs): (Vec<&PathBuf>, Vec<&PathBuf>) =
        triage_dirs.iter().partition(|d| is_quiescent(d, window_ms));
    let (quiescent_files, waiting_files): (Vec<&PathBuf>, Vec<&PathBuf>) =
        loose_files.iter().partition(|f| is_quiescent(f, window_ms));
    push_waiting_row(report, &waiting_dirs, &waiting_files, window_ms);
    if quiescent_dirs.is_empty() && quiescent_files.is_empty() {
        return Ok(());
    }
    let Some(triage) = ctx.args.triage_target.as_deref() else {
        report.push((
            "(manifest-less)".to_string(),
            ContribOutcome::Failed {
                reason: "manifest-less items are in the inbox but no --triage-target was given \
                          — pass one (e.g. --triage-target resource/inbox-triage); nothing is \
                          invented silently"
                    .to_string(),
            },
        ));
        return Ok(());
    };
    let node = {
        let projection = app.projection()?;
        resolve_contribution_node(&projection, triage)
    };
    let node = match node {
        Ok(node) => node,
        Err(e) => {
            report.push((
                "(manifest-less)".to_string(),
                ContribOutcome::Failed {
                    reason: format!("{e:#}"),
                },
            ));
            return Ok(());
        }
    };
    for dir in quiescent_dirs {
        let outcome =
            process_triage_dir(app, ctx, dir, &node).unwrap_or_else(|e| ContribOutcome::Failed {
                reason: format!("{e:#}"),
            });
        report.push((display_name(dir), outcome));
    }
    if !quiescent_files.is_empty() {
        let files: Vec<PathBuf> = quiescent_files.into_iter().cloned().collect();
        let outcome = process_triage_loose_files(app, ctx, &files, &node).unwrap_or_else(|e| {
            ContribOutcome::Failed {
                reason: format!("{e:#}"),
            }
        });
        report.push((LOOSE_FILES_ROW.to_string(), outcome));
    }
    Ok(())
}

/// Appends the single aggregate `(manifest-less)` waiting row, naming every
/// still-quiescing folder/file, when there is at least one — split out of
/// [`process_manifest_less`] purely to keep it under the house
/// function-length limit.
fn push_waiting_row(
    report: &mut Vec<(String, ContribOutcome)>,
    waiting_dirs: &[&PathBuf],
    waiting_files: &[&PathBuf],
    window_ms: u64,
) {
    if waiting_dirs.is_empty() && waiting_files.is_empty() {
        return;
    }
    let reasons = waiting_dirs
        .iter()
        .chain(waiting_files.iter())
        .map(|p| {
            format!(
                "{}: modified within the last {} — letting it quiesce",
                display_name(p),
                format_window(window_ms)
            )
        })
        .collect();
    report.push((
        "(manifest-less)".to_string(),
        ContribOutcome::Waiting { reasons },
    ));
}

/// One manifest-less folder's ingest + `.processed/` move, mirroring
/// [`validate_and_ingest`] for the manifested flow.
fn process_triage_dir(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    node: &(String, ParaKind, String),
) -> Result<ContribOutcome> {
    let outcome = triage_folder_ingest(app, ctx, dir, node.clone())?;
    let contrib = require_clean_outcome(&outcome)?;
    if !ctx.args.keep {
        move_into_processed(&ctx.args.inbox, dir)?;
    }
    Ok(contrib)
}

/// The quiescent loose files' ingest. Unlike a contribution (manifested or
/// a manifest-less folder — see [`require_clean_outcome`]), a
/// `(loose files)` row groups files that share nothing but having landed in
/// the inbox root at the same time, so this deliberately does NOT require a
/// clean outcome: one bad file (a 0-byte file, a permission problem) must
/// never wedge every good file in the same group forever.
///
/// Every file the engine drained out of the inbox — both newly `placed`
/// AND `skipped_duplicates` (content the catalog already had; re-dropping
/// identical bytes is still a successful outcome, just not a new copy) —
/// moves to `.processed/`. Leaving a duplicate behind would re-hash it and
/// re-emit its `TagAdd`s every single pass forever, an unbounded write to
/// the event log every peer replicates. Every file that `failed` or was
/// `rejected` (e.g. 0 bytes) is left in the inbox for the operator to fix
/// or remove — named on stderr by `run_shared_ingest`'s
/// `print_failure_detail`, since this call uses `IngestReport::Silent` —
/// and picked back up as a loose file on the next pass. A fresh
/// failure/rejection still fails the whole run's exit code, the same
/// polarity as any other operator fault, but never blocks the files that
/// drained.
fn process_triage_loose_files(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    files: &[PathBuf],
    node: &(String, ParaKind, String),
) -> Result<ContribOutcome> {
    let outcome = triage_loose_files_ingest(app, ctx, files, node.clone())?;
    if !ctx.args.keep {
        let drained: BTreeSet<&str> = outcome
            .placed
            .iter()
            .map(|p| p.rel.as_str())
            .chain(outcome.skipped_duplicates.iter().map(String::as_str))
            .collect();
        for file in files {
            if drained.contains(display_name(file).as_str()) {
                move_into_processed(&ctx.args.inbox, file)?;
            }
        }
    }
    let failed = outcome.failed.len() + outcome.rejected.len();
    if failed > 0 {
        return Ok(ContribOutcome::PartlyIngested {
            placed: outcome.placed.len(),
            skipped_duplicates: outcome.skipped_duplicates.len(),
            failed,
        });
    }
    Ok(ContribOutcome::Ingested {
        placed: outcome.placed.len(),
        skipped_duplicates: outcome.skipped_duplicates.len(),
    })
}

/// The `.processed/<name>` target for a move, with a numeric suffix on
/// collision.
fn processed_target(processed: &Path, name: &str) -> PathBuf {
    let mut target = processed.join(name);
    let mut suffix = 2u32;
    while target.exists() {
        target = processed.join(format!("{name}-{suffix}"));
        suffix += 1;
    }
    target
}

/// Atomic rename of a contribution folder or one loose file into
/// `.processed/` — `path` may be either, `std::fs::rename` doesn't care.
fn move_into_processed(inbox: &Path, path: &Path) -> Result<()> {
    let processed = inbox.join(".processed");
    std::fs::create_dir_all(&processed)
        .with_context(|| format!("creating {}", processed.display()))?;
    let target = processed_target(&processed, &display_name(path));
    std::fs::rename(path, &target)
        .with_context(|| format!("moving {} to {}", path.display(), target.display()))
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
        match outcome {
            ContribOutcome::Failed { reason } => {
                eprintln!("{name}: {reason}");
                any_failed = true;
            }
            // Per-file reasons already reached stderr when this was
            // constructed (see `process_triage_loose_files`) — nothing more
            // to print here, but it must still fail the pass.
            ContribOutcome::PartlyIngested { .. } => any_failed = true,
            ContribOutcome::Ingested { .. }
            | ContribOutcome::Waiting { .. }
            | ContribOutcome::RecordedFailure { .. } => {}
        }
    }
    anyhow::ensure!(
        !any_failed,
        "one or more items failed — see stdout for the full report"
    );
    Ok(())
}

fn print_report_json(report: &[(String, ContribOutcome)]) -> Result<()> {
    let rows: Vec<serde_json::Value> = report
        .iter()
        .map(|(name, outcome)| match outcome {
            ContribOutcome::Ingested {
                placed,
                skipped_duplicates,
            } => {
                let mut row = serde_json::json!({
                    "contribution": name, "status": "ingested", "placed": placed
                });
                if *skipped_duplicates > 0 {
                    row["skipped_duplicates"] = serde_json::json!(skipped_duplicates);
                }
                row
            }
            ContribOutcome::PartlyIngested {
                placed,
                skipped_duplicates,
                failed,
            } => {
                let mut row = serde_json::json!({
                    "contribution": name, "status": "partly_ingested",
                    "placed": placed, "failed": failed
                });
                if *skipped_duplicates > 0 {
                    row["skipped_duplicates"] = serde_json::json!(skipped_duplicates);
                }
                row
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
            ContribOutcome::Ingested {
                placed,
                skipped_duplicates,
            } if *skipped_duplicates > 0 => {
                println!("{name}: ingested {placed} file(s), {skipped_duplicates} already known");
            }
            ContribOutcome::Ingested { placed, .. } => {
                println!("{name}: ingested {placed} file(s)");
            }
            ContribOutcome::PartlyIngested {
                placed,
                skipped_duplicates,
                failed,
            } if *skipped_duplicates > 0 => {
                println!(
                    "{name}: PARTIAL — ingested {placed} file(s), {skipped_duplicates} already \
                     known, {failed} FAILED — see stderr"
                );
            }
            ContribOutcome::PartlyIngested { placed, failed, .. } => {
                println!(
                    "{name}: PARTIAL — ingested {placed} file(s), {failed} FAILED — see stderr"
                );
            }
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
    use crate::inbox_manifest::ManifestFile;

    #[test]
    fn inbox_key_is_stable_and_distinguishes_different_inboxes() {
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        let key_a1 = inbox_key(a.path()).expect("key a");
        let key_a2 = inbox_key(a.path()).expect("key a again");
        let key_b = inbox_key(b.path()).expect("key b");
        assert_eq!(key_a1, key_a2, "the same inbox path must key the same");
        assert_ne!(key_a1, key_b, "different inboxes must not collide");
        assert_eq!(key_a1.len(), 32, "xxh3-128 hex is 32 chars");
    }

    #[test]
    fn inbox_key_errors_on_a_path_that_cannot_be_canonicalized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert!(inbox_key(&missing).is_err());
    }

    fn one_file_manifest(name: &str, size: u64) -> ContributionManifest {
        ContributionManifest {
            version: 1,
            contributor: "dana".to_string(),
            para_target: None,
            source: None,
            note: None,
            files: vec![ManifestFile {
                name: name.to_string(),
                xxh64: "deadbeef00000000".to_string(),
                size,
            }],
        }
    }

    #[test]
    fn contribution_fingerprint_changes_when_a_listed_file_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("contribution.json"), b"{}").expect("write manifest");
        std::fs::write(dir.path().join("clip.mov"), b"one").expect("write file");
        let manifest = one_file_manifest("clip.mov", 3);
        let before = contribution_fingerprint(dir.path(), Some(&manifest));

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(dir.path().join("clip.mov"), b"two").expect("rewrite file");
        let after = contribution_fingerprint(dir.path(), Some(&manifest));

        assert_ne!(
            before, after,
            "changing a listed file's mtime/size must change the fingerprint"
        );
    }

    #[test]
    fn contribution_fingerprint_is_deterministic_for_unchanged_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("contribution.json"), b"{not valid").expect("write");
        let a = contribution_fingerprint(dir.path(), None);
        let b = contribution_fingerprint(dir.path(), None);
        assert_eq!(a, b, "fingerprinting the same file state twice must agree");
    }

    #[test]
    fn contribution_fingerprint_ignores_files_when_manifest_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("contribution.json"), b"{}").expect("write manifest");
        std::fs::write(dir.path().join("clip.mov"), b"one").expect("write file");
        let manifest = one_file_manifest("clip.mov", 3);
        let with_files = contribution_fingerprint(dir.path(), Some(&manifest));
        let manifest_only = contribution_fingerprint(dir.path(), None);
        assert_ne!(
            with_files, manifest_only,
            "None must fingerprint the manifest alone, distinct from folding in listed files"
        );
    }

    #[test]
    fn quiescence_ms_default_is_five_minutes() {
        // The default is only ever used symbolically elsewhere (env unset
        // -> QUIESCENCE_MS, formatted with format_window); pin the literal.
        assert_eq!(QUIESCENCE_MS, 300_000);
    }

    #[test]
    fn format_window_is_exact_at_the_millisecond_second_boundary() {
        assert_eq!(format_window(0), "0ms");
        assert_eq!(format_window(999), "999ms");
        assert_eq!(format_window(1000), "1s");
        assert_eq!(format_window(1500), "1s");
        assert_eq!(format_window(300_000), "300s");
    }

    #[test]
    fn processed_target_appends_a_numeric_suffix_on_repeated_collisions() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("drop"), b"x").expect("write");
        std::fs::write(dir.path().join("drop-2"), b"x").expect("write");
        std::fs::write(dir.path().join("drop-3"), b"x").expect("write");
        let target = processed_target(dir.path(), "drop");
        assert_eq!(
            target,
            dir.path().join("drop-4"),
            "the suffix must climb past every existing collision, not just the first"
        );
    }
}
