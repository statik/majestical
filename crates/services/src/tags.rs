//! `maj tag add`/`maj tag rm` compute, the vocabulary verbs (`maj tags
//! list`, `maj tag rename`, `maj tag merge`, `maj tags assign`), plus `maj
//! tags suggestions`: every AI
//! tag suggestion not yet confirmed or rejected, and `maj tags confirm`/`maj
//! tags reject`. `tag add`/`tag rm` moved from
//! `crates/cli/src/commands.rs::cmd_tag`; `confirm`/`reject` moved from
//! `crates/cli/src/tags_cmd.rs::cmd_confirm`/`cmd_reject`. `rejections_path`/
//! [`Rejection`] are shared between [`reject`]'s write side and
//! [`suggestions`]'s read side, so the two can't drift on file location or
//! line shape.
use crate::app::FsApp;
use crate::catalog::{ensure_asset_known, ensure_catalog};
use crate::error::ServiceError;
use crate::state_dir;
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::ports::TagSuggestion;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;
use std::collections::{BTreeMap, BTreeSet};
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
/// can't be read or appended to. The error carries any diagnostics the
/// sink was holding — see [`crate::notices::Notices::attach_on_err`].
pub fn tag_add(app: &mut FsApp, asset: &str, tag: &str) -> Result<(), ServiceError> {
    let result = tag_add_impl(app, asset, tag).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
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
/// `tag` is the DISPLAYED name — what `maj tags list` and the asset's own
/// tags report, post alias-resolution. The raw adds behind it can sit under
/// other names entirely (a rename moves the name without rewriting the
/// adds; a merge leaves adds under both names), so the removal cites every
/// live add id resolving to the displayed name. The consequence, and the
/// intent: a stale source name a rename has moved on from is not removable,
/// because it is no longer a tag anyone can see.
///
/// # Errors
/// Returns an error if `tag` is not set on `asset`, or the event log can't
/// be read or appended to. The error carries any diagnostics the sink was
/// holding: "not set" is exactly the message a half-read log explains, so
/// the skipped-corrupt-lines notice has to travel with it.
pub fn tag_rm(app: &mut FsApp, asset: &str, tag: &str) -> Result<(), ServiceError> {
    let result = tag_rm_impl(app, asset, tag).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
}

fn tag_rm_impl(app: &mut FsApp, asset: &str, tag: &str) -> Result<()> {
    let projection = app.projection()?;
    let asset_id = AssetId(asset.to_string());
    let mut observed = Vec::new();
    for raw in projection.raw_tags_resolving_to(&asset_id, tag) {
        observed.extend(projection.tag_add_ids(&asset_id, raw));
    }
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
/// record) or the event log can't be read/appended. The error carries any
/// diagnostics the sink was holding — see
/// [`crate::notices::Notices::attach_on_err`].
pub fn confirm(app: &mut FsApp, asset: &str, tags: &[String]) -> Result<(), ServiceError> {
    let result = confirm_impl(app, asset, tags).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
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
/// can't be opened/appended. The error carries whatever `notices` was
/// holding at that moment — resolving the state dir is itself a step that
/// records diagnostics (a legacy-file migration), and those explain the
/// very path the failure names, so they travel with it rather than
/// depending on the caller to drain on both paths.
pub fn reject(
    catalog_root: &Path,
    asset: &str,
    tags: &[String],
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    let result = reject_impl(catalog_root, asset, tags, notices).map_err(ServiceError::from);
    notices.attach_on_err(result)
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

/// One live tag in the catalog's vocabulary.
#[derive(Debug, serde::Serialize)]
pub struct TagRow {
    pub tag: String,
    pub count: u64,
    /// HLC wall-time of the newest surviving add, ms.
    pub last_used_ms: u64,
}

/// Everything `maj tags list` renders.
#[derive(Debug, serde::Serialize)]
pub struct TagsListOutcome {
    pub tags: Vec<TagRow>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// One asset a bulk assignment couldn't touch. `tags_assign` / `para_file`
/// report per-asset rows and never abort on one — see the decision rule in
/// [`crate::error`]: an operation that visits every row reports its
/// per-row failures inside `Ok`.
#[derive(Debug, serde::Serialize)]
pub struct AssignFailure {
    pub asset: String,
    pub reason: String,
}

/// Everything a bulk assignment (`maj tags assign`, `maj para file`) did.
/// `applied` counts events actually emitted, not assets asked for.
#[derive(Debug, serde::Serialize)]
pub struct AssignOutcome {
    pub applied: u64,
    pub failed: Vec<AssignFailure>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// What one rename event did: `maj tag rename` and `maj tag merge` emit the
/// same `Op::TagRenamed`, so they report the same shape.
#[derive(Debug, serde::Serialize)]
pub struct TagRenameOutcome {
    pub from: String,
    pub to: String,
    /// Assets whose effective tags changed (count at emit time).
    pub rewritten: u64,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Wall-clock ms of every `TagAdd` in `events`, keyed by event id. The
/// projection tracks which add ids are still live but not when they landed,
/// so `last_used_ms` needs the two halves joined here rather than widening
/// the projection's per-asset state (and with it the snapshot shape) to
/// carry a timestamp no CRDT rule reads. Takes the events the caller
/// already read rather than reading the log itself — see
/// [`tags_list_impl`].
fn tag_add_wall_ms(events: &[Event]) -> BTreeMap<EventId, u64> {
    let mut walls = BTreeMap::new();
    for event in events {
        if let Op::TagAdd { asset: _, tag: _ } = &event.op {
            walls.insert(event.id, event.hlc.wall_ms);
        }
    }
    walls
}

/// The newest wall time among one add group's ids. Both halves come from
/// the same log read, so every live id must be in `add_wall_ms` — the
/// `debug_assert` is there to catch a future caller that pairs a projection
/// with a different read, since the release fallback (treat the missing id
/// as contributing nothing) would otherwise quietly render as epoch 0.
fn newest_wall_ms(add_wall_ms: &BTreeMap<EventId, u64>, ids: &BTreeSet<EventId>) -> u64 {
    let mut newest = 0;
    for id in ids {
        let wall = add_wall_ms.get(id).copied();
        debug_assert!(
            wall.is_some(),
            "live add {id:?} has no TagAdd in the same log read"
        );
        newest = newest.max(wall.unwrap_or(0));
    }
    newest
}

/// `maj tags list`: the catalog's live vocabulary — every effective tag
/// after alias resolution, so a renamed-away name resolves to its target
/// and disappears — with the number of assets carrying it and the wall time
/// of its newest surviving add, sorted by tag name.
///
/// # Errors
/// Returns [`ServiceError::NoCatalog`] if there's no catalog at
/// `catalog_dir`, or an error if the event log can't be read.
pub fn tags_list(app: &FsApp, catalog_dir: &Path) -> Result<TagsListOutcome, ServiceError> {
    ensure_catalog(catalog_dir)?;
    tags_list_impl(app).map_err(ServiceError::from)
}

fn tags_list_impl(app: &FsApp) -> Result<TagsListOutcome> {
    // One read of the log serves both halves: which tags are live (the
    // projection) and when their adds landed (the wall times). Reading
    // twice would also push a corrupt-line notice twice — the sink records
    // every push, and a doubled warning reads as two damaged logs.
    let events = app.events()?;
    let projection = FsApp::projection_of(&events);
    let add_wall_ms = tag_add_wall_ms(&events);
    let mut tallies: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (asset, _state) in projection.assets() {
        // One pass over this asset's raw add groups, folding the newest
        // wall time per effective name — the two sides of a merge land on
        // the same key here. Then the asset counts once per distinct name,
        // which is what a tag's `count` means.
        let mut per_asset: BTreeMap<&str, u64> = BTreeMap::new();
        for (tag, ids) in projection.effective_tag_adds(asset) {
            let newest = newest_wall_ms(&add_wall_ms, ids);
            let slot = per_asset.entry(tag).or_default();
            *slot = (*slot).max(newest);
        }
        for (tag, newest) in per_asset {
            let tally = tallies.entry(tag.to_string()).or_default();
            tally.0 += 1;
            tally.1 = tally.1.max(newest);
        }
    }
    let tags = tallies
        .into_iter()
        .map(|(tag, (count, last_used_ms))| TagRow {
            tag,
            count,
            last_used_ms,
        })
        .collect();
    Ok(TagsListOutcome {
        tags,
        notices: app.notices().drain(),
    })
}

/// How many assets carry `tag` as an effective (post-alias) tag. Serves
/// both jobs a rename has: liveness (zero means no such tag) and the
/// `rewritten` count, which is read BEFORE emitting so it describes the
/// assets the rename is about to move.
fn assets_carrying(projection: &Projection, tag: &str) -> u64 {
    let mut count = 0;
    for (asset, _state) in projection.assets() {
        if projection
            .effective_tag_adds(asset)
            .any(|(effective, _ids)| effective == tag)
        {
            count += 1;
        }
    }
    count
}

/// The source half both plans share: `from` must be a name assets actually
/// carry, and how many carry it is the count the rename will rewrite.
fn live_source_count(projection: &Projection, from: &str) -> Result<u64> {
    let rewritten = assets_carrying(projection, from);
    anyhow::ensure!(
        rewritten > 0,
        "no tag '{from}' in this catalog — see `maj tags list` for the live vocabulary"
    );
    Ok(rewritten)
}

/// Validates a `maj tag rename` against `projection` and returns how many
/// assets it would rewrite — every guard the verb applies, with no event
/// emitted, so a dry-run preview and the real call can never disagree about
/// what is allowed or about the count.
///
/// A target that a rename has already moved away is rejected rather than
/// accepted as "free": the alias map would chain `from -> to -> wherever`,
/// so the assets would land on a name the caller never asked for while the
/// outcome claimed `to`.
///
/// # Errors
/// Returns an error if `from` and `to` are the same name, no asset carries
/// `from`, `to` has itself been renamed away, or some asset already carries
/// `to` (that's a merge).
pub fn rename_plan(projection: &Projection, from: &str, to: &str) -> Result<u64> {
    anyhow::ensure!(
        from != to,
        "tag rename needs two different names — '{from}' is already what it's called"
    );
    let rewritten = live_source_count(projection, from)?;
    if let Some(target) = projection.tag_alias_target(to) {
        anyhow::bail!(
            "tag '{to}' was renamed away to '{target}' — rename '{from}' to that name instead"
        );
    }
    anyhow::ensure!(
        assets_carrying(projection, to) == 0,
        "tag '{to}' already exists — folding '{from}' into it is a merge, not a rename; \
         use `maj tag merge {from} {to}`"
    );
    Ok(rewritten)
}

/// Validates a `maj tag merge` against `projection` and returns how many
/// assets it would rewrite. Same contract as [`rename_plan`] — all guards,
/// no event — and the same treatment of a target a rename already moved
/// away: naming where it went beats sending the caller to `tag rename`,
/// which would refuse it for exactly the same reason.
///
/// # Errors
/// Returns an error if `from` and `into` are the same tag, no asset carries
/// `from`, `into` has been renamed away, or no asset carries `into` (that's
/// a rename).
pub fn merge_plan(projection: &Projection, from: &str, into: &str) -> Result<u64> {
    anyhow::ensure!(
        from != into,
        "tag merge needs two different tags — '{from}' is already itself"
    );
    let rewritten = live_source_count(projection, from)?;
    if assets_carrying(projection, into) > 0 {
        return Ok(rewritten);
    }
    if let Some(target) = projection.tag_alias_target(into) {
        anyhow::bail!(
            "tag '{into}' was renamed away to '{target}' — merge '{from}' into that name instead"
        );
    }
    anyhow::bail!(
        "no tag '{into}' in this catalog — merging into a name nothing carries is a rename; \
         use `maj tag rename {from} {into}`"
    )
}

/// The one `TagRenamed` both verbs emit, plus the outcome they both report.
fn emit_rename(app: &mut FsApp, from: &str, to: &str, rewritten: u64) -> Result<TagRenameOutcome> {
    app.emit(vec![Op::TagRenamed {
        from: from.to_string(),
        to: to.to_string(),
    }])?;
    Ok(TagRenameOutcome {
        from: from.to_string(),
        to: to.to_string(),
        rewritten,
        notices: app.notices().drain(),
    })
}

/// `maj tag rename`: renames a live tag to a name nothing carries yet.
/// Renaming onto an existing tag is a merge and is refused here — the two
/// verbs emit the same event, but merging is the deliberate act of
/// collapsing two vocabularies, so it doesn't happen by accident under a
/// rename's name. [`rename_plan`] holds the rules.
///
/// # Errors
/// Returns [`rename_plan`]'s errors, or an error if the event log can't be
/// read or appended to.
pub fn tag_rename(app: &mut FsApp, from: &str, to: &str) -> Result<TagRenameOutcome, ServiceError> {
    let result = tag_rename_impl(app, from, to).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
}

fn tag_rename_impl(app: &mut FsApp, from: &str, to: &str) -> Result<TagRenameOutcome> {
    let rewritten = rename_plan(&app.projection()?, from, to)?;
    emit_rename(app, from, to, rewritten)
}

/// `maj tag merge`: folds one live tag into another live tag. Same event as
/// [`tag_rename`] — a merge IS a rename onto an occupied name — but both
/// ends must already exist, so merging into a name nothing carries is
/// refused and pointed back at `tag rename`. [`merge_plan`] holds the
/// rules.
///
/// # Errors
/// Returns [`merge_plan`]'s errors, or an error if the event log can't be
/// read or appended to.
pub fn tag_merge(
    app: &mut FsApp,
    from: &str,
    into: &str,
) -> Result<TagRenameOutcome, ServiceError> {
    let result = tag_merge_impl(app, from, into).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
}

fn tag_merge_impl(app: &mut FsApp, from: &str, into: &str) -> Result<TagRenameOutcome> {
    let rewritten = merge_plan(&app.projection()?, from, into)?;
    emit_rename(app, from, into, rewritten)
}

/// `maj tags assign`: adds every tag in `tags` to every asset in `assets` —
/// one `TagAdd` per pair. An asset that was never scanned is reported as a
/// [`AssignFailure`] row and skipped; the rest are still applied, so a
/// selection with one stale id doesn't cost the whole batch.
///
/// # Errors
/// Returns an error only when the whole operation fails: the event log
/// can't be read or appended to. A per-asset problem is a `failed` row.
pub fn tags_assign(
    app: &mut FsApp,
    assets: &[String],
    tags: &[String],
) -> Result<AssignOutcome, ServiceError> {
    let result = tags_assign_impl(app, assets, tags).map_err(ServiceError::from);
    app.notices().attach_on_err(result)
}

fn tags_assign_impl(app: &mut FsApp, assets: &[String], tags: &[String]) -> Result<AssignOutcome> {
    let projection = app.projection()?;
    let mut ops = Vec::new();
    let mut failed = Vec::new();
    for asset in assets {
        let asset_id = AssetId(asset.clone());
        if let Err(error) = ensure_asset_known(&projection, &asset_id) {
            failed.push(AssignFailure {
                asset: asset.clone(),
                reason: error.to_string(),
            });
            continue;
        }
        for tag in tags {
            ops.push(Op::TagAdd {
                asset: asset_id.clone(),
                tag: tag.clone(),
            });
        }
    }
    let emitted = app.emit(ops)?;
    Ok(AssignOutcome {
        applied: u64::try_from(emitted.len()).unwrap_or(u64::MAX),
        failed,
        notices: app.notices().drain(),
    })
}

#[cfg(test)]
mod tag_add_rm_tests {
    use super::*;
    use crate::test_support::{asset_id, seeded_app};

    #[test]
    fn tag_add_sets_a_tag_on_a_known_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        let asset = asset_id(0);
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
        let mut app = seeded_app(dir.path(), 1);
        let asset = asset_id(0);
        tag_add(&mut app, &asset.0, "demo").expect("tag_add");
        tag_rm(&mut app, &asset.0, "demo").expect("tag_rm");
        let projection = app.projection().expect("projection");
        assert!(!projection.tags(&asset).contains("demo"));
    }

    #[test]
    fn tag_rm_of_a_tag_not_set_errors_without_touching_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        let asset = asset_id(0);
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
    use crate::test_support::{asset_id, seeded_app};

    #[test]
    fn confirm_emits_a_tag_add_per_tag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        let asset = asset_id(0);
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
        let app = seeded_app(dir.path(), 1);
        let asset = asset_id(0);
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
mod organize_tests {
    use super::*;
    use crate::test_support::{asset_id, seeded_app};
    use majestical_core::clock::{Hlc, MachineId};

    fn segment_path(root: &Path) -> PathBuf {
        root.join("events").join("m1").join("0001.jsonl")
    }

    /// Appends handcrafted events straight to the machine's log segment.
    /// The only way to pin exact HLC wall times: `FsApp` mints them from the
    /// system clock, so an emitted add can't assert an exact
    /// `last_used_ms`.
    fn append_raw(root: &Path, events: &[Event]) {
        let segment = segment_path(root);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment)
            .expect("open segment");
        for event in events {
            let line = serde_json::to_string(event).expect("serialize");
            writeln!(file, "{line}").expect("append");
        }
    }

    fn tag_add_event(n: u128, wall_ms: u64, asset: &AssetId, tag: &str) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(wall_ms, n)),
            hlc: Hlc {
                wall_ms,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "m1".into(),
            op: Op::TagAdd {
                asset: asset.clone(),
                tag: tag.to_string(),
            },
        }
    }

    /// Plants a line the event log can't parse, so the next read pushes a
    /// skipped-corrupt-line notice into the app's sink — the diagnostic a
    /// failing verb then has to carry.
    fn corrupt_the_log(root: &Path) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(segment_path(root))
            .expect("open segment");
        writeln!(file, "this is not json").expect("append");
    }

    /// Every `TagRenamed` (from, to) pair in the log, in log order.
    fn renames(app: &FsApp) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for event in app.events().expect("events") {
            if let Op::TagRenamed { from, to } = event.op {
                pairs.push((from, to));
            }
        }
        pairs
    }

    /// Every `TagAdd` (asset, tag) pair in the log, in log order.
    fn adds(app: &FsApp) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for event in app.events().expect("events") {
            if let Op::TagAdd { asset, tag } = event.op {
                pairs.push((asset.0, tag));
            }
        }
        pairs
    }

    fn rows(outcome: &TagsListOutcome) -> Vec<(&str, u64, u64)> {
        outcome
            .tags
            .iter()
            .map(|row| (row.tag.as_str(), row.count, row.last_used_ms))
            .collect()
    }

    #[test]
    fn tags_list_reports_every_live_tag_sorted_with_its_asset_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "x").expect("add x to a0");
        tag_add(&mut app, &asset_id(0).0, "y").expect("add y to a0");
        tag_add(&mut app, &asset_id(1).0, "x").expect("add x to a1");
        tag_add(&mut app, &asset_id(1).0, "z").expect("add z to a1");

        let outcome = tags_list(&app, &root).expect("tags_list");
        let counted: Vec<(&str, u64)> = rows(&outcome)
            .into_iter()
            .map(|(tag, count, _)| (tag, count))
            .collect();
        assert_eq!(counted, vec![("x", 2), ("y", 1), ("z", 1)]);
    }

    #[test]
    fn tags_list_after_a_merge_drops_the_old_name_and_unions_the_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_add(&mut app, &asset_id(1).0, "golden-hour").expect("add");
        tag_merge(&mut app, "goldenhour", "golden-hour").expect("merge");

        let outcome = tags_list(&app, &root).expect("tags_list");
        let counted: Vec<(&str, u64)> = rows(&outcome)
            .into_iter()
            .map(|(tag, count, _)| (tag, count))
            .collect();
        assert_eq!(
            counted,
            vec![("golden-hour", 2)],
            "the merged-away name is gone and its assets join the target"
        );
    }

    /// The vocabulary `tags list` reports is the renamed one: `tags()` on a
    /// single asset can't show that the source name left the catalog-wide
    /// row set, which is what a head renders.
    #[test]
    fn tags_list_after_a_rename_shows_only_the_target_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 3);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_add(&mut app, &asset_id(1).0, "goldenhour").expect("add");
        tag_add(&mut app, &asset_id(2).0, "other").expect("add");
        tag_rename(&mut app, "goldenhour", "golden-hour").expect("rename");

        let outcome = tags_list(&app, &root).expect("tags_list");
        let counted: Vec<(&str, u64)> = rows(&outcome)
            .into_iter()
            .map(|(tag, count, _)| (tag, count))
            .collect();
        assert_eq!(
            counted,
            vec![("golden-hour", 2), ("other", 1)],
            "the source name is gone from the vocabulary, its assets under the target"
        );
    }

    /// The log is read once, so a damaged line is reported once — a second
    /// read would push the same warning again and read as two bad logs.
    #[test]
    fn tags_list_reports_a_corrupt_log_line_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "x").expect("add");
        corrupt_the_log(&root);

        let outcome = tags_list(&app, &root).expect("tags_list");
        assert_eq!(
            outcome.notices.len(),
            1,
            "one damaged line, one notice: {:?}",
            outcome.notices
        );
        assert!(
            outcome.notices[0].contains("skipped 1 corrupt event log line(s)"),
            "{}",
            outcome.notices[0]
        );
        assert_eq!(rows(&outcome).len(), 1, "the readable events still count");
    }

    /// `last_used_ms` is the newest *surviving* add: exact against
    /// handcrafted wall times, and it falls back to the older add once the
    /// newest one is removed. The newest add sits on the FIRST asset
    /// walked, so a row that merely kept the last value it saw reports
    /// `5_000` and fails here.
    #[test]
    fn tags_list_last_used_ms_tracks_the_newest_surviving_add() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 2);
        append_raw(
            &root,
            &[
                tag_add_event(1, 9_000, &asset_id(0), "x"),
                tag_add_event(2, 5_000, &asset_id(1), "x"),
                tag_add_event(3, 7_000, &asset_id(0), "y"),
            ],
        );

        let outcome = tags_list(&app, &root).expect("tags_list");
        assert_eq!(rows(&outcome), vec![("x", 2, 9_000), ("y", 1, 7_000)]);

        tag_rm(&mut app, &asset_id(0).0, "x").expect("rm the newest add");
        let outcome = tags_list(&app, &root).expect("tags_list");
        assert_eq!(
            rows(&outcome),
            vec![("x", 1, 5_000), ("y", 1, 7_000)],
            "a removed add stops counting toward last_used_ms"
        );
    }

    /// One asset carrying BOTH sides of a merge has two raw adds behind one
    /// displayed tag. The newer of them is the one the alias map sorts
    /// second ("goldenhour" after "golden-hour"), so taking the last raw
    /// name's time instead of the largest reports `4_000` and fails here.
    #[test]
    fn tags_list_last_used_ms_spans_both_raw_adds_on_one_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        append_raw(
            &root,
            &[
                tag_add_event(1, 6_000, &asset_id(0), "golden-hour"),
                tag_add_event(2, 4_000, &asset_id(0), "goldenhour"),
            ],
        );
        tag_merge(&mut app, "goldenhour", "golden-hour").expect("merge");

        let outcome = tags_list(&app, &root).expect("tags_list");
        assert_eq!(rows(&outcome), vec![("golden-hour", 1, 6_000)]);
    }

    /// A merge's target inherits the source's add times: the newest add
    /// behind the displayed name is what `last_used_ms` reports, whichever
    /// raw name it was written under.
    #[test]
    fn tags_list_last_used_ms_follows_a_merge_to_the_target_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 2);
        append_raw(
            &root,
            &[
                tag_add_event(1, 4_000, &asset_id(0), "goldenhour"),
                tag_add_event(2, 6_000, &asset_id(1), "golden-hour"),
            ],
        );
        tag_merge(&mut app, "goldenhour", "golden-hour").expect("merge");

        let outcome = tags_list(&app, &root).expect("tags_list");
        assert_eq!(rows(&outcome), vec![("golden-hour", 2, 6_000)]);
    }

    #[test]
    fn tag_rename_emits_one_rename_and_counts_the_assets_it_rewrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_add(&mut app, &asset_id(1).0, "goldenhour").expect("add");

        let outcome = tag_rename(&mut app, "goldenhour", "golden-hour").expect("rename");
        assert_eq!(outcome.from, "goldenhour");
        assert_eq!(outcome.to, "golden-hour");
        assert_eq!(outcome.rewritten, 2);
        assert_eq!(
            renames(&app),
            vec![("goldenhour".to_string(), "golden-hour".to_string())],
            "exactly one TagRenamed in the log"
        );
        let projection = app.projection().expect("projection");
        assert!(projection.tags(&asset_id(0)).contains("golden-hour"));
        assert!(!projection.tags(&asset_id(1)).contains("goldenhour"));
    }

    #[test]
    fn tag_rename_of_a_tag_nothing_carries_errors_without_touching_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        let err = tag_rename(&mut app, "nope", "golden-hour").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("nope"), "{message}");
        assert!(message.contains("maj tags list"), "{message}");
        assert!(renames(&app).is_empty(), "a rejected rename must not emit");
    }

    #[test]
    fn tag_rename_onto_a_live_tag_points_at_tag_merge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_add(&mut app, &asset_id(1).0, "golden-hour").expect("add");

        let err = tag_rename(&mut app, "goldenhour", "golden-hour").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("maj tag merge"), "{message}");
        assert!(renames(&app).is_empty(), "a rejected rename must not emit");
    }

    /// A name a rename already moved away is not a free target: the alias
    /// map would chain `a -> b -> c`, so the assets would land on "c" while
    /// the outcome claimed "b" — a rename that silently files things
    /// somewhere the caller never named. The error says where "b" went.
    #[test]
    fn tag_rename_onto_a_renamed_away_name_errors_naming_where_it_went() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        tag_add(&mut app, &asset_id(1).0, "b").expect("add");
        tag_rename(&mut app, "b", "c").expect("first rename");

        let err = tag_rename(&mut app, "a", "b").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("'b' was renamed away to 'c'"), "{message}");
        assert!(
            message.contains("rename 'a' to that name instead"),
            "{message}"
        );
        assert_eq!(
            renames(&app),
            vec![("b".to_string(), "c".to_string())],
            "only the first rename is in the log — the rejected one emitted nothing"
        );
        let projection = app.projection().expect("projection");
        assert!(
            projection.tags(&asset_id(0)).contains("a"),
            "the source keeps its name when the rename is refused"
        );
    }

    /// The merge side of the same hazard. Before the fix this reported "no
    /// tag 'b'" and sent the caller to `tag rename`, which refuses it for
    /// exactly the same reason — a loop of bad advice. Naming the target's
    /// new home is the honest answer.
    #[test]
    fn tag_merge_into_a_renamed_away_name_errors_naming_where_it_went() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        tag_add(&mut app, &asset_id(1).0, "b").expect("add");
        tag_rename(&mut app, "b", "c").expect("first rename");

        let err = tag_merge(&mut app, "a", "b").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("'b' was renamed away to 'c'"), "{message}");
        assert!(
            message.contains("merge 'a' into that name instead"),
            "{message}"
        );
        assert_eq!(
            renames(&app),
            vec![("b".to_string(), "c".to_string())],
            "the rejected merge emitted nothing"
        );
    }

    /// The plan functions are the rule Task 13's dry-run previews read, so
    /// they must answer with the same count the verb then reports and the
    /// same refusals — checked here against one projection, no events.
    #[test]
    fn the_plan_functions_answer_exactly_what_the_verbs_do() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 3);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        tag_add(&mut app, &asset_id(1).0, "a").expect("add");
        tag_add(&mut app, &asset_id(2).0, "b").expect("add");
        let projection = app.projection().expect("projection");

        assert_eq!(rename_plan(&projection, "a", "fresh").expect("plan"), 2);
        assert_eq!(merge_plan(&projection, "a", "b").expect("plan"), 2);
        assert!(
            rename_plan(&projection, "a", "b").is_err(),
            "that's a merge"
        );
        assert!(
            merge_plan(&projection, "a", "fresh").is_err(),
            "that's a rename"
        );
        assert!(rename_plan(&projection, "nope", "fresh").is_err());
        assert!(merge_plan(&projection, "a", "a").is_err());

        let events_before = app.events().expect("events").len();
        let outcome = tag_merge(&mut app, "a", "b").expect("merge");
        assert_eq!(
            outcome.rewritten,
            merge_plan(&projection, "a", "b").expect("plan"),
            "the verb reports the count the plan promised"
        );
        assert_eq!(
            app.events().expect("events").len(),
            events_before + 1,
            "planning emitted nothing; only the verb did"
        );
    }

    #[test]
    fn tag_rename_of_a_tag_to_itself_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "x").expect("add");
        let err = tag_rename(&mut app, "x", "x").expect_err("must fail");
        assert!(err.to_string().contains("two different names"));
        assert!(renames(&app).is_empty());
    }

    #[test]
    fn tag_merge_emits_the_rename_op_and_counts_the_rewritten_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 3);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        tag_add(&mut app, &asset_id(1).0, "a").expect("add");
        tag_add(&mut app, &asset_id(2).0, "b").expect("add");

        let outcome = tag_merge(&mut app, "a", "b").expect("merge");
        assert_eq!(outcome.rewritten, 2, "only the two 'a' assets move");
        assert_eq!(
            renames(&app),
            vec![("a".to_string(), "b".to_string())],
            "a merge is one TagRenamed, same as a rename"
        );
        let projection = app.projection().expect("projection");
        assert!(projection.tags(&asset_id(0)).contains("b"));
    }

    #[test]
    fn tag_merge_into_a_tag_nothing_carries_points_at_tag_rename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        let err = tag_merge(&mut app, "a", "b").expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("maj tag rename"), "{message}");
        assert!(renames(&app).is_empty());
    }

    #[test]
    fn tag_merge_of_a_tag_into_itself_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        let err = tag_merge(&mut app, "a", "a").expect_err("must fail");
        assert!(err.to_string().contains("two different tags"));
        assert!(renames(&app).is_empty());
    }

    #[test]
    fn tags_assign_applies_every_asset_tag_pair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        let outcome = tags_assign(
            &mut app,
            &[asset_id(0).0.clone(), asset_id(1).0.clone()],
            &["x".to_string(), "y".to_string()],
        )
        .expect("assign");
        assert_eq!(outcome.applied, 4);
        assert!(outcome.failed.is_empty());
        assert_eq!(adds(&app).len(), 4, "one TagAdd per (asset, tag) pair");
        let projection = app.projection().expect("projection");
        assert!(projection.tags(&asset_id(0)).contains("y"));
        assert!(projection.tags(&asset_id(1)).contains("x"));
    }

    #[test]
    fn tags_assign_reports_an_unknown_asset_and_still_applies_the_known_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        let outcome = tags_assign(
            &mut app,
            &[asset_id(0).0.clone(), "xxh3:never-scanned".to_string()],
            &["x".to_string()],
        )
        .expect("assign");
        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].asset, "xxh3:never-scanned");
        assert!(
            outcome.failed[0].reason.contains("unknown asset"),
            "{}",
            outcome.failed[0].reason
        );
        assert_eq!(adds(&app).len(), 1);
    }

    /// Spec-review F9: after a rename the raw adds still sit under the
    /// source name, so a removal keyed on the DISPLAYED name has to map
    /// back through the aliases — otherwise the tag a user can see is one
    /// they can never remove.
    #[test]
    fn tag_rm_removes_a_tag_a_rename_moved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_rename(&mut app, "goldenhour", "golden-hour").expect("rename");

        tag_rm(&mut app, &asset_id(0).0, "golden-hour").expect("rm by the displayed name");
        let projection = app.projection().expect("projection");
        assert!(
            projection.tags(&asset_id(0)).is_empty(),
            "the asset must read no tags at all afterwards"
        );
    }

    /// The other half of F9: the displayed vocabulary is the API, so the
    /// stale raw name is not removable — it isn't a tag any more.
    #[test]
    fn tag_rm_by_the_raw_name_a_rename_moved_on_from_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "goldenhour").expect("add");
        tag_rename(&mut app, "goldenhour", "golden-hour").expect("rename");

        let err = tag_rm(&mut app, &asset_id(0).0, "goldenhour").expect_err("must fail");
        assert!(err.to_string().contains("not set"), "{err}");
        let projection = app.projection().expect("projection");
        assert!(
            projection.tags(&asset_id(0)).contains("golden-hour"),
            "the rejected removal must leave the tag in place"
        );
    }

    /// A merge leaves adds under BOTH raw names; removing the displayed tag
    /// has to cite every one of them or the tag comes back.
    #[test]
    fn tag_rm_after_a_merge_removes_both_sides_adds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = seeded_app(dir.path(), 2);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        tag_add(&mut app, &asset_id(0).0, "b").expect("add");
        tag_add(&mut app, &asset_id(1).0, "b").expect("add");
        tag_merge(&mut app, "a", "b").expect("merge");

        tag_rm(&mut app, &asset_id(0).0, "b").expect("rm");
        let projection = app.projection().expect("projection");
        assert!(
            projection.tags(&asset_id(0)).is_empty(),
            "both raw adds behind 'b' must be removed"
        );
        assert!(
            projection.tags(&asset_id(1)).contains("b"),
            "the other asset keeps its own tag"
        );
    }

    #[test]
    fn a_failing_tag_add_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        corrupt_the_log(&root);

        let err = tag_add(&mut app, "xxh3:never-scanned", "x").expect_err("must fail");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing tag_add with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("unknown asset"), "{source}");
    }

    #[test]
    fn a_failing_confirm_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        corrupt_the_log(&root);

        let err =
            confirm(&mut app, "xxh3:never-scanned", &["x".to_string()]).expect_err("must fail");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing confirm with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("unknown asset"), "{source}");
    }

    /// `reject`'s sink is the caller's, not the app's, and its diagnostics
    /// come from resolving the state dir — the same step whose path the
    /// failure names. A head that drains only on the `Ok` path would lose
    /// exactly the line that explains the path.
    #[test]
    fn a_failing_reject_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        std::fs::create_dir_all(&root).expect("mkdir");
        // Resolved first, and deliberately: resolving the path runs the
        // same migration `reject` will, so doing it after the plant below
        // would consume the notice before the call under test ever runs.
        let path = rejections_path(&root, &crate::notices::Notices::new()).expect("path");
        std::fs::create_dir_all(&path).expect("directory in the log's place");
        // The diagnostic under test: a pre-phase-4 db in the sync root,
        // which the migration removes and records as it resolves the dir.
        std::fs::write(root.join("catalog.db"), b"legacy").expect("plant legacy db");

        let notices = crate::notices::Notices::new();
        let err = reject(&root, "xxh3:abc", &["x".to_string()], &notices)
            .expect_err("a directory in the log's place must fail the append");
        let ServiceError::WithNotices {
            notices: carried,
            source,
        } = err
        else {
            panic!("a failing reject with a non-empty sink must carry its notices");
        };
        assert!(
            carried
                .iter()
                .any(|n| n.contains("removed legacy catalog.db")),
            "{carried:?}"
        );
        assert!(
            source.to_string().contains("tag-rejections.jsonl"),
            "{source}"
        );
    }

    /// F9 made `tag_rm`'s rejection a name-resolution answer, not just a
    /// typo report — so on a half-read log the notice explaining WHY the
    /// tag looks unset has to ride the error rather than being dropped.
    #[test]
    fn a_failing_tag_rm_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        corrupt_the_log(&root);

        let err = tag_rm(&mut app, &asset_id(0).0, "not-set").expect_err("must fail");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing tag_rm with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("not set"), "{source}");
    }

    #[test]
    fn a_failing_tag_rename_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        corrupt_the_log(&root);

        let err = tag_rename(&mut app, "nope", "golden-hour").expect_err("must fail");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing rename with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("nope"), "{source}");
    }

    #[test]
    fn a_failing_tag_merge_carries_the_notices_its_sink_was_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        tag_add(&mut app, &asset_id(0).0, "a").expect("add");
        corrupt_the_log(&root);

        let err = tag_merge(&mut app, "a", "b").expect_err("must fail");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing merge with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("maj tag rename"), "{source}");
    }

    /// `tags_assign` never fails on a per-asset problem, so its carrier
    /// path needs a whole-operation failure: an unwritable log segment.
    #[test]
    #[cfg(unix)]
    fn a_failing_tags_assign_carries_the_notices_its_sink_was_holding() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = seeded_app(dir.path(), 1);
        corrupt_the_log(&root);
        let segment = segment_path(&root);
        std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o444))
            .expect("chmod 444");
        // Running as root (some CI containers) makes mode 444 unenforced,
        // and then there is no failure for notices to ride — check the
        // block independently rather than asserting into thin air.
        let enforced = std::fs::OpenOptions::new()
            .append(true)
            .open(&segment)
            .is_err();

        let result = tags_assign(&mut app, &[asset_id(0).0.clone()], &["x".to_string()]);

        std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o644))
            .expect("restore perms");
        if !enforced {
            return;
        }
        let err = result.expect_err("an unwritable log must fail the assign");
        let ServiceError::WithNotices { notices, source } = err else {
            panic!("a failing assign with a non-empty sink must carry its notices");
        };
        assert!(
            notices
                .iter()
                .any(|n| n.contains("skipped 1 corrupt event log line(s)")),
            "{notices:?}"
        );
        assert!(source.to_string().contains("appending events"), "{source}");
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
