//! `maj ingest` compute: the plan half (projection -> known-assets ->
//! `plan::plan_source` -> subdir render) and the verified-copy execution
//! (journal + engine + ASC MHL generations + catalog events). Moved from
//! `crates/cli/src/commands.rs`. `IngestReport` and every `print_ingest_*`
//! function stay in the CLI — this module never prints; the one stderr
//! notice `run_ingest` used to emit unconditionally (the resume-id line) is
//! now a `notice` callback the caller feeds however it likes (an `eprintln`,
//! or a no-op for a caller — `maj inbox process` — that runs this more than
//! once per process and doesn't want one resume line per contribution).
use crate::app::{FsApp, physical_now_ms};
use crate::iso8601::iso8601_ms;
use crate::para::resolve_para_node;
use crate::scan::{mtime_ms_of, resolve_volume};
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op, ParaKind, VerifyOutcome};
use majestical_core::projection::Projection;
use majestical_ingest::{engine, journal, mhl, plan, template};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The ingest dedupe surface MCP/GUI expose — a real enum so the schema (and
/// a future GUI dropdown) carries the closed value set instead of a free
/// string validated only at call time. Narrower than `plan::DedupeMode`:
/// `skip`/`copy` only, because `Link` still needs the per-destination
/// existing-instance lookup `maj ingest`'s own clap arg also excludes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DedupeMode {
    Skip,
    Copy,
}

impl From<DedupeMode> for plan::DedupeMode {
    fn from(v: DedupeMode) -> Self {
        match v {
            DedupeMode::Skip => Self::Skip,
            DedupeMode::Copy => Self::CopyAnyway,
        }
    }
}

/// Resolves `para` to an active PARA node's (id, kind, name). Ingest targets
/// must be non-archived even when addressed by a raw node id — unlike
/// `resolve_para_node`'s general allowance for archived nodes (needed so an
/// already-archived node can still be renamed by id), silently copying new
/// content into an archived node would resurrect it as a live destination.
///
/// # Errors
/// Returns an error if `para` doesn't resolve to an active PARA node.
pub fn resolve_ingest_node(
    projection: &Projection,
    para: &str,
) -> Result<(String, ParaKind, String)> {
    let node_id = resolve_para_node(projection, para)?;
    let state = projection
        .para_node(&node_id)
        .context("resolved node vanished from the projection")?;
    anyhow::ensure!(
        !state.archived(),
        "PARA node {node_id} is archived — ingest targets must be active; see `maj para list`"
    );
    let kind = state
        .kind()
        .with_context(|| format!("PARA node {node_id} has no kind recorded"))?;
    let name = state
        .name()
        .with_context(|| format!("PARA node {node_id} has no name recorded"))?
        .to_string();
    Ok((node_id, kind, name))
}

/// Builds the planner's `KnownAssets` from every instance size recorded
/// against every asset the catalog knows about. Asset ids are stored as
/// `xxh3:<hex>` (the only format `scan`/`ingest` ever mint); the planner's
/// dedupe hashes are bare hex, so the prefix is stripped here.
#[must_use]
pub fn known_assets_from_projection(projection: &Projection) -> plan::KnownAssets {
    let mut pairs = Vec::new();
    for (asset, state) in projection.assets() {
        let Some(hash) = asset.0.strip_prefix("xxh3:") else {
            continue;
        };
        for info in state.instances.values() {
            pairs.push((hash.to_string(), info.size));
        }
    }
    plan::KnownAssets::from_pairs(pairs)
}

/// Renders the destination-relative subdir: `<KindDir>/<name>/<template>`.
///
/// # Errors
/// Returns an error if `template_str` doesn't render (an unknown
/// placeholder, malformed syntax).
pub fn render_ingest_subdir(
    kind: ParaKind,
    name: &str,
    template_str: &str,
    source_label: &str,
) -> Result<String> {
    let date = iso8601_ms(physical_now_ms())[..10].to_string();
    let ctx = template::TemplateCtx {
        date,
        source_label: source_label.to_string(),
    };
    let rendered =
        template::render(template_str, &ctx).context("rendering ingest layout template")?;
    Ok(format!("{}/{name}/{rendered}", kind.dir_name()))
}

