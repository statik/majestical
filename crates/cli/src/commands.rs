//! One cmd_* handler per CLI verb. main.rs owns clap definitions and dispatch;
//! handlers own behavior.
use crate::{MetaCmd, ParaCmd, TagCmd};
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op, ParaKind, VerifyOutcome};
use majestical_core::projection::Projection;
use majestical_ingest::{engine, journal, mhl, plan, template};
use majestical_services::app::{FsApp, physical_now_ms};
use majestical_services::iso8601::iso8601_ms;
use majestical_services::para::resolve_para_node;
use majestical_services::scan::{mtime_ms_of, resolve_volume};
use majestical_services::volume_identity;
use majestical_services::volumes::VolumeRow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn cmd_catalog_init(catalog: &Path, machine_id: &str, author: &str) -> Result<()> {
    majestical_services::catalog::init(catalog, machine_id, author)?;
    println!("initialized catalog at {}", catalog.display());
    Ok(())
}

pub(crate) fn cmd_scan(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<()> {
    let outcome = majestical_services::scan::scan(app, dir, volume)?;
    println!("scanned: {} assets", outcome.assets);
    Ok(())
}

pub(crate) fn cmd_tag(app: &mut FsApp, cmd: TagCmd) -> Result<()> {
    match cmd {
        TagCmd::Add { asset, tag } => majestical_services::tags::tag_add(app, &asset, &tag)?,
        TagCmd::Rm { asset, tag } => majestical_services::tags::tag_rm(app, &asset, &tag)?,
    }
    println!("ok");
    Ok(())
}

pub(crate) fn cmd_meta(app: &mut FsApp, cmd: MetaCmd) -> Result<()> {
    match cmd {
        MetaCmd::Set {
            asset,
            field,
            value,
        } => {
            majestical_services::meta::meta_set(app, &asset, &field, &value)?;
            println!("ok");
        }
        MetaCmd::Get { asset, field, json } => {
            let outcome = majestical_services::meta::meta_get(app, &asset, field.as_deref())?;
            print_meta_get(&outcome, field.as_deref(), json);
        }
    }
    Ok(())
}

/// Prints either a single field's value or every field set on `asset`.
/// A single missing field prints nothing (an empty line in text mode, `null`
/// in JSON) rather than erroring — mirroring `search`'s "zero hits" style
/// rather than treating "not set yet" as a failure.
pub(crate) fn print_meta_get(
    outcome: &majestical_services::meta::MetaOutcome,
    field: Option<&str>,
    json: bool,
) {
    if let Some(field) = field {
        let value = outcome.fields.first().map(|(_, v)| v.as_str());
        if json {
            println!("{}", serde_json::json!({ field: value }));
        } else if let Some(value) = value {
            println!("{value}");
        } else {
            println!();
        }
        return;
    }
    if json {
        let obj: serde_json::Map<String, serde_json::Value> = outcome
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        println!("{}", serde_json::Value::Object(obj));
    } else {
        for (k, v) in &outcome.fields {
            println!("{k}\t{v}");
        }
    }
}

pub(crate) fn cmd_volumes_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::volumes::volumes_list(app, catalog_dir)?;
    if json {
        let rows: Vec<_> = outcome
            .volumes
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "label": row.label,
                    "last_seen": iso8601_ms(row.last_seen_ms),
                    "online": row.online,
                    "asset_count": row.asset_count,
                    "clock_suspect": row.clock_suspect,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "volumes": rows }));
    } else {
        print_volumes_table(&outcome.volumes);
    }
    Ok(())
}

