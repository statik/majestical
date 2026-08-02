//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app). Manifest-less entries (a subfolder with no manifest, or a
//! bare file) are skipped this release — no triage flow exists yet.
//! Reuses the verified-ingest pipeline end to end. Manifest parsing and
//! the read-only validation gates live in `inbox_manifest.rs`; this module
//! is the orchestration: the pass loop, hash gate, routing, ingest, and
//! the failure-marker store.
use crate::app::FsApp;
use crate::commands::{self, ExecuteIngest};
use crate::inbox_manifest::{ContributionManifest, MANIFEST_NAME, check_files, load_manifest};
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op, ParaKind};
use majestical_core::projection::Projection;
use majestical_ingest::{engine, hashing, plan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::xxh3_128;

/// Bundles the flags within the house 5-positional-parameter limit.
pub(crate) struct InboxArgs {
    pub inbox: PathBuf,
    pub dest: Vec<PathBuf>,
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

/// Per-machine record of contributions that failed validation, so a later
/// pass skips them with a notice instead of re-checking forever. Keyed by
/// folder name; cleared automatically when the fingerprint changes — the
/// manifest OR any listed file's mtime/size — so both a re-export and
/// fixing just the corrupt file re-validate.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FailureMarkers {
    #[serde(default)]
    failures: std::collections::BTreeMap<String, FailureMarker>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FailureMarker {
    reason: String,
    fingerprint: String,
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
    let (mtime, size) = std::fs::metadata(dir.join(MANIFEST_NAME))
        .map_or((0, 0), |m| (commands::mtime_ms_of(&m), m.len()));
    let _ = write!(encoded, "m:{mtime}:{size}");
    if let Some(manifest) = manifest {
        for file in &manifest.files {
            let (mtime, size) = std::fs::metadata(dir.join(&file.name))
                .map_or((0, 0), |m| (commands::mtime_ms_of(&m), m.len()));
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
    name: &str,
    fingerprint: &str,
) -> Option<ContribOutcome> {
    let marker = markers.failures.get(name)?;
    if marker.fingerprint == fingerprint {
        return Some(ContribOutcome::RecordedFailure {
            reason: marker.reason.clone(),
        });
    }
    markers.failures.remove(name);
    None
}

fn record_failure(markers: &mut FailureMarkers, name: &str, reason: &str, fingerprint: &str) {
    markers.failures.insert(
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

/// One converging pass over `args.inbox`: every manifested subfolder is
/// validated, verified-ingested, tagged with provenance, and (unless
/// `--keep`) moved to `.processed/`. Manifest-less entries (a subfolder
/// with no `contribution.json`, or a bare file) are collected and silently
/// ignored this task — a follow-up adds the quiescence-gated triage flow
/// for them.
///
/// # Errors
/// Returns an error if `inbox` isn't a directory, or if loading or storing
/// the failure-marker store fails. Reading the inbox directory itself is
/// the one per-contribution-loop failure that still aborts the pass (an
/// operator-facing I/O problem, not any one contribution's fault) — markers
/// recorded by contributions processed earlier in the same pass are always
/// persisted first, so that failure never discards them. Also returns an
/// error, after the report prints, if any contribution freshly failed this
/// run — a previously recorded failure is only a notice, not an error (see
/// `print_report`).
pub(crate) fn cmd_inbox_process(app: &mut FsApp, catalog: &Path, args: &InboxArgs) -> Result<()> {
    anyhow::ensure!(
        args.inbox.is_dir(),
        "inbox must be a directory: {}",
        args.inbox.display()
    );
    let mut markers = load_markers(catalog)?;
    let ctx = InboxCtx { catalog, args };
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
    for path in list_contribution_dirs(&ctx.args.inbox)? {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load_manifest(&path) {
            Ok(Some(manifest)) => {
                let outcome = process_contribution(app, ctx, &path, &manifest, markers)?;
                report.push((name, outcome));
            }
            Ok(None) => {} // manifest-less: a follow-up task
            Err(e) => {
                let fingerprint = contribution_fingerprint(&path, None);
                let outcome = if let Some(outcome) = recorded_failure(markers, &name, &fingerprint)
                {
                    outcome
                } else {
                    let reason = format!("{e:#}");
                    record_failure(markers, &name, &reason, &fingerprint);
                    ContribOutcome::Failed { reason }
                };
                report.push((name, outcome));
            }
        }
    }
    Ok(report)
}

/// Sorted, non-dot, directory-only entries directly under `inbox`. Bare
/// files are a follow-up task's manifest-less flow; `.processed/`,
/// `.DS_Store`, and any other dot-entry (including a sync tool's droppings)
/// are skipped so a completed pass is never re-walked.
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
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fingerprint = contribution_fingerprint(dir, Some(manifest));
    if let Some(outcome) = recorded_failure(markers, &name, &fingerprint) {
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
        record_failure(markers, &name, &reason, &fingerprint);
        return Ok(ContribOutcome::Failed { reason });
    }
    // Route before hashing: resolving the target is a cheap in-memory
    // projection lookup, while the hash gate below reads every listed byte.
    // A contribution parked on a typo'd or archived target must fail here,
    // not after re-reading a multi-hundred-gigabyte drop on every pass.
    if let Err(e) = route_contribution(app, manifest) {
        return Ok(ContribOutcome::Failed {
            reason: format!("{e:#}"),
        });
    }
    match hash_mismatch_reason(dir, manifest) {
        Ok(Some(reason)) => {
            record_failure(markers, &name, &reason, &fingerprint);
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
    // Everything past this point (routing, planning, the verified copy
    // itself) is likewise contribution-scoped and never recorded: a
    // nonexistent PARA node or a transient copy failure isn't fixed by
    // touching the manifest, so recording it against the fingerprint would
    // leave it stuck even after the operator fixes the real cause.
    match validate_and_ingest(app, ctx, dir, manifest) {
        Ok(outcome) => Ok(outcome),
        Err(e) => Ok(ContribOutcome::Failed {
            reason: format!("{e:#}"),
        }),
    }
}

/// A cheap, no-file-I/O check that this contribution's `para_target`
/// resolves to somewhere ingestible — no file bytes are read. Called before
/// [`hash_mismatch_reason`] purely to fail fast on a routing problem
/// (missing `para_target`, a typo'd or archived node) before that gate
/// re-reads every listed byte. [`ingest_contribution`] resolves the target
/// again once ingestion actually proceeds; a second cheap lookup costs
/// nothing next to the hash gate it's meant to guard.
fn route_contribution(app: &mut FsApp, manifest: &ContributionManifest) -> Result<()> {
    let Some(para) = manifest.para_target.as_deref() else {
        anyhow::bail!(
            "manifest has no para_target — add one to the manifest, or wait for the \
             manifest-less triage flow"
        );
    };
    let projection = app.projection()?;
    resolve_contribution_node(&projection, para)?;
    Ok(())
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

/// The routing + verified-ingest + `.processed/` move tail of
/// [`process_contribution`], split out purely to stay under the house
/// function-length limit. Every error here is contribution-scoped — the
/// caller converts it into a `Failed` row rather than propagating it.
fn validate_and_ingest(
    app: &mut FsApp,
    ctx: &InboxCtx<'_>,
    dir: &Path,
    manifest: &ContributionManifest,
) -> Result<ContribOutcome> {
    // para_target is optional in the wire format — a well-formed manifest
    // that simply hasn't been routed yet must fail only itself, not halt
    // every other contribution in the same pass. `route_contribution`
    // (called before the hash gate, earlier in `process_contribution`) has
    // already checked this in the common case; re-checked here too since
    // this function must stand on its own. The report line prepends the
    // contribution's name, so this message must not repeat it.
    let Some(para) = manifest.para_target.as_deref() else {
        anyhow::bail!(
            "manifest has no para_target — add one to the manifest, or wait for the \
             manifest-less triage flow"
        );
    };
    let outcome = ingest_contribution(app, ctx, dir, manifest, para)?;
    anyhow::ensure!(
        outcome.failed.is_empty() && outcome.rejected.is_empty() && outcome.diagnostics.is_empty(),
        "{} failed, {} rejected, {} diagnostic(s) placing this contribution's files — see stderr",
        outcome.failed.len(),
        outcome.rejected.len(),
        outcome.diagnostics.len()
    );
    if !ctx.args.keep {
        move_to_processed(&ctx.args.inbox, dir)?;
    }
    Ok(ContribOutcome::Ingested {
        placed: outcome.placed.len(),
        skipped_duplicates: outcome.skipped_duplicates.len(),
    })
}

/// Resolves `para` (validated by `load_manifest` to at most one interior
/// `/`) to an active ingest target. When it's in `<kind>/<name>` form and
/// doesn't resolve to an active node, this names the exact fix instead of
/// [`commands::resolve_ingest_node`]'s generic "see `maj para list`" (which
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
        && let Ok(kind) = commands::parse_kind(kind_str)
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
    commands::resolve_ingest_node(projection, para)
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
) -> Result<engine::Outcome> {
    let projection = app.projection()?;
    let (node_id, kind, name) = resolve_contribution_node(&projection, para)?;
    let known = commands::known_assets_from_projection(&projection);
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
    let (vol_id, vol_label) = commands::resolve_volume(dir, None);
    // Same default layout `maj ingest` uses — the contributor lands as a
    // tag below, not a subdirectory, so a manifested drop and a manual
    // ingest of the same PARA node share one layout.
    let subdir = commands::render_ingest_subdir(kind, &name, "{date}/{source-label}", &vol_label)?;
    let run = commands::run_ingest(
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
            // Suppresses this run's own stdout summary: `maj inbox process
            // --json` must print exactly one JSON document (the final
            // report), never one blob per ingested contribution.
            quiet: ctx.args.json,
        },
    )?;
    tag_provenance(app, manifest, &ingest_plan, &run.outcome)?;
    Ok(run.outcome)
}

/// Contributor + optional source, as plain `TagAdd`s on every distinct asset
/// this contribution touched — both newly placed files AND files the
/// planner found already known (`Decision::Duplicate`). Skipping duplicates
/// would silently drop provenance whenever a second contributor re-drops
/// content someone else already ingested: the asset is real and this
/// contribution genuinely vouches for it too, even though nothing new was
/// copied.
fn tag_provenance(
    app: &mut FsApp,
    manifest: &ContributionManifest,
    ingest_plan: &plan::IngestPlan,
    outcome: &engine::Outcome,
) -> Result<()> {
    let mut ops = Vec::new();
    let mut seen = BTreeSet::new();
    for placed in &outcome.placed {
        push_provenance_tag(
            &mut ops,
            &mut seen,
            AssetId(format!("xxh3:{}", placed.xxh3)),
            manifest,
        );
    }
    for file in &ingest_plan.files {
        if let plan::Decision::Duplicate { asset, .. } = &file.decision {
            push_provenance_tag(&mut ops, &mut seen, asset.clone(), manifest);
        }
    }
    app.emit(ops)?;
    Ok(())
}

fn push_provenance_tag(
    ops: &mut Vec<Op>,
    seen: &mut BTreeSet<AssetId>,
    asset: AssetId,
    manifest: &ContributionManifest,
) {
    if !seen.insert(asset.clone()) {
        return;
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
