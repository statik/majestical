//! Reads the derived blobs a head serves straight to a client — the
//! thumbnail image, the keyframe manifest, and one extracted keyframe image
//! — with the remedy text for a blob that hasn't been derived yet living
//! here, once, rather than in each head's own copy. Both `maj mcp`'s
//! `majestical://` resources and the desktop app's `thumb://` protocol read
//! through this.
//!
//! The keyframe manifest blob is written as plain JSON, NOT zstd-compressed,
//! unlike every other JSON derivation blob (transcripts, OCR, captions):
//! `run_keyframe_items` writes `keyframes_manifest_json`'s bytes straight
//! through `BlobStore::write_atomic` with no zstd step, so this reads the
//! blob bytes straight through too, with no decompression.
//!
//! A keyframe IMAGE is selected by INDEX into the manifest's `timestamps`
//! array, not by timestamp directly — a client only ever sees the index (the
//! `majestical://keyframes/{asset}/{index}` / `keyframe/{asset}/{index}`
//! route both heads expose), never the millisecond value the blob is
//! actually keyed by. [`read_keyframe_image`] is the one place that maps an
//! index to a timestamp and then to a blob, shared by both heads so the
//! mapping — and its two distinct failure shapes, a malformed index string
//! and an in-range-shaped-but-absent one — exists once.
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
    /// One extracted keyframe image ([`Derivation::KeyframeImage`]), at the
    /// manifest timestamp `timestamp_ms`. Callers with only an INDEX into
    /// the manifest want [`read_keyframe_image`] instead, which resolves the
    /// index to a timestamp first.
    KeyframeImage { timestamp_ms: u64 },
}

