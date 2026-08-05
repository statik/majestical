//! `maj tag add`/`maj tag rm` compute, plus `maj tags suggestions`: every AI
//! tag suggestion not yet confirmed or rejected, and `maj tags confirm`/`maj
//! tags reject`. `tag add`/`tag rm` moved from
//! `crates/cli/src/commands.rs::cmd_tag`; `confirm`/`reject` moved from
//! `crates/cli/src/tags_cmd.rs::cmd_confirm`/`cmd_reject`. `rejections_path`/
//! [`Rejection`] are shared between [`reject`]'s write side and
//! [`suggestions`]'s read side, so the two can't drift on file location or
//! line shape.
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
use std::io::Write as _;
use std::path::{Path, PathBuf};

// A real enum rather than a free string so the MCP JSON schema (and a future
// GUI dropdown) carries the closed value set, instead of a typo round-tripping
// to a call-time error. The doc comment below ships verbatim as the wire
// `description`, so it is written for the client, not for us.
/// `add`/`rm` set or remove a folksonomy tag directly;
/// `confirm_suggestion`/`reject_suggestion` act on pending AI tag
/// suggestions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TagOp {
    Add,
    Rm,
    ConfirmSuggestion,
    RejectSuggestion,
}

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

/// `maj tags confirm <asset> <tag>...`: emits a plain `TagAdd` per tag — the
/// same validation and op shape as `maj tag add`, so a confirmed suggestion
/// is indistinguishable from a hand-added tag in the event log. Moved from
/// `crates/cli/src/tags_cmd.rs::cmd_confirm`.
///
/// # Errors
/// Returns an error if the asset has never been scanned (no `AssetSeen` on
/// record) or the event log can't be read/appended.
pub fn confirm(app: &mut FsApp, asset: &str, tags: &[String]) -> Result<(), ServiceError> {
    confirm_impl(app, asset, tags).map_err(ServiceError::from)
}

fn confirm_impl(app: &mut FsApp, asset: &str, tags: &[String]) -> Result<()> {
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
    Ok(())
}

/// `maj tags reject <asset> <tag>...`: appends each pair to this machine's
/// rejection log. Never touches the event log — a rejection is a
/// per-machine "stop suggesting this" note, not a fact synced to teammates.
/// The pair is recorded as given, without checking it against any current
/// suggestion: a typo'd asset or tag just writes a rejection that never
/// matches anything, a harmless no-op line, rather than paying for a full
/// blob scan on every reject to validate it up front. Moved from
/// `crates/cli/src/tags_cmd.rs::cmd_reject`.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the rejection log
/// can't be opened/appended.
pub fn reject(
    catalog_root: &Path,
    asset: &str,
    tags: &[String],
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    reject_impl(catalog_root, asset, tags, notices).map_err(ServiceError::from)
}

