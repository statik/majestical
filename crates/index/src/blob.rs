//! Content-addressed derived-data store under `<sync-root>/blobs/`. Blobs are
//! keyed by derivation inputs (asset content hash + kind + model tag), so
//! writes are idempotent, rebuilds are directory walks, and two machines
//! deriving the same asset converge by construction.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::IndexError;
use crate::thumbs::THUMB_EDGE;

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
    /// One extracted keyframe image (thumb-scale WebP) at a manifest
    /// timestamp. Scoped to the manifest's model tag: the timestamps are
    /// the manifest's, so the images live and die with it.
    KeyframeImage {
        model_tag: &'a str,
        timestamp_ms: u64,
    },
    /// Marker written once EVERY timestamp in the manifest has its
    /// `KeyframeImage` blob (mirrors [`Derivation::OcrComplete`]). Images
    /// without this marker mean an interrupted run: the item re-plans and
    /// existing images make the retry cheap.
    KeyframeImagesComplete {
        model_tag: &'a str,
    },
    /// Whisper transcript (JSON, zstd-compressed) for a video/audio asset.
    Transcript {
        model_tag: &'a str,
    },
    /// Text embedding for one transcript chunk (see `crate::chunk`).
    TranscriptChunk {
        model_tag: &'a str,
        start_ms: u64,
    },
    /// Recognized text (JSON, zstd-compressed) for a still image.
    OcrImage {
        model_tag: &'a str,
    },
    /// Recognized text (JSON, zstd-compressed) for one video keyframe.
    OcrKeyframe {
        model_tag: &'a str,
        timestamp_ms: u64,
    },
    /// Per-page extracted text (JSON, zstd-compressed) for a PDF asset.
    PdfText {
        model_tag: &'a str,
    },
    /// Describer caption (JSON, zstd-compressed) for a still image or PDF
    /// page render.
    Caption {
        model_tag: &'a str,
    },
    /// Describer captions (JSON, zstd-compressed) for a video's keyframes.
    Captions {
        model_tag: &'a str,
    },
    /// Describer tags (JSON, zstd-compressed) for an asset.
    Tags {
        model_tag: &'a str,
    },
    /// Marker written once every timestamp in a video's keyframe manifest
    /// has an OCR blob — cheaper than diffing every timestamp in the
    /// planner on every status call.
    OcrComplete {
        model_tag: &'a str,
    },
    /// Marker written in place of any `TranscriptChunk` blobs when a
    /// transcript chunked to zero chunks (e.g. an empty transcript).
    ChunksEmpty {
        model_tag: &'a str,
    },
    /// Marker written once EVERY chunk of a transcript has its
    /// `TranscriptChunk` blob and the chunks are indexed — the
    /// transcript-embed done signal (mirrors [`Derivation::OcrComplete`]).
    /// Individual chunk blobs without this marker mean an interrupted run:
    /// the item re-plans, and the existing blobs make the retry cheap.
    ChunksComplete {
        model_tag: &'a str,
    },
}

/// The exact length of the hex remainder of a well-formed asset id:
/// `xxh3_128` (see `crates/services/src/scan.rs`) always formats as
/// `{:032x}`, i.e. 32 lowercase hex digits — never fewer (no zero-trimming)
/// nor more.
const ASSET_HEX_LEN: usize = 32;

/// The catalog asset id is `xxh3:<32 lowercase hex digits>`; blob paths use
/// the bare hex. Validates the remainder is exactly `ASSET_HEX_LEN`
/// lowercase hex digits — not just that the `xxh3:` prefix is present —
/// because every caller of this function (`BlobStore::path_for` via
/// `plan_work`, the MCP `thumb`/`keyframes` resources) joins the result
/// straight onto a filesystem path with no further checking. Without this,
/// an id like `xxh3:../../../etc/passwd` would strip the prefix and hand
/// back a path-traversal payload as if it were a hex string; rejecting
/// anything that isn't the right length and alphabet closes that whole
/// class of malformed input, not just the specific `..` case.
#[must_use]
pub fn asset_hex(asset_id: &str) -> Option<&str> {
    let hex = asset_id.strip_prefix("xxh3:")?;
    let is_lower_hex_digit =
        |b: u8| b.is_ascii_hexdigit() && (b.is_ascii_digit() || b.is_ascii_lowercase());
    let is_valid = hex.len() == ASSET_HEX_LEN && hex.bytes().all(is_lower_hex_digit);
    is_valid.then_some(hex)
}

