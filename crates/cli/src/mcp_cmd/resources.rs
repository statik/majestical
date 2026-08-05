//! `majestical://` MCP resources: read-only views straight over the
//! catalog's derived-data blob store, alongside the tools in `read_tools`
//! and `write_tools`. Two kinds today, one per derivation `maj index run`
//! produces that has no paired MCP tool:
//!
//! - `majestical://thumb/{asset_id}` — the asset's `thumb-320.webp` blob
//!   (`Derivation::Thumb`), served as base64 image bytes.
//! - `majestical://keyframes/{asset_id}` — the asset's keyframe manifest
//!   (`Derivation::KeyframeManifest`), served as JSON text.
//!
//! Keyframe IMAGES are never stored as blobs, only the manifest listing
//! their timestamps is (see `majestical_index::blob::Derivation`'s own
//! doc) — this resource serves that manifest, not an image. On-demand
//! frame extraction from the source video is watchlisted, not implemented
//! here (Task 9 records the deviation).
//!
//! The blob lookup itself — path derivation, the model tag both keyframe
//! derivations are keyed by, and the "run `maj index run` to derive it"
//! remedy — lives in `majestical_services::index::blobs`, shared with the
//! desktop app's `thumb://` protocol; this module only maps that lookup's
//! errors onto MCP's own error kinds.
use super::MajServer;
use base64::Engine as _;
use majestical_services::index::blobs::{self, BlobError};
use rmcp::ErrorData as McpError;
use rmcp::model::{ReadResourceResult, ResourceContents, ResourceTemplate};

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
             it yet. Keyframe IMAGES are not stored — only this timestamp manifest is.",
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
    if let Some(asset_id) = uri.strip_prefix("majestical://keyframes/") {
        return read_keyframes(server, uri, asset_id);
    }
    Err(McpError::resource_not_found(
        format!("{uri}: not a majestical:// resource this server serves"),
        None,
    ))
}

/// Maps a blob lookup failure onto MCP's error kinds: a malformed asset id
/// is the caller's mistake (`invalid_params`), a blob that hasn't been
/// derived yet is a not-found carrying the `maj index run` remedy, and any
/// other read failure is the server's problem.
fn blob_error(err: &BlobError) -> McpError {
    match err {
        BlobError::MalformedAssetId { .. } => McpError::invalid_params(format!("{err}"), None),
        BlobError::NotDerived { .. } => McpError::resource_not_found(format!("{err}"), None),
        BlobError::Read { .. } => McpError::internal_error(format!("{err}"), None),
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

fn read_keyframes(
    server: &MajServer,
    uri: &str,
    asset_id: &str,
) -> Result<ReadResourceResult, McpError> {
    let bytes = blobs::read(&server.catalog, blobs::Kind::Keyframes, asset_id)
        .map_err(|err| blob_error(&err))?;
    let text = String::from_utf8(bytes).map_err(|err| {
        McpError::internal_error(
            format!("keyframe manifest for {asset_id} is not valid UTF-8: {err}"),
            None,
        )
    })?;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type("application/json"),
    ]))
}
