//! Browse compute: catalog folder trees and per-folder listings, read the
//! way `volumes.rs` and `search.rs` read the projection/sqlite catalog.
//! `browse_tree` derives every volume's folder structure from
//! `AssetState::instances` keys; `browse_list` scopes, filters, sorts, and
//! paginates the assets under one folder, rendered through the same
//! [`SearchHit`] row `search.rs` uses so the GUI grid is shared.
use crate::app::FsApp;
use crate::catalog::open_catalog;
use crate::error::ServiceError;
use crate::search::{SearchHit, VolumeRef};
use crate::volumes::volume_is_online;
use anyhow::{Context, Result};
use majestical_core::event::AssetId;
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::ports::AssetSummary;
use majestical_core::projection::Projection;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// One folder in a volume's catalog tree.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct BrowseFolder {
    /// `/`-separated path relative to the volume root; "" is the root.
    pub path: String,
    /// Direct child folder names, sorted.
    pub children: Vec<String>,
    /// Assets in this folder's entire subtree (the Drilldown count).
    pub recursive_count: u64,
}

/// One volume's flat folder list — the GUI nests it client-side by path.
#[derive(Debug, serde::Serialize)]
pub struct BrowseVolume {
    pub id: String,
    pub label: String,
    pub online: bool,
    pub folders: Vec<BrowseFolder>,
}

