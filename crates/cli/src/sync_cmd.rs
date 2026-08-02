//! `maj sync`: location config plus push/pull/status orchestration over
//! `crates/sync`'s transfer engine. Locations are per-machine config
//! (mount points differ per machine) in the state dir's `sync.toml`,
//! never synced.

use crate::app::FsApp;
use anyhow::{Context, Result};
use majestical_sync::transfer;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) const NO_LOCATIONS_HINT: &str =
    "no sync locations configured — add one with `maj sync location add <name> <path>`";

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncConfig {
    /// The read-only-member switch: a machine with `readonly = true` never
    /// pushes (events already carry author identity, so this is the whole
    /// feature — a policy on the push side, not a data concept).
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
    /// Fields a newer `maj` wrote that this build doesn't know about.
    /// Flattened so `location add|rm` round-trip them unchanged instead of
    /// silently dropping them.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Location {
    pub name: String,
    pub path: PathBuf,
    /// See [`SyncConfig::extra`].
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl SyncConfig {
    /// Load config from `path`; a missing file returns `Self::default()` (a
    /// catalog that has never configured sync), never an error.
    ///
    /// # Errors
    /// Returns an error when `path` exists but can't be read, or its
    /// contents don't parse as TOML.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Serialize and write `path`, replacing it via a same-directory
    /// temp-file-then-rename so a concurrent reader never observes a
    /// partial write. The file is always rewritten wholesale from this
    /// struct: a hand-edited known field (e.g. `readonly`) survives a
    /// later `location add|rm`, but TOML comments do not — they have no
    /// representation in the struct, so they're dropped on the next store.
    ///
    /// # Errors
    /// Returns an error when serialization fails, or the write/rename
    /// fails.
    pub(crate) fn store(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing sync config")?;
        let file_name = path.file_name().map_or_else(
            || "sync.toml".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let tmp = path.with_file_name(format!("{file_name}.tmp"));
        std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))
    }
}

/// The per-catalog `sync.toml` path in this machine's state dir.
pub(crate) fn config_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("sync.toml"))
}

/// Registers a new sync location: validates `location` is an accessible,
/// UTF-8-representable directory, canonicalizes it (locations are mount
/// points and must be absolute at rest — a relative path would resolve
/// against whatever CWD a later push/pull happens to run from), idempotently
/// creates the `events/`/`blobs/` skeleton so the first push has somewhere
/// to land, and appends it to `config`.
///
/// # Errors
/// Returns an error when `name` is empty, `location` is not an accessible
/// directory, `location` is not valid UTF-8, `name` is already configured,
/// the skeleton directories can't be created, or `config` can't be stored.
fn add_location(config: &Path, name: &str, location: &Path) -> Result<()> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "sync location name must not be empty");
    anyhow::ensure!(
        location.is_dir(),
        "{} is not an accessible directory — mount it or check the path",
        location.display()
    );
    let canonical = location
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", location.display()))?;
    anyhow::ensure!(
        canonical.to_str().is_some(),
        "{} is not valid UTF-8 — sync locations must have UTF-8 paths so they can be stored in sync.toml",
        canonical.display()
    );
    let mut cfg = SyncConfig::load(config)?;
    anyhow::ensure!(
        !cfg.locations.iter().any(|l| l.name == name),
        "sync location '{name}' is already configured — remove it first with `maj sync location rm {name}`"
    );
    // Git-init style: idempotently create the layout so the first push
    // has somewhere to land. Never touches existing files.
    for sub in ["events", "blobs"] {
        let dir = canonical.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("initializing {}", dir.display()))?;
    }
    cfg.locations.push(Location {
        name: name.to_string(),
        path: canonical,
        extra: toml::Table::new(),
    });
    cfg.store(config)
}

/// Removes the location named `name` from `config`. Never touches the
/// location's own files (its `events/`/`blobs/` directories, and anything a
/// prior sync landed there) — only the config entry is dropped.
///
/// # Errors
/// Returns an error when no location named `name` exists, or `config`
/// can't be stored.
fn remove_location(config: &Path, name: &str) -> Result<()> {
    let mut cfg = SyncConfig::load(config)?;
    let before = cfg.locations.len();
    cfg.locations.retain(|l| l.name != name);
    anyhow::ensure!(
        cfg.locations.len() < before,
        "no sync location named '{name}' — see `maj sync location list`"
    );
    cfg.store(config)
}

