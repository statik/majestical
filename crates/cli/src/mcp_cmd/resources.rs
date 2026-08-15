//! `majestical://` MCP resources: read-only views straight over the
//! catalog's derived-data blob store, alongside the tools in `read_tools`
//! and `write_tools`. Three kinds today:
//!
//! - `majestical://thumb/{asset_id}` — the asset's `thumb-320.webp` blob
//!   (`Derivation::Thumb`), served as base64 image bytes.
//! - `majestical://keyframes/{asset_id}` — the asset's keyframe manifest
//!   (`Derivation::KeyframeManifest`), served as JSON text, augmented with an
//!   `images` array: one `majestical://keyframes/{asset_id}/{index}` URI per
//!   timestamp whose extracted image blob actually exists, in `timestamps`
//!   order.
//! - `majestical://keyframes/{asset_id}/{index}` — one keyframe image, named
//!   only by an entry from the manifest's own `images` array above
//!   (`Derivation::KeyframeImage`), same shape as `thumb`: base64 WebP
//!   bytes. Unadvertised as its own `resources/templates/list` entry —
//!   reachable only by following an `images` URI, never by guessing an
//!   index — so an agent always knows in advance which indices are actually
//!   servable. An index the manifest names but has no image blob for yet
//!   (or an index a caller invents outright) is a not-found, not a panic.
//!
//! The blob lookup itself — path derivation, the model tag every keyframe
//! derivation is keyed by, the index→timestamp mapping, and the "run `maj
//! index run` to derive it" remedy — lives in
//! `majestical_services::index::blobs`, shared with the desktop app's
//! `thumb://` protocol; this module only maps that lookup's errors onto
//! MCP's own error kinds.
use super::MajServer;
use base64::Engine as _;
use majestical_services::index::blobs::{self, BlobError};
use rmcp::ErrorData as McpError;
use rmcp::model::{ReadResourceResult, ResourceContents, ResourceTemplate};
use serde::de::Error as _;

const THUMB_URI_TEMPLATE: &str = "majestical://thumb/{asset_id}";
const KEYFRAMES_URI_TEMPLATE: &str = "majestical://keyframes/{asset_id}";

/// The two URI templates this server advertises via
/// `resources/templates/list`, each described in terms of what an agent
/// gets back — the same "state the remedy/outcome, not the mechanism"
/// style every tool doc comment in `read_tools`/`write_tools` already
/// follows.
pub(super) fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new(THUMB_URI_TEMPLATE, "thumb").with_description(
            "A 320px-edge WebP preview image for one asset, if `maj index run` has \
             thumbnailed it yet.",
        ),
        ResourceTemplate::new(KEYFRAMES_URI_TEMPLATE, "keyframes").with_description(
            "The JSON manifest of a video asset's detected keyframe timestamps (milliseconds \
             since the start of the video), if `maj index run --kinds keyframes` has processed \
             it yet. Carries an `images` array of `majestical://keyframes/{asset_id}/{index}` \
             URIs, one per timestamp whose keyframe image has actually been extracted (fetch \
             those, not a guessed index, for the image itself — `image/webp` bytes) — if \
             `maj index run --kinds keyframe-images` hasn't reached a timestamp yet, its index \
             is simply absent from `images`.",
        ),
    ]
}

