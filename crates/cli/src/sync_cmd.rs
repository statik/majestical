//! `maj sync`: renders push/pull/status/location results computed by
//! `majestical_services::sync`. Locations are per-machine config (mount
//! points differ per machine) in the state dir's `sync.toml`, never synced.
//! Compute for every verb lives in `majestical_services::sync`; this module
//! only reads clap's `--only` flag (which can't live in `services` since it
//! derives `clap::ValueEnum`), converts it, and renders.

use anyhow::{Context, Result};
use majestical_services::sync::{
    LocationRow, NO_LOCATIONS_HINT, Only, PullOutcome, PullRequest, PushRequest,
};
use std::path::Path;

/// `--only` surface for `maj sync push` and `maj sync pull`.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OnlyArg {
    Segments,
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

impl From<OnlyArg> for Only {
    fn from(value: OnlyArg) -> Self {
        match value {
            OnlyArg::Segments => Self::Segments,
            OnlyArg::Thumbs => Self::Thumbs,
            OnlyArg::Metadata => Self::Metadata,
            OnlyArg::Vectors => Self::Vectors,
            OnlyArg::Transcripts => Self::Transcripts,
        }
    }
}

pub(crate) fn cmd_location_add(catalog: &Path, name: &str, location: &Path) -> Result<()> {
    majestical_services::sync::location_add(catalog, name, location)?;
    println!("added sync location '{name}' at {}", location.display());
    Ok(())
}

pub(crate) fn cmd_location_rm(catalog: &Path, name: &str) -> Result<()> {
    majestical_services::sync::location_rm(catalog, name)?;
    println!("removed sync location '{name}' (its files were not touched)");
    Ok(())
}

/// The per-file failure lines every report (push or pull, either output
/// format) prints to stderr: `<location>: failed <path>: <reason>`, one per
/// entry in a ran location's failures.
fn print_failure_lines(rows: &[LocationRow]) {
    for r in rows {
        if let LocationRow::Ran { name, failures, .. } = r {
            for f in failures {
                eprintln!("{name}: failed {}: {}", f.path.display(), f.error);
            }
        }
    }
}

/// Enforces the exit policy over already-reported `rows`: nonzero when
/// EVERY requested location was skipped, failed outright, or otherwise
/// never ran, and ALSO when a location that DID run had per-file failures
/// within its own transfer — see
/// `majestical_services::sync::PushOutcome::overall_failed`'s doc for why.
/// This reconstructs the same two conditions (rather than just branching on
/// `overall_failed()`) so the two distinct cases keep their own precise
/// message naming which locations were at fault.
///
/// # Errors
/// See above.
fn check_exit_policy(rows: &[LocationRow], verb: &str) -> Result<()> {
    anyhow::ensure!(
        rows.iter().any(|r| matches!(r, LocationRow::Ran { .. })),
        "sync {verb} failed for every requested location ({}) — check they're mounted and reachable",
        rows.iter()
            .map(LocationRow::name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let failing: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            LocationRow::Ran { name, failures, .. } if !failures.is_empty() => Some(name.as_str()),
            LocationRow::Ran { .. } | LocationRow::Skipped { .. } | LocationRow::Failed { .. } => {
                None
            }
        })
        .collect();
    anyhow::ensure!(
        failing.is_empty(),
        "sync {verb} had per-file failures at {} — progress was kept; the next run retries",
        failing.join(", ")
    );
    Ok(())
}

/// `maj sync push`: replicate everything this catalog has (segments +
/// blobs) to configured locations. Compute lives in
/// `majestical_services::sync::push`; this renders its
/// [`majestical_services::sync::PushOutcome`].
///
/// # Errors
/// See `majestical_services::sync::push`'s doc, plus a nonzero exit when
/// every requested location failed/was skipped or any per-file failures
/// occurred (progress is still kept and reported either way).
pub(crate) fn cmd_push(
    catalog: &Path,
    location: Option<&str>,
    only: Option<OnlyArg>,
    json: bool,
) -> Result<()> {
    let outcome = majestical_services::sync::push(
        catalog,
        &PushRequest {
            location,
            only: only.map(Into::into),
        },
    )?;

    // Text rows, then failure lines (always, to stderr), then the JSON
    // document (a different stream — reordering it after the failure
    // lines changes nothing a test can observe), then the exit-policy
    // check last — same tail shape as `cmd_pull`.
    if !json {
        print_text_rows(&outcome.rows, "push");
    }
    print_failure_lines(&outcome.rows);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_rows(&outcome.rows))
                .context("serializing sync report")?
        );
    }
    check_exit_policy(&outcome.rows, "push")
}

/// Bundles `maj sync pull`'s flags within the house 5-positional-parameter
/// limit.
pub(crate) struct PullArgs {
    pub location: Option<String>,
    pub only: Option<OnlyArg>,
    pub json: bool,
}