pub(crate) fn cmd_location_add(catalog: &Path, name: &str, location: &Path) -> Result<()> {
    add_location(&config_path(catalog)?, name, location)?;
    println!("added sync location '{name}' at {}", location.display());
    Ok(())
}

pub(crate) fn cmd_location_rm(catalog: &Path, name: &str) -> Result<()> {
    remove_location(&config_path(catalog)?, name)?;
    println!("removed sync location '{name}' (its files were not touched)");
    Ok(())
}

/// `--only` surface for `maj sync push` and `maj sync pull`.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OnlyArg {
    Segments,
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

/// Narrows `plan` to one transfer class. `None` (no `--only`) returns the
/// plan unchanged.
fn filter_plan(plan: transfer::TransferPlan, only: Option<OnlyArg>) -> transfer::TransferPlan {
    let Some(only) = only else { return plan };
    let class = match only {
        OnlyArg::Segments => {
            return transfer::TransferPlan {
                segments: plan.segments,
                blobs: Vec::new(),
            };
        }
        OnlyArg::Thumbs => transfer::BlobClass::Thumbs,
        OnlyArg::Metadata => transfer::BlobClass::Metadata,
        OnlyArg::Vectors => transfer::BlobClass::Vectors,
        OnlyArg::Transcripts => transfer::BlobClass::Transcripts,
    };
    transfer::TransferPlan {
        segments: Vec::new(),
        blobs: plan
            .blobs
            .into_iter()
            .filter(|b| b.class == class)
            .collect(),
    }
}

/// Locations to operate on: every configured location, or exactly the named
/// one.
///
/// # Errors
/// Returns an error when no locations are configured at all, or `name` is
/// given but doesn't match any configured location.
fn resolve_targets<'a>(cfg: &'a SyncConfig, name: Option<&str>) -> Result<Vec<&'a Location>> {
    anyhow::ensure!(!cfg.locations.is_empty(), "{NO_LOCATIONS_HINT}");
    let Some(name) = name else {
        return Ok(cfg.locations.iter().collect());
    };
    cfg.locations.iter().find(|l| l.name == name).map_or_else(
        || {
            let known: Vec<&str> = cfg.locations.iter().map(|l| l.name.as_str()).collect();
            Err(anyhow::anyhow!(
                "no sync location named '{name}' — configured: {}",
                known.join(", ")
            ))
        },
        |l| Ok(vec![l]),
    )
}

/// Which side of a location pair is the transfer source. Push sends the
/// catalog's own state out to the location; pull fetches the location's
/// state into the catalog — otherwise identical plumbing (same plan/execute
/// call, same [`LocationResult`] shape), so [`transfer_one`] takes this
/// instead of duplicating itself per direction.
#[derive(Clone, Copy)]
enum Direction {
    Push,
    Pull,
}

/// One location's push or pull result: it either ran — with an outcome that
/// may itself carry per-file failures — or it never ran at all, for one of
/// two distinct reasons: the location's mount wasn't there (`Skipped`) or
/// the transfer engine itself failed setting up or running the transfer
/// (`Failed`, e.g. `plan_transfer`/`execute`'s own error). An enum rather
/// than a struct with `Option` fields: these three states are mutually
/// exclusive, and every caller has to handle all of them — a struct would
/// instead admit unrepresentable combinations.
enum LocationResult {
    Outcome {
        name: String,
        outcome: transfer::TransferOutcome,
    },
    Skipped {
        name: String,
        reason: String,
    },
    Failed {
        name: String,
        error: String,
    },
}

impl LocationResult {
    fn name(&self) -> &str {
        match self {
            Self::Outcome { name, .. } | Self::Skipped { name, .. } | Self::Failed { name, .. } => {
                name
            }
        }
    }
}

