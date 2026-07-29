//! `maj`: agent-first CLI over the catalog core. JSON-first output.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::{Clock, HlcClock, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::path::{Path, PathBuf};

struct SystemClock;
impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
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
        #[arg(long)]
        volume: String,
    },
    /// Add or remove folksonomy tags.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Search the catalog projection.
    Search {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        tag: Option<String>,
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

struct App {
    log: FileEventLog,
    hlc: HlcClock,
    author: String,
}

impl App {
    fn open(root: &Path, machine: &str) -> Result<Self> {
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::open(root, &machine)
            .with_context(|| format!("opening catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine.clone(), Box::new(SystemClock)),
            author: machine.0,
        })
    }

    /// Loads every event in the log once. Both `projection` (read paths) and
    /// `emit` (write paths, to fold the log into the clock before stamping)
    /// need the full event set, so callers doing both share one read.
    fn events(&self) -> Result<Vec<Event>> {
        self.log.read_all().context("reading event log")
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
        for e in self.events()? {
            self.hlc.observe(&e.hlc);
        }
        let events: Vec<Event> = ops
            .into_iter()
            .map(|op| {
                let hlc = self.hlc.now();
                Event {
                    id: EventId(ulid::Ulid::new()),
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Catalog {
            cmd: CatalogCmd::Init,
        } => {
            App::open(&cli.catalog, &cli.machine_id)?;
            println!("initialized catalog at {}", cli.catalog.display());
        }
        Cmd::Scan { dir, volume } => {
            let mut app = App::open(&cli.catalog, &cli.machine_id)?;
            let mut ops = Vec::new();
            for entry in walkdir::WalkDir::new(&dir).sort_by_file_name() {
                let entry = entry.context("walking scan directory")?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let bytes = std::fs::read(entry.path())
                    .with_context(|| format!("reading {}", entry.path().display()))?;
                let hash = xxhash_rust::xxh3::xxh3_128(&bytes);
                // Phase 1: lossy UTF-8 conversion of the relative path. JSON
                // events force UTF-8 anyway, so a non-UTF-8 path can't round
                // trip through the log yet; revisit once ingest needs to
                // preserve exact bytes.
                let rel = entry
                    .path()
                    .strip_prefix(&dir)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                ops.push(Op::AssetSeen {
                    asset: AssetId(format!("xxh3:{hash:032x}")),
                    volume: volume.clone(),
                    path: rel,
                    size: bytes.len() as u64,
                });
            }
            let n = ops.len();
            app.emit(ops)?;
            println!("scanned: {n} assets");
        }
        Cmd::Tag { cmd } => {
            let mut app = App::open(&cli.catalog, &cli.machine_id)?;
            match cmd {
                TagCmd::Add { asset, tag } => {
                    app.emit(vec![Op::TagAdd {
                        asset: AssetId(asset),
                        tag,
                    }])?;
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
        }
        Cmd::Search { name, tag, json } => {
            let app = App::open(&cli.catalog, &cli.machine_id)?;
            let projection = app.projection()?;
            let db_path = cli.catalog.join("catalog.db");
            let db = SqliteCatalog::rebuild(&db_path, &projection)
                .context("rebuilding sqlite projection")?;
            let ids = match (&name, &tag) {
                (Some(n), None) => db.search_by_name(n)?,
                (None, Some(t)) => db.search_by_tag(t)?,
                _ => anyhow::bail!("pass exactly one of --name or --tag"),
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
        }
    }
    Ok(())
}