/// Everything `ingest::plan` computed: the file-level plan, the rendered
/// destination subdir, the resolved node id, and the auto-detected source
/// volume identity (needed downstream by [`run_ingest`]'s `ExecuteIngest`).
#[derive(Debug, serde::Serialize)]
pub struct IngestPlanOutcome {
    pub plan: plan::IngestPlan,
    pub subdir: String,
    pub node_id: String,
    pub source_volume_id: String,
    pub source_volume_label: String,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// The plan half of `maj ingest`: resolves `para` to an active node,
/// diffs `source` against every known asset, and renders the destination
/// subdir. Never touches disk beyond reading `source` and the event log —
/// shared by `cmd_ingest`'s `--dry-run` print and its real run, and reused
/// as-is by a non-CLI head's own dry-run (e.g. `maj mcp`). Moved from the
/// first half of `crates/cli/src/commands.rs::cmd_ingest`.
///
/// # Errors
/// Returns an error if `para` doesn't resolve to an active PARA node, or
/// `source`'s directory walk or the subdir template fails.
pub fn plan(
    app: &FsApp,
    source: &Path,
    para: &str,
    template_str: &str,
    dedupe: plan::DedupeMode,
) -> Result<IngestPlanOutcome, crate::error::ServiceError> {
    plan_impl(app, source, para, template_str, dedupe).map_err(crate::error::ServiceError::from)
}

fn plan_impl(
    app: &FsApp,
    source: &Path,
    para: &str,
    template_str: &str,
    dedupe: plan::DedupeMode,
) -> Result<IngestPlanOutcome> {
    let projection = app.projection()?;
    let (node_id, kind, name) = resolve_ingest_node(&projection, para)?;
    let known = known_assets_from_projection(&projection);
    let ingest_plan = plan::plan_source(source, &known, dedupe)
        .with_context(|| format!("planning ingest from {}", source.display()))?;
    let (source_volume_id, source_volume_label) = resolve_volume(source, None);
    let subdir = render_ingest_subdir(kind, &name, template_str, &source_volume_label)?;
    Ok(IngestPlanOutcome {
        plan: ingest_plan,
        subdir,
        node_id,
        source_volume_id,
        source_volume_label,
        notices: app.notices().drain(),
    })
}

/// The verified-copy pipeline shared by `maj ingest` and `maj inbox
/// process`: journal + engine + ASC MHL generations + catalog events. The
/// caller has already planned and resolved the PARA node. Returns the
/// outcome (never erroring just because some files failed/were rejected/
/// produced a diagnostic) so callers decide for themselves what a failed
/// file means — `maj ingest` aborts the process, `maj inbox process` fails
/// only that one contribution without aborting its pass.
pub struct ExecuteIngest<'a> {
    pub plan: &'a plan::IngestPlan,
    pub dest: &'a [PathBuf],
    pub subdir: &'a str,
    pub node_id: &'a str,
    pub source_volume: (&'a str, &'a str),
    pub jobs: Option<usize>,
    pub resume: Option<&'a str>,
}

