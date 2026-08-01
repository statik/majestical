//! `maj`: agent-first CLI over the catalog core. JSON-first output.
mod app;
mod commands;
mod describer_cmd;
mod index_cmd;
mod iso8601;
mod query;
mod search;
mod state_dir;
mod tags_cmd;
mod volume_identity;

use anyhow::Result;
use app::FsApp;
use clap::{Parser, Subcommand, ValueEnum};
use majestical_ingest::plan::DedupeMode;
use std::path::{Path, PathBuf};

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
    /// Review AI-suggested tags: list, confirm into the folksonomy, or
    /// reject on this machine.
    Tags {
        #[command(subcommand)]
        cmd: TagsCmd,
    },
    /// Search the catalog: bare terms match names; key:value tokens are
    /// hard filters (tag: vol: para: kind: online: before: after:), '-'
    /// negates.
    Search {
        /// May start with '-' (a leading negated filter, e.g. `-tag:x`) —
        /// `allow_hyphen_values` stops clap from treating the whole query as
        /// an unrecognized option in that case. Omit when using `--saved`.
        #[arg(allow_hyphen_values = true)]
        query: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// Save this query under a name (and run it).
        #[arg(long, conflicts_with = "saved")]
        save: Option<String>,
        /// Run a previously saved search instead of a literal query.
        #[arg(long, conflicts_with_all = ["save", "query"])]
        saved: Option<String>,
    },
    /// Manage saved searches.
    Searches {
        #[command(subcommand)]
        cmd: SearchesCmd,
    },
    /// Work the derived-data queue (thumbnails, embeddings, keyframes).
    Index {
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// Manage the encoder model used for embeddings/keyframes.
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Configure the caption/tag-suggestion backend for this machine.
    Describer {
        #[command(subcommand)]
        cmd: DescriberCmd,
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
enum IndexCmd {
    /// Work the derivation queue (thumbnails, embeddings, keyframes,
    /// transcripts, OCR, PDF text; captions once a describer is configured).
    Run {
        /// Keep working the queue, polling every 5s for newly scanned assets.
        #[arg(long)]
        watch: bool,
        /// Parallel workers (default: CPU cores, max 4).
        #[arg(long)]
        threads: Option<usize>,
        /// Stop after this many items.
        #[arg(long)]
        limit: Option<usize>,
        /// Comma-separated subset:
        /// thumbs,embeddings,keyframes,transcripts,ocr,pdf,captions.
        #[arg(long, value_delimiter = ',')]
        kinds: Option<Vec<String>>,
        #[arg(long)]
        json: bool,
    },
    /// Show queue status per derivation kind.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Fetch model weights (all models unless --only narrows it).
    Fetch {
        /// Re-hash files that already exist.
        #[arg(long)]
        verify: bool,
        /// Fetch only the named model tags (repeatable).
        #[arg(long)]
        only: Vec<String>,
    },
}

#[derive(clap::Subcommand)]
enum DescriberCmd {
    /// Set the backend for this catalog on this machine.
    Set {
        #[arg(long, value_enum)]
        backend: DescriberBackendArg,
        #[arg(long)]
        model: String,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Show the current configuration (key redacted).
    Show,
    /// Probe the backend: connectivity, model presence, vision capability.
    Test,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DescriberBackendArg {
    Ollama,
    LmStudio,
    OpenRouter,
}

impl From<DescriberBackendArg> for majestical_describe::BackendKind {
    fn from(arg: DescriberBackendArg) -> Self {
        match arg {
            DescriberBackendArg::Ollama => Self::Ollama,
            DescriberBackendArg::LmStudio => Self::LmStudio,
            DescriberBackendArg::OpenRouter => Self::OpenRouter,
        }
    }
}

#[derive(Subcommand)]
enum SearchesCmd {
    /// List saved searches.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a saved search.
    Rm { name: String },
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

/// The human review flow for AI tag suggestions (`Caption`/tag-suggestion
/// blobs written by `maj index run --kinds captions`). Deliberately its own
/// namespace, not folded into `TagCmd`: `maj tag add/rm` are direct
/// folksonomy edits, while `maj tags suggestions/confirm/reject` review
/// derived data that was never a CRDT op until confirmed.
#[derive(Subcommand)]
enum TagsCmd {
    /// List pending AI tag suggestions not yet confirmed or rejected.
    Suggestions,
    /// Confirm suggestion(s) into the folksonomy — emits a plain `TagAdd`,
    /// indistinguishable from `maj tag add` in the event log.
    Confirm {
        asset: String,
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Reject suggestion(s) on this machine only (never synced).
    Reject {
        asset: String,
        #[arg(required = true)]
        tags: Vec<String>,
    },
}

/// Dispatches `maj index`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint.
fn dispatch_index(app: &FsApp, catalog: &Path, cmd: IndexCmd) -> Result<()> {
    match cmd {
        IndexCmd::Run {
            watch,
            threads,
            limit,
            kinds,
            json,
        } => {
            let args = index_cmd::IndexRunArgs {
                watch,
                threads,
                limit,
                kinds,
                json,
            };
            index_cmd::cmd_index_run(app, catalog, &args)
        }
        IndexCmd::Status { json } => index_cmd::cmd_index_status(app, catalog, json),
    }
}

/// Dispatches `maj describer`'s subcommands. Split out of `main` purely to
/// stay under the crate's max-function-length lint, matching
/// [`dispatch_index`].
fn dispatch_describer(catalog: &Path, cmd: DescriberCmd) -> Result<()> {
    match cmd {
        DescriberCmd::Set {
            backend,
            model,
            base_url,
            api_key,
        } => describer_cmd::cmd_set(
            catalog,
            &describer_cmd::SetArgs {
                backend: backend.into(),
                model,
                base_url,
                api_key,
            },
        ),
        DescriberCmd::Show => describer_cmd::cmd_show(catalog),
        DescriberCmd::Test => describer_cmd::cmd_test(catalog),
    }
}

/// Dispatches `maj tags`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint, matching [`dispatch_index`].
fn dispatch_tags(app: &mut FsApp, catalog: &Path, cmd: TagsCmd) -> Result<()> {
    match cmd {
        TagsCmd::Suggestions => tags_cmd::cmd_suggestions(app, catalog),
        TagsCmd::Confirm { asset, tags } => tags_cmd::cmd_confirm(app, &asset, &tags),
        TagsCmd::Reject { asset, tags } => tags_cmd::cmd_reject(catalog, &asset, &tags),
    }
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
        Cmd::Tags { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            dispatch_tags(&mut app, &cli.catalog, cmd)?;
        }
        Cmd::Search {
            query,
            limit,
            json,
            save,
            saved,
        } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            let args = search::SearchArgs {
                query,
                limit,
                json,
                save,
                saved,
            };
            search::cmd_search(&mut app, &cli.catalog, &args)?;
        }
        Cmd::Searches { cmd } => {
            let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            search::cmd_searches(&mut app, cmd)?;
        }
        Cmd::Index { cmd } => {
            let app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
            dispatch_index(&app, &cli.catalog, cmd)?;
        }
        // Deliberately does not open a catalog: fetching the encoder model
        // is a machine-local cache operation, unrelated to any one
        // catalog's event log. `--catalog`/`--machine-id` are still
        // required by clap here (they're top-level, non-Option args with
        // no default) — the same accepted wart as `Verify` above.
        Cmd::Model {
            cmd: ModelCmd::Fetch { verify, only },
        } => index_cmd::cmd_model_fetch(verify, &only)?,
        // Deliberately does not open a catalog: describer config lives in
        // the per-machine state dir, not the event log.
        Cmd::Describer { cmd } => dispatch_describer(&cli.catalog, cmd)?,
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
