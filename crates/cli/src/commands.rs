//! One cmd_* handler per CLI verb. main.rs owns clap definitions and dispatch;
//! handlers own behavior.
use crate::{BrowseCmd, MetaCmd, ParaCmd, TagCmd};
use anyhow::Result;
use majestical_ingest::{engine, mhl, plan};
use majestical_services::app::FsApp;
use majestical_services::browse::{BrowseListOutcome, BrowseRequest, BrowseVolume};
use majestical_services::iso8601::iso8601_ms;
use majestical_services::volumes::VolumeRow;
use std::path::{Path, PathBuf};

pub(crate) fn cmd_catalog_init(catalog: &Path, machine_id: &str, author: &str) -> Result<()> {
    majestical_services::catalog::init(catalog, machine_id, author)?;
    println!("initialized catalog at {}", catalog.display());
    Ok(())
}

pub(crate) fn cmd_scan(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<()> {
    let outcome = majestical_services::scan::scan(app, dir, volume)?;
    crate::print_notices(&outcome.notices);
    println!("scanned: {} assets", outcome.assets);
    Ok(())
}

pub(crate) fn cmd_tag(app: &mut FsApp, cmd: TagCmd) -> Result<()> {
    // Both verbs attach their sink to an `Err`, so the carrier is split
    // here — otherwise a failure renders the carrier's own label instead
    // of the notices and the real message.
    match cmd {
        TagCmd::Add { asset, tag } => {
            crate::surface_err_notices(majestical_services::tags::tag_add(app, &asset, &tag))?;
        }
        TagCmd::Rm { asset, tag } => {
            crate::surface_err_notices(majestical_services::tags::tag_rm(app, &asset, &tag))?;
        }
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
            crate::print_notices(&outcome.notices);
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
    crate::print_notices(&outcome.notices);
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
    crate::print_notices(&outcome.notices);
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
///
/// A multi-root run that fails partway through still reports the roots
/// already moved (or classified) BEFORE the failing one on stdout — real
/// filesystem mutations that happened must never go unreported just
/// because a later root errored — via
/// [`majestical_services::error::ServiceError::ParaArchivePartial`].
fn cmd_para_archive(app: &mut FsApp, node: &str, roots: &[PathBuf], dry_run: bool) -> Result<()> {
    use majestical_services::error::ServiceError;

    match majestical_services::para::archive(app, node, roots, dry_run) {
        Ok(outcome) => {
            crate::print_notices(&outcome.notices);
            print_archive_outcome(&outcome, dry_run);
            Ok(())
        }
        Err(ServiceError::ParaArchivePartial { moves, source }) => {
            print_archive_moves(&moves);
            Err(source)
        }
        Err(other) => Err(other.into()),
    }
}

/// Renders a completed `para archive` call's outcome: the two whole-run
/// messages when no `--root`s were given (`outcome.moves` is empty), or
/// each root's move line otherwise.
fn print_archive_outcome(outcome: &majestical_services::para::ArchiveOutcome, dry_run: bool) {
    if outcome.moves.is_empty() {
        if dry_run {
            println!("would archive (dry run; no --root given; no directories to move)");
        } else {
            println!("ok (no --root given; no directories moved)");
        }
        return;
    }
    print_archive_moves(&outcome.moves);
}

/// Renders each root's move line — shared by the success path and the
/// partial-failure path (the roots completed before a later root's failure
/// still get reported here).
fn print_archive_moves(moves: &[majestical_services::para::ArchiveMove]) {
    use majestical_services::para::MoveStatus;

    for mv in moves {
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
}

/// `maj browse tree`/`maj browse list`: dispatches to the two `browse`
/// service verbs and renders their outcome. Only the DISPATCH shape follows
/// `cmd_para`'s pattern (a subcommand `match` handing each arm to its own
/// `cmd_browse_*` helper) — that's a structural borrow, not a rendering
/// one: `cmd_para`'s `--json` output is hand-built (see `cmd_para_list`),
/// while browse's is not. See `cmd_browse_tree`'s doc for that policy.
pub(crate) fn cmd_browse(app: &mut FsApp, catalog_dir: &Path, cmd: BrowseCmd) -> Result<()> {
    match cmd {
        BrowseCmd::Tree { json } => cmd_browse_tree(app, catalog_dir, json),
        BrowseCmd::List {
            volume,
            path,
            no_flatten,
            sort,
            kind,
            limit,
            offset,
            json,
        } => {
            let req = BrowseRequest {
                volume,
                path,
                flatten: !no_flatten,
                sort,
                kind,
                limit,
                offset,
            };
            cmd_browse_list(app, catalog_dir, &req, json)
        }
    }
}

/// `browse`'s `--json` output is the service outcome struct serialized
/// AS-IS (`serde_json::to_string(&outcome)`), not a hand-built
/// `serde_json::json!` payload the way `cmd_volumes_list`/`cmd_para_list`
/// render theirs. This is deliberate, not a gap to "harmonize" later:
/// `volumes`/`para`'s hand-built payloads predate the services extraction,
/// from when the CLI's rendering WAS the wire contract. Browse has no such
/// history — its outcome structs (`BrowseTreeOutcome`/`BrowseListOutcome`)
/// are already the wire contract for every other head: the GUI reads them
/// directly, and the MCP `browse_tree`/`browse_assets` tools
/// (`mcp_cmd/read_tools.rs`) serialize them verbatim as `structuredContent`
/// per this server's own wire-contract doc (`mcp_cmd/mod.rs`). Reshaping
/// the CLI's `--json` here would fork that contract between heads for no
/// benefit — every consumer already gets, and expects, the struct as
/// `browse.rs` defines it.
///
/// One consequence of "as-is": in `--json` mode, `outcome.notices` appears
/// TWICE — once inside the JSON payload itself (dropped only when empty,
/// via `#[serde(skip_serializing_if)]`), and again on stderr from
/// `print_notices` below. That's not a bug either: every read verb in this
/// file prints notices to stderr unconditionally, `--json` or not (the
/// house convention every other `cmd_*_list` here follows), and an
/// outcome's own `notices` field is part of the struct being serialized
/// as-is per the policy above. Suppressing the payload's copy — or moving
/// `print_notices` inside the `else` branch to suppress the stderr copy —
/// would each break one of those two rules to "fix" the other.
/// `browse_list_json_offline_notice_appears_on_stderr_and_in_payload` in
/// `browse_smoke.rs` pins both appearances so neither regresses quietly.
fn cmd_browse_tree(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::browse::browse_tree(app, catalog_dir)?;
    crate::print_notices(&outcome.notices);
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        print_browse_tree(&outcome.volumes);
    }
    Ok(())
}

/// Renders every volume's folder tree: a header line per volume (label, id,
/// online/offline), then one indented line per non-root folder — full
/// `/`-separated path (not just the leaf name, so a folder's line is a
/// stable, greppable substring regardless of nesting) plus its recursive
/// asset count. The root folder ("") is implied by the header line, so it's
/// skipped here. Folders arrive from `browse_tree` already sorted
/// lexicographically by path (a `BTreeMap` internally), which happens to
/// also keep every folder after its parent.
fn print_browse_tree(volumes: &[BrowseVolume]) {
    if volumes.is_empty() {
        println!("no volumes");
        return;
    }
    for v in volumes {
        let online = if v.online { "online" } else { "offline" };
        println!("{} ({}) [{online}]", v.label, v.id);
        for folder in &v.folders {
            if folder.path.is_empty() {
                continue;
            }
            let depth = folder.path.matches('/').count();
            let indent = "  ".repeat(depth + 1);
            println!("{indent}{}  {}", folder.path, folder.recursive_count);
        }
    }
}

/// Same `--json`-is-the-outcome-struct-verbatim policy as `cmd_browse_tree`
/// — see that function's doc for why, and for the double-notice
/// consequence in `--json` mode.
fn cmd_browse_list(app: &FsApp, catalog_dir: &Path, req: &BrowseRequest, json: bool) -> Result<()> {
    let outcome = majestical_services::browse::browse_list(app, catalog_dir, req)?;
    crate::print_notices(&outcome.notices);
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        print_browse_list(&outcome);
    }
    Ok(())
}

/// Renders a `browse list` outcome: a count line ("N items across M
/// folders"), then one row per result — asset id, name, kind, size, and
/// volume labels with an online/offline dot — following
/// `search.rs::print_search_results_text`'s row style. The id leads each
/// row (not just name/kind/size) so a browsed row alone gives an agent or
/// user everything `maj meta`/`maj tag`/`maj para` need, without a
/// re-resolving search. `kind`/`size` render "-" when absent (never
/// expected for a browse row — see `browse_list`'s own doc — but `SearchHit`
/// is the same row type `search` uses, where they can be `None`); both use
/// the same placeholder rather than two different ones.
fn print_browse_list(outcome: &BrowseListOutcome) {
    println!(
        "{} items across {} folders",
        outcome.count, outcome.folder_count
    );
    for hit in &outcome.results {
        let volumes: Vec<String> = hit
            .volumes
            .iter()
            .map(|v| {
                let dot = if v.online { '\u{25cf}' } else { '\u{25cb}' };
                format!("{}{dot}", v.label)
            })
            .collect();
        let kind = hit.kind.as_deref().unwrap_or("-");
        let size = hit.size.map_or_else(|| "-".to_string(), |s| s.to_string());
        println!(
            "{}  {}  {kind}  {size}  [{}]",
            hit.asset,
            hit.name,
            volumes.join(",")
        );
    }
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

    let planned = majestical_services::ingest::plan(
        app,
        &args.source,
        &args.para,
        &args.template,
        args.dedupe,
    )?;

    crate::print_notices(&planned.notices);

    if args.dry_run {
        print_ingest_plan(&planned.plan, &planned.subdir, &args.dest, args.json);
        return Ok(());
    }

    let run = majestical_services::ingest::run_ingest(
        app,
        catalog_dir,
        &majestical_services::ingest::ExecuteIngest {
            plan: &planned.plan,
            dest: &args.dest,
            subdir: &planned.subdir,
            node_id: &planned.node_id,
            source_volume: (&planned.source_volume_id, &planned.source_volume_label),
            jobs: args.jobs,
            resume: args.resume.as_deref(),
        },
        &mut |line: &str| eprintln!("{line}"),
    )?;
    crate::print_notices(&run.notices);
    print_ingest_outcome(&run.run_id, &run.outcome, &run.generations, args.json);
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

/// Prints `maj ingest`'s outcome: text or JSON depending on `json`, then
/// every diagnostic — unconditionally, since suppressing them would
/// silently drop detail a caller building its own `Failed` row needs to
/// have surfaced somewhere. Called explicitly right after `run_ingest`
/// returns — unlike before the extraction, this no longer happens inside
/// `run_ingest` itself, since `majestical_services` never prints. `maj
/// inbox process`'s own per-contribution ingest runs print their
/// diagnostics the same way, but from inside
/// `majestical_services::inbox::report_failure_detail` — that path never
/// prints a stdout summary at all (its own pass-level report already
/// carries the outcome), so it has no `Text`/`Json` choice to make here.
fn print_ingest_outcome(
    run_id: &str,
    outcome: &engine::Outcome,
    generations: &[(PathBuf, mhl::WrittenGeneration)],
    json: bool,
) {
    if json {
        print_ingest_outcome_json(run_id, outcome, generations);
    } else {
        print_ingest_outcome_text(run_id, outcome, generations);
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
