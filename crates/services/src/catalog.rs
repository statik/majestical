//! Opens the ad hoc sqlite view of the catalog shared by every read path
//! (`search`, `volumes list`, `para list`) — the open+sync pair lives in
//! exactly one place. Moved verbatim from `crates/cli/src/commands.rs`.
//!
//! Also home to [`get_asset`]: not a CLI verb, but the single-asset assembly
//! read the upcoming MCP `get_asset` tool and the GUI inspector both need —
//! reads the in-memory [`Projection`] directly rather than opening the
//! sqlite view, since nothing here needs a query planner.
use crate::app::{FsApp, warn_skipped_corrupt_lines};
use crate::error::ServiceError;
use crate::volumes::volume_is_online;
use anyhow::{Context, Result};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::{AssetId, ParaKind};
use majestical_core::projection::{AssetState, Projection};
use majestical_index::blob::{BlobStore, Derivation, asset_hex};
use std::path::Path;

/// Opens the sqlite catalog from the per-machine local state dir (see
/// `state_dir`), applying only the events past its last-saved cursor (or
/// rebuilding from scratch if there's no usable saved state).
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or the sqlite
/// catalog fails to open or sync.
pub fn open_catalog(app: &FsApp, catalog_dir: &Path) -> Result<(SqliteCatalog, Projection)> {
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let mut skipped = 0usize;
    let (db, projection, _mode) =
        SqliteCatalog::open_synced(&paths.db_path, app.log(), &mut |_line| skipped += 1)
            .context("opening sqlite catalog")?;
    warn_skipped_corrupt_lines(skipped, catalog_dir);
    Ok((db, projection))
}

/// One volume holding an instance of the asset: its recorded identity, label,
/// online status, and the instance's own recorded attributes.
#[derive(serde::Serialize)]
pub struct AssetInstance {
    pub volume: String,
    pub volume_label: String,
    pub online: bool,
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
}

/// One recorded hash-verification observation for the asset — a plain fact
/// per the ASC MHL action model, not deduped to "latest per volume": an
/// asset can carry more than one observation for the same volume/path over
/// its history (e.g. an `Original` hash followed by a later `Verified` or
/// `Failed` check), and callers that only want the newest can sort by
/// `hashdate_ms` themselves.
#[derive(serde::Serialize)]
pub struct AssetVerification {
    pub volume: String,
    pub path: String,
    pub algo: String,
    pub value: String,
    pub outcome: majestical_core::event::VerifyOutcome,
    pub hashdate_ms: u64,
}

/// Everything [`get_asset`] assembles about one asset.
#[derive(serde::Serialize)]
pub struct AssetDetail {
    pub asset: String,
    pub instances: Vec<AssetInstance>,
    pub tags: Vec<String>,
    /// `<kind>/<name>` of the assigned PARA node, if any — falls back to the
    /// bare node id if the node itself is unknown (an assignment can outlive
    /// or race its node's own create in a partial event log). Archived nodes
    /// render identically to live ones: archiving doesn't clear an asset's
    /// assignment, so this deliberately doesn't distinguish them — a caller
    /// that needs the archived flag queries the node's own state separately
    /// (`maj para list`).
    pub para: Option<String>,
    pub fields: Vec<(String, String)>,
    pub verifications: Vec<AssetVerification>,
    pub has_thumb: bool,
}

/// The lowercase form `ParaKind` serializes as elsewhere in the project (see
/// `event::Op`'s `snake_case` wire format), used to render `para` as
/// `<kind>/<name>`.
fn para_kind_str(kind: ParaKind) -> &'static str {
    match kind {
        ParaKind::Project => "project",
        ParaKind::Area => "area",
        ParaKind::Resource => "resource",
        ParaKind::Archive => "archive",
    }
}

/// The label recorded for `volume`, or the volume id itself if the catalog
/// never saw a `VolumeSeen` for it (an instance can reference a volume id
/// with no matching sighting in a partial event log).
fn volume_label(projection: &Projection, volume: &str) -> String {
    projection
        .volumes()
        .find(|(id, _)| id.as_str() == volume)
        .and_then(|(_, state)| state.label())
        .unwrap_or(volume)
        .to_string()
}

/// Every recorded (volume, path) instance of the asset, each paired with its
/// volume's label and current online status.
fn build_instances(projection: &Projection, state: &AssetState) -> Vec<AssetInstance> {
    state
        .instances
        .iter()
        .map(|((volume, path), info)| {
            let label = volume_label(projection, volume);
            AssetInstance {
                online: volume_is_online(volume, &label),
                volume: volume.clone(),
                volume_label: label,
                path: path.clone(),
                size: info.size,
                mtime_ms: info.mtime_ms,
            }
        })
        .collect()
}