/// One `run_ingest` call's identity plus its engine result and the ASC MHL
/// generations it wrote — everything a head needs to render `maj ingest`'s
/// (or `maj inbox process`'s) outcome. The run id is needed by a caller's
/// own failure message, which lives outside this function.
#[derive(Debug, serde::Serialize)]
pub struct IngestRun {
    pub run_id: String,
    pub outcome: engine::Outcome,
    pub generations: Vec<(PathBuf, mhl::WrittenGeneration)>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Runs (or resumes) one verified-copy pass: opens/creates the run's
/// journal, copies+verifies every planned file to every destination,
/// writes an ASC MHL generation per destination for this run's placed
/// files, and emits the resulting catalog events. `notice` is called with
/// exactly one line — `run {id} — resume with: --resume {id}` — right
/// after the run id is chosen (fresh or resumed), before any copying
/// starts; a caller that runs this more than once per process (`maj inbox
/// process`, once per contribution) should pass a no-op so it doesn't print
/// one resume line per contribution — `--resume` isn't a flag `maj inbox
/// process` accepts anyway, so the advice would only ever be actionable
/// for `maj ingest`.
///
/// Two deliberate scope decisions carry through this run:
///
/// - The ASC MHL generation written per destination covers only this run's
///   placed files (built straight from `Outcome.placed`, not a re-hash of
///   the whole destination tree — re-hashing terabytes of unrelated content
///   at a reused destination root on every ingest is the wrong default). A
///   reused root's pre-existing content is not recorded until the next
///   `maj verify`, which correctly reports it as new.
/// - Per-destination copy/verify failures do not produce `VerificationRecorded`
///   events this phase: the engine's `Outcome::failed` reason joins every
///   destination's failure into one string with no clean per-destination
///   attribution, and emitting a Failed record against every destination
///   would wrongly mark healthy ones failed too. Truthful incompleteness
///   beats a confidently wrong record; see the phase 3 watchlist.
///
/// Moved from `crates/cli/src/commands.rs::run_ingest`.
///
/// # Errors
/// Returns an error if `--resume` names a run with no journal on disk, or
/// any planning/copy/journal/MHL/event-log step fails.
pub fn run_ingest(
    app: &mut FsApp,
    catalog_dir: &Path,
    exec: &ExecuteIngest<'_>,
    notice: &mut dyn FnMut(&str),
) -> Result<IngestRun, crate::error::ServiceError> {
    run_ingest_impl(app, catalog_dir, exec, notice).map_err(crate::error::ServiceError::from)
}

fn run_ingest_impl(
    app: &mut FsApp,
    catalog_dir: &Path,
    exec: &ExecuteIngest<'_>,
    notice: &mut dyn FnMut(&str),
) -> Result<IngestRun> {
    let run_id = exec
        .resume
        .map_or_else(|| ulid::Ulid::generate().to_string(), str::to_string);
    if exec.resume.is_some() {
        check_resume_journal_exists(catalog_dir, &run_id, app.notices())?;
    }
    notice(&format!("run {run_id} — resume with: --resume {run_id}"));
    let dests = build_dest_specs(exec.dest, exec.subdir);
    let outcome = run_ingest_engine(&RunEngineArgs {
        catalog_dir,
        run_id: &run_id,
        ingest_plan: exec.plan,
        dests: &dests,
        jobs: exec.jobs,
        notices: app.notices(),
    })?;
    let hashdate_ms = physical_now_ms();
    let hashdate = iso8601_ms(hashdate_ms);
    let generations = write_ingest_generations(&dests, &outcome, &hashdate)
        .context("writing ASC MHL generations")?;
    let dest_volumes = dest_volume_identities(exec.dest);
    let mut ops = volume_seen_ops((exec.source_volume.0, exec.source_volume.1), &dest_volumes);
    ops.extend(asset_and_para_ops(
        &outcome,
        &dest_volumes,
        exec.node_id,
        hashdate_ms,
    ));
    ops.extend(manifest_ops(&dest_volumes, &generations));
    app.emit(ops)?;
    Ok(IngestRun {
        run_id,
        outcome,
        generations,
        notices: app.notices().drain(),
    })
}

/// Default worker count: available CPU cores, capped at 8 — a card reader or
/// spinning-disk destination rarely benefits from more parallel streams than
/// that, and the cap bounds open-file-descriptor use per destination.
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(8)
}

fn build_dest_specs(dest_roots: &[PathBuf], subdir: &str) -> Vec<engine::DestSpec> {
    dest_roots
        .iter()
        .map(|root| engine::DestSpec {
            root: root.clone(),
            subdir: subdir.to_string(),
        })
        .collect()
}

fn journal_path_for(
    catalog_dir: &Path,
    run_id: &str,
    notices: &crate::notices::Notices,
) -> Result<PathBuf> {
    let paths = crate::state_dir::catalog_paths(catalog_dir, notices)?;
    Ok(paths.runs_dir.join(format!("{run_id}.jsonl")))
}

