//! The blob↔`text_fts` diff: rebuilds `text_fts` rows for any asset that has
//! a transcript/OCR/PDF-text/caption blob but no `text_fts` rows yet for
//! that source. Moved from `crates/cli/src/index_cmd.rs::heal_text_fts` and
//! its per-kind helpers. Run at the end of every `index run` pass (see
//! `index::run::run`), mirroring `run`'s own always-on blob↔Lance diffs.
use crate::index::blob_read::{
    read_caption_blob, read_ocr_blob_text, read_pdf_blob, read_transcript_blob,
    read_video_captions_blob,
};
use crate::index::run::ts_ms_i64;
use anyhow::Result;
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::AssetId;
use majestical_index::blob::BlobStore;
use majestical_index::chunk::chunk_segments;
use std::path::Path;

/// The blob↔`text_fts` diff, run at the end of EVERY pass (mirroring
/// `load_missing_vectors_from_blobs`'s role for Lance): any asset with a
/// transcript/OCR/PDF-text/caption blob but no `text_fts` rows for that
/// source gets its rows rebuilt from the blob. `db.text_assets(source)`
/// makes the pass cheap when nothing changed; a blob that decodes to no
/// usable text is re-examined each pass rather than tracked (rare, and
/// decoding one small blob is cheap).
///
/// # Errors
/// Returns an error on a blob-walk or sqlite failure; an individual
/// undecodable blob is reported to stderr and skipped instead.
pub(crate) fn heal_text_fts(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    heal_transcript_rows(db, blobs)?;
    heal_ocr_rows(db, blobs)?;
    heal_pdf_rows(db, blobs)?;
    heal_caption_rows(db, blobs)?;
    Ok(())
}

/// Heals caption rows from stills (`caption.json.zst`, locator -1) and from
/// videos (`captions.json.zst`, one row per described keyframe timestamp) —
/// blobs from any describer tag, this machine's or a teammate's.
fn heal_caption_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("caption")?;
    for (hex, _, path) in blobs.iter_named("caption.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        match read_caption_blob(&path) {
            Ok(caption) if !caption.text.trim().is_empty() => {
                db.upsert_text_rows(&asset, "caption", &[(-1, caption.text.as_str())])?;
            }
            Ok(_) => {}
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable caption blob: {err}");
                }
            }
        }
    }
    for (hex, _, path) in blobs.iter_named("captions.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let described = match read_video_captions_blob(&path) {
            Ok(described) => described,
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable video captions blob: {err}");
                }
                continue;
            }
        };
        let rows: Vec<(i64, &str)> = described
            .iter()
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(ts_ms, text)| (ts_ms_i64(*ts_ms), text.as_str()))
            .collect();
        if !rows.is_empty() {
            db.upsert_text_rows(&asset, "caption", &rows)?;
        }
    }
    Ok(())
}

fn heal_transcript_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("transcript")?;
    for (hex, _, path) in blobs.iter_named("transcript.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let transcript = match read_transcript_blob(&path) {
            Ok(transcript) => transcript,
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable transcript blob: {err}");
                }
                continue;
            }
        };
        let chunks = chunk_segments(&transcript.segments);
        let rows: Vec<(i64, &str)> = chunks
            .iter()
            .filter(|c| !c.text.trim().is_empty())
            .map(|c| (ts_ms_i64(c.start_ms), c.text.as_str()))
            .collect();
        if !rows.is_empty() {
            db.upsert_text_rows(&asset, "transcript", &rows)?;
        }
    }
    Ok(())
}

/// Heals OCR rows from stills (`image.json.zst`, locator -1) and from
/// completed videos. Video enumeration rides the `ocr-complete.json`
/// markers: kf blobs have variable names, so instead of a new `BlobStore`
/// walker, each marker's sibling `kf-<ts>.json.zst` files are read directly
/// — partially-OCR'd videos (no marker yet) are picked up once complete.
fn heal_ocr_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("ocr")?;
    for (hex, _, path) in blobs.iter_named("image.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        match read_ocr_blob_text(&path) {
            Ok(text) if !text.trim().is_empty() => {
                db.upsert_text_rows(&asset, "ocr", &[(-1, text.as_str())])?;
            }
            Ok(_) => {}
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable ocr blob: {err}");
                }
            }
        }
    }
    for (hex, _, marker_path) in blobs.iter_named("ocr-complete.json")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let rows = keyframe_ocr_rows(&marker_path);
        let row_refs: Vec<(i64, &str)> =
            rows.iter().map(|(ts, text)| (*ts, text.as_str())).collect();
        if !row_refs.is_empty() {
            db.upsert_text_rows(&asset, "ocr", &row_refs)?;
        }
    }
    Ok(())
}

/// Collects `(ts_ms, text)` rows from the `kf-<ts>.json.zst` OCR blobs
/// sitting beside a video's `ocr-complete.json` marker, sorted by
/// timestamp; unreadable entries are reported and skipped.
fn keyframe_ocr_rows(marker_path: &Path) -> Vec<(i64, String)> {
    let Some(dir) = marker_path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(ts_ms) = name
            .strip_prefix("kf-")
            .and_then(|rest| rest.strip_suffix(".json.zst"))
            .and_then(|ms| ms.parse::<i64>().ok())
        else {
            continue;
        };
        match read_ocr_blob_text(&entry.path()) {
            Ok(text) if !text.trim().is_empty() => rows.push((ts_ms, text)),
            Ok(_) => {}
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable keyframe ocr blob: {err}");
                }
            }
        }
    }
    rows.sort_unstable_by_key(|(ts, _)| *ts);
    rows
}

fn heal_pdf_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("pdf")?;
    for (hex, _, path) in blobs.iter_named("text.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let content = match read_pdf_blob(&path) {
            Ok(content) => content,
            Err(err) => {
                #[expect(
                    clippy::print_stderr,
                    reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
                )]
                {
                    eprintln!("note: skipping unreadable pdf text blob: {err}");
                }
                continue;
            }
        };
        let rows: Vec<(i64, &str)> = content
            .pages
            .iter()
            .enumerate()
            .filter(|(_, page)| !page.trim().is_empty())
            .map(|(index, page)| {
                // Locator is the 1-based page number.
                (i64::try_from(index + 1).unwrap_or(i64::MAX), page.as_str())
            })
            .collect();
        if !rows.is_empty() {
            db.upsert_text_rows(&asset, "pdf", &rows)?;
        }
    }
    Ok(())
}
