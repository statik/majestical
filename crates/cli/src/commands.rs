//! One cmd_* handler per CLI verb. main.rs owns clap definitions and dispatch;
//! handlers own behavior.
use crate::app::{FsApp, physical_now_ms, warn_skipped_corrupt_lines};
use crate::iso8601::iso8601_ms;
use crate::volume_identity;
use crate::{MetaCmd, ParaCmd, TagCmd};
use anyhow::{Context, Result};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::MAX_DRIFT_MS;
use majestical_core::event::{AssetId, Op, ParaKind, VerifyOutcome};
use majestical_core::projection::Projection;
use majestical_ingest::{engine, journal, mhl, plan, template};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Opens the sqlite catalog from the per-machine local state dir (see
/// `state_dir`), applying only the events past its last-saved cursor (or
/// rebuilding from scratch if there's no usable saved state). Shared by
/// every read path that needs an ad hoc sqlite view — `search`,
/// `volumes list`, and `para list` — so the open+sync pair lives in exactly
/// one place.
pub(crate) fn open_catalog(app: &FsApp, catalog_dir: &Path) -> Result<(SqliteCatalog, Projection)> {
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let mut skipped = 0usize;
    let (db, projection, _mode) =
        SqliteCatalog::open_synced(&paths.db_path, app.log(), &mut |_line| skipped += 1)
            .context("opening sqlite catalog")?;
    warn_skipped_corrupt_lines(skipped, catalog_dir);
    Ok((db, projection))
}

pub(crate) fn cmd_catalog_init(catalog: &Path, machine_id: &str, author: &str) -> Result<()> {
    FsApp::init(catalog, machine_id, author)?;
    println!("initialized catalog at {}", catalog.display());
    Ok(())
}

/// Resolves the (id, label) pair a scan should tag its events with. An
/// explicit `--volume` is used as both id and label — an override that
/// keeps e2e tests deterministic. Omitted, the volume's physical identity
/// is auto-detected (see `volume_identity`).
pub(crate) fn resolve_volume(dir: &Path, volume: Option<String>) -> (String, String) {
    if let Some(v) = volume {
        return (v.clone(), v);
    }
    let identity = volume_identity::resolve(dir);
    (identity.id, identity.label)
}

/// A file's real modification time, in milliseconds since the Unix epoch —
/// `0` (meaning "unknown") if the platform can't report it or it predates
/// the epoch, rather than failing the whole scan/ingest over one file's
/// clock oddity.
pub(crate) fn mtime_ms_of(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

pub(crate) fn cmd_scan(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<()> {
    let auto_detect = volume.is_none();
    let (volume_id, volume_label) = resolve_volume(dir, volume);
    let mut ops = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.context("walking scan directory")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", entry.path().display()))?;
        let size = metadata.len();
        let file = std::fs::File::open(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        // Stream the hash rather than loading the whole file: media
        // assets can be multi-gigabyte, so a `Vec<u8>` per file would
        // blow up memory on a scan of a card full of video.
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        let mut reader = std::io::BufReader::new(file);
        let mut buf = vec![0u8; 64 * 1024].into_boxed_slice();
        loop {
            let n = reader
                .read(&mut buf)
                .with_context(|| format!("reading {}", entry.path().display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let hash = hasher.digest128();
        // Phase 1: lossy UTF-8 conversion of the relative path. JSON
        // events force UTF-8 anyway, so a non-UTF-8 path can't round
        // trip through the log yet; revisit once ingest needs to
        // preserve exact bytes.
        let scan_rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        // An explicit `--volume` override has no real mount to re-base
        // against (it's a synthetic id kept for e2e-test determinism), so
        // its instances stay scan-dir-relative, as before. An auto-detected
        // volume gets a path relative to the volume's actual root, so a
        // later indexer run can re-find the bytes regardless of which
        // subdirectory was scanned.
        let rel = if auto_detect {
            let abs = entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.path().to_path_buf());
            let mount = volume_identity::mount_point_of(&abs);
            abs.strip_prefix(&mount).map_or_else(
                |_| scan_rel.clone(),
                |p| p.to_string_lossy().replace('\\', "/"),
            )
        } else {
            scan_rel
        };
        ops.push(Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: volume_id.clone(),
            path: rel,
            size,
            mtime_ms: mtime_ms_of(&metadata),
        });
    }
    let n = ops.len();
    ops.insert(
        0,
        Op::VolumeSeen {
            volume: volume_id,
            label: volume_label,
        },
    );
    app.emit(ops)?;
    println!("scanned: {n} assets");
    Ok(())
}

/// Both `tag add` and `meta set` write metadata about an asset that must
/// already have a physical observation on record — otherwise a typo'd id
/// silently creates a phantom catalog entry that `search` and `scan` can
/// never produce, and would look scanned when it never was.
pub(crate) fn ensure_asset_known(projection: &Projection, asset: &AssetId) -> Result<()> {
    anyhow::ensure!(
        projection.has_instances(asset),
        "unknown asset {} — scan its volume first, or check `maj search`",
        asset.0
    );
    Ok(())
}

pub(crate) fn cmd_tag(app: &mut FsApp, cmd: TagCmd) -> Result<()> {
    match cmd {
        TagCmd::Add { asset, tag } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            ensure_asset_known(&p, &asset)?;
            app.emit(vec![Op::TagAdd { asset, tag }])?;
        }
        TagCmd::Rm { asset, tag } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            let observed = p.tag_add_ids(&asset, &tag);
            anyhow::ensure!(
                !observed.is_empty(),
                "tag '{tag}' is not set on {} — nothing to remove",
                asset.0
            );
            app.emit(vec![Op::TagRemove {
                asset,
                tag,
                observed,
            }])?;
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
            let p = app.projection()?;
            let asset = AssetId(asset);
            ensure_asset_known(&p, &asset)?;
            app.emit(vec![Op::FieldSet {
                asset,
                field,
                value,
            }])?;
            println!("ok");
        }
        MetaCmd::Get { asset, field, json } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            print_meta_get(&p, &asset, field.as_deref(), json);
        }
    }
    Ok(())
}

/// Prints either a single field's value or every field set on `asset`.
/// A single missing field prints nothing (an empty line in text mode, `null`
/// in JSON) rather than erroring — mirroring `search`'s "zero hits" style
/// rather than treating "not set yet" as a failure.
pub(crate) fn print_meta_get(
    projection: &Projection,
    asset: &AssetId,
    field: Option<&str>,
    json: bool,
) {
    if let Some(field) = field {
        let value = projection.field(asset, field);
        if json {
            println!("{}", serde_json::json!({ field: value }));
        } else if let Some(value) = value {
            println!("{value}");
        } else {
            println!();
        }
        return;
    }
    let fields: Vec<(&str, &str)> = projection.fields(asset).collect();
    if json {
        let obj: serde_json::Map<String, serde_json::Value> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect();
        println!("{}", serde_json::Value::Object(obj));
    } else {
        for (k, v) in fields {
            println!("{k}\t{v}");
        }
    }
}

/// Cheap phase-2 "is this volume mounted right now" heuristic, not true
/// device enumeration. `label:`-id volumes are considered online if
/// `/Volumes/<label>` exists (or the label is the root volume's, which is
/// always present). `uuid:`-id volumes are considered online only if a
/// mount at `/Volumes/<label>` exists *and* resolving its identity still
/// yields the same id — so a same-named but different card reads offline.
/// False negative: a volume mounted somewhere other than `/Volumes` reads
/// offline even when present.
pub(crate) fn volume_is_online(id: &str, label: &str) -> bool {
    if label == volume_identity::ROOT_LABEL {
        return true;
    }
    let candidate = PathBuf::from("/Volumes").join(label);
    if !candidate.exists() {
        return false;
    }
    if id.starts_with("uuid:") {
        return volume_identity::resolve(&candidate).id == id;
    }
    true
}

pub(crate) fn cmd_volumes_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let (db, _projection) = open_catalog(app, catalog_dir)?;
    let volumes = db.volumes().context("querying volumes")?;
    let counts: HashMap<String, u64> = db
        .volume_asset_counts()
        .context("querying volume asset counts")?
        .into_iter()
        .collect();
    // A stored last-seen wall time past this ceiling could only have come
    // from a clock more than MAX_DRIFT_MS ahead of physical now — the HLC
    // clamp bounds the *local* clock's adoption of such a timestamp, but
    // doesn't touch what's already durable in the event log, so a poisoned
    // VolumeSeen can still win the LWW max and display forever unflagged.
    let suspect_ceiling = physical_now_ms().saturating_add(MAX_DRIFT_MS);

    if json {
        let rows: Vec<_> = volumes
            .iter()
            .map(|(id, label, last_seen_ms)| {
                serde_json::json!({
                    "id": id,
                    "label": label,
                    "last_seen": iso8601_ms(*last_seen_ms),
                    "online": volume_is_online(id, label),
                    "asset_count": counts.get(id).copied().unwrap_or(0),
                    "clock_suspect": *last_seen_ms > suspect_ceiling,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "volumes": rows }));
    } else {
        print_volumes_table(&volumes, &counts, suspect_ceiling);
    }
    Ok(())
}

/// Renders the human-readable volumes table with column widths sized to
/// the widest cell in each column (header included) — a fixed width breaks
/// alignment once an auto-detected `uuid:` id (41 chars) or a
/// "(clock suspect)"-annotated last-seen cell appears.
pub(crate) fn print_volumes_table(
    volumes: &[(String, String, u64)],
    counts: &HashMap<String, u64>,
    suspect_ceiling: u64,
) {
    let rows: Vec<(String, String, String, &'static str, u64)> = volumes
        .iter()
        .map(|(id, label, last_seen_ms)| {
            let mut last_seen = iso8601_ms(*last_seen_ms);
            if *last_seen_ms > suspect_ceiling {
                last_seen.push_str(" (clock suspect)");
            }
            let online = if volume_is_online(id, label) {
                "online"
            } else {
                "offline"
            };
            let count = counts.get(id).copied().unwrap_or(0);
            (id.clone(), label.clone(), last_seen, online, count)
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

pub(crate) fn parse_kind(kind: &str) -> Result<ParaKind> {
    match kind {
        "project" => Ok(ParaKind::Project),
        "area" => Ok(ParaKind::Area),
        "resource" => Ok(ParaKind::Resource),
        "archive" => Ok(ParaKind::Archive),
        other => {
            anyhow::bail!("unknown PARA kind '{other}' — one of: project, area, resource, archive")
        }
    }
}

/// Resolves `<kind>/<name>` or a raw node ULID against non-archived nodes.
/// The non-archived restriction applies only to the `<kind>/<name>` form; a
/// raw node id resolves an archived node too (intentional — once a node is
/// archived, its id is the only way left to address it).
pub(crate) fn resolve_para_node(projection: &Projection, reference: &str) -> Result<String> {
    if projection.para_node(reference).is_some() {
        return Ok(reference.to_string());
    }
    let Some((kind_str, name)) = reference.split_once('/') else {
        anyhow::bail!(
            "unknown PARA node '{reference}' — use <kind>/<name> or a node id from `maj para list`"
        );
    };
    let kind = parse_kind(kind_str)?;
    let matches: Vec<&String> = projection
        .para_nodes()
        .filter(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name))
        .map(|(id, _)| id)
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no active PARA node '{reference}' — see `maj para list`"),
        [id] => Ok((*id).clone()),
        many => anyhow::bail!(
            "'{reference}' is ambiguous (concurrent creates); use a node id: {}",
            many.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
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

/// Creates a node, rejecting a duplicate non-archived `(kind, name)` — two
/// active nodes with the same reference would be indistinguishable to
/// `resolve_para_node`.
fn cmd_para_add(app: &mut FsApp, kind_str: &str, name: &str) -> Result<()> {
    let kind = parse_kind(kind_str)?;
    let projection = app.projection()?;
    let duplicate = projection
        .para_nodes()
        .any(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name));
    anyhow::ensure!(
        !duplicate,
        "a PARA node '{kind_str}/{name}' already exists — see `maj para list`"
    );
    let node_id = ulid::Ulid::generate().to_string();
    app.emit(vec![Op::ParaNodeCreate {
        node: node_id.clone(),
        kind,
        name: name.to_string(),
    }])?;
    println!("{node_id}");
    Ok(())
}

fn cmd_para_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let (db, _projection) = open_catalog(app, catalog_dir)?;
    let nodes = db.para_nodes().context("querying para nodes")?;
    if json {
        let rows: Vec<_> = nodes
            .iter()
            .map(|(id, kind, name, archived)| {
                serde_json::json!({
                    "id": id, "kind": kind, "name": name, "archived": archived
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "nodes": rows }));
    } else {
        print_para_table(&nodes);
    }
    Ok(())
}

/// Renders the human-readable para-nodes table, following
/// `print_volumes_table`'s width-sizing pattern.
fn print_para_table(nodes: &[(String, String, String, bool)]) {
    let id_w = nodes.iter().map(|r| r.0.len()).max().unwrap_or(0).max(2);
    let kind_w = nodes.iter().map(|r| r.1.len()).max().unwrap_or(0).max(4);
    let name_w = nodes.iter().map(|r| r.2.len()).max().unwrap_or(0).max(4);
    println!(
        "{:<id_w$} {:<kind_w$} {:<name_w$} ARCHIVED",
        "ID", "KIND", "NAME"
    );
    for (id, kind, name, archived) in nodes {
        println!("{id:<id_w$} {kind:<kind_w$} {name:<name_w$} {archived}");
    }
}

fn cmd_para_rename(app: &mut FsApp, node: &str, name: &str) -> Result<()> {
    let projection = app.projection()?;
    let node_id = resolve_para_node(&projection, node)?;
    app.emit(vec![Op::ParaNodeRename {
        node: node_id,
        name: name.to_string(),
    }])?;
    println!("ok");
    Ok(())
}

/// Archives a node. With `--root`s, each root's materialized directory
/// (`<root>/<KindDir>/<name>`) is moved to `<root>/Archives/<name>` before
/// the archive event is emitted; with no roots, only the event is emitted
/// (skipped in `--dry-run`) and a note is printed that nothing was moved on
/// disk.
///
/// If a move fails partway through a multi-root run, the roots already
/// moved stay moved and the archive event is NOT emitted. A root whose
/// source is gone and target already exists is treated as already archived
/// and skipped rather than re-erroring — so re-running the exact same
/// command converges instead of failing forever on the root that succeeded
/// last time.
fn cmd_para_archive(app: &mut FsApp, node: &str, roots: &[PathBuf], dry_run: bool) -> Result<()> {
    let projection = app.projection()?;
    let node_id = resolve_para_node(&projection, node)?;
    let state = projection
        .para_node(&node_id)
        .context("resolved node vanished from the projection")?;
    let Some(kind) = state.kind() else {
        anyhow::bail!("PARA node {node_id} has no kind recorded — its create event may be missing");
    };
    let Some(name) = state.name() else {
        anyhow::bail!("PARA node {node_id} has no name recorded — its create event may be missing");
    };

    if roots.is_empty() {
        if dry_run {
            println!("would archive (dry run; no --root given; no directories to move)");
        } else {
            app.emit(vec![Op::ParaNodeArchive { node: node_id }])?;
            println!("ok (no --root given; no directories moved)");
        }
        return Ok(());
    }
    // A node of kind `archive` already materializes under `Archives/` (its
    // own `dir_name()`), so source and target would be the same path for
    // every root — reject up front rather than reporting a no-op "move" in
    // dry-run and a target-already-exists error in the real run.
    anyhow::ensure!(
        kind != ParaKind::Archive,
        "node of kind archive is already under Archives/ — nothing to move"
    );

    for root in roots {
        let source = root.join(kind.dir_name()).join(name);
        let archives_dir = root.join("Archives");
        let target = archives_dir.join(name);
        // Source gone, target present: an earlier partial run already moved
        // this root. Skip rather than erroring, so a plain re-run of the
        // same command converges instead of failing on the root that
        // already succeeded.
        if !source.is_dir() && target.is_dir() {
            println!("already archived at {} — skipping", target.display());
            continue;
        }
        if dry_run {
            println!("would move {} -> {}", source.display(), target.display());
            continue;
        }
        anyhow::ensure!(
            source.is_dir(),
            "source directory {} does not exist — nothing to archive",
            source.display()
        );
        anyhow::ensure!(
            !target.exists(),
            "archive target {} already exists",
            target.display()
        );
        std::fs::create_dir_all(&archives_dir)
            .with_context(|| format!("creating {}", archives_dir.display()))?;
        std::fs::rename(&source, &target)
            .with_context(|| format!("moving {} to {}", source.display(), target.display()))?;
        println!("moved {} -> {}", source.display(), target.display());
    }

    if !dry_run {
        app.emit(vec![Op::ParaNodeArchive { node: node_id }])?;
    }
    Ok(())
}

/// Re-verifies `dir` against its own ASC MHL history and appends a new
/// generation recording the result. Needs no catalog — the history lives
/// entirely under `dir/ascmhl`.
pub(crate) fn cmd_verify(dir: &Path, json: bool) -> Result<()> {
    let hashdate = iso8601_ms(physical_now_ms());
    let report =
        mhl::verify_dir(dir, &hashdate).with_context(|| format!("verifying {}", dir.display()))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "verified": report.verified,
                "altered": report.altered,
                "missing": report.missing,
                "new": report.new_files,
                "generation": report.written.generation,
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
            report.written.generation
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
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
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