/// Guards `--resume <id>`: a run id with no journal on disk is almost always
/// a typo, not a fresh run someone genuinely wants under that exact id —
/// silently starting one there would hide the mistake, and would write to
/// wherever `<id>` interpolates to in the path (a crafted id like
/// `../../x` would otherwise escape `runs/` entirely the first time
/// anything opens that path for append). Requiring the journal to already
/// exist closes both: nothing is *created* in the sync root until this
/// check passes. Resolving the state dir (`state_dir::catalog_paths`) may
/// still perform one-time legacy cleanup there — deleting a pre-phase-4
/// `catalog.db` or moving `runs/*.jsonl` out — but that only ever removes
/// stale derived files, never creates anything new.
fn check_resume_journal_exists(
    catalog_dir: &Path,
    run_id: &str,
    notices: &crate::notices::Notices,
) -> Result<()> {
    let journal_path = journal_path_for(catalog_dir, run_id, notices)?;
    anyhow::ensure!(
        journal_path.is_file(),
        "no journal for run '{run_id}' — check the id printed at the start of the original run"
    );
    Ok(())
}

/// Args for `run_ingest_engine`, bundled to keep its own signature within
/// the house 5-positional-parameter limit.
struct RunEngineArgs<'a> {
    catalog_dir: &'a Path,
    run_id: &'a str,
    ingest_plan: &'a plan::IngestPlan,
    dests: &'a [engine::DestSpec],
    jobs: Option<usize>,
    notices: &'a crate::notices::Notices,
}

/// Opens (or resumes) the run's journal and executes the copy/verify engine.
/// Always loads the journal before appending, even on a fresh run: loading a
/// journal that doesn't exist yet returns an empty fold, so a fresh run and
/// a `--resume` both flow through the same path rather than branching twice
/// on whether `--resume` was given. Callers resuming an existing run must
/// call `check_resume_journal_exists` first — this function creates the
/// journal file if it's missing, which is correct for a fresh run but would
/// silently paper over a typo'd `--resume` id.
fn run_ingest_engine(args: &RunEngineArgs<'_>) -> Result<engine::Outcome> {
    let RunEngineArgs {
        catalog_dir,
        run_id,
        ingest_plan,
        dests,
        jobs,
        notices,
    } = *args;
    let journal_path = journal_path_for(catalog_dir, run_id, notices)?;
    let resume_set = journal::Journal::load(&journal_path)
        .with_context(|| format!("loading journal at {}", journal_path.display()))?
        .placed;
    let mut journal = journal::Journal::open_append(&journal_path)
        .with_context(|| format!("opening journal at {}", journal_path.display()))?;
    let config = engine::EngineConfig {
        jobs: jobs.unwrap_or_else(default_jobs),
    };
    engine::run(
        ingest_plan,
        dests,
        &resume_set,
        &mut journal,
        &engine::RealSinks,
        &config,
    )
    .context("running ingest engine")
}

