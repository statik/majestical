//! Content-addressed derived-data store under `<sync-root>/blobs/`. Blobs are
//! keyed by derivation inputs (asset content hash + kind + model tag), so
//! writes are idempotent, rebuilds are directory walks, and two machines
//! deriving the same asset converge by construction.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::IndexError;

pub const THUMB_NAME: &str = "thumb-320.webp";
const ZSTD_LEVEL: i32 = 3;
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// One derivable artifact for an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivation<'a> {
    Thumb,
    ImageEmbedding {
        model_tag: &'a str,
    },
    KeyframeEmbedding {
        model_tag: &'a str,
        timestamp_ms: u64,
    },
    /// JSON list of keyframe timestamps; doubles as the "video fully
    /// keyframed" completion marker.
    KeyframeManifest {
        model_tag: &'a str,
    },
}

/// The catalog asset id is `xxh3:<32 hex>`; blob paths use the bare hex.
#[must_use]
pub fn asset_hex(asset_id: &str) -> Option<&str> {
    asset_id.strip_prefix("xxh3:")
}

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    #[must_use]
    pub fn new(sync_root: &Path) -> Self {
        Self {
            root: sync_root.join("blobs"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn path_for(&self, asset_hex: &str, derivation: &Derivation<'_>) -> PathBuf {
        let prefix = asset_hex.get(..2).unwrap_or("xx");
        let dir = self.root.join(prefix).join(asset_hex);
        match derivation {
            Derivation::Thumb => dir.join(THUMB_NAME),
            Derivation::ImageEmbedding { model_tag } => dir.join(model_tag).join("image.f32le.zst"),
            Derivation::KeyframeEmbedding {
                model_tag,
                timestamp_ms,
            } => dir
                .join(model_tag)
                .join(format!("kf-{timestamp_ms}.f32le.zst")),
            Derivation::KeyframeManifest { model_tag } => {
                dir.join(model_tag).join("keyframes.json")
            }
        }
    }

    /// Temp-name + rename so a crash never leaves a partial blob at a final
    /// path (the same rename-after-write rule the ingest engine follows).
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Blob`] if creating the parent directory,
    /// writing the temp file, or renaming it into place fails.
    pub fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), IndexError> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|source| IndexError::Blob {
            path: path.to_path_buf(),
            source,
        })?;

        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(".tmp-{}-{seq}", std::process::id());
        let temp_path = dir.join(temp_name);

        let write_result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temp_path)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if let Err(source) = write_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(IndexError::Blob {
                path: path.to_path_buf(),
                source,
            });
        }

        std::fs::rename(&temp_path, path).map_err(|source| IndexError::Blob {
            path: path.to_path_buf(),
            source,
        })
    }

    /// # Errors
    ///
    /// Returns [`IndexError::Blob`] if compression or the atomic write fails.
    pub fn write_vector(&self, path: &Path, vector: &[f32]) -> Result<(), IndexError> {
        let mut raw = Vec::with_capacity(vector.len() * 4);
        for value in vector {
            raw.extend_from_slice(&value.to_le_bytes());
        }
        let compressed =
            zstd::encode_all(&raw[..], ZSTD_LEVEL).map_err(|source| IndexError::Blob {
                path: path.to_path_buf(),
                source,
            })?;
        self.write_atomic(path, &compressed)
    }

    /// # Errors
    ///
    /// Returns [`IndexError::Blob`] if the file can't be read or decompressed,
    /// or [`IndexError::VectorShape`] if the decompressed length isn't a
    /// multiple of 4 bytes.
    pub fn read_vector(&self, path: &Path) -> Result<Vec<f32>, IndexError> {
        let compressed = std::fs::read(path).map_err(|source| IndexError::Blob {
            path: path.to_path_buf(),
            source,
        })?;
        let raw = zstd::decode_all(&compressed[..]).map_err(|source| IndexError::Blob {
            path: path.to_path_buf(),
            source,
        })?;
        if raw.len() % 4 != 0 {
            return Err(IndexError::VectorShape {
                path: path.to_path_buf(),
                len: raw.len(),
            });
        }
        let vector = raw
            .chunks_exact(4)
            .map(|chunk| {
                let bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
                f32::from_le_bytes(bytes)
            })
            .collect();
        Ok(vector)
    }
}

#[cfg(test)]
mod tests {
    use crate::blob::{BlobStore, Derivation, asset_hex};
    use crate::error::IndexError;

    #[test]
    fn blob_paths_are_derivation_keyed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            store.path_for(hex, &Derivation::Thumb),
            dir.path().join("blobs/01").join(hex).join("thumb-320.webp"),
        );
        assert_eq!(
            store.path_for(
                hex,
                &Derivation::ImageEmbedding {
                    model_tag: "siglip2-b16-v1"
                }
            ),
            dir.path()
                .join("blobs/01")
                .join(hex)
                .join("siglip2-b16-v1/image.f32le.zst"),
        );
        assert_eq!(
            store.path_for(
                hex,
                &Derivation::KeyframeEmbedding {
                    model_tag: "siglip2-b16-v1",
                    timestamp_ms: 4500
                }
            ),
            dir.path()
                .join("blobs/01")
                .join(hex)
                .join("siglip2-b16-v1/kf-4500.f32le.zst"),
        );
        assert_eq!(
            store.path_for(
                hex,
                &Derivation::KeyframeManifest {
                    model_tag: "siglip2-b16-v1"
                }
            ),
            dir.path()
                .join("blobs/01")
                .join(hex)
                .join("siglip2-b16-v1/keyframes.json"),
        );
    }

    #[test]
    fn vectors_round_trip_and_write_is_atomic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for("aa00", &Derivation::ImageEmbedding { model_tag: "m1" });
        let mut vector = Vec::with_capacity(768);
        let mut value = 0.0f32;
        for _ in 0..768 {
            vector.push(value / 768.0);
            value += 1.0;
        }
        store.write_vector(&path, &vector).expect("write");
        assert_eq!(store.read_vector(&path).expect("read"), vector);
        let siblings: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .flatten()
            .collect();
        assert_eq!(siblings.len(), 1, "no stray temp files beside the blob");
    }

    #[test]
    fn asset_hex_strips_the_hash_prefix() {
        assert_eq!(asset_hex("xxh3:abc123"), Some("abc123"));
        assert_eq!(asset_hex("sha1:abc123"), None);
    }

    #[test]
    fn read_vector_rejects_a_truncated_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for("bb11", &Derivation::ImageEmbedding { model_tag: "m1" });
        // Valid zstd stream whose decompressed length isn't a multiple of 4.
        let bogus = zstd::encode_all(&[1u8, 2, 3][..], 3).expect("compress");
        store.write_atomic(&path, &bogus).expect("write");
        assert!(matches!(
            store.read_vector(&path),
            Err(IndexError::VectorShape { len: 3, .. })
        ));
    }

    /// Replacing a blob is a rename into place, not a rewrite of the target
    /// file: a reader never sees a half-written blob, and the write succeeds
    /// even when the existing blob is read-only (rename needs write
    /// permission on the directory, not on the file being replaced).
    #[test]
    fn write_atomic_replaces_a_read_only_blob_by_rename() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for("cc22", &Derivation::Thumb);
        store.write_atomic(&path, b"old").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        store
            .write_atomic(&path, b"new")
            .expect("replace a read-only blob");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }
}