pub struct BlobStore {
    root: PathBuf,
}

/// One vector blob found on disk by [`BlobStore::iter_vectors`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorBlobRef {
    pub asset_hex: String,
    pub kind: String, // "image" | "keyframe" | "chunk"
    pub ts_ms: i64,
    pub path: PathBuf,
}

/// Classifies one filename under a `<hex>/<model_tag>/` dir as a vector, or
/// `None` for anything else (notably `keyframes.json`, the manifest that
/// lives alongside real vectors under the same dir, and
/// `kf-img-<edge>-<ts>.webp`, a keyframe image that shares the `kf-` prefix
/// this function splits on but fails the `.f32le.zst` suffix check).
fn classify_vector_file(name: &str) -> Option<(&'static str, i64)> {
    if name == "image.f32le.zst" {
        return Some(("image", -1));
    }
    if let Some(rest) = name.strip_prefix("kf-") {
        let ms = rest.strip_suffix(".f32le.zst")?;
        return Some(("keyframe", ms.parse().ok()?));
    }
    let ms = name.strip_prefix("chunk-")?.strip_suffix(".f32le.zst")?;
    Some(("chunk", ms.parse().ok()?))
}

impl BlobStore {
    #[must_use]
    pub fn new(sync_root: &Path) -> Self {
        Self {
            root: sync_root.join("blobs"),
        }
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
            Derivation::KeyframeImage {
                model_tag,
                timestamp_ms,
            } => dir
                .join(model_tag)
                .join(format!("kf-img-{THUMB_EDGE}-{timestamp_ms}.webp")),
            Derivation::KeyframeImagesComplete { model_tag } => {
                dir.join(model_tag).join("keyframe-images-complete.json")
            }
            Derivation::Transcript { model_tag } => dir.join(model_tag).join("transcript.json.zst"),
            Derivation::TranscriptChunk {
                model_tag,
                start_ms,
            } => dir
                .join(model_tag)
                .join(format!("chunk-{start_ms}.f32le.zst")),
            Derivation::OcrImage { model_tag } => dir.join(model_tag).join("image.json.zst"),
            Derivation::OcrKeyframe {
                model_tag,
                timestamp_ms,
            } => dir
                .join(model_tag)
                .join(format!("kf-{timestamp_ms}.json.zst")),
            Derivation::PdfText { model_tag } => dir.join(model_tag).join("text.json.zst"),
            Derivation::Caption { model_tag } => dir.join(model_tag).join("caption.json.zst"),
            Derivation::Captions { model_tag } => dir.join(model_tag).join("captions.json.zst"),
            Derivation::Tags { model_tag } => dir.join(model_tag).join("tags.json.zst"),
            Derivation::OcrComplete { model_tag } => dir.join(model_tag).join("ocr-complete.json"),
            Derivation::ChunksEmpty { model_tag } => dir.join(model_tag).join("chunks-empty.json"),
            Derivation::ChunksComplete { model_tag } => {
                dir.join(model_tag).join("chunks-complete.json")
            }
        }
    }

    /// True when transcript chunking has COMPLETED for the asset: either the
    /// [`Derivation::ChunksComplete`] marker (every chunk blob written and
    /// indexed) or the [`Derivation::ChunksEmpty`] marker (chunking
    /// legitimately produced zero chunks — an answer that must count as done
    /// too, or the planner would retry it forever) exists. Individual
    /// `TranscriptChunk` blobs deliberately do NOT count: they're written
    /// one at a time before the vector-store add, so their presence alone
    /// can mean an interrupted, partially indexed run.
    #[must_use]
    pub fn has_chunk_completion(&self, asset_hex: &str, model_tag: &str) -> bool {
        self.path_for(asset_hex, &Derivation::ChunksComplete { model_tag })
            .is_file()
            || self
                .path_for(asset_hex, &Derivation::ChunksEmpty { model_tag })
                .is_file()
    }

    /// Walks `blobs/` for every file named `file_name` across every asset and
    /// every model tag, returning `(asset_hex, model_tag, path)` triples.
    /// Unlike [`Self::iter_vectors`], this isn't pinned to one `model_tag` —
    /// it's built for tags/captions blobs, which a status/index consumer
    /// wants across whichever describer tag produced them. A missing
    /// `blobs/` root, or a missing per-asset dir, yields an empty result
    /// rather than an error — the walk is over a tree that mostly doesn't
    /// exist yet on a fresh catalog.
    ///
    /// # Errors
    /// Returns [`IndexError::Blob`] if a directory entry can't be read once
    /// the walk has started (a transient I/O error mid-walk).
    pub fn iter_named(
        &self,
        file_name: &str,
    ) -> Result<Vec<(String, String, PathBuf)>, IndexError> {
        let mut refs = Vec::new();
        let Ok(prefixes) = std::fs::read_dir(&self.root) else {
            return Ok(refs);
        };
        for prefix_entry in prefixes {
            let prefix_entry = prefix_entry.map_err(|source| IndexError::Blob {
                path: self.root.clone(),
                source,
            })?;
            if prefix_entry.file_type().is_ok_and(|t| t.is_dir()) {
                iter_named_under_prefix(&prefix_entry.path(), file_name, &mut refs)?;
            }
        }
        Ok(refs)
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

    /// Walks `blobs/` for every vector belonging to `model_tag` — the blob
    /// side of the blob↔Lance diff (this is how a teammate's synced vectors
    /// get indexed into this machine's local Lance store without
    /// re-inference). A missing `blobs/` root, or a missing per-asset
    /// `model_tag` subdirectory, yields an empty result rather than an
    /// error — the walk is over a tree that mostly doesn't exist yet on a
    /// fresh catalog.
    ///
    /// # Errors
    /// Returns [`IndexError::Blob`] if a directory entry can't be read once
    /// the walk has started (a transient I/O error mid-walk).
    pub fn iter_vectors(&self, model_tag: &str) -> Result<Vec<VectorBlobRef>, IndexError> {
        let mut refs = Vec::new();
        let Ok(prefixes) = std::fs::read_dir(&self.root) else {
            return Ok(refs);
        };
        for prefix_entry in prefixes {
            let prefix_entry = prefix_entry.map_err(|source| IndexError::Blob {
                path: self.root.clone(),
                source,
            })?;
            if prefix_entry.file_type().is_ok_and(|t| t.is_dir()) {
                iter_vectors_under_prefix(&prefix_entry.path(), model_tag, &mut refs)?;
            }
        }
        Ok(refs)
    }
}

