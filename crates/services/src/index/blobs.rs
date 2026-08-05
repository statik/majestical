//! Reads the two derived blobs a head serves straight to a client — the
//! thumbnail image and the keyframe manifest — with the remedy text for a
//! blob that hasn't been derived yet living here, once, rather than in each
//! head's own copy. Both `maj mcp`'s `majestical://` resources and the
//! desktop app's `thumb://` protocol read through this.
//!
//! Keyframe IMAGES are never stored as blobs, only the manifest listing
//! their timestamps is (see [`Derivation`]'s own doc) — [`Kind::Keyframes`]
//! reads that manifest, not an image.
//!
//! The keyframe manifest blob is written as plain JSON, NOT zstd-compressed,
//! unlike every other JSON derivation blob (transcripts, OCR, captions):
//! `run_keyframe_items` writes `keyframes_manifest_json`'s bytes straight
//! through `BlobStore::write_atomic` with no zstd step, so this reads the
//! blob bytes straight through too, with no decompression.
use majestical_index::blob::{BlobStore, Derivation, asset_hex};
use majestical_index::model::MODEL_TAG;
use std::path::{Path, PathBuf};

/// A derived blob kind servable as-is to a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The asset's `thumb-320.webp` preview ([`Derivation::Thumb`]).
    Thumb,
    /// The asset's keyframe timestamp manifest, as plain JSON bytes.
    Keyframes,
}

impl Kind {
    /// What this blob is called in a message to a person ("no {noun} for
    /// …").
    fn noun(self) -> &'static str {
        match self {
            Self::Thumb => "thumbnail",
            Self::Keyframes => "keyframe manifest",
        }
    }

    /// The `maj index run --kinds <flag>` value that derives this blob.
    fn kinds_flag(self) -> &'static str {
        match self {
            Self::Thumb => "thumbs",
            Self::Keyframes => "keyframes",
        }
    }

    /// Both keyframe derivations are keyed by the encoder's model tag (the
    /// `SigLIP2` vision tower) — the same constant `index::run` resolves
    /// them through when it writes them; there is only one vision encoder in
    /// a catalog, so the tag is a constant, not a per-call resolution.
    fn derivation(self) -> Derivation<'static> {
        match self {
            Self::Thumb => Derivation::Thumb,
            Self::Keyframes => Derivation::KeyframeManifest {
                model_tag: MODEL_TAG,
            },
        }
    }
}

/// Why a derived blob couldn't be handed back. Each variant maps to one
/// status in whichever protocol the head speaks: a malformed id is the
/// caller's mistake, a missing blob is a not-found carrying the remedy, and
/// a read failure is the server's problem.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("{asset_id}: not a valid asset id (expected xxh3:<hex>)")]
    MalformedAssetId { asset_id: String },
    #[error("no {noun} for {asset_id} — run `maj index run --kinds {kinds_flag}` to derive it")]
    NotDerived {
        asset_id: String,
        noun: &'static str,
        kinds_flag: &'static str,
    },
    #[error("reading {noun} blob at {path}: {source}")]
    Read {
        noun: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Reads one derived blob's bytes for `asset_id`. Callers that need a
/// catalog guard (the "run `maj catalog init` first" remedy for a root with
/// no catalog at all) must call [`crate::catalog::ensure_catalog`] first —
/// without it a missing catalog surfaces here as a plain missing blob.
///
/// # Errors
/// Returns [`BlobError::MalformedAssetId`] for an id that isn't
/// `xxh3:<hex>`, [`BlobError::NotDerived`] (naming the `maj index run`
/// remedy) when the blob doesn't exist yet, and [`BlobError::Read`] for any
/// other read failure.
pub fn read(catalog: &Path, kind: Kind, asset_id: &str) -> Result<Vec<u8>, BlobError> {
    let Some(hex) = asset_hex(asset_id) else {
        return Err(BlobError::MalformedAssetId {
            asset_id: asset_id.to_string(),
        });
    };
    let path = BlobStore::new(catalog).path_for(hex, &kind.derivation());
    std::fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => BlobError::NotDerived {
            asset_id: asset_id.to_string(),
            noun: kind.noun(),
            kinds_flag: kind.kinds_flag(),
        },
        _ => BlobError::Read {
            noun: kind.noun(),
            path,
            source: err,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{Kind, read};
    use majestical_index::blob::{BlobStore, asset_hex};
    use std::path::Path;

    /// `asset_hex` accepts exactly 32 lowercase hex digits after the prefix.
    const ASSET: &str = "xxh3:0123456789abcdef0123456789abcdef";

    fn write_blob(catalog: &Path, kind: Kind, bytes: &[u8]) {
        let hex = asset_hex(ASSET).expect("a well-formed fixture id");
        let store = BlobStore::new(catalog);
        let path = store.path_for(hex, &kind.derivation());
        store.write_atomic(&path, bytes).expect("write the blob");
    }

    #[test]
    fn a_derived_blob_reads_back_byte_for_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The keyframe manifest is stored uncompressed, so what was written is
        // what a head serves — no zstd step on either side.
        let manifest = br#"{"model_tag":"m","detected":2,"timestamps":[10,20]}"#;
        write_blob(dir.path(), Kind::Keyframes, manifest);

        let bytes = read(dir.path(), Kind::Keyframes, ASSET).expect("the manifest");

        assert_eq!(bytes, manifest);
    }

    #[test]
    fn an_id_that_is_not_xxh3_hex_is_rejected_before_any_file_is_touched() {
        let dir = tempfile::tempdir().expect("tempdir");

        let err = read(dir.path(), Kind::Thumb, "../../../etc/passwd")
            .expect_err("a traversal payload is not an asset id");

        assert_eq!(
            err.to_string(),
            "../../../etc/passwd: not a valid asset id (expected xxh3:<hex>)"
        );
    }

    /// The remedy is the whole point of this error: it names the blob in words
    /// a person uses and the exact `--kinds` value that derives it.
    #[test]
    fn a_blob_nobody_has_derived_names_its_own_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");

        let thumb = read(dir.path(), Kind::Thumb, ASSET).expect_err("nothing derived yet");
        let keyframes = read(dir.path(), Kind::Keyframes, ASSET).expect_err("nothing derived yet");

        assert_eq!(
            thumb.to_string(),
            format!("no thumbnail for {ASSET} — run `maj index run --kinds thumbs` to derive it")
        );
        assert_eq!(
            keyframes.to_string(),
            format!(
                "no keyframe manifest for {ASSET} — \
                 run `maj index run --kinds keyframes` to derive it"
            )
        );
    }

    /// A read that fails for any reason other than absence is the server's
    /// problem, not a missing derivation — a directory where the blob file
    /// belongs is the cheapest way to produce one.
    #[test]
    fn a_read_failure_that_is_not_absence_is_reported_as_a_read_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex = asset_hex(ASSET).expect("a well-formed fixture id");
        let path = BlobStore::new(dir.path()).path_for(hex, &Kind::Thumb.derivation());
        std::fs::create_dir_all(&path).expect("a directory in the blob's place");

        let err = read(dir.path(), Kind::Thumb, ASSET).expect_err("a directory is not a blob");

        let message = err.to_string();
        assert!(
            message.starts_with("reading thumbnail blob at "),
            "must name the blob and the path it failed at: {message}"
        );
        assert!(
            !message.contains("maj index run"),
            "a read failure must not claim the blob merely needs deriving: {message}"
        );
    }
}