/// Guards every sync entry point against operating on a directory that was
/// never `maj catalog init`ed. Without this, `cmd_pull` in particular would
/// manufacture a working catalog out of thin air: the transfer engine
/// creates `events/<machine>/` on the destination as a side effect of
/// copying a segment there, so by the time `FsApp::open` ran its own guard
/// afterward it would already see a (transfer-created) `events/` dir and
/// pass. A `catalog` root that doesn't exist at all would instead surface
/// as a raw `canonicalize` I/O error out of [`config_path`], with no
/// remedy — this runs first, before either of those.
///
/// # Errors
/// Returns an error naming `maj catalog init` as the remedy when `catalog`
/// has no `events/` directory.
fn ensure_catalog(catalog: &Path) -> Result<()> {
    anyhow::ensure!(
        catalog.join("events").is_dir(),
        "no catalog at {} — run `maj catalog init` first",
        catalog.display()
    );
    Ok(())
}

/// `maj sync push`: replicate everything this catalog has (segments +
/// blobs) to configured locations. Refuses outright when this machine is a
/// read-only sync member; otherwise every reachable location gets its own
/// independent transfer attempt, and per-file failures within a transfer
/// are recorded rather than aborting it (see [`transfer::TransferOutcome`]).
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, this machine is
/// readonly, every requested location failed or was skipped, or any
/// per-file failures occurred (progress is still kept and reported either
/// way — see [`check_exit_policy`]).
pub(crate) fn cmd_push(
    catalog: &Path,
    location: Option<&str>,
    only: Option<OnlyArg>,
    json: bool,
) -> Result<()> {
    ensure_catalog(catalog)?;
    let config = config_path(catalog)?;
    let cfg = SyncConfig::load(&config)?;
    anyhow::ensure!(
        !cfg.readonly,
        "readonly = true in {} — this machine is a read-only sync member and never pushes — set `readonly = false` there to push from this machine",
        config.display()
    );
    let targets = resolve_targets(&cfg, location)?;
    let results: Vec<LocationResult> = targets
        .into_iter()
        .map(|loc| transfer_one(catalog, loc, only, Direction::Push))
        .collect();

    // Same tail shape as `cmd_pull`: text rows, then failure lines (always,
    // to stderr), then the JSON document (a different stream — reordering
    // it after the failure lines changes nothing a test can observe), then
    // the exit-policy check last.
    if !json {
        print_text_rows(&results, "push");
    }
    print_failure_lines(&results);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_rows(&results))
                .context("serializing sync report")?
        );
    }
    check_exit_policy(&results, "push")
}

/// Runs one location's transfer in `direction`, converting an unreachable
/// location into a `Skipped` row and any `plan_transfer`/`execute` setup
/// error into a `Failed` row, rather than propagating an error out of this
/// function — one bad location must never abort every other location's
/// transfer.
fn transfer_one(
    catalog: &Path,
    loc: &Location,
    only: Option<OnlyArg>,
    direction: Direction,
) -> LocationResult {
    if !loc.path.is_dir() {
        return LocationResult::Skipped {
            name: loc.name.clone(),
            reason: format!("unreachable at {} — skipped", loc.path.display()),
        };
    }
    let (src, dst) = match direction {
        Direction::Push => (catalog, loc.path.as_path()),
        Direction::Pull => (loc.path.as_path(), catalog),
    };
    let run = transfer::plan_transfer(src, dst)
        .map(|plan| filter_plan(plan, only))
        .and_then(|plan| transfer::execute(src, dst, &plan));
    match run {
        Ok(outcome) => LocationResult::Outcome {
            name: loc.name.clone(),
            outcome,
        },
        Err(e) => LocationResult::Failed {
            name: loc.name.clone(),
            error: e.to_string(),
        },
    }
}

/// The per-file failure lines every report (push or pull, either output
/// format) prints to stderr: `<location>: failed <path>: <reason>`, one per
/// entry in a ran location's [`transfer::TransferOutcome::failures`].
fn print_failure_lines(results: &[LocationResult]) {
    for r in results {
        if let LocationResult::Outcome { name, outcome } = r {
            for (path, reason) in &outcome.failures {
                eprintln!("{name}: failed {}: {reason}", path.display());
            }
        }
    }
}

