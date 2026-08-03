//! `maj sync`: location config plus push/pull/status orchestration over
//! `crates/sync`'s transfer engine. Locations are per-machine config
//! (mount points differ per machine) in the state dir's `sync.toml`,
//! never synced. `SyncConfig`/`Location`/`config_path`/`resolve_targets`/
//! `NO_LOCATIONS_HINT` live in `majestical_services::sync`, shared with
//! `status`/`location list`'s compute there; this module keeps push/pull
//! (which transfer files and apply landed events) plus location add/rm and
//! rendering.

use anyhow::{Context, Result};
use majestical_services::app::FsApp;
use majestical_services::sync::{
    Location, NO_LOCATIONS_HINT, SyncConfig, config_path, resolve_targets,
};
use majestical_sync::transfer;
use std::collections::BTreeMap;
use std::path::Path;

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
    if catalog.join("events").is_dir() {
        Ok(())
    } else {
        Err(majestical_services::error::ServiceError::NoCatalog {
            root: catalog.to_path_buf(),
        }
        .into())
    }
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
    majestical_services::catalog::open_catalog(&app, catalog)?;

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

/// `maj sync status`: for every configured location, plans BOTH
/// directions — what a push would send (`ahead`) and what a pull would
/// fetch (`behind`) — without executing either. Compute (the walk itself,
/// unreachable/failed detection, per-machine/per-class counting) lives in
/// `majestical_services::sync::status`; this renders its
/// [`majestical_services::sync::StatusRow`]s.
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, or no sync
/// locations are configured.
pub(crate) fn cmd_status(catalog: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::sync::status(catalog)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status_json_rows(&outcome.rows))
                .context("serializing sync status report")?
        );
        return Ok(());
    }
    print_status_rows(&outcome.rows);
    if outcome.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}

/// One direction's (`ahead` or `behind`) JSON shape:
/// `{"segments": {"<machine>": {"files", "bytes"}, ...}, "blobs": {"thumbs", "metadata", "vectors", "transcripts"}}`.
fn direction_json(counts: &majestical_services::sync::DirectionCounts) -> serde_json::Value {
    serde_json::json!({
        "segments": counts.segments,
        "blobs": counts.blobs,
    })
}

fn status_json_rows(rows: &[majestical_services::sync::StatusRow]) -> Vec<serde_json::Value> {
    use majestical_services::sync::StatusRow;
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

fn print_status_rows(rows: &[majestical_services::sync::StatusRow]) {
    use majestical_services::sync::StatusRow;
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
/// blob line. The "in sync" collapse is a render-time decision over
/// already-computed counts — [`majestical_services::sync::DirectionCounts::is_empty`]
/// does the actual emptiness check.
fn print_reachable_row(
    name: &str,
    ahead: &majestical_services::sync::DirectionCounts,
    behind: &majestical_services::sync::DirectionCounts,
) {
    if ahead.is_empty() && behind.is_empty() {
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
fn print_direction(label: &str, counts: &majestical_services::sync::DirectionCounts) {
    let segment_summary = if counts.segments.is_empty() {
        "0 segment(s)".to_string()
    } else {
        counts
            .segments
            .iter()
            .map(|(machine, c)| format!("{machine}: {} segment(s) ({} bytes)", c.files, c.bytes))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let blobs = &counts.blobs;
    println!(
        "  {label}: {segment_summary}, blobs: thumbs {} / metadata {} / vectors {} / transcripts {}",
        blobs.thumbs, blobs.metadata, blobs.vectors, blobs.transcripts
    );
}

pub(crate) fn cmd_location_list(catalog: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::sync::locations_list(catalog)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "readonly": outcome.readonly,
                "locations": &outcome.locations,
            }))?
        );
        return Ok(());
    }
    if outcome.locations.is_empty() {
        println!("{NO_LOCATIONS_HINT}");
        return Ok(());
    }
    for l in &outcome.locations {
        println!("{}\t{}", l.name, l.path.display());
    }
    if outcome.readonly {
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

    #[test]
    #[cfg(unix)]
    fn load_propagates_a_non_not_found_io_error() {
        // Mirrors DescriberConfig::load's own NotFound guard test (phase 5
        // triage): a mutant that widens the guard to match every error
        // would fold a permission-denied config into "no config yet"
        // instead of surfacing it.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        std::fs::write(&path, "readonly = false\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
        let result = SyncConfig::load(&path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms");
        assert!(
            result.is_err(),
            "a permission-denied config must not be treated as absent: {result:?}"
        );
    }

    #[test]
    fn check_exit_policy_names_every_location_when_all_fail() {
        let results = vec![
            LocationResult::Skipped {
                name: "shuttle-drive".into(),
                reason: "unreachable".into(),
            },
            LocationResult::Failed {
                name: "attic-nas".into(),
                error: "boom".into(),
            },
        ];
        let err = check_exit_policy(&results, "push").expect_err("all failed must error");
        let msg = err.to_string();
        assert!(
            msg.contains("shuttle-drive") && msg.contains("attic-nas"),
            "the all-failed message must name every location: {msg}"
        );
    }

    #[test]
    fn summarize_pull_sums_events_across_locations() {
        let results = vec![
            LocationResult::Outcome {
                name: "a".into(),
                outcome: transfer::TransferOutcome {
                    events_added: vec![("m1".into(), 3)],
                    blobs_copied: 2,
                    ..Default::default()
                },
            },
            LocationResult::Outcome {
                name: "b".into(),
                outcome: transfer::TransferOutcome {
                    events_added: vec![("m1".into(), 4), ("m2".into(), 1)],
                    blobs_copied: 5,
                    ..Default::default()
                },
            },
        ];
        let summary = summarize_pull(&results);
        assert_eq!(summary.applied, 8, "events must sum across locations");
        assert_eq!(summary.blobs_fetched, 7, "blobs must sum across locations");
        assert_eq!(summary.machines, vec!["m1".to_string(), "m2".to_string()]);
    }
}
