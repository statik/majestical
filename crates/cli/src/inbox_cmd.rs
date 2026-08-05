//! `maj inbox process`: renders the pass report computed by
//! `majestical_services::inbox::process`. See that module's doc for the
//! full behavior (manifested contributions, manifest-less triage,
//! quiescence, the failure-marker store); this module only bundles the
//! CLI's flags and prints.
use anyhow::Result;
use majestical_services::app::FsApp;
use majestical_services::inbox::{ContribOutcome, InboxOutcome, ProcessRequest};
use std::path::{Path, PathBuf};

/// Bundles the flags within the house 5-positional-parameter limit.
pub(crate) struct InboxArgs {
    pub inbox: PathBuf,
    pub dest: Vec<PathBuf>,
    /// PARA node for manifest-less drops; required once any quiescent
    /// manifest-less item is present.
    pub triage_target: Option<String>,
    pub keep: bool,
    pub json: bool,
}

/// One converging pass over `args.inbox`. Compute lives in
/// `majestical_services::inbox::process`; this renders its
/// [`InboxOutcome`].
///
/// # Errors
/// See `majestical_services::inbox::process`'s doc, plus a nonzero exit if
/// any contribution freshly failed this run — a previously recorded
/// failure is only a notice, not an error (see [`print_report`]).
pub(crate) fn cmd_inbox_process(app: &mut FsApp, catalog: &Path, args: &InboxArgs) -> Result<()> {
    let outcome = majestical_services::inbox::process(
        app,
        catalog,
        &ProcessRequest {
            inbox: args.inbox.clone(),
            dest: args.dest.clone(),
            triage_target: args.triage_target.clone(),
            keep: args.keep,
        },
    )?;
    print_report(&outcome, args.json)
}

/// Prints every diagnostic the pass collected (to stderr, ahead of the
/// report, so each line keeps the position it had when services printed it
/// directly), then the pass report, then applies the exit policy: a FRESH failure
/// (this run) fails the pass; a previously RECORDED failure is a notice
/// only. Every fresh failure's detail also goes to stderr unconditionally
/// (not just in text mode) — with `--json`, stdout carries only the JSON
/// blob, so this is the only place that detail reaches the operator.
fn print_report(outcome: &InboxOutcome, json: bool) -> Result<()> {
    crate::print_notices(&outcome.notices);
    if json {
        print_report_json(&outcome.rows)?;
    } else if outcome.rows.is_empty() {
        println!("nothing to process");
    } else {
        print_report_text(&outcome.rows);
    }
    for row in &outcome.rows {
        if let ContribOutcome::Failed { reason } = &row.outcome {
            eprintln!("{}: {reason}", row.name);
        }
    }
    anyhow::ensure!(
        !outcome.overall_failed(),
        "one or more items failed — see stdout for the full report"
    );
    Ok(())
}

fn print_report_json(rows: &[majestical_services::inbox::ContribRow]) -> Result<()> {
    let json_rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| match &row.outcome {
            ContribOutcome::Ingested {
                placed,
                skipped_duplicates,
            } => {
                let mut r = serde_json::json!({
                    "contribution": row.name, "status": "ingested", "placed": placed
                });
                if *skipped_duplicates > 0 {
                    r["skipped_duplicates"] = serde_json::json!(skipped_duplicates);
                }
                r
            }
            ContribOutcome::PartlyIngested {
                placed,
                skipped_duplicates,
                failed,
            } => {
                let mut r = serde_json::json!({
                    "contribution": row.name, "status": "partly_ingested",
                    "placed": placed, "failed": failed
                });
                if *skipped_duplicates > 0 {
                    r["skipped_duplicates"] = serde_json::json!(skipped_duplicates);
                }
                r
            }
            ContribOutcome::Waiting { reasons } => {
                serde_json::json!({"contribution": row.name, "status": "waiting", "reasons": reasons})
            }
            ContribOutcome::RecordedFailure { reason } => {
                serde_json::json!({
                    "contribution": row.name, "status": "recorded_failure", "reason": reason
                })
            }
            ContribOutcome::Failed { reason } => {
                serde_json::json!({"contribution": row.name, "status": "failed", "reason": reason})
            }
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json_rows)?);
    Ok(())
}

fn print_report_text(rows: &[majestical_services::inbox::ContribRow]) {
    for row in rows {
        let name = &row.name;
        match &row.outcome {
            ContribOutcome::Ingested {
                placed,
                skipped_duplicates,
            } if *skipped_duplicates > 0 => {
                println!("{name}: ingested {placed} file(s), {skipped_duplicates} already known");
            }
            ContribOutcome::Ingested { placed, .. } => {
                println!("{name}: ingested {placed} file(s)");
            }
            ContribOutcome::PartlyIngested {
                placed,
                skipped_duplicates,
                failed,
            } if *skipped_duplicates > 0 => {
                println!(
                    "{name}: PARTIAL — ingested {placed} file(s), {skipped_duplicates} already \
                     known, {failed} FAILED — see stderr"
                );
            }
            ContribOutcome::PartlyIngested { placed, failed, .. } => {
                println!(
                    "{name}: PARTIAL — ingested {placed} file(s), {failed} FAILED — see stderr"
                );
            }
            ContribOutcome::Waiting { reasons } => {
                println!("{name}: waiting — {}", reasons.join("; "));
            }
            ContribOutcome::RecordedFailure { reason } => {
                println!("{name}: skipped (recorded failure) — {reason}");
            }
            ContribOutcome::Failed { reason } => println!("{name}: FAILED — {reason}"),
        }
    }
}
