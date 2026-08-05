//! `thumb://` — thumbnails and keyframe manifests straight from the blob
//! store to the webview, no image bytes over IPC (phase 7 spec). The
//! frontend builds URLs with `convertFileSrc(path, "thumb")`:
//!
//! - `thumb://localhost/thumb/<asset_id>` — the WebP thumbnail
//! - `thumb://localhost/keyframes/<asset_id>` — the keyframe manifest JSON
//!
//! Windows serves the same two paths under `http://thumb.localhost/…`,
//! which is why this reads the path out of the URI rather than matching one
//! fixed prefix. Asset ids arrive percent-encoded (`convertFileSrc` runs
//! `encodeURIComponent`, so `xxh3:` becomes `xxh3%3A`).
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
    let Some((kind, mime, asset_id)) = route(uri) else {
        return failure(
            StatusCode::NOT_FOUND,
            &format!("{uri}: not a thumb:// route this app serves"),
        );
    };
    match blobs::read(&cfg.catalog, kind, &asset_id) {
        Ok(bytes) => respond_with(StatusCode::OK, mime, bytes),
        Err(err) => failure(blob_status(&err), &err.to_string()),
    }
}

/// The blob kind, its media type, and the decoded asset id this URI names.
fn route(uri: &str) -> Option<(blobs::Kind, &'static str, String)> {
    let path = path_of(uri)?;
    if let Some(asset_id) = path.strip_prefix("/thumb/") {
        return Some((blobs::Kind::Thumb, "image/webp", decode(asset_id)));
    }
    if let Some(asset_id) = path.strip_prefix("/keyframes/") {
        return Some((blobs::Kind::Keyframes, "application/json", decode(asset_id)));
    }
    None
}

/// The path component of `uri`: everything from the `/` that follows the
/// authority up to any query or fragment.
fn path_of(uri: &str) -> Option<&str> {
    let after_scheme = uri.split_once("://")?.1;
    let path = &after_scheme[after_scheme.find('/')?..];
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(&path[..end])
}

fn decode(segment: &str) -> String {
    percent_encoding::percent_decode_str(segment)
        .decode_utf8_lossy()
        .into_owned()
}

/// Which HTTP status each lookup failure is: a malformed id is the caller's
/// mistake, a blob that hasn't been derived yet is a not-found carrying the
/// `maj index run` remedy, and a read failure is ours.
fn blob_status(err: &BlobError) -> StatusCode {
    match err {
        BlobError::MalformedAssetId { .. } => StatusCode::BAD_REQUEST,
        BlobError::NotDerived { .. } => StatusCode::NOT_FOUND,
        BlobError::Read { .. } => StatusCode::INTERNAL_SERVER_ERROR,
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
