//! `thumb://` — thumbnails, keyframe manifests, and extracted keyframe
//! images straight from the blob store to the webview, no image bytes over
//! IPC (phase 7 spec). The frontend builds URLs with `convertFileSrc(path,
//! "thumb")`:
//!
//! - `thumb://localhost/thumb/<asset_id>` — the WebP thumbnail
//! - `thumb://localhost/keyframes/<asset_id>` — the keyframe manifest JSON
//! - `thumb://localhost/keyframe/<asset_id>/<index>` — one extracted
//!   keyframe image, selected by its position in the manifest's
//!   `timestamps` (the `Inspector` strip already knows each timestamp's
//!   index, so it never needs the manifest augmented with URLs the way the
//!   MCP `majestical://keyframes/{asset}` resource is — see
//!   `majestical_services::index::blobs::read_keyframe_image`'s own doc for
//!   why an index, not a timestamp)
//!
//! Windows serves the same three paths under `http://thumb.localhost/…`,
//! which is why this reads the path out of the URI rather than matching one
//! fixed prefix. `convertFileSrc(filePath, protocol)` runs `encodeURIComponent`
//! over the WHOLE `filePath` argument, not just the asset id inside it — so
//! `keyframe/xxh3:abc/0` arrives as `keyframe%2Fxxh3%3Aabc%2F0`, every `/`
//! encoded right along with the `:`. [`path_of`] percent-decodes the WHOLE
//! path once, before any prefix match, the same way Tauri's own bundled
//! protocols do (`protocol/asset.rs`, `protocol/tauri.rs`, `ipc/protocol.rs`
//! in the `tauri` crate all decode up front) — decoding only the asset-id
//! segment AFTER matching a literal `/thumb/`-style prefix, as this module
//! used to, means the prefix match itself runs against still-encoded bytes
//! and never succeeds against a real webview's request; every route 404s.
//! One whole-path decode also makes the historical test convention (a
//! literal `/` with only the asset id's `:` encoded, e.g. `thumb/xxh3%3Aabc`)
//! and the real wire shape (`thumb%2Fxxh3%3Aabc`) converge on the identical
//! decoded string, so both keep working without special-casing either.
//!
//! Every failure is a plain HTTP status with the reason as the body: the
//! webview shows a broken image, and the reason is one devtools click away.
//! The blob lookup itself lives in `majestical_services::index::blobs`,
//! shared with `maj mcp`'s `majestical://` resources — including its
//! rejection of any asset id that isn't `xxh3:<hex>`, which is what makes a
//! traversal payload in the URL a 400 rather than a file read.
use crate::commands::{AppState, CatalogCfg};
use majestical_services::index::blobs::{self, BlobError};
use tauri::http::header::{CONTENT_TYPE, HeaderValue};
use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Manager};

/// Serves one `thumb://` request against whatever catalog the app currently
/// has selected.
#[must_use]
pub fn respond(app: &AppHandle, uri: &str) -> Response<Vec<u8>> {
    let state = app.state::<AppState>();
    handle(crate::commands::selected_catalog(&state).as_ref(), uri)
}

/// The whole protocol as a plain function of its two inputs — the selected
/// catalog (`None` when the user has picked none yet) and the request URI —
/// so `tests/commands.rs` drives every route and status without a webview.
#[must_use]
pub fn handle(cfg: Option<&CatalogCfg>, uri: &str) -> Response<Vec<u8>> {
    let Some(cfg) = cfg else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "no catalog selected yet — initialize or choose one first",
        );
    };
    if let Err(err) = majestical_services::catalog::ensure_catalog(&cfg.catalog) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, &format!("{err:#}"));
    }
    let Some((route, asset_id)) = route(uri) else {
        return failure(
            StatusCode::NOT_FOUND,
            &format!("{uri}: not a thumb:// route this app serves"),
        );
    };
    let result = match route {
        Route::Thumb => {
            blobs::read(&cfg.catalog, blobs::Kind::Thumb, &asset_id).map(|b| ("image/webp", b))
        }
        Route::Keyframes => blobs::read(&cfg.catalog, blobs::Kind::Keyframes, &asset_id)
            .map(|b| ("application/json", b)),
        Route::KeyframeImage { index_text } => {
            blobs::read_keyframe_image(&cfg.catalog, &asset_id, &index_text)
                .map(|b| ("image/webp", b))
        }
        // The `/keyframe/` split itself already rejected `asset_id` as
        // malformed — report that failure directly, the same
        // `BlobError::MalformedAssetId` (and so the same 400 + message)
        // `/thumb/` and `/keyframes/` give for the same payload, rather than
        // routing through `read_keyframe_image` and having its unrelated
        // index check misreport the real problem.
        Route::MalformedKeyframeAssetId => Err(BlobError::MalformedAssetId { asset_id }),
    };
    match result {
        Ok((mime, bytes)) => respond_with(StatusCode::OK, mime, bytes),
        Err(err) => failure(blob_status(&err), &err.to_string()),
    }
}

