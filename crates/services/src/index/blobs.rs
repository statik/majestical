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