/// The asset's assigned PARA node rendered as `<kind>/<name>`, or `None` if
/// unassigned.
fn resolve_para(projection: &Projection, asset: &AssetId) -> Option<String> {
    let node = projection.asset_para(asset)?;
    let Some(state) = projection.para_node(node) else {
        return Some(node.to_string());
    };
    match (state.kind(), state.name()) {
        (Some(kind), Some(name)) => Some(format!("{}/{name}", para_kind_str(kind))),
        _ => Some(node.to_string()),
    }
}

/// Every hash-verification fact recorded for the asset.
fn build_verifications(projection: &Projection, asset: &AssetId) -> Vec<AssetVerification> {
    projection
        .verifications(asset)
        .map(|record| AssetVerification {
            volume: record.volume.clone(),
            path: record.path.clone(),
            algo: record.algo.clone(),
            value: record.value.clone(),
            outcome: record.outcome,
            hashdate_ms: record.hashdate_ms,
        })
        .collect()
}

/// Whether a thumbnail blob has been derived for the asset.
fn thumb_exists(catalog_dir: &Path, asset_id: &str) -> bool {
    let blobs = BlobStore::new(catalog_dir);
    asset_hex(asset_id).is_some_and(|hex| blobs.path_for(hex, &Derivation::Thumb).is_file())
}

/// Assembles everything the catalog knows about one asset — instances,
/// tags, PARA assignment, fields, verification history, and whether a
/// thumbnail exists — in a single call. Not a CLI verb: it exists for the
/// MCP `get_asset` tool and the GUI inspector.
///
/// Returns `Ok(None)` for an unknown asset — "not found" is a value here
/// (both callers need to render it), not an error.
///
/// # Errors
/// Returns an error if the event log cannot be read.
pub fn get_asset(
    app: &FsApp,
    catalog_dir: &Path,
    asset_id: &str,
) -> Result<Option<AssetDetail>, ServiceError> {
    get_asset_impl(app, catalog_dir, asset_id).map_err(ServiceError::from)
}

fn get_asset_impl(app: &FsApp, catalog_dir: &Path, asset_id: &str) -> Result<Option<AssetDetail>> {
    let projection = app.projection()?;
    let asset = AssetId(asset_id.to_string());
    let Some((_, state)) = projection.assets().find(|(id, _)| **id == asset) else {
        return Ok(None);
    };
    Ok(Some(AssetDetail {
        asset: asset_id.to_string(),
        instances: build_instances(&projection, state),
        tags: projection.tags(&asset).into_iter().collect(),
        para: resolve_para(&projection, &asset),
        fields: projection
            .fields(&asset)
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        verifications: build_verifications(&projection, &asset),
        has_thumb: thumb_exists(catalog_dir, asset_id),
    }))
}

#[cfg(test)]
mod get_asset_tests {
    use super::*;
    use majestical_core::event::{Op, ParaKind, VerifyOutcome};

    fn asset_id() -> AssetId {
        AssetId("xxh3:0123456789abcdef0123456789abcdef".to_string())
    }

    #[test]
    fn get_asset_assembles_instances_tags_para_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let asset = asset_id();
        let node = ulid::Ulid::generate().to_string();
        app.emit(vec![
            Op::VolumeSeen {
                volume: "vol1".into(),
                label: "vol1".into(),
            },
            Op::AssetSeen {
                asset: asset.clone(),
                volume: "vol1".into(),
                path: "clips/a.mov".into(),
                size: 5,
                mtime_ms: 1000,
            },
            Op::TagAdd {
                asset: asset.clone(),
                tag: "demo".into(),
            },
            Op::FieldSet {
                asset: asset.clone(),
                field: "shot".into(),
                value: "sunset".into(),
            },
            Op::ParaNodeCreate {
                node: node.clone(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            },
            Op::AssetParaSet {
                asset: asset.clone(),
                node,
            },
        ])
        .expect("emit");
        let out = get_asset(&app, &root, &asset.0)
            .expect("get_asset")
            .expect("known asset");
        assert_eq!(out.instances.len(), 1);
        assert_eq!(out.tags, vec!["demo"]);
        assert_eq!(out.para.as_deref(), Some("project/client-x"));
        assert_eq!(out.fields[0], ("shot".to_string(), "sunset".to_string()));
        assert!(out.verifications.is_empty());
        assert!(!out.has_thumb);
    }