/// Reads one `majestical://` resource. Guards the catalog first (the same
/// `ensure_catalog` predicate every catalog-touching tool guards through —
/// without it, a missing catalog root would surface as a plain "no such
/// file" on the blob path instead of the `maj catalog init` remedy), then
/// dispatches on the URI's resource kind.
///
/// # Errors
/// Returns [`McpError::resource_not_found`] for an unrecognized URI scheme,
/// an asset id that isn't a well-formed `xxh3:<hex>` id, a blob that hasn't
/// been derived yet (naming the `maj index run` remedy), or a missing
/// catalog (naming the `maj catalog init` remedy, via
/// [`majestical_services::catalog::ensure_catalog`]'s own error).
pub(super) fn read(server: &MajServer, uri: &str) -> Result<ReadResourceResult, McpError> {
    majestical_services::catalog::ensure_catalog(&server.catalog)
        .map_err(|err| McpError::resource_not_found(format!("{err:#}"), None))?;
    if let Some(asset_id) = uri.strip_prefix("majestical://thumb/") {
        return read_thumb(server, uri, asset_id);
    }
    if let Some(rest) = uri.strip_prefix("majestical://keyframes/") {
        return match keyframes_sub_path(rest) {
            Some((asset_id, index)) => read_keyframe_image(server, uri, asset_id, index),
            None => read_keyframes(server, uri, rest),
        };
    }
    Err(McpError::resource_not_found(
        format!("{uri}: not a majestical:// resource this server serves"),
        None,
    ))
}

/// Splits `.../keyframes/{asset_id}/{index}` at the FIRST `/`, but only when
/// the part before it is a well-formed `xxh3:<hex>` asset id — otherwise
/// `None`, so the whole (malformed) remainder is handed to [`read_keyframes`]
/// as one asset id rather than mis-split. Without the well-formed check, a
/// traversal payload like `xxh3:../../../etc/passwd` (itself containing `/`)
/// would split at its own first `/` into a bogus "asset id" and "index"
/// pair, reaching [`read_keyframe_image`] under an asset id that was never
/// validated as a whole — the malformed-index path would then reject it
/// before [`blobs::read`]'s own `asset_hex` check ever ran, misreporting a
/// traversal payload as "not a valid keyframe index" instead of "not a valid
/// asset id". A malformed INDEX after a well-formed asset id (e.g. `/x`) is
/// still routed to [`read_keyframe_image`] deliberately — only the asset id
/// half needs to be well-formed for the split to be trusted; the index text
/// itself is free to be anything, since [`blobs::read_keyframe_image`]
/// validates it next. Uses [`blobs::is_well_formed_asset_id`] (not
/// `majestical_index::blob::asset_hex` directly) — the same wrapper the
/// desktop app's `thumb_protocol.rs` uses for its own analogous split, so
/// both heads share one "is this substring even shaped like an asset id"
/// check.
fn keyframes_sub_path(rest: &str) -> Option<(&str, &str)> {
    let (asset_id, index) = rest.split_once('/')?;
    blobs::is_well_formed_asset_id(asset_id).then_some((asset_id, index))
}

/// Maps a blob lookup failure onto MCP's error kinds: a malformed asset id
/// is the caller's mistake (`invalid_params`), a blob that hasn't been
/// derived yet or a keyframe index that names no timestamp is a not-found
/// (the latter carrying no `maj index run` remedy — no run of that command
/// would ever produce it), a corrupt manifest is the server's problem, and
/// any other read failure is the server's problem too.
fn blob_error(err: &BlobError) -> McpError {
    match err {
        BlobError::MalformedAssetId { .. } => McpError::invalid_params(format!("{err}"), None),
        BlobError::NotDerived { .. }
        | BlobError::MalformedKeyframeIndex { .. }
        | BlobError::KeyframeIndexOutOfRange { .. } => {
            McpError::resource_not_found(format!("{err}"), None)
        }
        BlobError::Read { .. } | BlobError::MalformedManifest { .. } => {
            McpError::internal_error(format!("{err}"), None)
        }
    }
}

fn read_thumb(
    server: &MajServer,
    uri: &str,
    asset_id: &str,
) -> Result<ReadResourceResult, McpError> {
    let bytes = blobs::read(&server.catalog, blobs::Kind::Thumb, asset_id)
        .map_err(|err| blob_error(&err))?;
    let blob = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ReadResourceResult::new(vec![
        ResourceContents::blob(blob, uri).with_mime_type("image/webp"),
    ]))
}

