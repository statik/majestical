//! `maj`: agent-first CLI over the catalog core. JSON-first output.
mod app;
mod commands;
mod iso8601;
mod volume_identity;

use anyhow::Result;
use app::FsApp;
use clap::{ArgGroup, Parser, Subcommand};
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
    }
    Ok(())
}
