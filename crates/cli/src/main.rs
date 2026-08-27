//! `maj`: agent-first CLI over the catalog core. JSON-first output.
mod commands;
mod describer_cmd;
mod inbox_cmd;
mod index_cmd;
mod mcp_cmd;
mod search;
mod sync_cmd;
mod tags_cmd;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use majestical_ingest::plan::DedupeMode;
use majestical_services::app::FsApp;
use majestical_services::error::ServiceError;
use majestical_services::notices::Notices;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "maj", version, about = "Majestical media catalog")]
struct Cli {
    /// Catalog directory (env `MAJ_CATALOG`). Optional at parse time — every
    /// verb but `doctor` requires one, enforced at dispatch by
    /// `require_catalog_and_machine_id` rather than by clap, so `doctor` can
    /// still run with neither configured (see its own module doc).
    #[arg(long, env = "MAJ_CATALOG", help = "Catalog directory")]
    catalog: Option<PathBuf>,
    /// Stable machine identity (env `MAJ_MACHINE_ID`). Optional at parse
    /// time for the same reason `catalog` is — see that field's doc.
    #[arg(long, env = "MAJ_MACHINE_ID", help = "Stable machine identity")]
    machine_id: Option<String>,
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
    /// Sync the catalog with configured locations (NAS, Dropbox folder,
    /// shuttle drive).
    Sync {
        #[command(subcommand)]
        cmd: SyncCmd,
    },
    /// Process a shared inbox folder: validated contributions plus
    /// manifest-less drops.
    Inbox {
        #[command(subcommand)]
        cmd: InboxCmd,
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
    /// Browse the catalog by folder: every volume's folder tree, or one
    /// folder's listing.
    Browse {
        #[command(subcommand)]
        cmd: BrowseCmd,
    },
    /// Re-verify a destination against its ASC MHL history; appends a generation.
    Verify {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Verified copy from a source directory into PARA-routed destinations,
    /// or `maj ingest unfinished` to list the runs a `--resume` could still
    /// finish.
    ///
    /// A source directory actually named `unfinished` reads as the listing
    /// verb; pass it as `./unfinished`.
    // The listing verb sits beside the copy's own arguments rather than
    // renaming the copy into `maj ingest run <source>`: `maj ingest
    // <source>` is the surface every existing script, doc, and resume hint
    // already names.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Ingest {
        #[command(subcommand)]
        cmd: Option<IngestCmd>,
        /// Directory to copy from (the card, the shuttle drive).
        #[arg(required = true)]
        source: Option<PathBuf>,
        /// Destination root(s); each gets an independently verified copy
        /// and its own ASC MHL history.
        #[arg(long, required = true)]
        dest: Vec<PathBuf>,
        /// Target PARA node (<kind>/<name> or node id).
        // `Option` + `required = true` rather than a bare `String`: the
        // `unfinished` subcommand negates the copy's requirements at the
        // clap level, but a non-`Option` field would still be extracted
        // unconditionally by the derive and fail there instead. Same for
        // `source` above.
        #[arg(long, required = true)]
        para: Option<String>,
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
    /// Diagnostic sweep of the environment and (optionally) one catalog:
    /// ffmpeg/imagemagick/model presence, catalog and state-dir health,
    /// orphaned temp files, platform capabilities.
    Doctor {
        /// Catalog to health-check. Independent of the top-level
        /// `--catalog`/`MAJ_CATALOG` — doctor is the one verb exempt from
        /// `require_catalog_and_machine_id`'s dispatch-time requirement, so
        /// omitting this is how a caller expresses "check the environment
        /// only," and a bare `maj doctor` with neither configured at all
        /// still runs.
        #[arg(long)]
        catalog: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Serve the catalog to MCP clients over stdio.
    Mcp,
}

#[derive(Subcommand)]
enum IngestCmd {
    /// List runs whose journal still has planned files that never landed —
    /// newest first, each resumable with `maj ingest <source> --resume <id>`.
    Unfinished {
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
    /// File one or more assets under a PARA node.
    File {
        /// Node reference (`<kind>/<name>` or a raw node id).
        node: String,
        #[arg(required = true)]
        assets: Vec<String>,
    },
}

#[derive(Subcommand)]
enum BrowseCmd {
    /// Every volume's folder tree, with a recursive asset count per folder.
    Tree {
        #[arg(long)]
        json: bool,
    },
    /// List assets under one folder of one volume — the whole subtree by
    /// default (see `--no-flatten`), sorted newest-first by default (see
    /// `--sort`).
    List {
        /// Volume id (see `maj volumes list`).
        #[arg(long, required = true)]
        volume: String,
        /// Folder path relative to the volume root ("" for the root).
        #[arg(long, default_value = "")]
        path: String,
        /// List only this folder's immediate children instead of its whole
        /// subtree.
        #[arg(long)]
        no_flatten: bool,
        /// "captured" (newest `mtime` first, default), "name" (A-Z), or
        /// "size" (largest first).
        #[arg(long)]
        sort: Option<String>,
        /// Filter to one media kind (image, video, audio, pdf, other).
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = majestical_services::browse::DEFAULT_LIMIT)]
        limit: usize,
        /// Skip this many matches (post-sort, pre-limit) before the page
        /// starts — pair with `--limit` to page through results past the
        /// first `--limit`-sized batch.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        json: bool,
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
enum SyncCmd {
    /// Manage this machine's sync locations (stored in the state dir,
    /// never synced).
    Location {
        #[command(subcommand)]
        cmd: SyncLocationCmd,
    },
    /// Replicate everything this catalog has (segments + blobs) to
    /// configured locations.
    Push {
        /// Push to only this location (default: every configured one).
        #[arg(long)]
        location: Option<String>,
        /// Restrict to one transfer class.
        #[arg(long, value_enum)]
        only: Option<sync_cmd::OnlyArg>,
        #[arg(long)]
        json: bool,
    },
    /// Fetch everything configured locations have that this catalog
    /// doesn't (segments + blobs), then apply newly landed events locally.
    Pull {
        /// Pull from only this location (default: every configured one).
        #[arg(long)]
        location: Option<String>,
        /// Restrict to one transfer class.
        #[arg(long, value_enum)]
        only: Option<sync_cmd::OnlyArg>,
        #[arg(long)]
        json: bool,
    },
    /// Report each configured location's reachability and ahead/behind
    /// counts, walked fresh from real files. Never executes a transfer.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum InboxCmd {
    /// One converging pass: validate, verified-ingest, tag provenance,
    /// move to .processed/. Manifest-less drops (a folder with no
    /// `contribution.json`, or a bare top-level file) triage to
    /// `--triage-target` once quiescent, tagged `source/inbox`.
    Process {
        inbox: PathBuf,
        /// Destination root(s), like `maj ingest --dest`.
        #[arg(long, required = true)]
        dest: Vec<PathBuf>,
        /// PARA node for manifest-less drops (`<kind>/<name>` or a raw node
        /// id). Required once any quiescent manifest-less item is present —
        /// never invented silently.
        #[arg(long)]
        triage_target: Option<String>,
        /// Leave processed contributions in place.
        #[arg(long)]
        keep: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SyncLocationCmd {
    /// Add a named location and initialize its events/ + blobs/ layout.
    Add { name: String, path: PathBuf },
    /// List this machine's sync locations.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a location from config (never touches its files).
    Rm { name: String },
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
    Add {
        asset: String,
        tag: String,
    },
    Rm {
        asset: String,
        tag: String,
    },
    /// Rename a live tag to a name nothing carries yet — merging onto an
    /// existing tag is a merge (`maj tag merge`), not a rename.
    Rename {
        from: String,
        to: String,
    },
    /// Fold one live tag into another live tag.
    Merge {
        from: String,
        into: String,
    },
    /// Bulk-add one or more tags to one or more assets in one call — the
    /// bulk form of `maj tag add`.
    Assign {
        /// Tag to add (repeatable: `--tag a --tag b`).
        #[arg(long = "tag", required = true)]
        tags: Vec<String>,
        #[arg(required = true)]
        assets: Vec<String>,
    },
}

/// The human review flow for AI tag suggestions (`Caption`/tag-suggestion
/// blobs written by `maj index run --kinds captions`). Deliberately its own
/// namespace, not folded into `TagCmd`: `maj tag add/rm` are direct
/// folksonomy edits, while `maj tags suggestions/confirm/reject` review
/// derived data that was never a CRDT op until confirmed.
#[derive(Subcommand)]
enum TagsCmd {
    /// List the catalog's live tag vocabulary — every effective tag after
    /// alias resolution, with its asset count and newest surviving add time.
    List {
        #[arg(long)]
        json: bool,
    },
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
            &majestical_services::describer_config::SetArgs {
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

/// Dispatches `maj sync`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint, matching [`dispatch_index`].
/// Takes `machine_id`/`author` (unlike [`dispatch_index`]'s app-only
/// signature) because `Pull` needs them to open the local catalog and
/// apply newly landed events itself, not just orchestrate file transfers.
fn dispatch_sync(catalog: &Path, machine_id: &str, author: &str, cmd: SyncCmd) -> Result<()> {
    match cmd {
        SyncCmd::Location { cmd } => match cmd {
            SyncLocationCmd::Add { name, path } => {
                sync_cmd::cmd_location_add(catalog, &name, &path)
            }
            SyncLocationCmd::List { json } => sync_cmd::cmd_location_list(catalog, json),
            SyncLocationCmd::Rm { name } => sync_cmd::cmd_location_rm(catalog, &name),
        },
        SyncCmd::Push {
            location,
            only,
            json,
        } => sync_cmd::cmd_push(catalog, location.as_deref(), only, json),
        SyncCmd::Pull {
            location,
            only,
            json,
        } => sync_cmd::cmd_pull(
            catalog,
            machine_id,
            author,
            &sync_cmd::PullArgs {
                location,
                only,
                json,
            },
        ),
        SyncCmd::Status { json } => sync_cmd::cmd_status(catalog, json),
    }
}

/// Dispatches `maj inbox`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint, matching [`dispatch_index`].
fn dispatch_inbox(app: &mut FsApp, catalog: &Path, cmd: InboxCmd) -> Result<()> {
    match cmd {
        InboxCmd::Process {
            inbox,
            dest,
            triage_target,
            keep,
            json,
        } => {
            let args = inbox_cmd::InboxArgs {
                inbox,
                dest,
                triage_target,
                keep,
                json,
            };
            inbox_cmd::cmd_inbox_process(app, catalog, &args)
        }
    }
}

/// Dispatches `maj tags`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint, matching [`dispatch_index`].
fn dispatch_tags(app: &mut FsApp, catalog: &Path, cmd: TagsCmd) -> Result<()> {
    match cmd {
        TagsCmd::List { json } => tags_cmd::cmd_tags_list(app, catalog, json),
        TagsCmd::Suggestions => tags_cmd::cmd_suggestions(app, catalog),
        TagsCmd::Confirm { asset, tags } => tags_cmd::cmd_confirm(app, &asset, &tags),
        TagsCmd::Reject { asset, tags } => tags_cmd::cmd_reject(catalog, &asset, &tags),
    }
}

/// Prints every service-collected diagnostic to stderr, verbatim and in
/// order — the CLI head's half of the `crates/services` notices contract.
/// Service entry points that take no `FsApp` (`sync`, `describer`, `tags
/// reject`) call this themselves as soon as the call returns, before
/// rendering the outcome, so these lines keep the stderr position they had
/// when services printed them directly.
pub(crate) fn drain_notices(notices: &Notices) {
    print_notices(&notices.drain());
}

/// Prints the notices a verb carried home on its outcome struct — the same
/// lines [`drain_notices`] prints, arriving on the outcome instead of the
/// sink. Renderers call this BEFORE writing their stdout, so each line keeps
/// the stderr position it had when services printed it directly.
pub(crate) fn print_notices(lines: &[String]) {
    for line in lines {
        eprintln!("{line}");
    }
}

/// Splits a `WithNotices` carrier: prints its notices to stderr — the same
/// lines, same stream, same position-before-the-error the Ok path gives
/// them — and hands back the inner error, so downstream match arms see the
/// same variants they always did. Errors that aren't carriers pass through.
pub(crate) fn surface_err_notices<T>(result: Result<T, ServiceError>) -> Result<T, ServiceError> {
    result.map_err(|err| match err {
        ServiceError::WithNotices { notices, source } => {
            print_notices(&notices);
            *source
        }
        other => other,
    })
}

/// `Cmd::Ingest { cmd: None, .. }`'s fields (the copy form), bundled so
/// [`dispatch_ingest_copy`] takes one struct instead of nine positional
/// arguments.
struct IngestCopyFields {
    source: Option<PathBuf>,
    dest: Vec<PathBuf>,
    para: Option<String>,
    template: String,
    dedupe: DedupeArg,
    jobs: Option<usize>,
    dry_run: bool,
    resume: Option<String>,
    json: bool,
}

/// Dispatches `maj ingest <source> --dest .. --para ..` (the copy form).
/// Split out of `main` purely to stay under the crate's max-function-length
/// lint, matching [`dispatch_inbox`]. Resolves `source`/`para` — `required =
/// true` in clap, except when the `unfinished` subcommand negates them,
/// which `main`'s arm above this one already took by the time this runs —
/// before running the copy. Takes `catalog`/`machine_id` as their own
/// borrows rather than `&Cli`: by the time this runs, `main`'s `match
/// cli.cmd` has already partially moved `cli`, so only per-field borrows
/// (the same ones every other arm passes) are available, not the whole
/// struct.
fn dispatch_ingest_copy(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    fields: IngestCopyFields,
) -> Result<()> {
    let source = fields
        .source
        .context("maj ingest needs a source directory")?;
    let para = fields.para.context("maj ingest needs --para")?;
    let args = commands::IngestArgs {
        source,
        dest: fields.dest,
        para,
        template: fields.template,
        dedupe: fields.dedupe.into(),
        jobs: fields.jobs,
        dry_run: fields.dry_run,
        resume: fields.resume,
        json: fields.json,
    };
    with_app(catalog, machine_id, author, |app| {
        commands::cmd_ingest(app, catalog, &args)
    })
}

/// Opens the catalog, runs one command against it, then drains the app's
/// notices — on the error path too, so a warning collected before a failure
/// still reaches the user.
fn with_app(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    run: impl FnOnce(&mut FsApp) -> Result<()>,
) -> Result<()> {
    let mut app = FsApp::open(catalog, machine_id, author)?;
    let result = run(&mut app);
    drain_notices(app.notices());
    result
}

/// Resolves the `--catalog`/`MAJ_CATALOG` and `--machine-id`/
/// `MAJ_MACHINE_ID` every verb but `doctor` requires. Enforced here, at
/// dispatch, rather than by clap's own "required" flag: both fields parse as
/// optional on `Cli` so a bare `maj doctor` still runs with neither
/// configured (see `Cmd::Doctor`'s own doc — diagnosing exactly that state
/// is the verb's purpose). Errors name both the flag and the env var, so a
/// caller with neither set knows exactly what to supply.
fn require_catalog_and_machine_id(cli: &Cli) -> Result<(PathBuf, String)> {
    let catalog = cli
        .catalog
        .clone()
        .context("--catalog <dir> or MAJ_CATALOG is required for this command")?;
    let machine_id = cli
        .machine_id
        .clone()
        .context("--machine-id <id> or MAJ_MACHINE_ID is required for this command")?;
    Ok((catalog, machine_id))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // `doctor` is the one verb exempt from `require_catalog_and_machine_id`
    // below, so it's dispatched first, before that resolution can fail.
    if let Cmd::Doctor { catalog, json } = cli.cmd {
        return commands::cmd_doctor(catalog, json);
    }
    let (catalog, machine_id) = require_catalog_and_machine_id(&cli)?;
    let author = cli.author.clone().unwrap_or_else(|| machine_id.clone());
    match cli.cmd {
        Cmd::Catalog {
            cmd: CatalogCmd::Init,
        } => commands::cmd_catalog_init(&catalog, &machine_id, &author)?,
        Cmd::Scan { dir, volume } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_scan(app, &dir, volume)
        })?,
        Cmd::Tag { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_tag(app, cmd)
        })?,
        Cmd::Tags { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            dispatch_tags(app, &catalog, cmd)
        })?,
        Cmd::Search {
            query,
            limit,
            json,
            save,
            saved,
        } => with_app(&catalog, &machine_id, &author, |app| {
            let args = search::SearchArgs {
                query,
                limit,
                json,
                save,
                saved,
            };
            search::cmd_search(app, &catalog, &args)
        })?,
        Cmd::Searches { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            search::cmd_searches(app, cmd)
        })?,
        Cmd::Index { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            dispatch_index(app, &catalog, cmd)
        })?,
        // Deliberately does not open a catalog: fetching the encoder model
        // is a machine-local cache operation, unrelated to any one
        // catalog's event log — `catalog`/`machine_id` above were still
        // resolved (and could have failed) for this arm, the same accepted
        // wart `Verify` below carries.
        Cmd::Model {
            cmd: ModelCmd::Fetch { verify, only },
        } => index_cmd::cmd_model_fetch(verify, &only)?,
        // Deliberately does not open a catalog: describer config lives in
        // the per-machine state dir, not the event log.
        Cmd::Describer { cmd } => dispatch_describer(&catalog, cmd)?,
        // Deliberately does not open the catalog itself here: sync location
        // config lives in the per-machine state dir, not the event log —
        // `Pull` opens the catalog internally, once it needs to apply.
        Cmd::Sync { cmd } => dispatch_sync(&catalog, &machine_id, &author, cmd)?,
        Cmd::Inbox { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            dispatch_inbox(app, &catalog, cmd)
        })?,
        Cmd::Volumes {
            cmd: VolumesCmd::List { json },
        } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_volumes_list(app, &catalog, json)
        })?,
        Cmd::Meta { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_meta(app, cmd)
        })?,
        Cmd::Para { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_para(app, &catalog, cmd)
        })?,
        Cmd::Browse { cmd } => with_app(&catalog, &machine_id, &author, |app| {
            commands::cmd_browse(app, &catalog, cmd)
        })?,
        // Deliberately does not open a catalog: `verify` re-checks a
        // destination directory against its own ASC MHL history, which
        // needs neither the event log nor a machine identity — `catalog`/
        // `machine_id` above were still resolved (and could have failed)
        // for this arm, a wart documented in Task 6's report rather than
        // restructured, since every other subcommand does need them.
        Cmd::Verify { dir, json } => commands::cmd_verify(&dir, json)?,
        // Like `Verify` above, the listing verb deliberately does not open a
        // catalog: run journals live in this machine's state dir, not the
        // event log.
        Cmd::Ingest {
            cmd: Some(IngestCmd::Unfinished { json }),
            ..
        } => commands::cmd_ingest_unfinished(&catalog, json)?,
        Cmd::Ingest {
            cmd: None,
            source,
            dest,
            para,
            template,
            dedupe,
            jobs,
            dry_run,
            resume,
            json,
        } => dispatch_ingest_copy(
            &catalog,
            &machine_id,
            &author,
            IngestCopyFields {
                source,
                dest,
                para,
                template,
                dedupe,
                jobs,
                dry_run,
                resume,
                json,
            },
        )?,
        // Unreachable: the `if let Cmd::Doctor { .. }` above already
        // returned for this variant, before `catalog`/`machine_id` were
        // even resolved. Kept only so this match stays exhaustive over `Cmd`.
        Cmd::Doctor { .. } => unreachable!("handled by the if-let above"),
        // Deliberately does not open a catalog here: `mcp_cmd::serve` opens
        // (and re-opens) it per tool call, not at startup — see its own
        // module doc for why, mirroring `Verify`/`Model` above.
        Cmd::Mcp => mcp_cmd::serve(&catalog, &machine_id, &author)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `maj ingest` carries both a copy's arguments and a listing
    /// subcommand, so both spellings must keep parsing: the bare source
    /// form every script and resume hint uses, and `unfinished` routing to
    /// the listing instead of being swallowed as a source path.
    #[test]
    fn ingest_parses_both_a_source_copy_and_the_unfinished_listing() {
        let copy = Cli::try_parse_from([
            "maj",
            "--catalog",
            "/c",
            "--machine-id",
            "m1",
            "ingest",
            "/media/card",
            "--dest",
            "/d1",
            "--para",
            "project/x",
        ])
        .expect("the copy form still parses");
        match copy.cmd {
            Cmd::Ingest { cmd, source, .. } => {
                assert!(cmd.is_none(), "no subcommand was named");
                assert_eq!(source, Some(PathBuf::from("/media/card")));
            }
            _ => panic!("expected the copy form to parse as Cmd::Ingest"),
        }

        let listing = Cli::try_parse_from([
            "maj",
            "--catalog",
            "/c",
            "--machine-id",
            "m1",
            "ingest",
            "unfinished",
            "--json",
        ])
        .expect("the listing form parses without --dest/--para");
        match listing.cmd {
            Cmd::Ingest {
                cmd: Some(IngestCmd::Unfinished { json }),
                source,
                ..
            } => {
                assert!(json);
                assert_eq!(source, None, "'unfinished' is the verb, not the source");
            }
            _ => panic!("expected `ingest unfinished` to parse as the listing subcommand"),
        }

        // The two forms are alternatives, not a copy that can also list:
        // asking for both is a clap error, not a silently ignored half.
        let both = Cli::try_parse_from([
            "maj",
            "--catalog",
            "/c",
            "--machine-id",
            "m1",
            "ingest",
            "/media/card",
            "--dest",
            "/d1",
            "--para",
            "project/x",
            "unfinished",
        ]);
        // `Cli` has no `Debug`, so match rather than `expect_err`.
        let Err(both) = both else {
            panic!("a copy cannot also be a listing");
        };
        assert_eq!(
            both.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "the copy's arguments and the listing verb are alternatives: {both}"
        );
    }

    #[test]
    fn surface_err_notices_unwraps_the_carrier_to_its_inner_error() {
        use majestical_services::error::ServiceError;
        let carried: Result<(), ServiceError> = Err(ServiceError::WithNotices {
            notices: vec!["warned".to_string()],
            source: Box::new(ServiceError::NoCatalog {
                root: std::path::PathBuf::from("/nowhere"),
            }),
        });
        let inner = surface_err_notices(carried).expect_err("stays Err");
        assert!(
            matches!(inner, ServiceError::NoCatalog { .. }),
            "downstream match arms must see the pre-carrier variants"
        );
    }
}