/// Renders the human-readable volumes table with column widths sized to
/// the widest cell in each column (header included) — a fixed width breaks
/// alignment once an auto-detected `uuid:` id (41 chars) or a
/// "(clock suspect)"-annotated last-seen cell appears.
pub(crate) fn print_volumes_table(volumes: &[VolumeRow]) {
    let rows: Vec<(String, String, String, &'static str, u64)> = volumes
        .iter()
        .map(|row| {
            let mut last_seen = iso8601_ms(row.last_seen_ms);
            if row.clock_suspect {
                last_seen.push_str(" (clock suspect)");
            }
            let online = if row.online { "online" } else { "offline" };
            (
                row.id.clone(),
                row.label.clone(),
                last_seen,
                online,
                row.asset_count,
            )
        })
        .collect();
    let id_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(2);
    let label_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(5);
    let seen_w = rows.iter().map(|r| r.2.len()).max().unwrap_or(0).max(9);
    let online_w = rows.iter().map(|r| r.3.len()).max().unwrap_or(0).max(6);
    println!(
        "{:<id_w$} {:<label_w$} {:<seen_w$} {:<online_w$} ASSETS",
        "ID", "LABEL", "LAST SEEN", "ONLINE"
    );
    for (id, label, last_seen, online, count) in &rows {
        println!("{id:<id_w$} {label:<label_w$} {last_seen:<seen_w$} {online:<online_w$} {count}");
    }
}

pub(crate) fn cmd_para(app: &mut FsApp, catalog_dir: &Path, cmd: ParaCmd) -> Result<()> {
    match cmd {
        ParaCmd::Add { kind, name } => cmd_para_add(app, &kind, &name)?,
        ParaCmd::List { json } => cmd_para_list(app, catalog_dir, json)?,
        ParaCmd::Rename { node, name } => cmd_para_rename(app, &node, &name)?,
        ParaCmd::Archive {
            node,
            root,
            dry_run,
        } => cmd_para_archive(app, &node, &root, dry_run)?,
    }
    Ok(())
}

fn cmd_para_add(app: &mut FsApp, kind_str: &str, name: &str) -> Result<()> {
    let majestical_services::para::NodeId(node_id) =
        majestical_services::para::add(app, kind_str, name)?;
    println!("{node_id}");
    Ok(())
}

fn cmd_para_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::para::para_list(app, catalog_dir)?;
    if json {
        let rows: Vec<_> = outcome
            .nodes
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id, "kind": row.kind, "name": row.name, "archived": row.archived
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "nodes": rows }));
    } else {
        print_para_table(&outcome.nodes);
    }
    Ok(())
}

/// Renders the human-readable para-nodes table, following
/// `print_volumes_table`'s width-sizing pattern.
fn print_para_table(nodes: &[majestical_services::para::ParaNodeRow]) {
    let id_w = nodes.iter().map(|r| r.id.len()).max().unwrap_or(0).max(2);
    let kind_w = nodes.iter().map(|r| r.kind.len()).max().unwrap_or(0).max(4);
    let name_w = nodes.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);
    println!(
        "{:<id_w$} {:<kind_w$} {:<name_w$} ARCHIVED",
        "ID", "KIND", "NAME"
    );
    for row in nodes {
        let (id, kind, name, archived) = (&row.id, &row.kind, &row.name, row.archived);
        println!("{id:<id_w$} {kind:<kind_w$} {name:<name_w$} {archived}");
    }
}

fn cmd_para_rename(app: &mut FsApp, node: &str, name: &str) -> Result<()> {
    majestical_services::para::rename(app, node, name)?;
    println!("ok");
    Ok(())
}

/// Archives a node. With `--root`s, each root's materialized directory
/// (`<root>/<KindDir>/<name>`) is moved to `<root>/Archives/<name>` before
/// the archive event is emitted; with no roots, only the event is emitted
/// (skipped in `--dry-run`) and a note is printed that nothing was moved on
/// disk. `outcome.moves` is empty exactly when no roots were given (every
/// root produces exactly one [`majestical_services::para::ArchiveMove`]),
/// which is what distinguishes the two print shapes below.
fn cmd_para_archive(app: &mut FsApp, node: &str, roots: &[PathBuf], dry_run: bool) -> Result<()> {
    use majestical_services::para::MoveStatus;

    let outcome = majestical_services::para::archive(app, node, roots, dry_run)?;
    if outcome.moves.is_empty() {
        if dry_run {
            println!("would archive (dry run; no --root given; no directories to move)");
        } else {
            println!("ok (no --root given; no directories moved)");
        }
        return Ok(());
    }
    for mv in &outcome.moves {
        match mv.status {
            MoveStatus::AlreadyArchived => {
                println!("already archived at {} — skipping", mv.to.display());
            }
            MoveStatus::Planned => {
                println!("would move {} -> {}", mv.from.display(), mv.to.display());
            }
            MoveStatus::Moved => {
                println!("moved {} -> {}", mv.from.display(), mv.to.display());
            }
        }
    }
    Ok(())
}