/// Enforces the exit policy over an already-reported `results`: nonzero
/// when EVERY requested location was skipped, failed outright, or
/// otherwise never ran, and ALSO when a location that DID run had per-file
/// failures within its own transfer — that location's other files still
/// copied (the engine records and continues past per-file errors), but a
/// sync that could not move everything must not exit 0 under cron. The
/// final error names the failing locations and, for the all-failed case,
/// says progress was kept and the next run retries.
///
/// Both `cmd_push` and `cmd_pull` report first and call this LAST:
/// `cmd_pull` in particular applies pulled events between reporting and
/// this check, so a per-file failure at one location never blocks
/// already-landed segments from being applied.
///
/// # Errors
/// See above.
fn check_exit_policy(results: &[LocationResult], verb: &str) -> Result<()> {
    anyhow::ensure!(
        results
            .iter()
            .any(|r| matches!(r, LocationResult::Outcome { .. })),
        "sync {verb} failed for every requested location ({}) — check they're mounted and reachable",
        results
            .iter()
            .map(LocationResult::name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let failing: Vec<&str> = results
        .iter()
        .filter_map(|r| match r {
            LocationResult::Outcome { name, outcome } if !outcome.failures.is_empty() => {
                Some(name.as_str())
            }
            LocationResult::Outcome { .. }
            | LocationResult::Skipped { .. }
            | LocationResult::Failed { .. } => None,
        })
        .collect();
    anyhow::ensure!(
        failing.is_empty(),
        "sync {verb} had per-file failures at {} — progress was kept; the next run retries",
        failing.join(", ")
    );
    Ok(())
}

/// Bundles `maj sync pull`'s flags within the house 5-positional-parameter
/// limit.
pub(crate) struct PullArgs {
    pub location: Option<String>,
    pub only: Option<OnlyArg>,
    pub json: bool,
}

/// `maj sync pull`: fetch everything configured locations have that this
/// catalog doesn't (segments + blobs), then apply the newly landed events
/// to the local sqlite catalog. Like `cmd_push`, refuses outright when
/// there's no catalog at `catalog` (see [`ensure_catalog`]); unlike
/// `cmd_push`, there is no readonly refusal — a read-only member still
/// needs to pull.
///
/// Order matters: transfer every location, THEN apply, THEN check the exit
/// policy — see [`check_exit_policy`]'s doc for why a per-file blob failure
/// at one location must never block already-landed segments from being
/// applied.
///
/// In `--json` mode the rows are held back and folded into one combined
/// object printed only after the apply below (see [`print_pull_summary`]),
/// so an apply failure in `--json` mode prints NOTHING to stdout — unlike
/// text mode, which has already printed its rows by then. Nothing is lost
/// either way: the transfer already landed on disk regardless, and the
/// next run's plan is empty for whatever transferred and simply re-applies
/// for whatever didn't.
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, the local
/// catalog can't be opened or synced, every requested location failed or
/// was skipped, or any per-file failures occurred (progress is still
/// applied and reported either way).
pub(crate) fn cmd_pull(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    args: &PullArgs,
) -> Result<()> {
    ensure_catalog(catalog)?;
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    let targets = resolve_targets(&cfg, args.location.as_deref())?;
    let results: Vec<LocationResult> = targets
        .into_iter()
        .map(|loc| transfer_one(catalog, loc, args.only, Direction::Pull))
        .collect();

    // Text rows have no single-document constraint, so they print now;
    // `--json` instead folds the same rows into the one combined object
    // printed below, once the apply below has run.
    if !args.json {
        print_text_rows(&results, "pull");
    }
    print_failure_lines(&results);

    // Apply pulled events to the local catalog BEFORE checking the exit
    // policy: a per-file blob failure elsewhere must still let already-
    // landed segments become searchable, not leave them stranded on disk
    // unapplied because an early return skipped straight past this.
    // Opening the sqlite catalog applies past its saved cursor — the open
    // IS the apply; there is no separate step to call.
    let app = FsApp::open(catalog, machine_id, author)?;
    crate::commands::open_catalog(&app, catalog)?;

    let summary = summarize_pull(&results);
    print_pull_summary(&results, &summary, args.json)?;
    check_exit_policy(&results, "pull")
}

/// What `cmd_pull` reports at the end: events newly landed, which machines
/// they came from, and how many blobs landed.
struct PullSummary {
    /// Events newly landed in the file-based event log this run — a
    /// transfer-engine fact (counted from each copied segment's new byte
    /// range), not a sqlite fact, and unaffected by whether the sqlite
    /// apply above succeeds.
    applied: usize,
    machines: Vec<String>,
    blobs_fetched: usize,
}

/// Aggregates `results` into a [`PullSummary`].
///
/// A pulled event can never be double-counted across two locations holding
/// the same segment tail: `events_added` is already aggregated per machine
/// within one location's outcome (the transfer engine's own accounting,
/// counted from the destination after each copy lands), and the second
/// location's own plan is measured against that already-caught-up
/// destination — its range has shrunk to nothing by the time it's diffed,
/// so it contributes nothing further. This only sums across locations.
fn summarize_pull(results: &[LocationResult]) -> PullSummary {
    let mut per_machine: BTreeMap<&str, usize> = BTreeMap::new();
    let mut blobs_fetched = 0usize;
    for r in results {
        if let LocationResult::Outcome { outcome, .. } = r {
            blobs_fetched += outcome.blobs_copied;
            for (machine, n) in &outcome.events_added {
                *per_machine.entry(machine.as_str()).or_default() += n;
            }
        }
    }
    let applied = per_machine.values().sum();
    let machines = per_machine.into_keys().map(str::to_string).collect();
    PullSummary {
        applied,
        machines,
        blobs_fetched,
    }
}

/// Prints `cmd_pull`'s final summary: one JSON object (`{locations,
/// applied_events, machines, blobs_fetched}`) in `--json` mode — the rows
/// folded in here rather than printed separately, so a caller sees exactly
/// one parseable document — or two text lines: what applied, and (only
/// when blobs actually landed) the `maj index run` remedy notice.
///
/// # Errors
/// Returns an error only if the JSON writer fails.
fn print_pull_summary(results: &[LocationResult], summary: &PullSummary, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "locations": json_rows(results),
                "applied_events": summary.applied,
                "machines": summary.machines,
                "blobs_fetched": summary.blobs_fetched,
            }))
            .context("serializing sync pull report")?
        );
        return Ok(());
    }
    let names = if summary.machines.is_empty() {
        String::new()
    } else {
        format!(" ({})", summary.machines.join(", "))
    };
    println!(
        "applied {} new event(s) from {} machine(s){names}",
        summary.applied,
        summary.machines.len()
    );
    if summary.blobs_fetched > 0 {
        println!(
            "fetched {} blob(s); run `maj index run` to make fetched vectors and text searchable",
            summary.blobs_fetched
        );
    }
    Ok(())
}

