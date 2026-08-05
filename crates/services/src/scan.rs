//! `maj scan` compute: hash every file under a directory into `AssetSeen`
//! events. Moved from `crates/cli/src/commands.rs::cmd_scan`; the CLI keeps
//! only the `scanned: {n} assets` print, fed from [`ScanOutcome`].
use crate::app::FsApp;
use crate::error::ServiceError;
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_core::event::{AssetId, Op};
use std::io::Read;
use std::path::Path;

/// Resolves the (id, label) pair a scan should tag its events with. An
/// explicit `--volume` is used as both id and label — an override that
/// keeps e2e tests deterministic. Omitted, the volume's physical identity
/// is auto-detected (see `volume_identity`). Also used by `maj ingest`'s
/// source-volume resolution and `maj inbox process`'s quiescence probe —
/// the same "override or auto-detect" rule applies to both.
#[must_use]
pub fn resolve_volume(dir: &Path, volume: Option<String>) -> (String, String) {
    if let Some(v) = volume {
        return (v.clone(), v);
    }
    let identity = volume_identity::resolve(dir);
    (identity.id, identity.label)
}

/// A file's real modification time, in milliseconds since the Unix epoch —
/// `0` (meaning "unknown") if the platform can't report it or it predates
/// the epoch, rather than failing the whole scan/ingest over one file's
/// clock oddity. Shared by `scan`, `maj ingest`, and `maj inbox process`.
#[must_use]
pub fn mtime_ms_of(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Everything `maj scan` renders: how many assets it observed, and which
/// volume it tagged them with (auto-detected or the `--volume` override).
#[derive(Debug, serde::Serialize)]
pub struct ScanOutcome {
    pub assets: usize,
    pub volume_id: String,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// `maj scan`: hashes every file under `dir` into the catalog as `AssetSeen`
/// events, tagged with `volume` (or an auto-detected identity).
///
/// # Errors
/// Returns an error if `dir` can't be walked, a file's metadata or bytes
/// can't be read, or the event log can't be read or appended to.
pub fn scan(
    app: &mut FsApp,
    dir: &Path,
    volume: Option<String>,
) -> Result<ScanOutcome, ServiceError> {
    scan_impl(app, dir, volume).map_err(ServiceError::from)
}

fn scan_impl(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<ScanOutcome> {
    let auto_detect = volume.is_none();
    let (volume_id, volume_label) = resolve_volume(dir, volume);
    let mut ops = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.context("walking scan directory")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", entry.path().display()))?;
        let size = metadata.len();
        let file = std::fs::File::open(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        // Stream the hash rather than loading the whole file: media
        // assets can be multi-gigabyte, so a `Vec<u8>` per file would
        // blow up memory on a scan of a card full of video.
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        let mut reader = std::io::BufReader::new(file);
        let mut buf = vec![0u8; 64 * 1024].into_boxed_slice();
        loop {
            let n = reader
                .read(&mut buf)
                .with_context(|| format!("reading {}", entry.path().display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let hash = hasher.digest128();
        // Phase 1: lossy UTF-8 conversion of the relative path. JSON
        // events force UTF-8 anyway, so a non-UTF-8 path can't round
        // trip through the log yet; revisit once ingest needs to
        // preserve exact bytes.
        let scan_rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        // An explicit `--volume` override has no real mount to re-base
        // against (it's a synthetic id kept for e2e-test determinism), so
        // its instances stay scan-dir-relative, as before. An auto-detected
        // volume gets a path relative to the volume's actual root, so a
        // later indexer run can re-find the bytes regardless of which
        // subdirectory was scanned.
        let rel = if auto_detect {
            let abs = entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.path().to_path_buf());
            let mount = volume_identity::mount_point_of(&abs);
            abs.strip_prefix(&mount).map_or_else(
                |_| scan_rel.clone(),
                |p| p.to_string_lossy().replace('\\', "/"),
            )
        } else {
            scan_rel
        };
        ops.push(Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: volume_id.clone(),
            path: rel,
            size,
            mtime_ms: mtime_ms_of(&metadata),
        });
    }
    let n = ops.len();
    ops.insert(
        0,
        Op::VolumeSeen {
            volume: volume_id.clone(),
            label: volume_label,
        },
    );
    app.emit(ops)?;
    Ok(ScanOutcome {
        assets: n,
        volume_id,
        notices: app.notices().drain(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reports_asset_count_and_volume_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("a.txt"), b"alpha").expect("write");
        std::fs::write(src.join("b.txt"), b"beta").expect("write");
        let outcome = scan(&mut app, &src, Some("vol1".into())).expect("scan");
        assert_eq!(outcome.assets, 2);
        assert_eq!(outcome.volume_id, "vol1");
    }

    #[test]
    fn scan_of_an_empty_directory_reports_zero_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        let outcome = scan(&mut app, &src, Some("vol1".into())).expect("scan");
        assert_eq!(outcome.assets, 0);
    }

    #[test]
    fn resolve_volume_with_an_override_uses_it_as_both_id_and_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (id, label) = resolve_volume(dir.path(), Some("myvol".into()));
        assert_eq!((id.as_str(), label.as_str()), ("myvol", "myvol"));
    }
}
