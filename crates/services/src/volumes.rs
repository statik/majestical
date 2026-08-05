//! `maj volumes list` compute: catalog volumes joined with per-volume asset
//! counts, online status, and a clock-suspect flag. Moved from
//! `crates/cli/src/commands.rs::cmd_volumes_list`; the CLI keeps `iso8601_ms`
//! formatting and table/`json!` rendering, fed from [`VolumesOutcome`].
use crate::app::{FsApp, physical_now_ms};
use crate::catalog::open_catalog;
use crate::error::ServiceError;
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_core::clock::MAX_DRIFT_MS;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One volume row: its recorded identity, when it was last seen, whether
/// it's currently mounted, how many assets it holds, and whether its
/// last-seen timestamp is implausibly far in the future.
#[derive(Serialize)]
pub struct VolumeRow {
    pub id: String,
    pub label: String,
    pub last_seen_ms: u64,
    pub online: bool,
    pub asset_count: u64,
    pub clock_suspect: bool,
}

/// Everything `maj volumes list` renders.
#[derive(Serialize)]
pub struct VolumesOutcome {
    pub volumes: Vec<VolumeRow>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Cheap phase-2 "is this volume mounted right now" heuristic, not true
/// device enumeration. `label:`-id volumes are considered online if
/// `/Volumes/<label>` exists (or the label is the root volume's, which is
/// always present). `uuid:`-id volumes are considered online only if a
/// mount at `/Volumes/<label>` exists *and* resolving its identity still
/// yields the same id — so a same-named but different card reads offline.
/// False negative: a volume mounted somewhere other than `/Volumes` reads
/// offline even when present.
#[must_use]
pub fn volume_is_online(id: &str, label: &str) -> bool {
    if label == volume_identity::ROOT_LABEL {
        return true;
    }
    let candidate = PathBuf::from("/Volumes").join(label);
    if !candidate.exists() {
        return false;
    }
    if id.starts_with("uuid:") {
        return volume_identity::resolve(&candidate).id == id;
    }
    true
}

/// `maj volumes list`: every volume the catalog has ever seen, with its
/// asset count, online status, and clock-suspect flag.
///
/// # Errors
/// Returns an error if the sqlite catalog can't be opened/synced or the
/// volumes/asset-count queries fail.
pub fn volumes_list(app: &FsApp, catalog_dir: &Path) -> Result<VolumesOutcome, ServiceError> {
    volumes_list_impl(app, catalog_dir).map_err(ServiceError::from)
}

fn volumes_list_impl(app: &FsApp, catalog_dir: &Path) -> Result<VolumesOutcome> {
    let (db, _projection) = open_catalog(app, catalog_dir)?;
    let volumes = db.volumes().context("querying volumes")?;
    let counts: HashMap<String, u64> = db
        .volume_asset_counts()
        .context("querying volume asset counts")?
        .into_iter()
        .collect();
    // A stored last-seen wall time past this ceiling could only have come
    // from a clock more than MAX_DRIFT_MS ahead of physical now — the HLC
    // clamp bounds the *local* clock's adoption of such a timestamp, but
    // doesn't touch what's already durable in the event log, so a poisoned
    // VolumeSeen can still win the LWW max and display forever unflagged.
    let suspect_ceiling = physical_now_ms().saturating_add(MAX_DRIFT_MS);
    let rows = volumes
        .iter()
        .map(|(id, label, last_seen_ms)| VolumeRow {
            id: id.clone(),
            label: label.clone(),
            last_seen_ms: *last_seen_ms,
            online: volume_is_online(id, label),
            asset_count: counts.get(id).copied().unwrap_or(0),
            clock_suspect: *last_seen_ms > suspect_ceiling,
        })
        .collect();
    Ok(VolumesOutcome {
        volumes: rows,
        notices: app.notices().drain(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::event::Op;

    #[test]
    fn volumes_list_reports_asset_count_and_offline_by_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        app.emit(vec![
            Op::VolumeSeen {
                volume: "vol1".into(),
                label: "vol1".into(),
            },
            Op::AssetSeen {
                asset: majestical_core::event::AssetId(
                    "xxh3:0123456789abcdef0123456789abcdef".into(),
                ),
                volume: "vol1".into(),
                path: "clip.txt".into(),
                size: 5,
                mtime_ms: 1000,
            },
        ])
        .expect("emit");
        let outcome = volumes_list(&app, &root).expect("volumes_list");
        assert_eq!(outcome.volumes.len(), 1);
        let row = &outcome.volumes[0];
        assert_eq!(row.id, "vol1");
        assert_eq!(row.asset_count, 1);
        assert!(!row.online, "a synthetic --volume id has no real mount");
        assert!(!row.clock_suspect);
    }

    #[test]
    fn volume_is_online_treats_root_label_as_always_present() {
        assert!(volume_is_online("label:root", volume_identity::ROOT_LABEL));
    }

    #[test]
    fn volume_is_online_is_false_for_an_unmounted_label() {
        assert!(!volume_is_online(
            "label:nonexistent-volume-xyz",
            "nonexistent-volume-xyz"
        ));
    }
}