/// [`browse_tree`]'s outcome: every volume's folder tree, plus any
/// degradation notices (an offline volume names itself).
#[derive(Debug, serde::Serialize)]
pub struct BrowseTreeOutcome {
    pub volumes: Vec<BrowseVolume>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Request for [`browse_list`]: a folder scope on one volume, sort order, an
/// optional `kind:` filter, and pagination.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrowseRequest {
    pub volume: String,
    /// "" for the volume root.
    pub path: String,
    pub flatten: bool,
    /// "captured" (default: newest `mtime_ms` first), "name" (ascending),
    /// or "size" (descending). Task 7's `--sort` help and Task 9's sort menu
    /// read their direction wording from here.
    pub sort: Option<String>,
    /// Filter to one media kind (the names `media_kind` produces).
    pub kind: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// [`browse_list`]'s outcome: matching assets (pre-pagination counts),
/// rendered through the same [`SearchHit`] row `search.rs` produces —
/// `score` is always 0.0 (browse has no ranking) and `known` is
/// unconditionally `true`: every emitted row is built from a real catalog
/// summary, resolved from the very projection `results`' ids came from. If a
/// summary is ever missing — which should be impossible, see
/// [`build_rows`]'s doc — that asset is dropped from `results` entirely and
/// a notice names it, rather than fabricating a placeholder row.
#[derive(Debug, serde::Serialize)]
pub struct BrowseListOutcome {
    /// Total matching assets before limit/offset.
    pub count: u64,
    /// Distinct folders holding at least one in-scope instance — an asset
    /// with instances in two scoped folders counts both, even though it
    /// appears once in `results`.
    pub folder_count: u64,
    pub results: Vec<SearchHit>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Valid [`BrowseRequest::sort`] values, listed in the error for an
/// unrecognized one.
const SORT_VALUES: [&str; 3] = ["captured", "name", "size"];

/// [`BrowseRequest::limit`]'s default across every head — the CLI's
/// `--limit` and the MCP `browse_assets` tool's `limit` param both reference
/// this rather than repeating the literal `50`, so the three defaults (here,
/// clap, and the tool schema) can never drift apart.
pub const DEFAULT_LIMIT: usize = 50;

/// `maj browse tree`: every volume's folder structure, derived from every
/// cataloged instance path on it.
///
/// # Errors
/// Returns an error if the sqlite catalog can't be opened/synced or the
/// volumes query fails.
pub fn browse_tree(app: &FsApp, catalog_dir: &Path) -> Result<BrowseTreeOutcome, ServiceError> {
    browse_tree_impl(app, catalog_dir).map_err(ServiceError::from)
}

fn browse_tree_impl(app: &FsApp, catalog_dir: &Path) -> Result<BrowseTreeOutcome> {
    let (db, projection) = open_catalog(app, catalog_dir)?;
    let volume_rows = db.volumes().context("querying volumes")?;
    let mut per_volume = folder_maps_by_volume(&projection);
    let mut offline_labels = Vec::new();
    let mut volumes = Vec::new();
    for (id, label, _last_seen_ms) in &volume_rows {
        let online = volume_is_online(id, label);
        if !online {
            offline_labels.push(label.clone());
        }
        let (children, assets) = per_volume.remove(id).unwrap_or_default();
        volumes.push(BrowseVolume {
            id: id.clone(),
            label: label.clone(),
            online,
            folders: build_folders(&children, assets),
        });
    }
    let mut notices = Vec::new();
    if !offline_labels.is_empty() {
        notices.push(offline_notice_summary(&offline_labels));
    }
    notices.extend(app.notices().drain());
    Ok(BrowseTreeOutcome { volumes, notices })
}

/// The wording shared by both verbs for a single offline volume's
/// degradation notice — cataloged data is still browsable, only the live
/// mount is gone.
fn offline_notice(label: &str) -> String {
    format!("volume '{label}' is offline — showing cataloged data only")
}

/// `browse_tree` visits every volume in one response, so a catalog with a
/// dozen archived drives must not produce a dozen banner lines — the
/// per-volume signal is already structural (`BrowseVolume::online`). One
/// offline volume keeps the singular wording; more collapse into a single
/// count-plus-names line.
fn offline_notice_summary(labels: &[String]) -> String {
    let [label] = labels else {
        return format!(
            "{} volumes are offline — showing cataloged data only: {}",
            labels.len(),
            labels.join(", ")
        );
    };
    offline_notice(label)
}

/// Per-volume folder maps: direct-child names by parent path, and the set of
/// distinct asset ids with at least one instance in that folder's subtree —
/// folded from every asset's recorded `(volume, path)` instances across the
/// whole catalog. A `BTreeSet` per folder, not a running count, because an
/// asset with two instances in the same subtree must count once, not twice.
type FolderMaps = (
    BTreeMap<String, BTreeSet<String>>,
    BTreeMap<String, BTreeSet<AssetId>>,
);

fn folder_maps_by_volume(projection: &Projection) -> HashMap<String, FolderMaps> {
    let mut per_volume: HashMap<String, FolderMaps> = HashMap::new();
    for (asset, state) in projection.assets() {
        for (volume, path) in state.instances.keys() {
            let (children, assets) = per_volume.entry(volume.clone()).or_default();
            insert_path(children, assets, path, asset);
        }
    }
    per_volume
}

/// Folds one instance path into a volume's folder tree: every `/`-separated
/// prefix directory (root `""` included) gets `asset` added to its subtree
/// set, and each directory registers as a child of its parent. The path's
/// final segment is the filename, never a folder. Idempotent per asset —
/// inserting into a `BTreeSet` a second time (a second instance of the same
/// asset under the same folder) is a no-op, which is exactly what keeps
/// `recursive_count` an asset count rather than an instance count.
fn insert_path(
    children: &mut BTreeMap<String, BTreeSet<String>>,
    assets: &mut BTreeMap<String, BTreeSet<AssetId>>,
    path: &str,
    asset: &AssetId,
) {
    assets
        .entry(String::new())
        .or_default()
        .insert(asset.clone());
    let Some(dir) = path.rsplit_once('/').map(|(dir, _name)| dir) else {
        return; // root-level file: only the root set above applies.
    };
    let mut prefix = String::new();
    for segment in dir.split('/') {
        let child_path = if prefix.is_empty() {
            segment.to_string()
        } else {
            format!("{prefix}/{segment}")
        };
        children
            .entry(prefix.clone())
            .or_default()
            .insert(segment.to_string());
        assets
            .entry(child_path.clone())
            .or_default()
            .insert(asset.clone());
        prefix = child_path;
    }
}

/// Flattens a volume's folder maps into sorted [`BrowseFolder`] rows — the
/// root `""` folder is always present, even for a volume with zero
/// instances. Takes `assets` by value: this is the last use of the map, so
/// moving its entries into the output avoids cloning every folder's asset
/// set just to add the root entry.
fn build_folders(
    children: &BTreeMap<String, BTreeSet<String>>,
    mut assets: BTreeMap<String, BTreeSet<AssetId>>,
) -> Vec<BrowseFolder> {
    assets.entry(String::new()).or_default();
    assets
        .into_iter()
        .map(|(path, ids)| {
            let child_names = children
                .get(&path)
                .map(|set| set.iter().cloned().collect())
                .unwrap_or_default();
            BrowseFolder {
                path,
                children: child_names,
                recursive_count: ids.len() as u64,
            }
        })
        .collect()
}

/// `maj browse list`: assets scoped to one volume and folder, sorted,
/// optionally kind-filtered, and paginated — rendered through the same
/// [`SearchHit`] row `search` uses. `folder_count` counts distinct folders
/// holding at least one in-scope instance — an asset with instances in two
/// scoped folders counts both, even though it appears once in `results`.
///
/// # Errors
/// Returns an error if `req.volume` doesn't name a cataloged volume, or
/// `req.sort`/`req.kind` name an unrecognized value.
pub fn browse_list(
    app: &FsApp,
    catalog_dir: &Path,
    req: &BrowseRequest,
) -> Result<BrowseListOutcome, ServiceError> {
    browse_list_impl(app, catalog_dir, req).map_err(ServiceError::from)
}

fn browse_list_impl(
    app: &FsApp,
    catalog_dir: &Path,
    req: &BrowseRequest,
) -> Result<BrowseListOutcome> {
    validate_sort(req.sort.as_deref())?;
    validate_kind(req.kind.as_deref())?;
    let (db, projection) = open_catalog(app, catalog_dir)?;
    let volume_rows = db.volumes().context("querying volumes")?;
    let (_, label, _) = volume_rows
        .iter()
        .find(|(id, _, _)| id == &req.volume)
        .with_context(|| format!("unknown volume '{}' — run `maj volumes list`", req.volume))?;
    let mut notices = Vec::new();
    if !volume_is_online(&req.volume, label) {
        notices.push(offline_notice(label));
    }

    let matches = matching_instances(&projection, req);
    let folder_count = matches
        .iter()
        .map(|m| parent_dir(&m.path))
        .collect::<BTreeSet<_>>()
        .len();
    let deduped = dedupe_by_asset(matches);
    let ids: Vec<AssetId> = deduped.iter().map(|m| m.asset.clone()).collect();
    let summaries = db
        .asset_summaries(&ids)
        .context("fetching asset summaries")?;
    let mut rows = build_rows(&deduped, &summaries, app.notices());
    sort_rows(&mut rows, req.sort.as_deref());
    let count = rows.len();
    let results = rows
        .into_iter()
        .skip(req.offset)
        .take(req.limit)
        .map(|r| r.hit)
        .collect();

    notices.extend(app.notices().drain());
    Ok(BrowseListOutcome {
        count: count as u64,
        folder_count: folder_count as u64,
        results,
        notices,
    })
}

fn validate_sort(sort: Option<&str>) -> Result<()> {
    if let Some(s) = sort {
        anyhow::ensure!(
            SORT_VALUES.contains(&s),
            "unknown sort '{s}' — one of: {}",
            SORT_VALUES.join(", ")
        );
    }
    Ok(())
}

fn validate_kind(kind: Option<&str>) -> Result<()> {
    if let Some(k) = kind {
        let valid = MediaKind::ALL.map(MediaKind::as_str);
        anyhow::ensure!(
            valid.contains(&k),
            "unknown kind '{k}' — one of: {}",
            valid.join(", ")
        );
    }
    Ok(())
}

/// The directory portion of a catalog path — "" for a root-level file.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _name)| dir)
}

/// Whether `path` falls under `scope` per `browse_list`'s folder semantics.
/// `flatten` matches any path nested anywhere under `scope`; `prefix` is the
/// precomputed `"{scope}/"` string (`None` when `scope` is empty — flatten
/// over the whole volume, which matches unconditionally) so callers looping
/// over every instance allocate it once, not per instance. Non-flatten
/// matches only a path whose immediate parent directory equals `scope`
/// exactly.
fn in_scope(path: &str, scope: &str, flatten: bool, prefix: Option<&str>) -> bool {
    if flatten {
        prefix.is_none_or(|p| path.starts_with(p))
    } else {
        parent_dir(path) == scope
    }
}

fn kind_matches(path: &str, kind: Option<&str>) -> bool {
    kind.is_none_or(|k| media_kind(path).as_str() == k)
}

/// One asset instance that survived `browse_list`'s scope + kind filter.
struct MatchingInstance {
    asset: AssetId,
    path: String,
    size: u64,
    mtime_ms: u64,
}

/// Every instance on `req.volume` that falls under `req.path`'s scope and
/// (if set) matches `req.kind`.
fn matching_instances(projection: &Projection, req: &BrowseRequest) -> Vec<MatchingInstance> {
    // Computed once per call, not once per instance — see `in_scope`'s doc.
    let prefix = (req.flatten && !req.path.is_empty()).then(|| format!("{}/", req.path));
    let mut matches = Vec::new();
    for (asset, state) in projection.assets() {
        for ((volume, path), info) in &state.instances {
            if volume != &req.volume {
                continue;
            }
            let scoped = in_scope(path, &req.path, req.flatten, prefix.as_deref());
            if !scoped || !kind_matches(path, req.kind.as_deref()) {
                continue;
            }
            matches.push(MatchingInstance {
                asset: asset.clone(),
                path: path.clone(),
                size: info.size,
                mtime_ms: info.mtime_ms,
            });
        }
    }
    matches
}

/// Collapses matching instances to one per asset — an asset with two
/// matching instances under scope must still appear once. The surviving
/// instance is the asset's representative: its `size`/`mtime_ms`/`kind`
/// populate the asset's [`SearchHit`] row, so the pick must be deterministic
/// — highest `mtime_ms`, tie broken toward the lexicographically smaller
/// path.
fn dedupe_by_asset(matches: Vec<MatchingInstance>) -> Vec<MatchingInstance> {
    let mut best: BTreeMap<AssetId, MatchingInstance> = BTreeMap::new();
    for m in matches {
        match best.get(&m.asset) {
            Some(current) if !is_better(&m, current) => {}
            _ => {
                best.insert(m.asset.clone(), m);
            }
        }
    }
    best.into_values().collect()
}

/// Whether `candidate` should replace `current` as an asset's representative
/// instance — see [`dedupe_by_asset`]'s pick rule.
fn is_better(candidate: &MatchingInstance, current: &MatchingInstance) -> bool {
    if candidate.mtime_ms != current.mtime_ms {
        return candidate.mtime_ms > current.mtime_ms;
    }
    candidate.path < current.path
}

/// One [`SearchHit`] row plus the sort keys `browse_list` needs but the wire
/// row itself doesn't carry.
struct BrowseRow {
    hit: SearchHit,
    mtime_ms: u64,
    size: u64,
}

/// Builds one [`BrowseRow`] per deduped match, joining each asset's catalog
/// summary the same way `search.rs`'s `build_outcome` does — except a
/// missing summary is never fabricated as an empty placeholder row: `ids`
/// (and so `matches`) come from the same projection `open_catalog` just
/// synced the sqlite catalog from, so a lookup miss here should be
/// impossible. If it somehow happens anyway, that asset is skipped and
/// `notices` gets a line naming it, rather than emitting a row with a blank
/// name/tags/volumes that would look like a real, if empty, asset.
fn build_rows(
    matches: &[MatchingInstance],
    summaries: &[AssetSummary],
    notices: &crate::notices::Notices,
) -> Vec<BrowseRow> {
    let by_id: HashMap<&AssetId, &AssetSummary> = summaries.iter().map(|s| (&s.asset, s)).collect();
    let mut rows = Vec::new();
    for m in matches {
        let Some(summary) = by_id.get(&m.asset) else {
            notices.push(format!(
                "browse: no catalog summary for asset {} — dropped from results",
                m.asset.0
            ));
            continue;
        };
        let volumes = summary
            .volumes
            .iter()
            .map(|(id, label)| VolumeRef {
                id: id.clone(),
                label: label.clone(),
                online: volume_is_online(id, label),
            })
            .collect();
        rows.push(BrowseRow {
            hit: SearchHit {
                asset: m.asset.0.clone(),
                // Browse has no ranking — every row scores 0.0.
                score: 0.0,
                // Always real: see this function's doc.
                known: true,
                name: summary.name.clone(),
                volumes,
                tags: summary.tags.clone(),
                para: summary.para.clone(),
                timestamp_ms: None,
                source: None,
                locator: None,
                snippet: None,
                size: Some(m.size),
                mtime_ms: Some(m.mtime_ms),
                kind: Some(media_kind(&m.path).as_str().to_string()),
            },
            mtime_ms: m.mtime_ms,
            size: m.size,
        });
    }
    rows
}

/// Sorts `rows` per `sort` — validated by [`validate_sort`] before this ever
/// runs, so an unrecognized value can't reach here; `None`/`"captured"` both
/// take the default arm. Every arm breaks ties on asset id — deliberately
/// redundant pagination insurance: `rows` already arrives in asset-id order
/// out of `dedupe_by_asset`'s `BTreeMap`, but the tiebreak keeps that
/// guarantee explicit here rather than resting on an upstream implementation
/// detail a future refactor could silently change.
fn sort_rows(rows: &mut [BrowseRow], sort: Option<&str>) {
    match sort.unwrap_or("captured") {
        "name" => rows.sort_by(|a, b| {
            a.hit
                .name
                .cmp(&b.hit.name)
                .then_with(|| a.hit.asset.cmp(&b.hit.asset))
        }),
        "size" => rows.sort_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| a.hit.asset.cmp(&b.hit.asset))
        }),
        _ => rows.sort_by(|a, b| {
            b.mtime_ms
                .cmp(&a.mtime_ms)
                .then_with(|| a.hit.asset.cmp(&b.hit.asset))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume_identity::ROOT_LABEL;
    use majestical_core::event::Op;
    use std::path::PathBuf;

    /// Volume id/label pinned to the always-online root label — see
    /// `volume_is_online`'s doc: `label:`-id volumes with `ROOT_LABEL` read
    /// online unconditionally, so this fixture reads the same on every CI
    /// runner regardless of what's actually mounted at `/Volumes`.
    fn online_volume_id() -> &'static str {
        "label:root"
    }
    fn online_volume_label() -> &'static str {
        ROOT_LABEL
    }

    /// A label with no `/Volumes/<label>` mount anywhere a test could run —
    /// `volume_is_online` reads it offline deterministically.
    const OFFLINE_VOLUME_ID: &str = "label:probe-offline-vol-browse-xyz";
    const OFFLINE_VOLUME_LABEL: &str = "probe-offline-vol-browse-xyz";

    fn asset_id(tag: &str) -> AssetId {
        AssetId(format!("xxh3:{tag:0<32}"))
    }

    /// Emits the shared fixture: online volume V with assets at `A/x.mov`,
    /// `A/B/y.jpg`, `C/z.pdf`, and offline volume W with one asset at
    /// `D/w.mov`.
    fn seed_fixture(app: &mut FsApp) {
        app.emit(vec![
            Op::VolumeSeen {
                volume: online_volume_id().into(),
                label: online_volume_label().into(),
            },
            Op::VolumeSeen {
                volume: OFFLINE_VOLUME_ID.into(),
                label: OFFLINE_VOLUME_LABEL.into(),
            },
            Op::AssetSeen {
                asset: asset_id("x"),
                volume: online_volume_id().into(),
                path: "A/x.mov".into(),
                size: 10,
                mtime_ms: 3000,
            },
            Op::AssetSeen {
                asset: asset_id("y"),
                volume: online_volume_id().into(),
                path: "A/B/y.jpg".into(),
                size: 20,
                mtime_ms: 1000,
            },
            Op::AssetSeen {
                asset: asset_id("z"),
                volume: online_volume_id().into(),
                path: "C/z.pdf".into(),
                size: 30,
                mtime_ms: 2000,
            },
            Op::AssetSeen {
                asset: asset_id("w"),
                volume: OFFLINE_VOLUME_ID.into(),
                path: "D/w.mov".into(),
                size: 40,
                mtime_ms: 4000,
            },
        ])
        .expect("emit fixture");
    }

    /// A fresh temp catalog with the shared fixture already seeded. Returns
    /// the `TempDir` too — it must stay bound in the caller (even as `_dir`)
    /// or the directory is deleted out from under `root` before the test
    /// runs.
    fn seeded() -> (tempfile::TempDir, PathBuf, FsApp) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        seed_fixture(&mut app);
        (dir, root, app)
    }

    /// A `BrowseRequest` scoped to `path` on the fixture's online volume,
    /// flattened, unsorted, unfiltered, and capped at 50 rows from the
    /// start — the common case every test tweaks via struct-update syntax
    /// (e.g. `BrowseRequest { flatten: false, ..req("A") }`).
    fn req(path: &str) -> BrowseRequest {
        BrowseRequest {
            volume: online_volume_id().into(),
            path: path.into(),
            flatten: true,
            sort: None,
            kind: None,
            limit: 50,
            offset: 0,
        }
    }

    fn folder<'a>(volume: &'a BrowseVolume, path: &str) -> &'a BrowseFolder {
        volume
            .folders
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no folder '{path}' in {:?}", volume.folders))
    }

    #[test]
    fn browse_tree_computes_exact_folder_structure_per_volume() {
        let (_dir, root, app) = seeded();

        let out = browse_tree(&app, &root).expect("browse_tree");
        assert_eq!(out.volumes.len(), 2);

        let v = out
            .volumes
            .iter()
            .find(|v| v.id == online_volume_id())
            .expect("volume V");
        assert!(v.online, "root-labeled volume must read online");
        assert_eq!(v.folders.len(), 4, "'', A, A/B, C");
        assert_eq!(folder(v, "").children, vec!["A", "C"]);
        assert_eq!(folder(v, "").recursive_count, 3);
        assert_eq!(folder(v, "A").children, vec!["B"]);
        assert_eq!(folder(v, "A").recursive_count, 2);
        assert!(folder(v, "A/B").children.is_empty());
        assert_eq!(folder(v, "A/B").recursive_count, 1);
        assert!(folder(v, "C").children.is_empty());
        assert_eq!(folder(v, "C").recursive_count, 1);

        let w = out
            .volumes
            .iter()
            .find(|v| v.id == OFFLINE_VOLUME_ID)
            .expect("volume W");
        assert!(!w.online, "an unmounted label must read offline");
        assert_eq!(folder(w, "").children, vec!["D"]);
        assert_eq!(folder(w, "").recursive_count, 1);
        assert!(folder(w, "D").children.is_empty());
        assert_eq!(folder(w, "D").recursive_count, 1);

        assert!(
            out.notices.iter().any(|n| n.contains(OFFLINE_VOLUME_LABEL)),
            "an offline volume must be named in a notice: {:?}",
            out.notices
        );
    }

    #[test]
    fn browse_tree_of_an_empty_volume_is_just_the_root_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        app.emit(vec![Op::VolumeSeen {
            volume: online_volume_id().into(),
            label: online_volume_label().into(),
        }])
        .expect("emit");

        let out = browse_tree(&app, &root).expect("browse_tree");
        assert_eq!(out.volumes.len(), 1);
        assert_eq!(
            out.volumes[0].folders,
            vec![BrowseFolder {
                path: String::new(),
                children: Vec::new(),
                recursive_count: 0,
            }],
            "a volume with no cataloged instances has exactly one folder: an empty root"
        );
    }

    #[test]
    fn browse_tree_collapses_several_offline_volumes_into_one_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        app.emit(vec![
            Op::VolumeSeen {
                volume: "label:offline-a".into(),
                label: "offline-a".into(),
            },
            Op::VolumeSeen {
                volume: "label:offline-b".into(),
                label: "offline-b".into(),
            },
        ])
        .expect("emit");

        let out = browse_tree(&app, &root).expect("browse_tree");
        assert_eq!(
            out.notices.len(),
            1,
            "two offline volumes must fold into one banner line, not two: {:?}",
            out.notices
        );
        assert!(out.notices[0].contains("offline-a"));
        assert!(out.notices[0].contains("offline-b"));
    }

    #[test]
    fn browse_list_flatten_true_counts_the_whole_subtree() {
        let (_dir, root, app) = seeded();

        let out = browse_list(&app, &root, &req("A")).expect("browse_list");
        assert_eq!(out.count, 2, "x.mov and y.jpg are both under A");
        assert_eq!(out.folder_count, 2, "A and A/B");
    }

    #[test]
    fn browse_list_flatten_false_is_the_immediate_folder_only() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                flatten: false,
                ..req("A")
            },
        )
        .expect("browse_list");
        assert_eq!(out.count, 1, "only x.mov sits directly in A");
        let hit = &out.results[0];
        assert_eq!(hit.name, "x.mov");
        assert_eq!(hit.size, Some(10), "the scoped instance's own size");
        assert_eq!(hit.mtime_ms, Some(3000), "the scoped instance's own mtime");
        assert_eq!(hit.kind.as_deref(), Some("video"));
    }

    #[test]
    fn browse_list_flatten_false_at_root_scope_only_matches_root_level_files() {
        let (_dir, root, mut app) = seeded();
        app.emit(vec![Op::AssetSeen {
            asset: asset_id("root"),
            volume: online_volume_id().into(),
            path: "root.mov".into(),
            size: 5,
            mtime_ms: 50,
        }])
        .expect("emit a root-level asset");

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                flatten: false,
                ..req("")
            },
        )
        .expect("browse_list");
        assert_eq!(
            out.count, 1,
            "only root.mov sits at the volume root — A/x.mov etc. don't"
        );
        assert_eq!(out.results[0].name, "root.mov");
    }

    #[test]
    fn browse_list_rows_carry_exact_size_mtime_kind_and_online_volume() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                sort: Some("name".into()),
                ..req("")
            },
        )
        .expect("browse_list");
        let by_name: HashMap<&str, &SearchHit> =
            out.results.iter().map(|r| (r.name.as_str(), r)).collect();
        let x = by_name["x.mov"];
        assert_eq!(
            (x.size, x.mtime_ms, x.kind.as_deref()),
            (Some(10), Some(3000), Some("video"))
        );
        assert_eq!(x.volumes.len(), 1);
        assert!(
            x.volumes[0].online,
            "the root-labeled volume must read online in the row's VolumeRef too, \
             the same predicate BrowseVolume::online uses"
        );
        let y = by_name["y.jpg"];
        assert_eq!(
            (y.size, y.mtime_ms, y.kind.as_deref()),
            (Some(20), Some(1000), Some("image"))
        );
        let z = by_name["z.pdf"];
        assert_eq!(
            (z.size, z.mtime_ms, z.kind.as_deref()),
            (Some(30), Some(2000), Some("pdf"))
        );
    }

    /// An asset with two matching instances under scope must pick one
    /// deterministic representative — highest `mtime_ms`, tie broken toward
    /// the lexicographically smaller path — and its `size`/`mtime_ms`/`kind`
    /// must come from exactly that instance, not an arbitrary one. And the
    /// tree's `recursive_count` for the same subtree must agree with the
    /// list's `count`: both dedupe by asset id, so a folder with one asset
    /// in two instances reads as ONE asset from either verb — that agreement
    /// is the whole point of the sidebar badge predicting the grid.
    ///
    /// The newer instance is deliberately the lexicographically LATER path.
    /// Instances arrive in path order (`AssetState::instances` is a
    /// `BTreeMap` keyed by `(volume, path)`), so a fixture whose newest
    /// instance also sorts first cannot tell the pick rule from "keep
    /// whichever arrived first" — which is exactly what phase 7D's mutation
    /// run caught: four mutants of `is_better`/`dedupe_by_asset`'s guard all
    /// survived an earlier version of this test that used `E/newer.mov` and
    /// `E/older.mov`.
    #[test]
    fn browse_tree_and_list_agree_on_asset_count_for_two_instances_of_one_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let shared = asset_id("shared");
        app.emit(vec![
            Op::VolumeSeen {
                volume: online_volume_id().into(),
                label: online_volume_label().into(),
            },
            Op::AssetSeen {
                asset: shared.clone(),
                volume: online_volume_id().into(),
                path: "E/a-older.mov".into(),
                size: 100,
                mtime_ms: 500,
            },
            Op::AssetSeen {
                asset: shared,
                volume: online_volume_id().into(),
                path: "E/z-newer.mov".into(),
                size: 200,
                mtime_ms: 900,
            },
        ])
        .expect("emit");

        let list = browse_list(&app, &root, &req("")).expect("browse_list");
        assert_eq!(
            list.count, 1,
            "one asset, two instances — deduped to one row"
        );
        // `size`/`mtime_ms`/`kind` come from the representative instance;
        // `name` does NOT — it comes from the asset's catalog summary,
        // which is one row per asset regardless of how many instances it
        // has. Asserting the representative through `name` would be
        // asserting the summary instead.
        let hit = &list.results[0];
        assert_eq!(hit.size, Some(200), "the newer-mtime instance wins");
        assert_eq!(hit.mtime_ms, Some(900), "the newer-mtime instance wins");

        let tree = browse_tree(&app, &root).expect("browse_tree");
        let v = tree
            .volumes
            .iter()
            .find(|v| v.id == online_volume_id())
            .expect("volume V");
        assert_eq!(
            folder(v, "E").recursive_count,
            1,
            "the tree's folder badge must agree with the list's count: one \
             asset, not two instances"
        );
        assert_eq!(
            folder(v, "").recursive_count,
            1,
            "same asset, counted once at the root too"
        );
    }

    /// The other half of the representative rule: equal `mtime_ms` breaks
    /// toward the lexicographically smaller path. The two instances carry
    /// different sizes because `size` is the only field on the row that
    /// distinguishes them — see the test above on why `name` cannot.
    #[test]
    fn two_instances_with_the_same_mtime_pick_the_smaller_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let shared = asset_id("tied");
        app.emit(vec![
            Op::VolumeSeen {
                volume: online_volume_id().into(),
                label: online_volume_label().into(),
            },
            Op::AssetSeen {
                asset: shared.clone(),
                volume: online_volume_id().into(),
                path: "F/a.mov".into(),
                size: 10,
                mtime_ms: 700,
            },
            Op::AssetSeen {
                asset: shared,
                volume: online_volume_id().into(),
                path: "F/b.mov".into(),
                size: 20,
                mtime_ms: 700,
            },
        ])
        .expect("emit");

        let list = browse_list(&app, &root, &req("")).expect("browse_list");
        assert_eq!(list.count, 1, "one asset, two instances");
        assert_eq!(
            list.results[0].size,
            Some(10),
            "the smaller path breaks the tie — a.mov's size, not b.mov's"
        );
    }

    #[test]
    fn browse_list_sorts_by_name_ascending() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                sort: Some("name".into()),
                ..req("")
            },
        )
        .expect("browse_list");
        let names: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["x.mov", "y.jpg", "z.pdf"]);
    }

    #[test]
    fn browse_list_sorts_by_captured_newest_first_by_default() {
        let (_dir, root, app) = seeded();

        let out = browse_list(&app, &root, &req("")).expect("browse_list");
        // x.mov=3000, z.pdf=2000, y.jpg=1000 — newest mtime_ms first.
        let names: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["x.mov", "z.pdf", "y.jpg"]);
    }

    #[test]
    fn browse_list_sorts_by_size_descending() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                sort: Some("size".into()),
                ..req("")
            },
        )
        .expect("browse_list");
        // z.pdf=30, y.jpg=20, x.mov=10 — largest first.
        let names: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["z.pdf", "y.jpg", "x.mov"]);
    }

    #[test]
    fn browse_list_kind_filter_matches_only_that_kind() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                kind: Some("image".into()),
                ..req("")
            },
        )
        .expect("browse_list");
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "y.jpg");
    }

    #[test]
    fn browse_list_rejects_an_unknown_sort_naming_valid_values() {
        let (_dir, root, app) = seeded();

        let err = browse_list(
            &app,
            &root,
            &BrowseRequest {
                sort: Some("bogus".into()),
                ..req("")
            },
        )
        .expect_err("must reject unknown sort");
        let msg = err.to_string();
        assert!(msg.contains("bogus"));
        assert!(msg.contains("captured"));
        assert!(msg.contains("name"));
        assert!(msg.contains("size"));
    }

    #[test]
    fn browse_list_rejects_an_unknown_kind_naming_valid_values() {
        let (_dir, root, app) = seeded();

        let err = browse_list(
            &app,
            &root,
            &BrowseRequest {
                kind: Some("bogus".into()),
                ..req("")
            },
        )
        .expect_err("must reject unknown kind");
        let msg = err.to_string();
        assert!(msg.contains("bogus"));
        assert!(msg.contains("image"));
        assert!(msg.contains("video"));
        assert!(msg.contains("audio"));
        assert!(msg.contains("pdf"));
        assert!(msg.contains("other"));
    }

    #[test]
    fn browse_list_limit_and_offset_paginate_after_sorting() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                sort: Some("name".into()),
                limit: 1,
                offset: 1,
                ..req("")
            },
        )
        .expect("browse_list");
        assert_eq!(out.count, 3, "count is pre-pagination");
        assert_eq!(
            out.folder_count, 3,
            "folder_count is pre-pagination too — A, A/B, C"
        );
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].name, "y.jpg", "second row in name order");
    }

    #[test]
    fn browse_list_offset_past_the_end_is_empty_results_with_counts_intact() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                offset: 10,
                ..req("")
            },
        )
        .expect("browse_list");
        assert_eq!(
            out.count, 3,
            "count doesn't shrink just because offset overruns"
        );
        assert_eq!(out.folder_count, 3);
        assert!(out.results.is_empty());
    }

    #[test]
    fn browse_list_rejects_an_unknown_volume_naming_it_and_the_remedy() {
        let (_dir, root, app) = seeded();

        let err = browse_list(
            &app,
            &root,
            &BrowseRequest {
                volume: "no-such-volume".into(),
                ..req("")
            },
        )
        .expect_err("must reject unknown volume");
        let msg = err.to_string();
        assert!(msg.contains("no-such-volume"));
        assert!(msg.contains("maj volumes list"));
    }

    #[test]
    fn browse_list_on_an_offline_volume_works_from_the_catalog_alone_with_a_notice() {
        let (_dir, root, app) = seeded();

        let out = browse_list(
            &app,
            &root,
            &BrowseRequest {
                volume: OFFLINE_VOLUME_ID.into(),
                ..req("")
            },
        )
        .expect("browse_list against an offline volume must still work");
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "w.mov");
        assert!(
            out.notices.iter().any(|n| n.contains(OFFLINE_VOLUME_LABEL)),
            "offline volume must be noticed: {:?}",
            out.notices
        );
    }
}