fn read_keyframe_image(
    server: &MajServer,
    uri: &str,
    asset_id: &str,
    index: &str,
) -> Result<ReadResourceResult, McpError> {
    let bytes = blobs::read_keyframe_image(&server.catalog, asset_id, index)
        .map_err(|err| blob_error(&err))?;
    let blob = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ReadResourceResult::new(vec![
        ResourceContents::blob(blob, uri).with_mime_type("image/webp"),
    ]))
}

/// Serves the keyframe manifest, augmented with an `images` array: one
/// `majestical://keyframes/{asset_id}/{index}` URI per `timestamps` entry
/// whose extracted image blob actually exists, in `timestamps` order. A
/// timestamp the manifest names but whose image hasn't been extracted yet
/// (or ever will be — extraction can fail permanently, same as thumbs) is
/// simply absent from `images`, never a broken link — an agent that only
/// ever fetches entries from `images` can never hit a not-found on this
/// resource's own say-so.
///
/// Timestamps come from [`blobs::keyframe_timestamps`] — the SAME strict,
/// byte-level parse [`blobs::read_keyframe_image`] uses to resolve an index
/// — rather than a second, looser parse of this module's own. Two parsers
/// disagreeing on what a "valid" manifest is would mean this resource could
/// advertise an `images` URI for an index that `read_keyframe_image` then
/// rejects (e.g. `{"timestamps":[1500,-1]}`: a looser parse that silently
/// drops the bad `-1` entry would still publish index 0 for `1500`, but a
/// mismatched index numbering elsewhere would 404). One shared parse means
/// one shared answer to "is this manifest even usable at all".
fn read_keyframes(
    server: &MajServer,
    uri: &str,
    asset_id: &str,
) -> Result<ReadResourceResult, McpError> {
    let bytes = blobs::read(&server.catalog, blobs::Kind::Keyframes, asset_id)
        .map_err(|err| blob_error(&err))?;
    let timestamps =
        blobs::keyframe_timestamps(&bytes, asset_id).map_err(|err| blob_error(&err))?;
    // Reuses `BlobError::MalformedManifest`'s own `Display` (via `blob_error`,
    // the same mapping every other `BlobError` on this path goes through)
    // rather than hand-writing the message a second time — one source of
    // truth for what "the manifest isn't valid JSON" says. This second parse
    // (into a generic `Value`, rather than the strict struct
    // `keyframe_timestamps` uses above) exists only to keep every OTHER
    // field of the manifest (`model_tag`, `detected`, …) in the response
    // verbatim; it parses the same bytes `keyframe_timestamps` already
    // accepted, so in practice it cannot itself fail.
    let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        blob_error(&BlobError::MalformedManifest {
            asset_id: asset_id.to_string(),
            source,
        })
    })?;
    // `serde_json::Value`'s index-assignment (`manifest["images"] = ...`)
    // coerces `Null` into an object but PANICS on any other non-object
    // shape (an array, string, number, bool) — a corrupted or hand-edited
    // blob could take the whole long-lived `maj mcp` process down with it.
    // `keyframe_timestamps` above already requires an object with a
    // `timestamps` field, so this `else` is unreachable in practice; it
    // stays as the actual guard against that panic rather than relying on
    // that invariant holding forever.
    let Some(object) = manifest.as_object_mut() else {
        return Err(blob_error(&BlobError::MalformedManifest {
            asset_id: asset_id.to_string(),
            source: serde_json::Error::custom(
                "keyframe manifest bytes parsed as JSON but not as a JSON object",
            ),
        }));
    };
    let images: Vec<serde_json::Value> =
        blobs::existing_keyframe_image_indices(&server.catalog, asset_id, &timestamps)
            .into_iter()
            .map(|index| {
                serde_json::Value::String(format!("majestical://keyframes/{asset_id}/{index}"))
            })
            .collect();
    object.insert("images".to_string(), serde_json::Value::Array(images));
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(manifest.to_string(), uri).with_mime_type("application/json"),
    ]))
}