/// Re-verifies `dir` against its own ASC MHL history and appends a new
/// generation recording the result. Needs no catalog — the history lives
/// entirely under `dir/ascmhl`.
pub(crate) fn cmd_verify(dir: &Path, json: bool) -> Result<()> {
    let report = majestical_services::verify::verify_dir_op(dir)?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "verified": report.verified,
                "altered": report.altered,
                "missing": report.missing,
                "new": report.new_files,
                "generation": report.generation,
            })
        );
    } else {
        for rel in &report.altered {
            println!("ALTERED {rel}");
        }
        for rel in &report.missing {
            println!("MISSING {rel}");
        }
        for rel in &report.new_files {
            println!("NEW {rel}");
        }
        println!(
            "{} verified, {} altered, {} missing, {} new — wrote generation {}",
            report.verified.len(),
            report.altered.len(),
            report.missing.len(),
            report.new_files.len(),
            report.generation
        );
    }

    anyhow::ensure!(
        report.altered.is_empty() && report.missing.is_empty(),
        "verification failed: {} altered, {} missing",
        report.altered.len(),
        report.missing.len()
    );
    Ok(())
}

/// Args for `maj ingest`, bundled into one struct to keep `cmd_ingest`'s own
/// signature within the house 5-positional-parameter limit.
pub(crate) struct IngestArgs {
    pub(crate) source: PathBuf,
    pub(crate) dest: Vec<PathBuf>,
    pub(crate) para: String,
    pub(crate) template: String,
    pub(crate) dedupe: plan::DedupeMode,
    pub(crate) jobs: Option<usize>,
    pub(crate) dry_run: bool,
    pub(crate) resume: Option<String>,
    pub(crate) json: bool,
}

/// Verified copy from `args.source` into every `args.dest` root, PARA-routed
/// and recorded as catalog events.
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
/// # Errors
/// Returns an error if the source isn't a directory, the PARA target can't
/// be resolved or is archived, or any planning/copy/journal/MHL step fails.
/// Also returns an error — after printing the outcome — if the run ends
/// with any failed file, rejected file, or diagnostic.
pub(crate) fn cmd_ingest(app: &mut FsApp, catalog_dir: &Path, args: &IngestArgs) -> Result<()> {
    anyhow::ensure!(
        args.source.is_dir(),
        "source must be a directory: {}",
        args.source.display()
    );

    let projection = app.projection()?;
    let (node_id, kind, name) = resolve_ingest_node(&projection, &args.para)?;
    let known = known_assets_from_projection(&projection);
    let ingest_plan = plan::plan_source(&args.source, &known, args.dedupe)
        .with_context(|| format!("planning ingest from {}", args.source.display()))?;

    let (source_volume_id, source_volume_label) = resolve_volume(&args.source, None);
    let subdir = render_ingest_subdir(kind, &name, &args.template, &source_volume_label)?;

    if args.dry_run {
        print_ingest_plan(&ingest_plan, &subdir, &args.dest, args.json);
        return Ok(());
    }

    let run = run_ingest(
        app,
        catalog_dir,
        &ExecuteIngest {
            plan: &ingest_plan,
            dest: &args.dest,
            subdir: &subdir,
            node_id: &node_id,
            source_volume: (&source_volume_id, &source_volume_label),
            jobs: args.jobs,
            resume: args.resume.as_deref(),
            report: if args.json {
                IngestReport::Json
            } else {
                IngestReport::Text
            },
        },
    )?;
    anyhow::ensure!(
        run.outcome.failed.is_empty()
            && run.outcome.rejected.is_empty()
            && run.outcome.diagnostics.is_empty(),
        "ingest run {}: {} failed, {} rejected, {} diagnostic(s)",
        run.run_id,
        run.outcome.failed.len(),
        run.outcome.rejected.len(),
        run.outcome.diagnostics.len()
    );
    Ok(())
}