fn print_text_rows(results: &[LocationResult], verb: &str) {
    for r in results {
        match r {
            LocationResult::Outcome { name, outcome } => {
                let failed = if outcome.failures.is_empty() {
                    String::new()
                } else {
                    format!(", {} failed", outcome.failures.len())
                };
                println!(
                    "{name}: {verb}ed {} segment(s) ({} bytes), {} blob(s) ({} bytes){failed}",
                    outcome.segments_copied,
                    outcome.segment_bytes,
                    outcome.blobs_copied,
                    outcome.blob_bytes
                );
            }
            LocationResult::Skipped { name, reason } => println!("{name}: {reason}"),
            LocationResult::Failed { name, error } => {
                println!("{name}: transfer failed — {error}");
            }
        }
    }
}

/// Builds the JSON row for each location — shared by `cmd_push`'s own
/// `[rows...]` array document and `cmd_pull`'s merged `{locations,
/// applied_events, machines, blobs_fetched}` object, so the two can never
/// drift on row shape. A row for a location that ran is `{location,
/// segments, segment_bytes, blobs, blob_bytes, failures}`; an unreachable
/// location is `{location, skipped}`; a `plan_transfer`/`execute` setup
/// failure is `{location, error}`.
fn json_rows(results: &[LocationResult]) -> Vec<serde_json::Value> {
    results
        .iter()
        .map(|r| match r {
            LocationResult::Outcome { name, outcome } => serde_json::json!({
                "location": name,
                "segments": outcome.segments_copied,
                "segment_bytes": outcome.segment_bytes,
                "blobs": outcome.blobs_copied,
                "blob_bytes": outcome.blob_bytes,
                "failures": outcome.failures.iter().map(|(path, error)| {
                    serde_json::json!({ "path": path.display().to_string(), "error": error })
                }).collect::<Vec<_>>(),
            }),
            LocationResult::Skipped { name, reason } => serde_json::json!({
                "location": name,
                "skipped": reason,
            }),
            LocationResult::Failed { name, error } => serde_json::json!({
                "location": name,
                "error": error,
            }),
        })
        .collect()
}