fn iter_vectors_under_prefix(
    prefix_dir: &Path,
    model_tag: &str,
    refs: &mut Vec<VectorBlobRef>,
) -> Result<(), IndexError> {
    let Ok(asset_dirs) = std::fs::read_dir(prefix_dir) else {
        return Ok(());
    };
    for asset_entry in asset_dirs {
        let asset_entry = asset_entry.map_err(|source| IndexError::Blob {
            path: prefix_dir.to_path_buf(),
            source,
        })?;
        if !asset_entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let hex = asset_entry.file_name().to_string_lossy().into_owned();
        let model_dir = asset_entry.path().join(model_tag);
        let Ok(files) = std::fs::read_dir(&model_dir) else {
            continue;
        };
        for file_entry in files {
            let file_entry = file_entry.map_err(|source| IndexError::Blob {
                path: model_dir.clone(),
                source,
            })?;
            let name = file_entry.file_name();
            let Some((kind, ts_ms)) = classify_vector_file(&name.to_string_lossy()) else {
                continue;
            };
            refs.push(VectorBlobRef {
                asset_hex: hex.clone(),
                kind: kind.to_string(),
                ts_ms,
                path: file_entry.path(),
            });
        }
    }
    Ok(())
}

fn iter_named_under_prefix(
    prefix_dir: &Path,
    file_name: &str,
    refs: &mut Vec<(String, String, PathBuf)>,
) -> Result<(), IndexError> {
    let Ok(asset_dirs) = std::fs::read_dir(prefix_dir) else {
        return Ok(());
    };
    for asset_entry in asset_dirs {
        let asset_entry = asset_entry.map_err(|source| IndexError::Blob {
            path: prefix_dir.to_path_buf(),
            source,
        })?;
        if !asset_entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let hex = asset_entry.file_name().to_string_lossy().into_owned();
        let Ok(model_dirs) = std::fs::read_dir(asset_entry.path()) else {
            continue;
        };
        for model_entry in model_dirs {
            let model_entry = model_entry.map_err(|source| IndexError::Blob {
                path: asset_entry.path(),
                source,
            })?;
            if !model_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let model_tag = model_entry.file_name().to_string_lossy().into_owned();
            let candidate = model_entry.path().join(file_name);
            if candidate.is_file() {
                refs.push((hex.clone(), model_tag, candidate));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::blob::{BlobStore, Derivation, THUMB_NAME, asset_hex};
    use crate::error::IndexError;
    use crate::thumbs::THUMB_EDGE;

    /// A thumbnail size bump (`THUMB_EDGE`) must not silently keep writing
    /// blobs under the old size's filename — the two must always agree, or
    /// two different sizes would collide at the same content-addressed path.
    #[test]
    fn thumb_name_encodes_the_current_thumb_edge() {
        assert!(
            THUMB_NAME.contains(&THUMB_EDGE.to_string()),
            "THUMB_NAME ({THUMB_NAME}) must encode THUMB_EDGE ({THUMB_EDGE})"
        );
    }

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
    fn keyframe_image_paths_are_model_scoped_and_per_timestamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let a = store.path_for(
            hex,
            &Derivation::KeyframeImage {
                model_tag: "m1",
                timestamp_ms: 1500,
            },
        );
        let b = store.path_for(
            hex,
            &Derivation::KeyframeImage {
                model_tag: "m1",
                timestamp_ms: 2500,
            },
        );
        assert!(
            a.ends_with(format!("m1/kf-img-{THUMB_EDGE}-1500.webp")),
            "got {}",
            a.display()
        );
        assert!(
            b.ends_with(format!("m1/kf-img-{THUMB_EDGE}-2500.webp")),
            "got {}",
            b.display()
        );

        // A different model tag must land in its own directory, not collide
        // with `m1` — this is what earns "model_scoped" in the test name.
        let m2 = store.path_for(
            hex,
            &Derivation::KeyframeImage {
                model_tag: "m2",
                timestamp_ms: 1500,
            },
        );
        assert_ne!(a, m2, "different model tags must not share a path");

        let done = store.path_for(hex, &Derivation::KeyframeImagesComplete { model_tag: "m1" });
        assert!(
            done.ends_with("m1/keyframe-images-complete.json"),
            "got {}",
            done.display()
        );
    }

    /// A thumbnail size bump (`THUMB_EDGE`) must not silently keep writing
    /// keyframe image blobs under the old size's filename — the two must
    /// always agree, or two different sizes would collide at the same
    /// content-addressed path (mirrors `thumb_name_encodes_the_current_thumb_edge`).
    #[test]
    fn keyframe_image_name_encodes_the_current_thumb_edge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for(
            "aabb",
            &Derivation::KeyframeImage {
                model_tag: "m1",
                timestamp_ms: 1500,
            },
        );
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("keyframe image path has a filename");
        assert!(
            name.contains(&THUMB_EDGE.to_string()),
            "keyframe image filename ({name}) must encode THUMB_EDGE ({THUMB_EDGE})"
        );
    }

    #[test]
    fn transcript_blob_path_is_model_tagged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for(
            "aabbccdd",
            &Derivation::Transcript {
                model_tag: "whisper-large-v3-turbo-q5-v1",
            },
        );
        assert!(path.ends_with("aa/aabbccdd/whisper-large-v3-turbo-q5-v1/transcript.json.zst"));
    }

    #[test]
    fn transcript_chunk_blob_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for(
            "aabb",
            &Derivation::TranscriptChunk {
                model_tag: "minilm-l6-v2-v1",
                start_ms: 45_000,
            },
        );
        assert!(path.ends_with("aa/aabb/minilm-l6-v2-v1/chunk-45000.f32le.zst"));
    }

    #[test]
    fn ocr_blob_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let image = store.path_for(
            "aabb",
            &Derivation::OcrImage {
                model_tag: "applevision-r3-v1",
            },
        );
        assert!(image.ends_with("aa/aabb/applevision-r3-v1/image.json.zst"));
        let kf = store.path_for(
            "aabb",
            &Derivation::OcrKeyframe {
                model_tag: "applevision-r3-v1",
                timestamp_ms: 7_000,
            },
        );
        assert!(kf.ends_with("aa/aabb/applevision-r3-v1/kf-7000.json.zst"));
    }

    #[test]
    fn pdf_text_blob_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let path = store.path_for(
            "aabb",
            &Derivation::PdfText {
                model_tag: "pdfkit-v1",
            },
        );
        assert!(path.ends_with("aa/aabb/pdfkit-v1/text.json.zst"));
    }

    #[test]
    fn caption_captions_and_tags_blob_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caption = store.path_for(
            "aabb",
            &Derivation::Caption {
                model_tag: "describe-qwen3-vl-8b",
            },
        );
        assert!(caption.ends_with("aa/aabb/describe-qwen3-vl-8b/caption.json.zst"));
        let captions = store.path_for(
            "aabb",
            &Derivation::Captions {
                model_tag: "describe-qwen3-vl-8b",
            },
        );
        assert!(captions.ends_with("aa/aabb/describe-qwen3-vl-8b/captions.json.zst"));
        let tags = store.path_for(
            "aabb",
            &Derivation::Tags {
                model_tag: "describe-qwen3-vl-8b",
            },
        );
        assert!(tags.ends_with("aa/aabb/describe-qwen3-vl-8b/tags.json.zst"));
    }

    #[test]
    fn ocr_complete_and_chunks_empty_blob_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let ocr_complete = store.path_for(
            "aabb",
            &Derivation::OcrComplete {
                model_tag: "applevision-r3-v1",
            },
        );
        assert!(ocr_complete.ends_with("aa/aabb/applevision-r3-v1/ocr-complete.json"));
        let chunks_empty = store.path_for(
            "aabb",
            &Derivation::ChunksEmpty {
                model_tag: "minilm-l6-v2-v1",
            },
        );
        assert!(chunks_empty.ends_with("aa/aabb/minilm-l6-v2-v1/chunks-empty.json"));
        let chunks_complete = store.path_for(
            "aabb",
            &Derivation::ChunksComplete {
                model_tag: "minilm-l6-v2-v1",
            },
        );
        assert!(chunks_complete.ends_with("aa/aabb/minilm-l6-v2-v1/chunks-complete.json"));
    }

    #[test]
    fn has_chunk_completion_requires_a_marker_not_just_chunk_blobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        assert!(
            !store.has_chunk_completion("aabb", "minilm-l6-v2-v1"),
            "no model dir at all must be false"
        );

        // A chunk vector blob alone can mean an interrupted run whose store
        // add never happened — it must NOT count as completion.
        let chunk_path = store.path_for(
            "aabb",
            &Derivation::TranscriptChunk {
                model_tag: "minilm-l6-v2-v1",
                start_ms: 0,
            },
        );
        store
            .write_vector(&chunk_path, &[0.1, 0.2])
            .expect("write chunk");
        assert!(
            !store.has_chunk_completion("aabb", "minilm-l6-v2-v1"),
            "a chunk blob without a completion marker is NOT done"
        );

        let complete_marker = store.path_for(
            "aabb",
            &Derivation::ChunksComplete {
                model_tag: "minilm-l6-v2-v1",
            },
        );
        store
            .write_atomic(&complete_marker, b"{}")
            .expect("write complete marker");
        assert!(store.has_chunk_completion("aabb", "minilm-l6-v2-v1"));

        let empty_marker = store.path_for(
            "ccdd",
            &Derivation::ChunksEmpty {
                model_tag: "minilm-l6-v2-v1",
            },
        );
        store
            .write_atomic(&empty_marker, b"{}")
            .expect("write empty marker");
        assert!(
            store.has_chunk_completion("ccdd", "minilm-l6-v2-v1"),
            "the empty-transcript marker alone must also count as done"
        );
    }

    #[test]
    fn iter_named_finds_a_file_across_assets_and_model_tags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let tags_a = store.path_for(
            "aa11aa11aa11aa11aa11aa11aa11aa11",
            &Derivation::Tags {
                model_tag: "describe-qwen3-vl-8b",
            },
        );
        store.write_atomic(&tags_a, b"[]").expect("write tags a");
        let tags_b = store.path_for(
            "bb22bb22bb22bb22bb22bb22bb22bb22",
            &Derivation::Tags {
                model_tag: "describe-other-model",
            },
        );
        store.write_atomic(&tags_b, b"[]").expect("write tags b");

        let mut refs = store.iter_named("tags.json.zst").expect("iter_named");
        refs.sort();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs[0],
            (
                "aa11aa11aa11aa11aa11aa11aa11aa11".to_string(),
                "describe-qwen3-vl-8b".to_string(),
                tags_a
            )
        );
        assert_eq!(
            refs[1],
            (
                "bb22bb22bb22bb22bb22bb22bb22bb22".to_string(),
                "describe-other-model".to_string(),
                tags_b
            )
        );
    }

    #[test]
    fn iter_vectors_finds_transcript_chunk_vectors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let chunk_path = store.path_for(
            "cc33cc33cc33cc33cc33cc33cc33cc33",
            &Derivation::TranscriptChunk {
                model_tag: "minilm-l6-v2-v1",
                start_ms: 45_000,
            },
        );
        store
            .write_vector(&chunk_path, &[0.7, 0.8])
            .expect("write chunk vector");

        let refs = store.iter_vectors("minilm-l6-v2-v1").expect("iter_vectors");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].asset_hex, "cc33cc33cc33cc33cc33cc33cc33cc33");
        assert_eq!(refs[0].kind, "chunk");
        assert_eq!(refs[0].ts_ms, 45_000);
        assert_eq!(refs[0].path, chunk_path);
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
        assert_eq!(
            asset_hex("xxh3:0123456789abcdef0123456789abcdef"),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(asset_hex("sha1:0123456789abcdef0123456789abcdef"), None);
    }

    /// The remainder must be exactly [`ASSET_HEX_LEN`] lowercase hex digits —
    /// too short, too long, uppercase, or containing non-hex characters must
    /// all be rejected, not just an absent `xxh3:` prefix. This is the
    /// traversal defense: `BlobStore::path_for` joins whatever `asset_hex`
    /// hands back straight onto a filesystem path with no further checking,
    /// so a malformed id like `xxh3:../../../etc/passwd` must be rejected
    /// here rather than silently becoming a path-traversal payload.
    #[test]
    fn asset_hex_rejects_malformed_remainders() {
        assert_eq!(
            asset_hex("xxh3:../../../etc/passwd"),
            None,
            "a traversal payload must not be treated as hex"
        );
        assert_eq!(
            asset_hex("xxh3:0123456789abcdef0123456789abcde"),
            None,
            "31 hex digits (one short) must be rejected"
        );
        assert_eq!(
            asset_hex("xxh3:0123456789abcdef0123456789abcdef0"),
            None,
            "33 hex digits (one over) must be rejected"
        );
        assert_eq!(
            asset_hex("xxh3:0123456789ABCDEF0123456789ABCDEF"),
            None,
            "uppercase hex must be rejected — asset ids are always lowercase"
        );
        assert_eq!(
            asset_hex("xxh3:0123456789abcdefg123456789abcde"),
            None,
            "a non-hex character (g) must be rejected"
        );
        assert_eq!(
            asset_hex("xxh3:"),
            None,
            "an empty remainder must be rejected"
        );
    }

    #[test]
    fn iter_vectors_walks_only_the_requested_model_tags_vectors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());

        let image_path = store.path_for(
            "aa11aa11aa11aa11aa11aa11aa11aa11",
            &Derivation::ImageEmbedding { model_tag: "m1" },
        );
        store
            .write_vector(&image_path, &[0.1, 0.2])
            .expect("write image vector");

        let kf_path = store.path_for(
            "bb22bb22bb22bb22bb22bb22bb22bb22",
            &Derivation::KeyframeEmbedding {
                model_tag: "m1",
                timestamp_ms: 4500,
            },
        );
        store
            .write_vector(&kf_path, &[0.3, 0.4])
            .expect("write keyframe vector");

        // Same asset as the image vector, but a different model tag — must
        // not show up in an `m1` walk.
        let other_model_path = store.path_for(
            "aa11aa11aa11aa11aa11aa11aa11aa11",
            &Derivation::ImageEmbedding { model_tag: "m2" },
        );
        store
            .write_vector(&other_model_path, &[0.5, 0.6])
            .expect("write other-model vector");

        // The keyframe manifest lives alongside real vectors under the same
        // model_tag dir — must be ignored, not misclassified as a vector.
        let manifest_path = store.path_for(
            "bb22bb22bb22bb22bb22bb22bb22bb22",
            &Derivation::KeyframeManifest { model_tag: "m1" },
        );
        store
            .write_atomic(&manifest_path, b"[]")
            .expect("write manifest");

        // A keyframe OCR blob (`kf-<ts>.json.zst`) shares the `kf-` prefix
        // with keyframe embedding vectors — the `.f32le.zst` suffix check
        // must keep it out of the vector walk even if the tags ever shared
        // a dir.
        let ocr_kf_path = store.path_for(
            "bb22bb22bb22bb22bb22bb22bb22bb22",
            &Derivation::OcrKeyframe {
                model_tag: "m1",
                timestamp_ms: 7_000,
            },
        );
        store
            .write_atomic(&ocr_kf_path, b"{}")
            .expect("write ocr keyframe blob");

        // A keyframe image (`kf-img-<edge>-<ts>.webp`) also shares the `kf-`
        // prefix — the `.f32le.zst` suffix check and the integer parse of the
        // remainder both keep it out (the parse is what rejects it even if
        // the suffix check were loosened: `img-<edge>-<ts>` is not an i64).
        let kf_img_path = store.path_for(
            "bb22bb22bb22bb22bb22bb22bb22bb22",
            &Derivation::KeyframeImage {
                model_tag: "m1",
                timestamp_ms: 7_000,
            },
        );
        store
            .write_atomic(&kf_img_path, b"webp-bytes")
            .expect("write keyframe image blob");

        let mut refs = store.iter_vectors("m1").expect("iter_vectors");
        refs.sort_by(|a, b| (&a.asset_hex, &a.kind).cmp(&(&b.asset_hex, &b.kind)));

        assert_eq!(refs.len(), 2, "exactly the two m1 vectors, nothing else");
        assert_eq!(refs[0].asset_hex, "aa11aa11aa11aa11aa11aa11aa11aa11");
        assert_eq!(refs[0].kind, "image");
        assert_eq!(refs[0].ts_ms, -1);
        assert_eq!(refs[0].path, image_path);
        assert_eq!(refs[1].asset_hex, "bb22bb22bb22bb22bb22bb22bb22bb22");
        assert_eq!(refs[1].kind, "keyframe");
        assert_eq!(refs[1].ts_ms, 4500);
        assert_eq!(refs[1].path, kf_path);
    }

    #[test]
    fn iter_vectors_on_a_store_with_no_blobs_yet_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        assert_eq!(store.iter_vectors("m1").expect("iter_vectors"), Vec::new());
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