impl Kind {
    /// What this blob is called in a message to a person ("no {noun} for
    /// …").
    fn noun(self) -> &'static str {
        match self {
            Self::Thumb => "thumbnail",
            Self::Keyframes => "keyframe manifest",
            Self::KeyframeImage { .. } => "keyframe image",
        }
    }

    /// The `maj index run --kinds <flag>` value that derives this blob.
    fn kinds_flag(self) -> &'static str {
        match self {
            Self::Thumb => "thumbs",
            Self::Keyframes => "keyframes",
            Self::KeyframeImage { .. } => "keyframe-images",
        }
    }

    /// Every keyframe derivation is keyed by the encoder's model tag (the
    /// `SigLIP2` vision tower) — the same constant `index::run` resolves
    /// them through when it writes them; there is only one vision encoder in
    /// a catalog, so the tag is a constant, not a per-call resolution.
    fn derivation(self) -> Derivation<'static> {
        match self {
            Self::Thumb => Derivation::Thumb,
            Self::Keyframes => Derivation::KeyframeManifest {
                model_tag: MODEL_TAG,
            },
            Self::KeyframeImage { timestamp_ms } => Derivation::KeyframeImage {
                model_tag: MODEL_TAG,
                timestamp_ms,
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
    /// The keyframe manifest blob itself read fine, but its bytes aren't the
    /// JSON shape `keyframes_manifest_json` (`crates/services/src/index/run.rs`)
    /// writes — a corrupt or hand-edited blob, not a caller mistake.
    #[error("keyframe manifest for {asset_id} is not valid JSON: {source}")]
    MalformedManifest {
        asset_id: String,
        source: serde_json::Error,
    },
    /// The `{index}` path segment in `.../keyframes/{asset}/{index}` (or the
    /// desktop app's `keyframe/{asset}/{index}`) isn't a non-negative
    /// integer at all — the caller's mistake, but reported as not-found
    /// (like [`Self::KeyframeIndexOutOfRange`]) rather than a client error,
    /// since there is no well-formed request this index could ever satisfy.
    #[error("{asset_id}: {requested:?} is not a valid keyframe index (expected a whole number)")]
    MalformedKeyframeIndex { asset_id: String, requested: String },
    /// The index parses fine but names no timestamp in the manifest —
    /// either too large, or the manifest has fewer timestamps than the
    /// caller assumed.
    #[error("{asset_id}: no keyframe image at index {index} ({count} in the manifest)")]
    KeyframeIndexOutOfRange {
        asset_id: String,
        index: usize,
        count: usize,
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

/// Just the field [`keyframe_timestamps`] needs out of the keyframe
/// manifest — `detected` and `model_tag` (see `keyframes_manifest_json` in
/// `crates/services/src/index/run.rs`) ride along in the real blob but are
/// irrelevant to mapping an index to a timestamp, so `serde` drops them.
#[derive(serde::Deserialize)]
struct KeyframeManifestTimestamps {
    timestamps: Vec<u64>,
}

/// Parses raw keyframe-manifest bytes down to their ordered timestamps — the
/// ONE strict, byte-level parse both heads must share: `read_keyframe_image`
/// (via [`read_keyframe_timestamps`]) uses it to resolve an index, and the
/// MCP `majestical://keyframes/{asset}` resource
/// (`crates/cli/src/mcp_cmd/resources.rs`) uses it to decide which indices
/// belong in its `images` list. Requires the bytes deserialize as a JSON
/// OBJECT with a `timestamps` array of non-negative integers — ANY other
/// shape (a bare array, string, number, bool, null, or an object whose
/// `timestamps` entries aren't all valid, e.g. `{"timestamps":[1500,-1]}`)
/// is [`BlobError::MalformedManifest`], never a silently-truncated list. Two
/// heads parsing the same bytes two different ways is what would let one
/// advertise an index the other then rejects — the strictness has to be
/// shared, not just the bytes.
///
/// # Errors
/// Returns [`BlobError::MalformedManifest`] if `bytes` don't deserialize
/// into that shape.
pub fn keyframe_timestamps(bytes: &[u8], asset_id: &str) -> Result<Vec<u64>, BlobError> {
    serde_json::from_slice::<KeyframeManifestTimestamps>(bytes)
        .map(|manifest| manifest.timestamps)
        .map_err(|source| BlobError::MalformedManifest {
            asset_id: asset_id.to_string(),
            source,
        })
}

/// Reads and parses an asset's keyframe manifest down to its ordered
/// timestamps — the index space [`read_keyframe_image`] resolves indices
/// against. Private: [`read_keyframe_image`] is the only catalog-level
/// caller either head needs (the MCP resource reads the blob itself, since
/// it also needs the raw bytes for its own response body, and calls
/// [`keyframe_timestamps`] directly on them).
///
/// Returns [`BlobError::MalformedAssetId`]/[`BlobError::NotDerived`]/
/// [`BlobError::Read`] exactly as [`read`] does for [`Kind::Keyframes`], plus
/// [`BlobError::MalformedManifest`] exactly as [`keyframe_timestamps`] does.
fn read_keyframe_timestamps(catalog: &Path, asset_id: &str) -> Result<Vec<u64>, BlobError> {
    let bytes = read(catalog, Kind::Keyframes, asset_id)?;
    keyframe_timestamps(&bytes, asset_id)
}

/// Reads one extracted keyframe image's bytes, selected by INDEX into the
/// asset's keyframe manifest (position in `timestamps`, the same order the
/// `majestical://keyframes/{asset}/{index}` / `keyframe/{asset}/{index}`
/// routes both heads expose use) rather than by raw timestamp — `index_text`
/// is parsed and bounds-checked here, before it ever reaches a path join, so
/// neither a malformed nor an out-of-range index can influence which file
/// gets read.
///
/// # Errors
/// Returns [`BlobError::MalformedAssetId`] for an id that isn't
/// `xxh3:<hex>`, [`BlobError::MalformedManifest`] for a corrupt manifest
/// blob, [`BlobError::MalformedKeyframeIndex`] for an `index_text` that
/// isn't a non-negative integer, [`BlobError::KeyframeIndexOutOfRange`] for
/// an index the manifest has no timestamp at, and
/// [`BlobError::NotDerived`]/[`BlobError::Read`] for the manifest or image
/// blob read itself exactly as [`read`] would.
pub fn read_keyframe_image(
    catalog: &Path,
    asset_id: &str,
    index_text: &str,
) -> Result<Vec<u8>, BlobError> {
    let Ok(index) = index_text.parse::<usize>() else {
        return Err(BlobError::MalformedKeyframeIndex {
            asset_id: asset_id.to_string(),
            requested: index_text.to_string(),
        });
    };
    let timestamps = read_keyframe_timestamps(catalog, asset_id)?;
    let Some(&timestamp_ms) = timestamps.get(index) else {
        return Err(BlobError::KeyframeIndexOutOfRange {
            asset_id: asset_id.to_string(),
            index,
            count: timestamps.len(),
        });
    };
    read(catalog, Kind::KeyframeImage { timestamp_ms }, asset_id)
}

/// Which INDICES into `timestamps` already have an extracted image blob —
/// the existence check the MCP `majestical://keyframes/{asset}` resource
/// uses to build its `images` list (only indices whose blob is actually
/// servable, not every index the manifest merely names). Takes indices, not
/// timestamps, because no client-facing surface is meant to see a
/// timestamp at all — only the index — the same rule this module's own
/// header states for [`read_keyframe_image`]; a `timestamp_ms`-keyed public
/// existence check would let a caller build a client-facing artifact
/// (a URI, a cache key) out of the timestamp directly, defeating that rule.
/// A malformed `asset_id` reports no indices rather than erroring: this is a
/// query, and every caller already validated the id by successfully reading
/// the manifest `timestamps` came from.
#[must_use]
pub fn existing_keyframe_image_indices(
    catalog: &Path,
    asset_id: &str,
    timestamps: &[u64],
) -> Vec<usize> {
    let Some(hex) = asset_hex(asset_id) else {
        return Vec::new();
    };
    let store = BlobStore::new(catalog);
    let mut indices = Vec::new();
    for (index, &timestamp_ms) in timestamps.iter().enumerate() {
        let path = store.path_for(hex, &Kind::KeyframeImage { timestamp_ms }.derivation());
        if path.is_file() {
            indices.push(index);
        }
    }
    indices
}

/// Whether `asset_id` is a well-formed `xxh3:<hex>` id, with no filesystem
/// access at all — a re-export of [`asset_hex`]'s own validation, boolean
/// rather than the hex remainder, for callers that only need to decide
/// whether a candidate substring COULD be an asset id (the desktop app's
/// `thumb://` protocol splits `keyframe/{asset_id}/{index}` on this, since
/// `majestical-index` — where the real `asset_hex` lives — is only a
/// dev-dependency of that crate, not one its shipped binary links).
#[must_use]
pub fn is_well_formed_asset_id(asset_id: &str) -> bool {
    asset_hex(asset_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        BlobError, Kind, existing_keyframe_image_indices, keyframe_timestamps, read,
        read_keyframe_image, read_keyframe_timestamps,
    };
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

    fn write_manifest(catalog: &Path, timestamps: &[u64]) {
        let manifest =
            serde_json::json!({"model_tag": "m", "detected": timestamps.len(), "timestamps": timestamps})
                .to_string();
        write_blob(catalog, Kind::Keyframes, manifest.as_bytes());
    }

    #[test]
    fn read_keyframe_timestamps_parses_the_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), &[1500, 4500]);

        let timestamps =
            read_keyframe_timestamps(dir.path(), ASSET).expect("parse the planted manifest");

        assert_eq!(timestamps, vec![1500, 4500]);
    }

    #[test]
    fn read_keyframe_timestamps_on_unparsable_bytes_is_a_malformed_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_blob(dir.path(), Kind::Keyframes, b"{ not json");

        let err = read_keyframe_timestamps(dir.path(), ASSET)
            .expect_err("corrupt manifest bytes must not parse");

        assert!(
            matches!(err, BlobError::MalformedManifest { .. }),
            "{err:?}"
        );
    }

    /// The index selects a POSITION in the manifest's timestamp order, then
    /// that timestamp's own blob is read — this pins that indirection, not
    /// just that some bytes come back.
    #[test]
    fn read_keyframe_image_maps_index_to_the_timestamp_at_that_position() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), &[1500, 4500]);
        write_blob(
            dir.path(),
            Kind::KeyframeImage { timestamp_ms: 1500 },
            b"frame-0",
        );
        write_blob(
            dir.path(),
            Kind::KeyframeImage { timestamp_ms: 4500 },
            b"frame-1",
        );

        assert_eq!(
            read_keyframe_image(dir.path(), ASSET, "0").expect("index 0"),
            b"frame-0"
        );
        assert_eq!(
            read_keyframe_image(dir.path(), ASSET, "1").expect("index 1"),
            b"frame-1"
        );
    }

    #[test]
    fn read_keyframe_image_out_of_range_index_is_clean_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), &[1500, 4500]);

        let err = read_keyframe_image(dir.path(), ASSET, "2").expect_err("only 2 timestamps");

        assert_eq!(
            err.to_string(),
            format!("{ASSET}: no keyframe image at index 2 (2 in the manifest)")
        );
    }

    #[test]
    fn read_keyframe_image_malformed_index_is_clean_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_manifest(dir.path(), &[1500, 4500]);

        let err = read_keyframe_image(dir.path(), ASSET, "x").expect_err("not a number");

        assert_eq!(
            err.to_string(),
            format!("{ASSET}: \"x\" is not a valid keyframe index (expected a whole number)")
        );
    }

    /// A malformed index must be rejected without ever reading the manifest
    /// (let alone joining a path) — this plants NO manifest at all, so a
    /// version that read the manifest first would fail with `NotDerived`
    /// instead of `MalformedKeyframeIndex`.
    #[test]
    fn read_keyframe_image_malformed_index_never_touches_the_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");

        let err = read_keyframe_image(dir.path(), ASSET, "not-a-number")
            .expect_err("malformed index, no manifest planted");

        assert!(
            matches!(err, BlobError::MalformedKeyframeIndex { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn existing_keyframe_image_indices_lists_only_indices_with_a_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let timestamps = [1500, 4500, 7500];
        assert_eq!(
            existing_keyframe_image_indices(dir.path(), ASSET, &timestamps),
            Vec::<usize>::new(),
            "no images extracted yet"
        );

        // Only the FIRST and LAST timestamps have an extracted image — the
        // middle one (index 1) does not, so it must be skipped, not treated
        // as a gap that stops the walk.
        write_blob(
            dir.path(),
            Kind::KeyframeImage { timestamp_ms: 1500 },
            b"frame-0",
        );
        write_blob(
            dir.path(),
            Kind::KeyframeImage { timestamp_ms: 7500 },
            b"frame-2",
        );

        assert_eq!(
            existing_keyframe_image_indices(dir.path(), ASSET, &timestamps),
            vec![0, 2]
        );
    }

    #[test]
    fn existing_keyframe_image_indices_on_a_malformed_id_is_empty_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            existing_keyframe_image_indices(dir.path(), "../../../etc/passwd", &[1500]),
            Vec::<usize>::new()
        );
    }

    /// The exact panic risk this shared parse exists to close: `[]` is valid
    /// JSON but not a JSON OBJECT, so a caller that indexed a
    /// `serde_json::Value` parsed from these bytes (`value["timestamps"]` or
    /// `value["images"] = ...`) would panic — `Value`'s `IndexMut` coerces
    /// `Null` into an object but panics on any other non-object shape.
    /// `keyframe_timestamps` never indexes a `Value` at all, so it reports a
    /// clean error instead.
    #[test]
    fn keyframe_timestamps_on_a_valid_json_non_object_is_a_malformed_manifest_not_a_panic() {
        for bytes in [b"[]".as_slice(), b"\"x\"", b"3", b"true", b"null"] {
            let err = keyframe_timestamps(bytes, ASSET)
                .expect_err("valid JSON that is not the manifest object shape");
            assert!(
                matches!(err, BlobError::MalformedManifest { .. }),
                "{bytes:?}: {err:?}"
            );
        }
    }

    /// The strictness both heads must share: one out-of-range entry fails
    /// the WHOLE parse rather than being silently dropped — otherwise a
    /// lenient caller could advertise an index for a timestamp a strict
    /// caller (`read_keyframe_image`) would then reject.
    #[test]
    fn keyframe_timestamps_rejects_the_whole_manifest_on_one_bad_entry() {
        let err = keyframe_timestamps(br#"{"timestamps":[1500,-1]}"#, ASSET)
            .expect_err("a negative timestamp is not a valid u64");
        assert!(
            matches!(err, BlobError::MalformedManifest { .. }),
            "{err:?}"
        );
    }
}
