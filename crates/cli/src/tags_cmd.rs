//! `maj tags list`: the catalog's live vocabulary. Suggestion review: list
//! pending, confirm into the folksonomy, reject into a per-machine jsonl
//! (never synced, survives projection rebuilds). Compute for all of these
//! lives in `majestical_services::tags`; this module only renders.
use anyhow::Result;
use majestical_services::app::FsApp;
use majestical_services::iso8601::iso8601_ms;
use majestical_services::tags::TagRow;
use std::path::Path;

/// `maj tags list [--json]`: every live tag after alias resolution, sorted
/// by name, with its asset count and newest surviving add time.
///
/// `--json` prints [`majestical_services::tags::TagsListOutcome`] AS-IS —
/// see `crates/cli/src/commands.rs::cmd_browse_tree`'s doc for the policy
/// this follows (every outcome struct is already the wire contract for the
/// GUI and MCP heads, so the CLI's `--json` reshapes nothing).
///
/// # Errors
/// Returns [`majestical_services::error::ServiceError::NoCatalog`] if
/// there's no catalog at `catalog_root`, or an error if the event log can't
/// be read.
pub(crate) fn cmd_tags_list(app: &FsApp, catalog_root: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::tags::tags_list(app, catalog_root)?;
    crate::print_notices(&outcome.notices);
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        print_tags_table(&outcome.tags);
    }
    Ok(())
}

/// Renders the human-readable tag-vocabulary table, following
/// `commands.rs::print_volumes_table`'s width-sizing pattern.
fn print_tags_table(tags: &[TagRow]) {
    let tag_w = tags.iter().map(|r| r.tag.len()).max().unwrap_or(0).max(3);
    let count_w = tags
        .iter()
        .map(|r| r.count.to_string().len())
        .max()
        .unwrap_or(0)
        .max(5);
    println!("{:<tag_w$} {:<count_w$} LAST USED", "TAG", "COUNT");
    for row in tags {
        let last_used = iso8601_ms(row.last_used_ms);
        println!("{:<tag_w$} {:<count_w$} {last_used}", row.tag, row.count);
    }
}

/// `maj tags suggestions`: lists every pending AI tag suggestion across the
/// catalog, sorted by asset then tag.
///
/// # Errors
/// Returns an error if the event log can't be read or the state dir can't
/// be resolved.
pub(crate) fn cmd_suggestions(app: &FsApp, catalog_root: &Path) -> Result<()> {
    let outcome = majestical_services::tags::suggestions(app, catalog_root)?;
    crate::print_notices(&outcome.notices);
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
    crate::surface_err_notices(majestical_services::tags::confirm(app, asset, tags))?;
    for tag in tags {
        println!("confirmed: {asset} {tag}");
    }
    println!("{} tag(s) confirmed on {asset}", tags.len());
    Ok(())
}

/// `maj tags reject <asset> <tag>...`: appends each pair to this machine's
/// rejection log. Never touches the event log — a rejection is a
/// per-machine "stop suggesting this" note, not a fact synced to
/// teammates.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the rejection
/// log can't be opened/appended.
pub(crate) fn cmd_reject(catalog_root: &Path, asset: &str, tags: &[String]) -> Result<()> {
    let notices = majestical_services::notices::Notices::new();
    let result = majestical_services::tags::reject(catalog_root, asset, tags, &notices);
    // `reject` attaches the sink to an `Err`, so on that path the drain
    // below finds nothing and the carrier's split prints the same lines in
    // the same place; on `Ok` the drain is still what surfaces them.
    crate::drain_notices(&notices);
    crate::surface_err_notices(result)?;
    println!(
        "{} tag(s) rejected on {asset} (this machine only)",
        tags.len()
    );
    Ok(())
}
