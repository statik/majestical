//! `maj`: agent-first CLI over the catalog core. JSON-first output.
mod iso8601;
mod volume_identity;

use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use iso8601::iso8601_ms;
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::{Clock, HlcClock, MAX_DRIFT_MS, MachineId, ObserveOutcome};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::ports::EventLog;
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Current wall-clock time in milliseconds since the Unix epoch, shared by
/// `SystemClock` and the clamp-warning's "how far ahead" calculation below.
fn physical_now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

struct SystemClock;
impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        physical_now_ms()
    }
}

#[derive(Parser)]
#[command(name = "maj", version, about = "Majestical media catalog")]
struct Cli {
    /// Catalog directory (env `MAJ_CATALOG`).
    #[arg(long, env = "MAJ_CATALOG", help = "Catalog directory")]
    catalog: PathBuf,
    /// Stable machine identity (env `MAJ_MACHINE_ID`).
    #[arg(long, env = "MAJ_MACHINE_ID", help = "Stable machine identity")]
    machine_id: String,
    /// Human/service identity recorded on emitted events (env `MAJ_AUTHOR`).
    /// Defaults to the machine id when omitted.
    #[arg(long, env = "MAJ_AUTHOR", help = "Author identity for emitted events")]
    author: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the catalog directory.
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
    /// Hash every file under a directory into the catalog as `AssetSeen` events.
    #[command(about = "Hash every file under a directory into the catalog as AssetSeen events")]
    Scan {
        dir: PathBuf,
        /// Stable volume id and label. Omit to auto-detect (macOS: the
        /// volume's `VolumeUUID`; elsewhere: the mount point's name).
        #[arg(long)]
        volume: Option<String>,
    },
    /// Add or remove folksonomy tags.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Search the catalog projection.
    #[command(group(
        ArgGroup::new("search_by").args(["name", "tag"]).required(true).multiple(false)
    ))]
    Search {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List every volume the catalog has ever seen.
    Volumes {
        #[command(subcommand)]
        cmd: VolumesCmd,
    },
}

#[derive(Subcommand)]
enum VolumesCmd {
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CatalogCmd {
    Init,
}

#[derive(Subcommand)]
enum TagCmd {
    Add { asset: String, tag: String },
    Rm { asset: String, tag: String },
}

struct App<L> {
    log: L,
    hlc: HlcClock,
    author: String,
    catalog_root: PathBuf,
}

/// The CLI's concrete adapter wiring: a real, filesystem-backed event log.
/// The `App<L>` split exists so tests and future adapters can swap it out.
type FsApp = App<FileEventLog>;

impl FsApp {
    /// Opens an already-initialized catalog. Errors rather than creating one
    /// implicitly — a missing catalog is almost always a typo'd path, and
    /// silently creating an empty one there would hide that.
    fn open(root: &Path, machine: &str, author: &str) -> Result<Self> {
        anyhow::ensure!(
            root.join("events").is_dir(),
            "no catalog at {} — run `maj catalog init` first",
            root.display()
        );
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::open(root, &machine)
            .with_context(|| format!("opening catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine, Box::new(SystemClock)),
            author: author.to_string(),
            catalog_root: root.to_path_buf(),
        })
    }

    /// Creates a fresh catalog at `root` (`maj catalog init`).
    fn init(root: &Path, machine: &str, author: &str) -> Result<Self> {
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::init(root, &machine)
            .with_context(|| format!("initializing catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine, Box::new(SystemClock)),
            author: author.to_string(),
            catalog_root: root.to_path_buf(),
        })
    }
}

impl<L: EventLog> App<L> {
    /// Loads every event currently in the log. Each call re-reads the log
    /// from disk; per-process caching arrives with the adapter refactor.
    ///
    /// Corrupt lines are skipped rather than failing the read; a warning is
    /// printed to stderr so the user knows metadata may be missing, without
    /// polluting stdout (which carries this process's data output).
    fn events(&self) -> Result<Vec<Event>> {
        let mut skipped = 0usize;
        let events = self
            .log
            .read_all_reporting(&mut |_line| skipped += 1)
            .context("reading event log")?;
        if skipped > 0 {
            eprintln!(
                "warning: skipped {skipped} corrupt event log line(s) in {}/events — a torn write or damaged transport; affected metadata may be missing",
                self.catalog_root.display()
            );
        }
        Ok(events)
    }

    fn projection_of(events: &[Event]) -> Projection {
        let mut p = Projection::default();
        for e in events {
            p.apply(e);
        }
        p
    }

    fn projection(&self) -> Result<Projection> {
        Ok(Self::projection_of(&self.events()?))
    }