/// One machine's ahead/behind segment tally within a direction: files
/// pending and the bytes the destination is missing. `saturating_sub`
/// mirrors `crates/sync/src/transfer.rs`'s own `copy_one_segment` (`bytes:
/// seg.src_len.saturating_sub(seg.dst_len)`) — both read the same
/// [`transfer::SegmentCopy`] fields, and a plain `-` would panic on
/// underflow in a debug build if the two ever disagreed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
struct SegmentCounts {
    files: usize,
    bytes: u64,
}

/// Blob counts by [`transfer::BlobClass`], always present — even at zero —
/// so the JSON contract's key set never varies with what's actually
/// pending. A struct rather than a `BTreeMap<&str, usize>`: field access
/// can't panic the way indexing a map by a hand-typed string literal could
/// (e.g. a typo'd key, or a class added to the enum but not the map).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
struct BlobCounts {
    thumbs: usize,
    metadata: usize,
    vectors: usize,
    transcripts: usize,
}

impl BlobCounts {
    fn from_blobs(blobs: &[transfer::BlobCopy]) -> Self {
        let mut counts = Self::default();
        for b in blobs {
            match b.class {
                transfer::BlobClass::Thumbs => counts.thumbs += 1,
                transfer::BlobClass::Metadata => counts.metadata += 1,
                transfer::BlobClass::Vectors => counts.vectors += 1,
                transfer::BlobClass::Transcripts => counts.transcripts += 1,
            }
        }
        counts
    }
}

/// One location's status: reachable (both directions' full plans, so text
/// and JSON rendering read the exact same walk and can never disagree),
/// unreachable (the mount isn't there), or failed (`plan_transfer` itself
/// errored — e.g. a permission problem on a location that IS mounted).
/// The latter two are both reported rows per the spec, never errors that
/// abort the rest of the report — one bad location must never hide every
/// other location's status.
enum StatusRow {
    Reachable {
        name: String,
        ahead: transfer::TransferPlan,
        behind: transfer::TransferPlan,
    },
    Unreachable {
        name: String,
        path: PathBuf,
    },
    Failed {
        name: String,
        error: String,
    },
}

/// `maj sync status`: for every configured location, plans BOTH
/// directions — what a push would send (`ahead`) and what a pull would
/// fetch (`behind`) — without executing either
/// ([`transfer::plan_transfer`] only reads; it never creates a `tmp/`
/// staging dir or touches anything, unlike [`transfer::execute`]). Every
/// count comes from a fresh diff of real files at this moment; nothing is
/// cached, so a file that changes underneath a location between two
/// `status` calls changes the next call's counts (see the
/// `status_counts_are_walked_not_cached` sabotage test). An unreachable
/// location, or one whose plan itself fails, is a reported row, never an
/// error that aborts the report — `status` exits 0 as long as at least one
/// location is configured; it reports, it doesn't enforce (push/pull carry
/// the exit policy — see [`check_exit_policy`]).
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, or no sync
/// locations are configured.
pub(crate) fn cmd_status(catalog: &Path, json: bool) -> Result<()> {
    ensure_catalog(catalog)?;
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    let targets = resolve_targets(&cfg, None)?;
    let rows: Vec<StatusRow> = targets
        .into_iter()
        .map(|loc| status_row(catalog, loc))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status_json_rows(&rows))
                .context("serializing sync status report")?
        );
        return Ok(());
    }
    print_status_rows(&rows);
    if cfg.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}