    /// Pins every `AssetInstance` field, not just `instances.len()` — a prior
    /// version of this test only checked the count, so a bug scrambling
    /// `volume_label` or `online` (e.g. swapping which volume's label gets
    /// attached, or hardcoding `online: false`) passed the suite unnoticed.
    /// Two volumes with genuinely different online status (the always-online
    /// root label vs. an unmounted label) forces `online` to actually differ
    /// per instance rather than incidentally agreeing.
    #[test]
    fn get_asset_reports_differing_instances_across_two_volumes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let asset = asset_id();
        app.emit(vec![
            Op::VolumeSeen {
                volume: "label:root".into(),
                label: "root".into(),
            },
            Op::VolumeSeen {
                volume: "label:probe-offline-vol-xyz".into(),
                label: "probe-offline-vol-xyz".into(),
            },
            Op::AssetSeen {
                asset: asset.clone(),
                volume: "label:root".into(),
                path: "clips/a.mov".into(),
                size: 5,
                mtime_ms: 1000,
            },
            Op::AssetSeen {
                asset: asset.clone(),
                volume: "label:probe-offline-vol-xyz".into(),
                path: "backup/a.mov".into(),
                size: 9,
                mtime_ms: 2000,
            },
        ])
        .expect("emit");
        let out = get_asset(&app, &root, &asset.0)
            .expect("get_asset")
            .expect("known asset");
        assert_eq!(out.instances.len(), 2);
        let root_instance = out
            .instances
            .iter()
            .find(|i| i.volume == "label:root")
            .expect("root instance");
        assert_eq!(
            (
                root_instance.volume.as_str(),
                root_instance.volume_label.as_str(),
                root_instance.online,
                root_instance.path.as_str(),
                root_instance.size,
                root_instance.mtime_ms,
            ),
            ("label:root", "root", true, "clips/a.mov", 5, 1000),
            "root-labeled volume must be online and carry its own attributes"
        );
        let offline_instance = out
            .instances
            .iter()
            .find(|i| i.volume == "label:probe-offline-vol-xyz")
            .expect("offline instance");
        assert_eq!(
            (
                offline_instance.volume.as_str(),
                offline_instance.volume_label.as_str(),
                offline_instance.online,
                offline_instance.path.as_str(),
                offline_instance.size,
                offline_instance.mtime_ms,
            ),
            (
                "label:probe-offline-vol-xyz",
                "probe-offline-vol-xyz",
                false,
                "backup/a.mov",
                9,
                2000
            ),
            "an unmounted label must read offline and keep its own attributes"
        );
    }

    #[test]
    fn get_asset_of_an_unknown_asset_is_ok_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let out =
            get_asset(&app, &root, "xxh3:ffffffffffffffffffffffffffffffff").expect("get_asset");
        assert!(out.is_none());
    }

    #[test]
    fn get_asset_reports_has_thumb_when_a_thumb_blob_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let asset = asset_id();
        app.emit(vec![Op::AssetSeen {
            asset: asset.clone(),
            volume: "vol1".into(),
            path: "clips/a.mov".into(),
            size: 5,
            mtime_ms: 1000,
        }])
        .expect("emit");
        let blobs = BlobStore::new(&root);
        let hex = asset_hex(&asset.0).expect("hex");
        let thumb_path = blobs.path_for(hex, &Derivation::Thumb);
        blobs
            .write_atomic(&thumb_path, b"fake-thumb")
            .expect("write thumb");
        let out = get_asset(&app, &root, &asset.0)
            .expect("get_asset")
            .expect("known asset");
        assert!(out.has_thumb);
    }

    #[test]
    fn get_asset_lists_recorded_verifications() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let asset = asset_id();
        app.emit(vec![
            Op::AssetSeen {
                asset: asset.clone(),
                volume: "vol1".into(),
                path: "clips/a.mov".into(),
                size: 5,
                mtime_ms: 1000,
            },
            Op::VerificationRecorded {
                asset: asset.clone(),
                volume: "vol1".into(),
                path: "clips/a.mov".into(),
                algo: "xxh64".into(),
                value: "0011223344556677".into(),
                outcome: VerifyOutcome::Verified,
                hashdate_ms: 42,
            },
        ])
        .expect("emit");
        let out = get_asset(&app, &root, &asset.0)
            .expect("get_asset")
            .expect("known asset");
        assert_eq!(out.verifications.len(), 1);
        let v = &out.verifications[0];
        assert_eq!(v.volume, "vol1");
        assert_eq!(v.path, "clips/a.mov");
        assert_eq!(v.algo, "xxh64");
        assert_eq!(v.value, "0011223344556677");
        assert_eq!(v.outcome, VerifyOutcome::Verified);
        assert_eq!(v.hashdate_ms, 42);
    }
}
