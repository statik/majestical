//! Suggestion review: list pending, confirm into the folksonomy, reject
//! into a per-machine jsonl (never synced, survives projection rebuilds).
//! Listing's compute lives in `majestical_services::tags`; this module
//! renders it and owns the two writing verbs (`confirm`/`reject`).
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op};
use majestical_services::app::FsApp;
use majestical_services::catalog::ensure_asset_known;
use majestical_services::tags::{Rejection, rejections_path};
use std::io::Write as _;
use std::path::Path;

/// `maj tags suggestions`: lists every pending AI tag suggestion across the
/// catalog, sorted by asset then tag.
///
/// # Errors
/// Returns an error if the event log can't be read or the state dir can't
/// be resolved.
pub(crate) fn cmd_suggestions(app: &FsApp, catalog_root: &Path) -> Result<()> {
    let outcome = majestical_services::tags::suggestions(app, catalog_root)?;
    if outcome.pending.is_empty() {
        println!(
            "no pending suggestions — captions/tags derive during \
             `maj index run` with a describer configured"
        );
        return Ok(());
    }
    for entry in &outcome.pending {
        let vocab = if entry.in_vocab { "known" } else { "new" };
        println!(
            "{}  {}  {:.2}  {vocab}  {}",
            entry.asset, entry.tag, entry.confidence, entry.model_tag
        );
    }
    println!("{} pending suggestion(s)", outcome.pending.len());
    println!(
        "confirm with `maj tags confirm <asset> <tag>...`, \
         reject with `maj tags reject <asset> <tag>...`"
    );
    Ok(())
}

/// `maj tags confirm <asset> <tag>...`: emits a plain `TagAdd` per tag —
/// the same validation and op shape as `maj tag add`, so a confirmed
/// suggestion is indistinguishable from a hand-added tag in the event log.
///
/// # Errors
/// Returns an error if the asset has never been scanned (no
/// `AssetSeen` on record) or the event log can't be read/appended.
pub(crate) fn cmd_confirm(app: &mut FsApp, asset: &str, tags: &[String]) -> Result<()> {
    let projection = app.projection()?;
    let asset_id = AssetId(asset.to_string());
    ensure_asset_known(&projection, &asset_id)?;
    let ops = tags
        .iter()
        .map(|tag| Op::TagAdd {
            asset: asset_id.clone(),
            tag: tag.clone(),
        })
        .collect();
    app.emit(ops)?;
    for tag in tags {
        println!("confirmed: {asset} {tag}");
    }
    println!("{} tag(s) confirmed on {asset}", tags.len());
    Ok(())
}

/// `maj tags reject <asset> <tag>...`: appends each pair to this machine's
/// rejection log. Never touches the event log — a rejection is a
/// per-machine "stop suggesting this" note, not a fact synced to
/// teammates. The pair is recorded as given, without checking it against
/// any current suggestion: a typo'd asset or tag just writes a rejection
/// that never matches anything, a harmless no-op line, rather than paying
/// for a full blob scan on every reject to validate it up front.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the rejection
/// log can't be opened/appended.
pub(crate) fn cmd_reject(catalog_root: &Path, asset: &str, tags: &[String]) -> Result<()> {
    let path = rejections_path(catalog_root)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    for tag in tags {
        let line = serde_json::to_string(&Rejection {
            asset: asset.to_string(),
            tag: tag.clone(),
        })
        .context("serializing rejection")?;
        writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    }
    println!(
        "{} tag(s) rejected on {asset} (this machine only)",
        tags.len()
    );
    Ok(())
}