/// Which blob this URI names — everything but the mapping from an INDEX to a
/// timestamp for [`Route::KeyframeImage`], which happens downstream in
/// [`blobs::read_keyframe_image`] rather than here, so a malformed or
/// out-of-range index never reaches this module's own path handling at all.
enum Route {
    Thumb,
    Keyframes,
    KeyframeImage {
        index_text: String,
    },
    /// `/keyframe/{asset_id}/{index}` where `asset_id` itself failed
    /// validation — reported directly as `BlobError::MalformedAssetId`
    /// rather than routed through [`blobs::read_keyframe_image`], whose
    /// index check would otherwise run first and misreport a bad asset id
    /// as a bad index.
    MalformedKeyframeAssetId,
}

/// The route and the (already decoded, via [`path_of`]) asset id this URI
/// names.
fn route(uri: &str) -> Option<(Route, String)> {
    let path = path_of(uri)?;
    if let Some(asset_id) = path.strip_prefix("/thumb/") {
        return Some((Route::Thumb, asset_id.to_string()));
    }
    if let Some(asset_id) = path.strip_prefix("/keyframes/") {
        return Some((Route::Keyframes, asset_id.to_string()));
    }
    if let Some(rest) = path.strip_prefix("/keyframe/") {
        return Some(match keyframe_asset_and_index(rest) {
            Ok((asset_id, index_text)) => (Route::KeyframeImage { index_text }, asset_id),
            Err(asset_id) => (Route::MalformedKeyframeAssetId, asset_id),
        });
    }
    None
}

/// Splits `.../keyframe/{asset_id}/{index}` (already percent-decoded by
/// [`path_of`]) at the FIRST `/`, but only when the part before it is a
/// well-formed `xxh3:<hex>` asset id — otherwise `Err` with the whole
/// remainder as one asset id, so the caller reports the SAME "not a valid
/// asset id" failure `/thumb/` and `/keyframes/` give for a malformed id,
/// rather than misattributing it to the index. Also handles the
/// missing-index case (`rest` has no `/` at all): if `rest` alone is already
/// a well-formed asset id, it is reported as such with an empty index text
/// (which then fails [`blobs::read_keyframe_image`]'s own integer parse as
/// an ordinary malformed index) rather than as a malformed asset id.
///
/// Without the well-formed check, a malformed asset id that itself decodes
/// to contain a `/` (a traversal payload's own `%2F`s, or any other stray
/// `/`) would mis-split at ITS OWN first `/` into a bogus asset-id/index
/// pair — mirrors `maj mcp`'s own guard
/// (`crates/cli/src/mcp_cmd/resources.rs::keyframes_sub_path`).
fn keyframe_asset_and_index(rest: &str) -> Result<(String, String), String> {
    if let Some((asset_id, index_text)) = rest.split_once('/') {
        if blobs::is_well_formed_asset_id(asset_id) {
            return Ok((asset_id.to_string(), index_text.to_string()));
        }
        return Err(rest.to_string());
    }
    if blobs::is_well_formed_asset_id(rest) {
        return Ok((rest.to_string(), String::new()));
    }
    Err(rest.to_string())
}

/// The path component of `uri` — everything from the `/` that follows the
/// authority up to any query or fragment — percent-DECODED as a whole,
/// before any route matches against it. `convertFileSrc` encodes the whole
/// path it's given, not just the asset id inside it, so decoding has to
/// happen here, once, before `route`'s prefix matches, not per-segment
/// after a match already (impossibly) succeeded against encoded bytes.
fn path_of(uri: &str) -> Option<String> {
    let after_scheme = uri.split_once("://")?.1;
    let path = &after_scheme[after_scheme.find('/')?..];
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(decode(&path[..end]))
}

fn decode(segment: &str) -> String {
    percent_encoding::percent_decode_str(segment)
        .decode_utf8_lossy()
        .into_owned()
}

/// Which HTTP status each lookup failure is: a malformed id is the caller's
/// mistake, a blob that hasn't been derived yet — or a keyframe index that
/// names no timestamp — is a not-found, and a read or manifest-parse failure
/// is ours.
fn blob_status(err: &BlobError) -> StatusCode {
    match err {
        BlobError::MalformedAssetId { .. } => StatusCode::BAD_REQUEST,
        BlobError::NotDerived { .. }
        | BlobError::MalformedKeyframeIndex { .. }
        | BlobError::KeyframeIndexOutOfRange { .. } => StatusCode::NOT_FOUND,
        BlobError::Read { .. } | BlobError::MalformedManifest { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn respond_with(code: StatusCode, mime: &'static str, body: Vec<u8>) -> Response<Vec<u8>> {
    let mut response = Response::new(body);
    *response.status_mut() = code;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    response
}

fn failure(code: StatusCode, reason: &str) -> Response<Vec<u8>> {
    respond_with(
        code,
        "text/plain; charset=utf-8",
        reason.as_bytes().to_vec(),
    )
}
