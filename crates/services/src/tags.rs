//! `maj tag add`/`maj tag rm` compute, plus `maj tags suggestions`: every AI
//! tag suggestion not yet confirmed or rejected. `tag add`/`tag rm` moved
//! from `crates/cli/src/commands.rs::cmd_tag`; `tags confirm`/`tags reject`
//! (which write the folksonomy or the rejection log) stay in the CLI.
//! `rejections_path`/[`Rejection`] are shared with `tags reject`'s write
//! side, so the two can't drift on file location or line shape.
use crate::app::FsApp;
use crate::catalog::ensure_asset_known;
use crate::error::ServiceError;
use crate::state_dir;
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op};
use majestical_core::ports::TagSuggestion;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `maj tag add`: adds a folksonomy tag to an already-known asset.
///
/// # Errors
/// Returns an error if `asset` has no recorded instance, or the event log
/// can't be read or appended to.
pub fn tag_add(app: &mut FsApp, asset: &str, tag: &str) -> Result<(), ServiceError> {
    tag_add_impl(app, asset, tag).map_err(ServiceError::from)
}

fn tag_add_impl(app: &mut FsApp, asset: &str, tag: &str) -> Result<()> {
    let projection = app.projection()?;
    let asset_id = AssetId(asset.to_string());
    ensure_asset_known(&projection, &asset_id)?;
    app.emit(vec![Op::TagAdd {
        asset: asset_id,
        tag: tag.to_string(),
    }])?;
    Ok(())
}

/// `maj tag rm`: removes a folksonomy tag, rejecting a tag that isn't
/// currently set on the asset.
///
/// # Errors
/// Returns an error if `tag` is not set on `asset`, or the event log can't
/// be read or appended to.
pub fn tag_rm(app: &mut FsApp, asset: &str, tag: &str) -> Result<(), ServiceError> {
    tag_rm_impl(app, asset, tag).map_err(ServiceError::from)
}

fn tag_rm_impl(app: &mut FsApp, asset: &str, tag: &str) -> Result<()> {
    let projection = app.projection()?;
    let asset_id = AssetId(asset.to_string());
    let observed = projection.tag_add_ids(&asset_id, tag);
    anyhow::ensure!(
        !observed.is_empty(),
        "tag '{tag}' is not set on {} — nothing to remove",
        asset_id.0
    );
    app.emit(vec![Op::TagRemove {
        asset: asset_id,
        tag: tag.to_string(),
        observed,
    }])?;
    Ok(())
}

/// One rejected (asset, tag) pair, as recorded in the per-machine
/// rejections jsonl.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Rejection {
    pub asset: String,
    pub tag: String,
}

/// Path to this machine's rejection log — per-machine, append-only, never
/// synced (it lives in the local state dir, not the sync-root event log),
/// so a rejection survives a projection rebuild but never propagates to a
/// teammate's machine.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved.
pub fn rejections_path(catalog_root: &Path) -> Result<PathBuf> {
    Ok(state_dir::state_dir_for(catalog_root)?.join("tag-rejections.jsonl"))
}

/// Loads every rejection ever appended on this machine. A missing file
/// means no rejections yet — not an error, since the file is created lazily
/// by the first `tags reject`. A malformed line, on the other hand, fails
/// fast: this file is append-only and owned entirely by `tags reject`, so a
/// line that doesn't parse means something else corrupted it (a torn
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
#[derive(serde::Serialize)]
pub struct SuggestionRow {
    pub asset: String,
    pub tag: String,
    pub confidence: f64,
    pub in_vocab: bool,
    pub model_tag: String,
}

/// Everything `maj tags suggestions` renders.
#[derive(serde::Serialize)]
pub struct SuggestionsOutcome {
    pub pending: Vec<SuggestionRow>,
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
fn pending_suggestions(catalog_root: &Path, projection: &Projection) -> Result<Vec<SuggestionRow>> {
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
                // See the `#[expect]` note on `warn_skipped_corrupt_lines`
                // in app.rs: services inherits print_stderr = "deny"
                // crate-wide; this is a verbatim stderr diagnostic moved
                // from cli, not yet a rendered outcome.
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!(
                        "note: skipping unreadable tag-suggestions blob {}: {error}",
                        path.display()
                    );
                }
                continue;
            }
        };
        let asset_id = AssetId(asset.clone());
        let existing = projection.tags(&asset_id);
        for suggestion in suggestions {
            let already_tagged = existing.contains(&suggestion.tag);
            let rejected = rejections.contains(&(asset.clone(), suggestion.tag.clone()));
            if !already_tagged && !rejected {
                pending.push(SuggestionRow {
                    asset: asset.clone(),
                    tag: suggestion.tag,
                    confidence: suggestion.confidence,
                    in_vocab: suggestion.in_vocab,
                    model_tag: suggestion.model_tag,
                });
            }
        }
    }
    pending.sort_by(|a, b| {
        (a.asset.as_str(), a.tag.as_str()).cmp(&(b.asset.as_str(), b.tag.as_str()))
    });
    Ok(pending)
}

/// `maj tags suggestions`: lists every pending AI tag suggestion across the
/// catalog, sorted by asset then tag.
///
/// # Errors
/// Returns an error if the event log can't be read or the state dir can't
/// be resolved.
pub fn suggestions(app: &FsApp, catalog_root: &Path) -> Result<SuggestionsOutcome, ServiceError> {
    suggestions_impl(app, catalog_root).map_err(ServiceError::from)
}

fn suggestions_impl(app: &FsApp, catalog_root: &Path) -> Result<SuggestionsOutcome> {
    let projection = app.projection()?;
    let pending = pending_suggestions(catalog_root, &projection)?;
    Ok(SuggestionsOutcome { pending })
}

#[cfg(test)]
mod tag_add_rm_tests {
    use super::*;
    use majestical_core::event::Op;

    fn seeded_app(dir: &std::path::Path) -> (FsApp, AssetId) {
        let root = dir.join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let asset = AssetId("xxh3:0123456789abcdef0123456789abcdef".into());
        app.emit(vec![Op::AssetSeen {
            asset: asset.clone(),
            volume: "vol1".into(),
            path: "clip.txt".into(),
            size: 5,
            mtime_ms: 1000,
        }])
        .expect("emit");
        (app, asset)
    }

    #[test]
    fn tag_add_sets_a_tag_on_a_known_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, asset) = seeded_app(dir.path());
        tag_add(&mut app, &asset.0, "demo").expect("tag_add");
        let projection = app.projection().expect("projection");
        assert!(projection.tags(&asset).contains("demo"));
    }

    #[test]
    fn tag_add_on_an_unknown_asset_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let err = tag_add(&mut app, "xxh3:never-scanned", "demo").expect_err("must fail");
        assert!(err.to_string().contains("unknown asset"));
    }

    #[test]
    fn tag_rm_removes_a_previously_added_tag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, asset) = seeded_app(dir.path());
        tag_add(&mut app, &asset.0, "demo").expect("tag_add");
        tag_rm(&mut app, &asset.0, "demo").expect("tag_rm");
        let projection = app.projection().expect("projection");
        assert!(!projection.tags(&asset).contains("demo"));
    }

    #[test]
    fn tag_rm_of_a_tag_not_set_errors_without_touching_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, asset) = seeded_app(dir.path());
        let err = tag_rm(&mut app, &asset.0, "not-set").expect_err("must fail");
        assert!(err.to_string().contains("not set"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_of_an_empty_catalog_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = FsApp::init(&root, "m1", "m1").expect("init");
        let outcome = suggestions(&app, &root).expect("suggestions");
        assert!(outcome.pending.is_empty());
    }
}