/// The verified-copy pipeline shared by `maj ingest` and `maj inbox
/// process`: journal + engine + ASC MHL generations + catalog events +
/// outcome print. The caller has already planned and resolved the PARA
/// node. Returns the outcome (never erroring just because some files
/// failed/were rejected/produced a diagnostic) so callers decide for
/// themselves what a failed file means — `maj ingest` aborts the process,
/// `maj inbox process` fails only that one contribution without aborting
/// its pass.
pub(crate) struct ExecuteIngest<'a> {
    pub plan: &'a plan::IngestPlan,
    pub dest: &'a [PathBuf],
    pub subdir: &'a str,
    pub node_id: &'a str,
    pub source_volume: (&'a str, &'a str),
    pub jobs: Option<usize>,
    pub resume: Option<&'a str>,
    pub report: IngestReport,
}

/// This run's stdout summary. `Silent` is for a caller that runs
/// `run_ingest` more than once per process and prints its own combined
/// summary at the end (`maj inbox process`, once per contribution) — with
/// `--json`, stdout must stay exactly one document, and even in text mode
/// a per-run engine summary is preamble noise once the caller's own report
/// already carries the outcome. Diagnostics reach stderr regardless of
/// which variant is chosen.
#[derive(Debug, Clone, Copy)]
pub(crate) enum IngestReport {
    Text,
    Json,
    Silent,
}

/// One `run_ingest` call's identity plus its engine result — the run id is
/// needed by `cmd_ingest`'s own failure message, which lives outside this
/// function (see `ExecuteIngest`'s doc).
pub(crate) struct IngestRun {
    pub run_id: String,
    pub outcome: engine::Outcome,
}

pub(crate) fn run_ingest(
    app: &mut FsApp,
    catalog_dir: &Path,
    exec: &ExecuteIngest<'_>,
) -> Result<IngestRun> {
    let run_id = exec
        .resume
        .map_or_else(|| ulid::Ulid::generate().to_string(), str::to_string);
    if exec.resume.is_some() {
        check_resume_journal_exists(catalog_dir, &run_id)?;
    }
    // Suppressed for `Silent`: a caller that runs this more than once per
    // process (`maj inbox process`, once per contribution) would otherwise
    // print one resume line per contribution, and `--resume` isn't a flag
    // `maj inbox process` accepts anyway — the advice would be actionable
    // only for `maj ingest`, which uses `Text`/`Json`.
    if !matches!(exec.report, IngestReport::Silent) {
        eprintln!("run {run_id} — resume with: --resume {run_id}");
    }
    let dests = build_dest_specs(exec.dest, exec.subdir);
    let outcome = run_ingest_engine(catalog_dir, &run_id, exec.plan, &dests, exec.jobs)?;
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
    print_ingest_outcome(&run_id, &outcome, &generations, exec.report);
    Ok(IngestRun { run_id, outcome })
}

