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
//! Both derivations are keyed by the encoder's model tag
//! (`majestical_index::model::MODEL_TAG`, the `SigLIP2` vision tower) — the
//! same tag `crates/services/src/index/run.rs` resolves `ImageEmbedding`,
//! `KeyframeEmbedding`, and `KeyframeManifest` blobs through today (there is
//! only one vision encoder in this catalog; the tag is a constant, not a
//! per-call resolution), so this reads through the SAME constant rather
//! than re-deriving it.
//!
//! The keyframe manifest blob is written as plain JSON, NOT zstd-compressed
//! — unlike every other JSON derivation blob (transcripts, OCR, captions).
//! `run_keyframe_items` (`crates/services/src/index/run.rs`) writes
//! `keyframes_manifest_json`'s bytes straight through `BlobStore::write_atomic`
//! with no zstd step, so this resource reads the blob bytes straight through
//! too, with no decompression.
use super::MajServer;
use base64::Engine as _;
use majestical_index::blob::{BlobStore, Derivation, asset_hex};
use majestical_index::model::MODEL_TAG;
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
/// an asset id that isn't a well-formed `xxh3:<hex>` id, or a blob that
/// hasn't been derived yet (naming the `maj index run` remedy). Returns
/// whatever [`MajServer::ensure_catalog`] itself reports for a missing
/// catalog.
pub(super) fn read(server: &MajServer, uri: &str) -> Result<ReadResourceResult, McpError> {
    server
        .ensure_catalog()
        .map_err(|result| tool_error_to_mcp_error(&result))?;
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

/// `ensure_catalog` (and every other `MajServer` guard) returns a tool-level
/// `CallToolResult`, but resource errors are a different wire shape
/// (`McpError`/`ErrorData`, surfaced as the JSON-RPC response's top-level
/// `error`, not a successful result's `isError`) — this pulls the remedy
/// text (e.g. "run `maj catalog init` first") back out of the tool error's
/// rendered content so a resource read reports the identical remedy through
/// its own wire shape instead of inventing a second copy of the message.
fn tool_error_to_mcp_error(result: &rmcp::model::CallToolResult) -> McpError {
    let message = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .map_or_else(|| "no catalog".to_string(), |text| text.text.clone());
    McpError::resource_not_found(message, None)
}

fn read_thumb(
    server: &MajServer,
    uri: &str,
    asset_id: &str,
) -> Result<ReadResourceResult, McpError> {
    let Some(hex) = asset_hex(asset_id) else {
        return Err(McpError::invalid_params(
            format!("{asset_id}: not a valid asset id (expected xxh3:<hex>)"),
            None,
        ));
    };
    let store = BlobStore::new(&server.catalog);
    let path = store.path_for(hex, &Derivation::Thumb);
    let bytes = std::fs::read(&path).map_err(|_| {
        McpError::resource_not_found(
            format!(
                "no thumbnail for {asset_id} — run `maj index run --kinds thumbs` to derive it"
            ),
            None,
        )
    })?;
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
    let Some(hex) = asset_hex(asset_id) else {
        return Err(McpError::invalid_params(
            format!("{asset_id}: not a valid asset id (expected xxh3:<hex>)"),
            None,
        ));
    };
    let store = BlobStore::new(&server.catalog);
    let path = store.path_for(
        hex,
        &Derivation::KeyframeManifest {
            model_tag: MODEL_TAG,
        },
    );
    let bytes = std::fs::read(&path).map_err(|_| {
        McpError::resource_not_found(
            format!(
                "no keyframe manifest for {asset_id} — run `maj index run --kinds keyframes` \
                 to derive it"
            ),
            None,
        )
    })?;
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