/// `maj sync pull`: fetch everything configured locations have that this
/// catalog doesn't (segments + blobs), then apply the newly landed events to
/// the local sqlite catalog. Compute (including the apply) lives in
/// `majestical_services::sync::pull`; this renders its
/// [`majestical_services::sync::PullOutcome`].
///
/// In `--json` mode the rows are held back and folded into one combined
/// object printed only after the service call returns (see
/// [`print_pull_summary`]), so an apply failure in `--json` mode prints
/// NOTHING to stdout — unlike text mode, which has already printed its rows
/// by then. Nothing is lost either way: the transfer already landed on disk
/// regardless, and the next run's plan is empty for whatever transferred
/// and simply re-applies for whatever didn't.
///
/// # Errors
/// See `majestical_services::sync::pull`'s doc, plus a nonzero exit under
/// the same policy as [`cmd_push`].
pub(crate) fn cmd_pull(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    args: &PullArgs,
) -> Result<()> {
    let outcome = majestical_services::sync::pull(
        catalog,
        machine_id,
        author,
        &PullRequest {
            location: args.location.as_deref(),
            only: args.only.map(Into::into),
        },
    )?;

    if !args.json {
        print_text_rows(&outcome.rows, "pull");
    }
    print_failure_lines(&outcome.rows);
    print_pull_summary(&outcome, args.json)?;
    check_exit_policy(&outcome.rows, "pull")
}

/// Prints `cmd_pull`'s final summary: one JSON object (`{locations,
/// applied_events, machines, blobs_fetched}`) in `--json` mode — the rows
/// folded in here rather than printed separately, so a caller sees exactly
/// one parseable document — or two text lines: what applied, and (only
/// when blobs actually landed) the `maj index run` remedy notice.
///
/// # Errors
/// Returns an error only if the JSON writer fails.
fn print_pull_summary(outcome: &PullOutcome, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "locations": json_rows(&outcome.rows),
                "applied_events": outcome.applied_events,
                "machines": outcome.machines,
                "blobs_fetched": outcome.blobs_fetched,
            }))
            .context("serializing sync pull report")?
        );
        return Ok(());
    }
    let names = if outcome.machines.is_empty() {
        String::new()
    } else {
        format!(" ({})", outcome.machines.join(", "))
    };
    println!(
        "applied {} new event(s) from {} machine(s){names}",
        outcome.applied_events,
        outcome.machines.len()
    );
    if outcome.blobs_fetched > 0 {
        println!(
            "fetched {} blob(s); run `maj index run` to make fetched vectors and text searchable",
            outcome.blobs_fetched
        );
    }
    Ok(())
}

fn print_text_rows(rows: &[LocationRow], verb: &str) {
    for r in rows {
        match r {
            LocationRow::Ran {
                name,
                segments_copied,
                segment_bytes,
                blobs_copied,
                blob_bytes,
                failures,
                ..
            } => {
                let failed = if failures.is_empty() {
                    String::new()
                } else {
                    format!(", {} failed", failures.len())
                };
                println!(
                    "{name}: {verb}ed {segments_copied} segment(s) ({segment_bytes} bytes), {blobs_copied} blob(s) ({blob_bytes} bytes){failed}"
                );
            }
            LocationRow::Skipped { name, reason } => println!("{name}: {reason}"),
            LocationRow::Failed { name, error } => {
                println!("{name}: transfer failed — {error}");
            }
        }
    }
}

/// Builds the JSON row for each location — shared by `cmd_push`'s own
/// `[rows...]` array document and `cmd_pull`'s merged `{locations,
/// applied_events, machines, blobs_fetched}` object, so the two can never
/// drift on row shape. A row that ran is `{location, segments,
/// segment_bytes, blobs, blob_bytes, failures}`; an unreachable location is
/// `{location, skipped}`; a `plan_transfer`/`execute` setup failure is
/// `{location, error}`.
fn json_rows(rows: &[LocationRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|r| match r {
            LocationRow::Ran {
                name,
                segments_copied,
                segment_bytes,
                blobs_copied,
                blob_bytes,
                failures,
                ..
            } => serde_json::json!({
                "location": name,
                "segments": segments_copied,
                "segment_bytes": segment_bytes,
                "blobs": blobs_copied,
                "blob_bytes": blob_bytes,
                "failures": failures.iter().map(|f| {
                    serde_json::json!({ "path": f.path.display().to_string(), "error": f.error })
                }).collect::<Vec<_>>(),
            }),
            LocationRow::Skipped { name, reason } => serde_json::json!({
                "location": name,
                "skipped": reason,
            }),
            LocationRow::Failed { name, error } => serde_json::json!({
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
    fn check_exit_policy_names_every_location_when_all_fail() {
        let rows = vec![
            LocationRow::Skipped {
                name: "shuttle-drive".into(),
                reason: "unreachable".into(),
            },
            LocationRow::Failed {
                name: "attic-nas".into(),
                error: "boom".into(),
            },
        ];
        let err = check_exit_policy(&rows, "push").expect_err("all failed must error");
        let msg = err.to_string();
        assert!(
            msg.contains("shuttle-drive") && msg.contains("attic-nas"),
            "the all-failed message must name every location: {msg}"
        );
    }
}