/// Builds the run's MHL hash list straight from `Outcome.placed` (guidance:
/// the engine already computed and verified each placed file's xxh64+size
/// during copy, so re-hashing the destination tree here would redo that work
/// — and would also sweep in any pre-existing, unrelated content at a reused
/// destination root). See [`run_ingest`]'s doc comment for the consequence.
fn build_generation_hash_list(outcome: &engine::Outcome, hashdate: &str) -> mhl::HashList {
    let entries = outcome
        .placed
        .iter()
        .map(|placed| mhl::MhlEntry {
            rel: placed.dest_rel.clone(),
            size: placed.size,
            xxh64: placed.xxh64.clone(),
            action: mhl::HashAction::Original,
            hashdate: hashdate.to_string(),
        })
        .collect();
    mhl::HashList {
        creation_date: hashdate.to_string(),
        hostname: mhl::local_hostname(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        entries,
    }
}

/// Writes a new generation per destination from this run's placed files —
/// unless nothing was placed. A dedupe-only or fully-resumed run leaves
/// `Outcome.placed` empty; writing a generation from an empty hash list
/// anyway would not merge with the previous one (`write_generation` always
/// writes exactly the list it's given, unlike `verify_dir`'s diff-and-merge),
/// so it would make the destination's latest generation forget every file
/// a prior run genuinely placed and verified there — the next `maj verify`
/// would then report all of them as "new" instead of leaving them verified.
/// Skipping the write when there is nothing new keeps history intact.
fn write_ingest_generations(
    dests: &[engine::DestSpec],
    outcome: &engine::Outcome,
    hashdate: &str,
) -> Result<Vec<(PathBuf, mhl::WrittenGeneration)>> {
    if outcome.placed.is_empty() {
        return Ok(Vec::new());
    }
    let hash_list = build_generation_hash_list(outcome, hashdate);
    dests
        .iter()
        .map(|dest| {
            let written = mhl::write_generation(&dest.root, &hash_list).with_context(|| {
                format!("writing ASC MHL generation at {}", dest.root.display())
            })?;
            Ok((dest.root.clone(), written))
        })
        .collect()
}

/// Resolves each destination root's real volume identity (diskutil-backed
/// on macOS, with `volume_identity`'s documented mount-label fallback
/// elsewhere) rather than lumping every destination under one root volume.
fn dest_volume_identities(dest_roots: &[PathBuf]) -> Vec<(PathBuf, String, String)> {
    dest_roots
        .iter()
        .map(|root| {
            let identity = volume_identity::resolve(root);
            (root.clone(), identity.id, identity.label)
        })
        .collect()
}

fn volume_seen_ops(
    source_volume: (&str, &str),
    dest_volumes: &[(PathBuf, String, String)],
) -> Vec<Op> {
    let mut ops = vec![Op::VolumeSeen {
        volume: source_volume.0.to_string(),
        label: source_volume.1.to_string(),
    }];
    ops.extend(dest_volumes.iter().map(|(_, id, label)| Op::VolumeSeen {
        volume: id.clone(),
        label: label.clone(),
    }));
    ops
}

/// Re-bases a placed file's destination-root-relative path to be relative to
/// its destination volume's actual mount root instead — same treatment as
/// an auto-detected `scan`, so the indexer can later re-find these bytes
/// regardless of which destination root was used. Unlike `scan`, ingest has
/// no synthetic `--volume` override to special-case: `dest_volumes` ids are
/// always `volume_identity::resolve`'s real, auto-detected identities.
/// Falls back to the destination-relative path if the strip fails (e.g. the
/// file vanished between placement and this call).
fn vol_rel_path(root: &Path, dest_rel: &str) -> String {
    let abs = root.join(dest_rel);
    let abs = abs.canonicalize().unwrap_or(abs);
    let mount = volume_identity::mount_point_of(&abs);
    abs.strip_prefix(&mount).map_or_else(
        |_| dest_rel.to_string(),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}

/// `AssetSeen` + `VerificationRecorded` for every placed file at every
/// destination, plus one `AssetParaSet` per distinct asset actually placed
/// this run (not one per file — a burst-shot asset placed under several
/// rels would otherwise mint redundant, identical assignments).
fn asset_and_para_ops(
    outcome: &engine::Outcome,
    dest_volumes: &[(PathBuf, String, String)],
    node_id: &str,
    hashdate_ms: u64,
) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut seen_assets: BTreeSet<AssetId> = BTreeSet::new();
    for placed in &outcome.placed {
        let asset = AssetId(format!("xxh3:{}", placed.xxh3));
        for (root, dest_id, _) in dest_volumes {
            let mtime_ms =
                std::fs::metadata(root.join(&placed.dest_rel)).map_or(0, |m| mtime_ms_of(&m));
            let vol_rel = vol_rel_path(root, &placed.dest_rel);
            ops.push(Op::AssetSeen {
                asset: asset.clone(),
                volume: dest_id.clone(),
                path: vol_rel.clone(),
                size: placed.size,
                mtime_ms,
            });
            ops.push(Op::VerificationRecorded {
                asset: asset.clone(),
                volume: dest_id.clone(),
                path: vol_rel,
                algo: "xxh64".to_string(),
                value: placed.xxh64.clone(),
                outcome: VerifyOutcome::Verified,
                hashdate_ms,
            });
        }
        if seen_assets.insert(asset.clone()) {
            ops.push(Op::AssetParaSet {
                asset,
                node: node_id.to_string(),
            });
        }
    }
    ops
}

fn manifest_ops(
    dest_volumes: &[(PathBuf, String, String)],
    generations: &[(PathBuf, mhl::WrittenGeneration)],
) -> Vec<Op> {
    dest_volumes
        .iter()
        .filter_map(|(root, id, _)| {
            let (_, written) = generations.iter().find(|(r, _)| r == root)?;
            // `file_name()` is never `None` here: `write_generation` always
            // builds `written.path` as `ascmhl_dir.join(<generated filename>)`
            // with a non-empty generated filename, never a bare `..` or `/`.
            let mhl_path = format!(
                "ascmhl/{}",
                written
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            );
            Some(Op::ManifestRecorded {
                volume: id.clone(),
                mhl_path,
                generation: written.generation,
                roothash: written.roothash.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::event::ParaKind;

    fn init_app(dir: &Path) -> FsApp {
        FsApp::init(&dir.join("cat"), "m1", "m1").expect("init")
    }

    #[test]
    fn dedupe_mode_wire_strings_are_pinned() {
        for (mode, wire) in [(DedupeMode::Skip, "skip"), (DedupeMode::Copy, "copy")] {
            assert_eq!(
                serde_json::to_value(mode).expect("ser"),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<DedupeMode>(serde_json::json!(wire)).expect("de"),
                mode
            );
        }
        assert!(serde_json::from_value::<DedupeMode>(serde_json::json!("bogus")).is_err());
    }

    #[test]
    fn plan_of_a_nonexistent_para_target_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = init_app(dir.path());
        let source = tempfile::tempdir().expect("tempdir");
        let err = plan(
            &app,
            source.path(),
            "project/nope",
            "{date}",
            plan::DedupeMode::Skip,
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("no active PARA node"));
    }

    /// A raw node id (unlike the `<kind>/<name>` form, which
    /// `resolve_para_node` already restricts to non-archived nodes) still
    /// resolves an archived node — `resolve_ingest_node`'s own explicit
    /// check is the only thing rejecting it as an ingest target.
    #[test]
    fn plan_of_an_archived_target_by_raw_id_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let crate::para::NodeId(id) =
            crate::para::add(&mut app, "project", "client-x").expect("add");
        crate::para::archive(&mut app, &id, &[], false).expect("archive");
        let source = tempfile::tempdir().expect("tempdir");
        let err = plan(&app, source.path(), &id, "{date}", plan::DedupeMode::Skip)
            .expect_err("must fail");
        assert!(err.to_string().contains("archived"));
    }

    #[test]
    fn plan_renders_a_subdir_from_the_target_and_template() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        crate::para::add(&mut app, "project", "client-x").expect("add");
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("a.mov"), b"hello").expect("write");
        let outcome = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::Skip,
        )
        .expect("plan");
        assert_eq!(outcome.subdir, "Projects/client-x/raw");
        assert_eq!(outcome.plan.files.len(), 1);
        assert!(!outcome.node_id.is_empty());
    }

    #[test]
    fn run_ingest_places_files_and_calls_notice_once_with_the_run_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        crate::para::add(&mut app, "project", "client-x").expect("add");
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("a.mov"), b"hello").expect("write");
        let planned = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::Skip,
        )
        .expect("plan");
        let dest = tempfile::tempdir().expect("tempdir");
        let dest_roots = vec![dest.path().to_path_buf()];
        let mut notices = Vec::new();
        let run = run_ingest(
            &mut app,
            &dir.path().join("cat"),
            &ExecuteIngest {
                plan: &planned.plan,
                dest: &dest_roots,
                subdir: &planned.subdir,
                node_id: &planned.node_id,
                source_volume: (&planned.source_volume_id, &planned.source_volume_label),
                jobs: Some(1),
                resume: None,
            },
            &mut |line| notices.push(line.to_string()),
        )
        .expect("run_ingest");
        assert_eq!(run.outcome.placed.len(), 1);
        assert!(run.outcome.failed.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains(&run.run_id));
        assert!(notices[0].contains("--resume"));
        assert_eq!(run.generations.len(), 1);
    }

    #[test]
    fn run_ingest_with_an_unknown_resume_id_errors_naming_the_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        crate::para::add(&mut app, "project", "client-x").expect("add");
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("a.mov"), b"hello").expect("write");
        let planned = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::Skip,
        )
        .expect("plan");
        let dest = tempfile::tempdir().expect("tempdir");
        let dest_roots = vec![dest.path().to_path_buf()];
        let err = run_ingest(
            &mut app,
            &dir.path().join("cat"),
            &ExecuteIngest {
                plan: &planned.plan,
                dest: &dest_roots,
                subdir: &planned.subdir,
                node_id: &planned.node_id,
                source_volume: (&planned.source_volume_id, &planned.source_volume_label),
                jobs: Some(1),
                resume: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            },
            &mut |_| {},
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("no journal for run"));
    }

    #[test]
    fn known_assets_from_projection_makes_plan_source_see_a_matching_file_as_a_duplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        crate::para::add(&mut app, "project", "client-x").expect("add");
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("a.mov"), b"hello").expect("write");
        // The real xxh3-128 hex of b"hello", so the planner's own hashing of
        // a.mov matches this recorded asset exactly.
        let hash = xxhash_rust::xxh3::xxh3_128(b"hello");
        app.emit(vec![Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: "vol1".into(),
            path: "elsewhere.mov".into(),
            size: 5,
            mtime_ms: 1000,
        }])
        .expect("emit");
        let outcome = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::Skip,
        )
        .expect("plan");
        assert_eq!(outcome.plan.files.len(), 1);
        assert!(matches!(
            outcome.plan.files[0].decision,
            plan::Decision::Duplicate { .. }
        ));
    }

    /// `--dedupe skip` vs `--dedupe copy` must plan a known-duplicate file
    /// differently — `plan::Decision::Duplicate`'s `action` field carries
    /// whichever mode was requested, so `engine::run` (untested by this
    /// crate) knows whether to skip the copy or place it anyway. Pins the
    /// planning half of that contract; `CopyAnyway`'s actual copy behavior
    /// is `majestical_ingest`'s own responsibility, exercised end to end by
    /// `crates/cli/tests/cli_smoke.rs` and the parity harness.
    #[test]
    fn dedupe_skip_and_copy_anyway_plan_a_known_duplicate_differently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        crate::para::add(&mut app, "project", "client-x").expect("add");
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("a.mov"), b"hello").expect("write");
        let hash = xxhash_rust::xxh3::xxh3_128(b"hello");
        app.emit(vec![Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: "vol1".into(),
            path: "elsewhere.mov".into(),
            size: 5,
            mtime_ms: 1000,
        }])
        .expect("emit");

        let skip_plan = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::Skip,
        )
        .expect("plan skip");
        let copy_plan = plan(
            &app,
            source.path(),
            "project/client-x",
            "raw",
            plan::DedupeMode::CopyAnyway,
        )
        .expect("plan copy");

        assert_eq!(
            skip_plan.plan.files[0].decision,
            plan::Decision::Duplicate {
                asset: AssetId(format!("xxh3:{hash:032x}")),
                action: plan::DedupeMode::Skip,
            }
        );
        assert_eq!(
            copy_plan.plan.files[0].decision,
            plan::Decision::Duplicate {
                asset: AssetId(format!("xxh3:{hash:032x}")),
                action: plan::DedupeMode::CopyAnyway,
            }
        );
        assert_ne!(
            skip_plan.plan.files[0].decision, copy_plan.plan.files[0].decision,
            "skip and copy-anyway must plan the same duplicate file differently"
        );
    }

    #[test]
    fn render_ingest_subdir_uses_kind_dir_name_and_the_node_name() {
        let subdir = render_ingest_subdir(ParaKind::Area, "health", "{source-label}", "camera-a")
            .expect("render");
        assert_eq!(subdir, "Areas/health/camera-a");
    }
}