/// Resolves `para` to an active PARA node's (id, kind, name). Ingest targets
/// must be non-archived even when addressed by a raw node id — unlike
/// `resolve_para_node`'s general allowance for archived nodes (needed so an
/// already-archived node can still be renamed by id), silently copying new
/// content into an archived node would resurrect it as a live destination.
pub(crate) fn resolve_ingest_node(
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
pub(crate) fn known_assets_from_projection(projection: &Projection) -> plan::KnownAssets {
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
pub(crate) fn render_ingest_subdir(
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

fn decision_label(decision: &plan::Decision) -> &'static str {
    match decision {
        plan::Decision::Copy => "COPY",
        plan::Decision::Duplicate { .. } => "DUPLICATE",
        plan::Decision::Rejected { .. } => "REJECTED",
    }
}

/// `--dry-run` output: the plan only — nothing is copied and no journal is
/// written.
fn print_ingest_plan(ingest_plan: &plan::IngestPlan, subdir: &str, dests: &[PathBuf], json: bool) {
    if json {
        let dest_strs: Vec<String> = dests.iter().map(|d| d.display().to_string()).collect();
        println!(
            "{}",
            serde_json::json!({ "plan": ingest_plan, "subdir": subdir, "dests": dest_strs })
        );
        return;
    }
    for file in &ingest_plan.files {
        println!("{} {}", decision_label(&file.decision), file.rel);
    }
    println!("subdir: {subdir}");
    for dest in dests {
        println!("dest: {}", dest.display());
    }
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

fn journal_path_for(catalog_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let paths = majestical_services::state_dir::catalog_paths(catalog_dir)?;
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
fn check_resume_journal_exists(catalog_dir: &Path, run_id: &str) -> Result<()> {
    let journal_path = journal_path_for(catalog_dir, run_id)?;
    anyhow::ensure!(
        journal_path.is_file(),
        "no journal for run '{run_id}' — check the id printed at the start of the original run"
    );
    Ok(())
}

/// Opens (or resumes) the run's journal and executes the copy/verify engine.
/// Always loads the journal before appending, even on a fresh run: loading a
/// journal that doesn't exist yet returns an empty fold, so a fresh run and
/// a `--resume` both flow through the same path rather than branching twice
/// on whether `--resume` was given. Callers resuming an existing run must
/// call `check_resume_journal_exists` first — this function creates the
/// journal file if it's missing, which is correct for a fresh run but would
/// silently paper over a typo'd `--resume` id.
fn run_ingest_engine(
    catalog_dir: &Path,
    run_id: &str,
    ingest_plan: &plan::IngestPlan,
    dests: &[engine::DestSpec],
    jobs: Option<usize>,
) -> Result<engine::Outcome> {
    let journal_path = journal_path_for(catalog_dir, run_id)?;
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
/// destination root). See `cmd_ingest`'s doc comment for the consequence.
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

/// `Silent` suppresses only the stdout summary — diagnostics still go to
/// stderr regardless, since suppressing them too would silently drop detail
/// a caller building its own `Failed` row needs to have surfaced somewhere.
fn print_ingest_outcome(
    run_id: &str,
    outcome: &engine::Outcome,
    generations: &[(PathBuf, mhl::WrittenGeneration)],
    report: IngestReport,
) {
    match report {
        IngestReport::Text => print_ingest_outcome_text(run_id, outcome, generations),
        IngestReport::Json => print_ingest_outcome_json(run_id, outcome, generations),
        IngestReport::Silent => {}
    }
    for note in &outcome.diagnostics {
        eprintln!("diagnostic: {note}");
    }
}

fn print_ingest_outcome_json(
    run_id: &str,
    outcome: &engine::Outcome,
    generations: &[(PathBuf, mhl::WrittenGeneration)],
) {
    let failed: Vec<_> = outcome
        .failed
        .iter()
        .map(|f| serde_json::json!({ "rel": f.rel, "reason": f.reason }))
        .collect();
    let rejected: Vec<_> = outcome
        .rejected
        .iter()
        .map(|f| serde_json::json!({ "rel": f.rel, "reason": f.reason }))
        .collect();
    let generations_json: Vec<_> = generations
        .iter()
        .map(|(root, w)| {
            serde_json::json!({ "root": root.display().to_string(), "generation": w.generation })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "run": run_id,
            "placed": outcome.placed.len(),
            "failed": failed,
            "skipped_duplicates": outcome.skipped_duplicates.len(),
            "rejected": rejected,
            "resumed": outcome.skipped_resumed,
            "generations": generations_json,
        })
    );
}

fn print_ingest_outcome_text(
    run_id: &str,
    outcome: &engine::Outcome,
    generations: &[(PathBuf, mhl::WrittenGeneration)],
) {
    println!(
        "run {run_id}: placed {}, failed {}, skipped_duplicates {}, rejected {}, resumed {}",
        outcome.placed.len(),
        outcome.failed.len(),
        outcome.skipped_duplicates.len(),
        outcome.rejected.len(),
        outcome.skipped_resumed,
    );
    for f in &outcome.failed {
        println!("FAILED {}: {}", f.rel, f.reason);
    }
    for r in &outcome.rejected {
        println!("REJECTED {}: {}", r.rel, r.reason);
    }
    for (root, w) in generations {
        println!("generation {} at {}", w.generation, root.display());
    }
}
