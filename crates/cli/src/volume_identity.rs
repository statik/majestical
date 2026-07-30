//! Resolve a stable identity for the volume containing a path.
//!
//! macOS: `diskutil info -plist <mount>` -> `VolumeUUID` (stable across
//! renames and machines). Anything else (or a `diskutil` failure): fall back
//! to the mount's last path component as both id and label — weaker, but
//! scan must never fail because identity resolution did.
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;

/// Label (and `label:`-id suffix) used when a mount point has no path
/// component of its own — i.e. "/". Shared with the CLI's `volume_is_online`
/// heuristic, which treats this label as always present.
pub const ROOT_LABEL: &str = "root";

pub struct VolumeIdentity {
    pub id: String,
    pub label: String,
}

#[must_use]
pub fn resolve(path: &Path) -> VolumeIdentity {
    let mount = mount_point_of(path);
    let label = mount.file_name().map_or_else(
        || ROOT_LABEL.to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    #[cfg(target_os = "macos")]
    if let Some(uuid) = diskutil_volume_uuid(&mount) {
        return VolumeIdentity {
            id: format!("uuid:{uuid}"),
            label,
        };
    }
    VolumeIdentity {
        id: format!("label:{label}"),
        label,
    }
}

/// Currently mounted volumes: id → mount point. "/" first so the root volume
/// wins its id even when /Volumes carries a symlink to it.
#[must_use]
pub(crate) fn mounted_volumes() -> std::collections::BTreeMap<String, std::path::PathBuf> {
    let mut map = std::collections::BTreeMap::new();
    let mut add = |path: std::path::PathBuf| {
        let identity = resolve(&path);
        map.entry(identity.id).or_insert(path);
    };
    add(std::path::PathBuf::from("/"));
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            add(entry.path());
        }
    }
    map
}

/// Walks up from `path` until the device id changes; the last path before
/// the change is the mount point. Falls back to `path` itself if metadata
/// can't be read (e.g. it doesn't exist).
fn mount_point_of(path: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Ok(start_meta) = std::fs::metadata(&start) else {
        return start;
    };
    let dev = start_meta.dev();
    let mut current = start.clone();
    while let Some(parent) = current.parent() {
        match std::fs::metadata(parent) {
            Ok(m) if m.dev() == dev => current = parent.to_path_buf(),
            _ => break,
        }
    }
    current
}

#[cfg(target_os = "macos")]
fn diskutil_volume_uuid(mount: &Path) -> Option<String> {
    let out = Command::new("diskutil")
        .args(["info", "-plist"])
        .arg(mount)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value: plist::Value = plist::from_bytes(&out.stdout).ok()?;
    value
        .as_dictionary()?
        .get("VolumeUUID")?
        .as_string()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_point_of_a_tempdir_is_an_existing_ancestor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).expect("mkdir -p");
        let mount = mount_point_of(&nested);
        assert!(mount.exists(), "resolved mount point must exist: {mount:?}");
        let canonical_nested = nested.canonicalize().expect("canonicalize nested");
        assert!(
            canonical_nested.starts_with(&mount),
            "{canonical_nested:?} must descend from resolved mount {mount:?}"
        );
    }

    #[test]
    fn resolve_never_panics_on_a_missing_path() {
        let identity = resolve(Path::new("/definitely/does/not/exist/anywhere"));
        assert!(!identity.id.is_empty());
        assert!(!identity.label.is_empty());
    }
}
