//! Suggestion review: list pending, confirm into the folksonomy, reject
//! into a per-machine jsonl (never synced, survives projection rebuilds).
use crate::commands::ensure_asset_known;
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op};
use majestical_core::ports::TagSuggestion;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;
use majestical_services::app::FsApp;
use majestical_services::state_dir;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// One rejected (asset, tag) pair, as recorded in the per-machine
/// rejections jsonl.
#[derive(serde::Serialize, serde::Deserialize)]
struct Rejection {
    asset: String,
    tag: String,
}

/// Path to this machine's rejection log — per-machine, append-only, never
/// synced (it lives in the local state dir, not the sync-root event log),
/// so a rejection survives a projection rebuild but never propagates to a
/// teammate's machine.
fn rejections_path(catalog_root: &Path) -> Result<PathBuf> {
    Ok(state_dir::state_dir_for(catalog_root)?.join("tag-rejections.jsonl"))
}

/// Loads every rejection ever appended on this machine. A missing file
/// means no rejections yet — not an error, since the file is created lazily
/// by the first `tags reject`. A malformed line, on the other hand, fails
/// fast: this file is append-only and owned entirely by [`cmd_reject`], so
/// a line that doesn't parse means something else corrupted it (a torn
/// write, a stray edit), and silently dropping it would silently resurface
/// a tag the user already rejected.
fn load_rejections(catalog_root: &Path) -> Result<BTreeSet<(String, String)>> {
    let path = rejections_path(catalog_root)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut rejections = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rejection: Rejection = serde_json::from_str(line).with_context(|| {
            format!(
                "corrupt rejection at {}:{}: {line:?}",
                path.display(),
                i + 1
            )
        })?;
        rejections.insert((rejection.asset, rejection.tag));
    }
    Ok(rejections)
}

/// One suggestion still awaiting human review.
struct Pending {
    asset: String,
    suggestion: TagSuggestion,
}

/// Reads a zstd-compressed JSON `Vec<TagSuggestion>` blob.
fn read_tags_blob(path: &Path) -> Result<Vec<TagSuggestion>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let json = zstd::decode_all(bytes.as_slice())
        .with_context(|| format!("decompressing {}", path.display()))?;
    serde_json::from_slice(&json).with_context(|| format!("parsing {}", path.display()))
}

/// Every suggestion not yet acted on: not already a tag on the asset (the
/// projection's folksonomy is the source of truth there), and not already
/// rejected on this machine. Suggestions themselves come from
/// `tags.json.zst` blobs written by the caption runner — derived data, so
/// "pending" is always computed live rather than persisted anywhere.
/// A blob that fails to read or decode is skipped with a stderr note
/// rather than failing the whole listing — it may be mid-write by another
/// process, and one bad blob shouldn't hide every other asset's
/// suggestions. Blobs from more than one describer model tag can list the
/// same `(asset, tag)` pair twice, once per model tag — each row is kept
/// (the model tag is part of what's displayed), and confirming or
/// rejecting that tag clears every one of them at once, since both act on
/// the `(asset, tag)` pair alone.
fn pending_suggestions(catalog_root: &Path, projection: &Projection) -> Result<Vec<Pending>> {
    let blobs = BlobStore::new(catalog_root);
    let rejections = load_rejections(catalog_root)?;
    let mut pending = Vec::new();
    for (asset_hex, _model_tag, path) in blobs
        .iter_named("tags.json.zst")
        .context("walking tag-suggestion blobs")?
    {
        let asset = format!("xxh3:{asset_hex}");
        let suggestions = match read_tags_blob(&path) {
            Ok(suggestions) => suggestions,
            Err(error) => {
                eprintln!(
                    "note: skipping unreadable tag-suggestions blob {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let asset_id = AssetId(asset.clone());
        let existing = projection.tags(&asset_id);
        for suggestion in suggestions {
            let already_tagged = existing.contains(&suggestion.tag);
            let rejected = rejections.contains(&(asset.clone(), suggestion.tag.clone()));
            if !already_tagged && !rejected {
                pending.push(Pending {
                    asset: asset.clone(),
                    suggestion,
                });
            }
        }
    }
    pending.sort_by(|a, b| {
        (a.asset.as_str(), a.suggestion.tag.as_str())
            .cmp(&(b.asset.as_str(), b.suggestion.tag.as_str()))
    });
    Ok(pending)
}

/// `maj tags suggestions`: lists every pending AI tag suggestion across the
/// catalog, sorted by asset then tag.
///
/// # Errors
/// Returns an error if the event log can't be read or the state dir can't
/// be resolved.
pub(crate) fn cmd_suggestions(app: &FsApp, catalog_root: &Path) -> Result<()> {
    let projection = app.projection()?;
    let pending = pending_suggestions(catalog_root, &projection)?;
    if pending.is_empty() {
        println!(
            "no pending suggestions — captions/tags derive during \
             `maj index run` with a describer configured"
        );
        return Ok(());
    }
    for entry in &pending {
        let vocab = if entry.suggestion.in_vocab {
            "known"
        } else {
            "new"
        };
        let suggestion = &entry.suggestion;
        println!(
            "{}  {}  {:.2}  {vocab}  {}",
            entry.asset, suggestion.tag, suggestion.confidence, suggestion.model_tag
        );
    }
    println!("{} pending suggestion(s)", pending.len());
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
