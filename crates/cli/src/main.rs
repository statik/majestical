//! `maj`: agent-first CLI over the catalog core. JSON-first output.
use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::{Clock, HlcClock, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::io::Read;
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

    /// Loads every event currently in the log. Each call re-reads the log
    /// from disk; per-process caching arrives with the adapter refactor.
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

// Restoring the workspace's full lint table (see Cargo.toml) brought in
// `too_many_lines`. Splitting each `Cmd` arm into its own function is the
// real fix, but that extraction is deferred to the adapter-layer phase;
// this crate's single-file, single-`main` shape is intentional for now.
#[expect(
    clippy::too_many_lines,
    reason = "extraction deferred to the adapter-layer phase"
)]
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
                    .strip_prefix(&dir)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                ops.push(Op::AssetSeen {
                    asset: AssetId(format!("xxh3:{hash:032x}")),
                    volume: volume.clone(),
                    path: rel,
                    size,
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
        }
    }
    Ok(())
}