    fn emit(&mut self, ops: Vec<Op>) -> Result<Vec<Event>> {
        // Fold existing log into the clock so new events order after it.
        // A peer's clock may be wrong; count how many events got clamped
        // rather than adopted, and track the worst offender, so the
        // operator can be warned once below.
        let mut clamped = 0usize;
        let mut worst_remote_wall_ms = 0u64;
        for e in self.events()? {
            if let ObserveOutcome::ClampedFuture { remote_wall_ms } = self.hlc.observe(&e.hlc) {
                clamped += 1;
                worst_remote_wall_ms = worst_remote_wall_ms.max(remote_wall_ms);
            }
        }
        if clamped > 0 {
            let days_ahead =
                worst_remote_wall_ms.saturating_sub(physical_now_ms()) / (24 * 60 * 60 * 1000);
            eprintln!(
                "warning: {clamped} event(s) carry timestamps more than 24h in the future (worst: ~{days_ahead}d ahead) — a peer's clock may be wrong; ordering was clamped locally"
            );
        }
        // ulid 3.x generates through a monotonic Generator; on same-millisecond
        // random-part overflow (astronomically rare), fall back to a fresh
        // random id rather than failing the whole emit.
        let mut ulid_gen = ulid::Generator::new();
        let events: Vec<Event> = ops
            .into_iter()
            .map(|op| {
                let hlc = self.hlc.now();
                let id = match ulid_gen.generate() {
                    Ok(id) => id,
                    Err(overflow) => overflow.commit_overflow_random(),
                };
                Event {
                    id: EventId(id),
                    hlc,
                    author: self.author.clone(),
                    op,
                }
            })
            .collect();
        self.log.append(&events).context("appending events")?;
        Ok(events)
    }
}

fn cmd_catalog_init(cli: &Cli, author: &str) -> Result<()> {
    FsApp::init(&cli.catalog, &cli.machine_id, author)?;
    println!("initialized catalog at {}", cli.catalog.display());
    Ok(())
}

/// Resolves the (id, label) pair a scan should tag its events with. An
/// explicit `--volume` is used as both id and label — an override that
/// keeps e2e tests deterministic. Omitted, the volume's physical identity
/// is auto-detected (see `volume_identity`).
fn resolve_volume(dir: &Path, volume: Option<String>) -> (String, String) {
    if let Some(v) = volume {
        return (v.clone(), v);
    }
    let identity = volume_identity::resolve(dir);
    (identity.id, identity.label)
}

fn cmd_scan(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<()> {
    let (volume_id, volume_label) = resolve_volume(dir, volume);
    let mut ops = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.context("walking scan directory")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let size = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", entry.path().display()))?
            .len();
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
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        ops.push(Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: volume_id.clone(),
            path: rel,
            size,
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

/// `tag add` writes metadata about an asset that must already have a
/// physical observation on record — otherwise a typo'd id silently creates
/// a phantom catalog entry that `search` and `scan` can never produce, and
/// would look scanned when it never was.
fn ensure_asset_known(projection: &Projection, asset: &AssetId) -> Result<()> {
    anyhow::ensure!(
        projection.has_instances(asset),
        "unknown asset {} — scan its volume first, or check `maj search`",
        asset.0
    );
    Ok(())
}

fn cmd_tag(app: &mut FsApp, cmd: TagCmd) -> Result<()> {
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

fn cmd_search(
    app: &FsApp,
    catalog_dir: &Path,
    name: Option<String>,
    tag: Option<String>,
    json: bool,
) -> Result<()> {
    let projection = app.projection()?;
    let db_path = catalog_dir.join("catalog.db");
    let mut db = SqliteCatalog::open(&db_path).context("opening sqlite catalog")?;
    db.rebuild(&projection)
        .context("rebuilding sqlite projection")?;
    let ids = match (name, tag) {
        (Some(n), None) => db.search_by_name(&n)?,
        (None, Some(t)) => db.search_by_tag(&t)?,
        // The `search_by` ArgGroup (required, mutually exclusive)
        // guarantees clap rejects these combinations before `main`
        // ever runs, so this arm can't be reached.
        (Some(_), Some(_)) | (None, None) => {
            unreachable!("clap's search_by ArgGroup allows exactly one of these")
        }
    };
    if json {
        let results: Vec<_> = ids
            .iter()
            .map(|a| serde_json::json!({ "asset": a.0 }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "count": ids.len(), "results": results })
        );
    } else {
        for a in &ids {
            println!("{}", a.0);
        }
        println!("{} results", ids.len());
    }
    Ok(())
}

/// Cheap phase-2 "is this volume mounted right now" heuristic, not true
/// device enumeration. `label:`-id volumes are considered online if
/// `/Volumes/<label>` exists (or the label is the root volume's, which is
/// always present). `uuid:`-id volumes are considered online only if a
/// mount at `/Volumes/<label>` exists *and* resolving its identity still
/// yields the same id — so a same-named but different card reads offline.
/// False negative: a volume mounted somewhere other than `/Volumes` reads
/// offline even when present.
fn volume_is_online(id: &str, label: &str) -> bool {
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

fn cmd_volumes_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let projection = app.projection()?;
    let db_path = catalog_dir.join("catalog.db");
    let mut db = SqliteCatalog::open(&db_path).context("opening sqlite catalog")?;
    db.rebuild(&projection)
        .context("rebuilding sqlite projection")?;
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
fn print_volumes_table(
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let author = cli.author.clone().unwrap_or_else(|| cli.machine_id.clone());
    match cli.cmd {
        Cmd::Catalog {
            cmd: CatalogCmd::Init,
        } => cmd_catalog_init(&cli, &author)?,
        Cmd::Scan { dir, volume } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            cmd_scan(&mut app, &dir, volume)?;
        }
        Cmd::Tag { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            cmd_tag(&mut app, cmd)?;
        }
        Cmd::Search { name, tag, json } => {
            let app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            cmd_search(&app, &cli.catalog, name, tag, json)?;
        }
        Cmd::Volumes {
            cmd: VolumesCmd::List { json },
        } => {
            let app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            cmd_volumes_list(&app, &cli.catalog, json)?;
        }
    }
    Ok(())
}