fn reject_impl(
    catalog_root: &Path,
    asset: &str,
    tags: &[String],
    notices: &crate::notices::Notices,
) -> Result<()> {
    let path = rejections_path(catalog_root, notices)?;
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
pub fn rejections_path(catalog_root: &Path, notices: &crate::notices::Notices) -> Result<PathBuf> {
    Ok(state_dir::state_dir_for(catalog_root, notices)?.join("tag-rejections.jsonl"))
}

/// Loads every rejection ever appended on this machine. A missing file
/// means no rejections yet — not an error, since the file is created lazily
/// by the first `tags reject`. A malformed line, on the other hand, fails
/// fast: this file is append-only and owned entirely by `tags reject`, so a
/// line that doesn't parse means something else corrupted it (a torn
/// write, a stray edit), and silently dropping it would silently resurface
/// a tag the user already rejected.
fn load_rejections(
    catalog_root: &Path,
    notices: &crate::notices::Notices,
) -> Result<BTreeSet<(String, String)>> {
    let path = rejections_path(catalog_root, notices)?;
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
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
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
/// A blob that fails to read or decode is skipped with a notice
/// rather than failing the whole listing — it may be mid-write by another
/// process, and one bad blob shouldn't hide every other asset's
/// suggestions. Blobs from more than one describer model tag can list the
/// same `(asset, tag)` pair twice, once per model tag — each row is kept
/// (the model tag is part of what's displayed), and confirming or
/// rejecting that tag clears every one of them at once, since both act on
/// the `(asset, tag)` pair alone.
fn pending_suggestions(
    catalog_root: &Path,
    projection: &Projection,
    notices: &crate::notices::Notices,
) -> Result<Vec<SuggestionRow>> {
    let blobs = BlobStore::new(catalog_root);
    let rejections = load_rejections(catalog_root, notices)?;
    let mut pending = Vec::new();
    for (asset_hex, _model_tag, path) in blobs
        .iter_named("tags.json.zst")
        .context("walking tag-suggestion blobs")?
    {
        let asset = format!("xxh3:{asset_hex}");
        let suggestions = match read_tags_blob(&path) {
            Ok(suggestions) => suggestions,
            Err(error) => {
                notices.push(format!(
                    "note: skipping unreadable tag-suggestions blob {}: {error}",
                    path.display()
                ));
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
    let pending = pending_suggestions(catalog_root, &projection, app.notices())?;
    Ok(SuggestionsOutcome {
        pending,
        notices: app.notices().drain(),
    })
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
        let events_before = app.events().expect("events").len();
        let err = tag_rm(&mut app, &asset.0, "not-set").expect_err("must fail");
        assert!(err.to_string().contains("not set"));
        let events_after = app.events().expect("events").len();
        assert_eq!(
            events_after, events_before,
            "a rejected removal must not append a TagRemove event"
        );
        let projection = app.projection().expect("projection");
        assert!(!projection.tags(&asset).contains("not-set"));
    }
}

#[cfg(test)]
mod confirm_reject_tests {
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
    fn confirm_emits_a_tag_add_per_tag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, asset) = seeded_app(dir.path());
        confirm(
            &mut app,
            &asset.0,
            &["demo".to_string(), "landscape".to_string()],
        )
        .expect("confirm");
        let projection = app.projection().expect("projection");
        assert!(projection.tags(&asset).contains("demo"));
        assert!(projection.tags(&asset).contains("landscape"));
    }

    #[test]
    fn confirm_on_an_unknown_asset_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let err =
            confirm(&mut app, "xxh3:never-scanned", &["demo".to_string()]).expect_err("must fail");
        assert!(err.to_string().contains("unknown asset"));
    }

    #[test]
    fn reject_appends_one_line_per_tag_to_the_rejection_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        std::fs::create_dir_all(&root).expect("mkdir");
        reject(
            &root,
            "xxh3:abc",
            &["demo".to_string(), "landscape".to_string()],
            &crate::notices::Notices::new(),
        )
        .expect("reject");
        let path =
            rejections_path(&root, &crate::notices::Notices::new()).expect("rejections_path");
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("landscape"));
    }

    #[test]
    fn reject_never_touches_the_event_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (app, asset) = seeded_app(dir.path());
        let events_before = app.events().expect("events").len();
        reject(
            &dir.path().join("cat"),
            &asset.0,
            &["demo".to_string()],
            &crate::notices::Notices::new(),
        )
        .expect("reject");
        let events_after = app.events().expect("events").len();
        assert_eq!(
            events_after, events_before,
            "reject must never emit an event"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_op_wire_strings_are_pinned() {
        for (op, wire) in [
            (TagOp::Add, "add"),
            (TagOp::Rm, "rm"),
            (TagOp::ConfirmSuggestion, "confirm_suggestion"),
            (TagOp::RejectSuggestion, "reject_suggestion"),
        ] {
            assert_eq!(
                serde_json::to_value(op).expect("ser"),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<TagOp>(serde_json::json!(wire)).expect("de"),
                op
            );
        }
        assert!(serde_json::from_value::<TagOp>(serde_json::json!("bogus")).is_err());
    }

    #[test]
    fn suggestions_of_an_empty_catalog_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = FsApp::init(&root, "m1", "m1").expect("init");
        let outcome = suggestions(&app, &root).expect("suggestions");
        assert!(outcome.pending.is_empty());
    }
}