/// Plans both directions for one location. Read-only, by construction: it
/// only ever calls [`transfer::plan_transfer`], never
/// [`transfer::execute`]. A `plan_transfer` error becomes a
/// [`StatusRow::Failed`] rather than propagating out of this function.
fn status_row(catalog: &Path, loc: &Location) -> StatusRow {
    if !loc.path.is_dir() {
        return StatusRow::Unreachable {
            name: loc.name.clone(),
            path: loc.path.clone(),
        };
    }
    match plan_both_directions(catalog, &loc.path) {
        Ok((ahead, behind)) => StatusRow::Reachable {
            name: loc.name.clone(),
            ahead,
            behind,
        },
        Err(error) => StatusRow::Failed {
            name: loc.name.clone(),
            error: error.to_string(),
        },
    }
}

/// Plans `ahead` (catalog -> location) then `behind` (location -> catalog),
/// short-circuiting on the first failure. Split out of [`status_row`] so
/// the two-directions-in-one-attempt shape can use `?` instead of a manual
/// match over a tuple of two [`Result`]s.
///
/// # Errors
/// Returns [`transfer::TransferError`] if either direction's plan fails.
fn plan_both_directions(
    catalog: &Path,
    location: &Path,
) -> Result<(transfer::TransferPlan, transfer::TransferPlan), transfer::TransferError> {
    let ahead = transfer::plan_transfer(catalog, location)?;
    let behind = transfer::plan_transfer(location, catalog)?;
    Ok((ahead, behind))
}

/// Segment counts grouped per machine — the spec's granularity, unlike
/// push/pull's report, which totals every machine's bytes into one figure.
/// A plan never emits a zero-length [`transfer::SegmentCopy`], so every
/// entry here is already nonzero: both JSON and text rendering can iterate
/// this directly with no separate filter.
fn segments_by_machine(segments: &[transfer::SegmentCopy]) -> BTreeMap<String, SegmentCounts> {
    let mut by_machine: BTreeMap<String, SegmentCounts> = BTreeMap::new();
    for s in segments {
        let counts = by_machine.entry(s.machine.clone()).or_default();
        counts.files += 1;
        counts.bytes += s.src_len.saturating_sub(s.dst_len);
    }
    by_machine
}

/// True when a plan has nothing pending in either segments or blobs — the
/// collapse condition for text mode's `<name>: in sync` line.
fn plan_is_empty(plan: &transfer::TransferPlan) -> bool {
    plan.segments.is_empty() && plan.blobs.is_empty()
}

/// One direction's (`ahead` or `behind`) JSON shape:
/// `{"segments": {"<machine>": {"files", "bytes"}, ...}, "blobs": {"thumbs", "metadata", "vectors", "transcripts"}}`.
fn direction_json(plan: &transfer::TransferPlan) -> serde_json::Value {
    serde_json::json!({
        "segments": segments_by_machine(&plan.segments),
        "blobs": BlobCounts::from_blobs(&plan.blobs),
    })
}

fn status_json_rows(rows: &[StatusRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|r| match r {
            StatusRow::Reachable {
                name,
                ahead,
                behind,
            } => serde_json::json!({
                "location": name,
                "reachable": true,
                "ahead": direction_json(ahead),
                "behind": direction_json(behind),
            }),
            StatusRow::Unreachable { name, path } => serde_json::json!({
                "location": name,
                "reachable": false,
                "path": path,
            }),
            StatusRow::Failed { name, error } => serde_json::json!({
                "location": name,
                "error": error,
            }),
        })
        .collect()
}

fn print_status_rows(rows: &[StatusRow]) {
    for row in rows {
        match row {
            StatusRow::Reachable {
                name,
                ahead,
                behind,
            } => print_reachable_row(name, ahead, behind),
            StatusRow::Unreachable { name, path } => {
                println!(
                    "{name}: unreachable at {} — mount it and retry",
                    path.display()
                );
            }
            StatusRow::Failed { name, error } => {
                println!("{name}: status failed — {error}");
            }
        }
    }
}

/// Prints one reachable location's text report: a single `<name>: in sync`
/// line when both directions have nothing pending, otherwise a `<name>:`
/// header followed by one indented line per direction — never the old
/// per-line `{name}: {label}:` prefix repeated across every segment and
/// blob line.
fn print_reachable_row(
    name: &str,
    ahead: &transfer::TransferPlan,
    behind: &transfer::TransferPlan,
) {
    if plan_is_empty(ahead) && plan_is_empty(behind) {
        println!("{name}: in sync");
        return;
    }
    println!("{name}:");
    print_direction("ahead (push would send)", ahead);
    print_direction("behind (pull would fetch)", behind);
}

