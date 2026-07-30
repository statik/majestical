//! `maj`: agent-first CLI over the catalog core. JSON-first output.
mod app;
mod commands;
mod iso8601;
mod volume_identity;

use anyhow::Result;
use app::FsApp;
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use majestical_ingest::plan::DedupeMode;
use std::path::PathBuf;

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
    /// Get or set LWW metadata fields on an asset.
    Meta {
        #[command(subcommand)]
        cmd: MetaCmd,
    },
    /// Manage PARA organization nodes.
    Para {
        #[command(subcommand)]
        cmd: ParaCmd,
    },
    /// Re-verify a destination against its ASC MHL history; appends a generation.
    Verify {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Verified copy from a source directory into PARA-routed destinations.
    Ingest {
        source: PathBuf,
        /// Destination root(s); each gets an independently verified copy
        /// and its own ASC MHL history.
        #[arg(long, required = true)]
        dest: Vec<PathBuf>,
        /// Target PARA node (<kind>/<name> or node id).
        #[arg(long)]
        para: String,
        /// Layout inside the node. Tokens: {date}, {source-label}.
        #[arg(long, default_value = "{date}/{source-label}")]
        template: String,
        #[arg(long, value_enum, default_value_t = DedupeArg::Skip)]
        dedupe: DedupeArg,
        /// Parallel copy workers (default: CPU cores, max 8).
        #[arg(long)]
        jobs: Option<usize>,
        /// Print the plan and exit without copying.
        #[arg(long)]
        dry_run: bool,
        /// Resume a previous run's journal (run id printed at start).
        #[arg(long)]
        resume: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// `maj ingest --dedupe` surface: only `skip` and `copy` are exposed this
/// phase. `DedupeMode::Link` (hard-link mode) needs a per-destination
/// existing-instance lookup that isn't wired up yet — see the phase 3
/// deferrals in the watchlist.
#[derive(Clone, Copy, ValueEnum)]
enum DedupeArg {
    Skip,
    Copy,
}

impl From<DedupeArg> for DedupeMode {
    fn from(v: DedupeArg) -> Self {
        match v {
            DedupeArg::Skip => Self::Skip,
            DedupeArg::Copy => Self::CopyAnyway,
        }
    }
}

#[derive(Subcommand)]
enum ParaCmd {
    /// Create a node: `maj para add project client-x`.
    Add { kind: String, name: String },
    /// List every PARA node.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Rename a node (last-write-wins across machines).
    Rename { node: String, name: String },
    /// Archive a node; with --root, also moves the materialized directory.
    Archive {
        node: String,
        /// Destination root(s) where the node is materialized on disk.
        #[arg(long)]
        root: Vec<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum MetaCmd {
    /// Set a field's value (last-write-wins across machines).
    Set {
        asset: String,
        field: String,
        value: String,
    },
    /// Get a single field's value, or every field set on the asset.
    Get {
        asset: String,
        /// Omit to print every field.
        field: Option<String>,
        #[arg(long)]
        json: bool,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let author = cli.author.clone().unwrap_or_else(|| cli.machine_id.clone());
    match cli.cmd {
        Cmd::Catalog {
            cmd: CatalogCmd::Init,
        } => commands::cmd_catalog_init(&cli.catalog, &cli.machine_id, &author)?,
        Cmd::Scan { dir, volume } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_scan(&mut app, &dir, volume)?;
        }
        Cmd::Tag { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_tag(&mut app, cmd)?;
        }
        Cmd::Search { name, tag, json } => {
            let app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_search(&app, &cli.catalog, name, tag, json)?;
        }
        Cmd::Volumes {
            cmd: VolumesCmd::List { json },
        } => {
            let app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_volumes_list(&app, &cli.catalog, json)?;
        }
        Cmd::Meta { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_meta(&mut app, cmd)?;
        }
        Cmd::Para { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            commands::cmd_para(&mut app, &cli.catalog, cmd)?;
        }
        // Deliberately does not open a catalog: `verify` re-checks a
        // destination directory against its own ASC MHL history, which
        // needs neither the event log nor a machine identity. `--catalog`/
        // `--machine-id` are still required by clap here (they're
        // top-level, non-Option args with no default) — a wart documented
        // in Task 6's report rather than restructured, since every other
        // subcommand does need them.
        Cmd::Verify { dir, json } => commands::cmd_verify(&dir, json)?,
        Cmd::Ingest {
            source,
            dest,
            para,
            template,
            dedupe,
            jobs,
            dry_run,
            resume,
            json,
        } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            let args = commands::IngestArgs {
                source,
                dest,
                para,
                template,
                dedupe: dedupe.into(),
                jobs,
                dry_run,
                resume,
                json,
            };
            commands::cmd_ingest(&mut app, &cli.catalog, &args)?;
        }
    }
    Ok(())
}
