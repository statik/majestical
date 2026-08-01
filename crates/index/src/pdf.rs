//! PDF text extraction + first-page rendering via `PDFKit`. All objc2
//! unsafety is confined to this module behind safe fns (same policy as
//! `ocr.rs`); the public surface is plain Rust types. `PDFKit` has no
//! versioned "model" knob to pin — [`PDF_MODEL_TAG`] names the extraction
//! scheme so blobs re-derive if the scheme ever changes.

use std::path::Path;

use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2_app_kit::NSImage;
use objc2_foundation::{NSData, NSSize};
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument};

use crate::error::IndexError;

pub const PDF_MODEL_TAG: &str = "pdfkit-v1";

/// Extracted text for one PDF document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PdfContent {
    /// Extracted text per page, index 0 = page 1. Pages with no text
    /// layer are empty strings (an answer, not an error).
    pub pages: Vec<String>,
}

impl PdfContent {
    /// # Errors
    /// Serialization failure (never expected for these plain types).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self).map_err(|error| IndexError::Model(format!("pdf json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes)
            .map_err(|error| IndexError::Model(format!("pdf parse: {error}")))
    }
}

/// Opens `path` as a `PDFDocument`, rejecting locked (password-protected)
/// documents — their pages would silently extract as empty rather than
/// failing, so the lock is surfaced as a decode error instead.
fn open_document(path: &Path) -> Result<Retained<PDFDocument>, IndexError> {
    let bytes = std::fs::read(path).map_err(|error| IndexError::Decode {
        path: path.to_path_buf(),
        message: format!("read pdf: {error}"),
    })?;
    let data = NSData::with_bytes(&bytes);
    // SAFETY: -[PDFDocument initWithData:] parses an immutable NSData we
    // exclusively created from the file's bytes; it returns nil (mapped to
    // None) when the bytes aren't a PDF, with no other preconditions.
    let document = unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) };
    let Some(document) = document else {
        return Err(IndexError::Decode {
            path: path.to_path_buf(),
            message: "not a valid pdf".to_string(),
        });
    };
    // SAFETY: -[PDFDocument isLocked] reads a BOOL property from a document
    // we hold retained; no preconditions.
    if unsafe { document.isLocked() } {
        return Err(IndexError::Decode {
            path: path.to_path_buf(),
            message: "password-protected pdf".to_string(),
        });
    }
    Ok(document)
}

/// Per-page text via `PDFPage.string`.
///
/// # Errors
/// Returns [`IndexError::Decode`] when the file cannot be opened as a PDF
/// (missing, malformed, or password-protected).
pub fn extract_text(path: &Path) -> Result<PdfContent, IndexError> {
    let document = open_document(path)?;
    // SAFETY: -[PDFDocument pageCount] reads an NSUInteger property from a
    // document we hold retained; no preconditions.
    let count = unsafe { document.pageCount() };
    let mut pages = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: -[PDFDocument pageAtIndex:] with index < pageCount on a
        // retained document; returns nil (None) rather than trapping if the
        // page is unavailable.
        let page = unsafe { document.pageAtIndex(index) };
        // SAFETY: -[PDFPage string] copies the page's text content from a
        // page we hold retained; nil (no text layer) maps to None.
        let text = page.and_then(|page| unsafe { page.string() });
        pages.push(text.map(|text| text.to_string()).unwrap_or_default());
    }
    Ok(PdfContent { pages })
}

/// Render page 1 with its longest edge at `edge` px, as RGB — feeds the
/// existing thumbnail + `SigLIP` embedding path so PDFs join visual search.
///
/// # Errors
/// Returns [`IndexError::Decode`] on open failure, an empty document,
/// degenerate page bounds, or an unreadable rendering.
pub fn render_first_page(path: &Path, edge: u32) -> Result<image::RgbImage, IndexError> {
    let decode_error = |message: String| IndexError::Decode {
        path: path.to_path_buf(),
        message,
    };
    let document = open_document(path)?;
    // SAFETY: -[PDFDocument pageAtIndex:] on a retained document; a
    // past-the-end index answers nil (None), so index 0 on an empty
    // document is safe.
    let page = unsafe { document.pageAtIndex(0) }
        .ok_or_else(|| decode_error("pdf has no pages".to_string()))?;

    // SAFETY: -[PDFPage boundsForBox:] returns an NSRect by value from a
    // retained page; MediaBox is a valid PDFDisplayBox.
    let bounds = unsafe { page.boundsForBox(PDFDisplayBox::MediaBox) };
    let (width, height) = (bounds.size.width, bounds.size.height);
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return Err(decode_error(format!(
            "degenerate page bounds {width}x{height}"
        )));
    }
    // PDFKit aspect-fits the thumbnail inside the requested box, so a
    // square box puts the longest post-rotation edge at exactly `edge` for
    // any orientation; the golden tests pin the longest-edge invariant.
    let size = NSSize {
        width: f64::from(edge),
        height: f64::from(edge),
    };
    // SAFETY: -[PDFPage thumbnailOfSize:forBox:] renders offscreen into a
    // new NSImage; the size is finite and positive (checked above) and
    // MediaBox is a valid PDFDisplayBox.
    let thumbnail: Retained<NSImage> =
        unsafe { page.thumbnailOfSize_forBox(size, PDFDisplayBox::MediaBox) };
    let tiff = thumbnail
        .TIFFRepresentation()
        .ok_or_else(|| decode_error("pdf render produced no bitmap".to_string()))?;
    let rendered = image::load_from_memory(&tiff.to_vec())
        .map_err(|error| decode_error(format!("pdf render tiff decode: {error}")))?;
    Ok(rendered.to_rgb8())
}