/// Prints one direction as a single indented line: the per-machine segment
/// tally (joined by commas when more than one machine is pending, or
/// `0 segment(s)` when none), then the blob-class counts — always shown,
/// even at zero, so a converged direction still reads as explicitly
/// checked rather than silently omitted.
fn print_direction(label: &str, plan: &transfer::TransferPlan) {
    let segments = segments_by_machine(&plan.segments);
    let segment_summary = if segments.is_empty() {
        "0 segment(s)".to_string()
    } else {
        segments
            .iter()
            .map(|(machine, counts)| {
                format!(
                    "{machine}: {} segment(s) ({} bytes)",
                    counts.files, counts.bytes
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let blobs = BlobCounts::from_blobs(&plan.blobs);
    println!(
        "  {label}: {segment_summary}, blobs: thumbs {} / metadata {} / vectors {} / transcripts {}",
        blobs.thumbs, blobs.metadata, blobs.vectors, blobs.transcripts
    );
}

pub(crate) fn cmd_location_list(catalog: &Path, json: bool) -> Result<()> {
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "readonly": cfg.readonly,
                "locations": &cfg.locations,
            }))?
        );
        return Ok(());
    }
    if cfg.locations.is_empty() {
        println!("{NO_LOCATIONS_HINT}");
        return Ok(());
    }
    for l in &cfg.locations {
        println!("{}\t{}", l.name, l.path.display());
    }
    if cfg.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_and_defaults_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loaded = SyncConfig::load(&path).expect("load missing");
        assert!(!loaded.readonly);
        assert!(loaded.locations.is_empty());

        let mut location_extra = toml::Table::new();
        location_extra.insert("future-field".into(), toml::Value::String("kept".into()));
        let config = SyncConfig {
            readonly: true,
            locations: vec![Location {
                name: "nas".into(),
                path: "/Volumes/Team/sync".into(),
                extra: location_extra,
            }],
            extra: toml::Table::new(),
        };
        config.store(&path).expect("store");
        let loaded = SyncConfig::load(&path).expect("load");
        assert_eq!(loaded, config);

        // An unknown key (as a newer `maj` might have written) survives a
        // load -> store -> load round trip untouched.
        loaded.store(&path).expect("store again");
        let reloaded = SyncConfig::load(&path).expect("reload");
        assert_eq!(reloaded, loaded);
        assert_eq!(
            reloaded.locations[0].extra.get("future-field"),
            Some(&toml::Value::String("kept".into()))
        );
    }

    #[test]
    fn add_rejects_duplicate_names_and_rm_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "nas", &loc).expect("first add");
        assert!(
            loc.join("events").is_dir() && loc.join("blobs").is_dir(),
            "add initializes the events/ + blobs/ skeleton"
        );
        let err = add_location(&path, "nas", &loc).expect_err("dup must fail");
        assert!(err.to_string().contains("already configured"));
        let err = remove_location(&path, "ghost").expect_err("unknown rm");
        assert!(err.to_string().contains("no sync location named"));
    }

    #[test]
    fn add_rejects_an_unreachable_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let err = add_location(&path, "nas", &dir.path().join("missing"))
            .expect_err("unreachable path must fail");
        assert!(err.to_string().contains("not an accessible directory"));
    }

    #[test]
    fn add_rejects_an_empty_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let err = add_location(&path, "  ", dir.path()).expect_err("empty name must fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn add_stores_a_canonicalized_absolute_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "nas", &loc).expect("add");
        let cfg = SyncConfig::load(&path).expect("load");
        let stored = &cfg.locations[0].path;
        assert!(stored.is_absolute());
        assert_eq!(*stored, loc.canonicalize().expect("canonicalize"));
    }

    #[test]
    fn add_stores_a_trimmed_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "  nas  ", &loc).expect("add");
        let cfg = SyncConfig::load(&path).expect("load");
        assert_eq!(cfg.locations[0].name, "nas");
    }
}
