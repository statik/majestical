//! Read-side parsers for derivation blobs, shared by `index::run` (which
//! re-reads its own writes — an existing captions blob, a transcript being
//! re-chunked) and `index::heal` (which walks every blob of a kind to
//! rebuild `text_fts`). Moved from `crates/cli/src/index_cmd.rs`.
use anyhow::{Context, Result};
use majestical_index::ocr::OcrResult;
use majestical_index::pdf::PdfContent;
use majestical_index::transcribe::Transcript;
use std::path::Path;

/// Reads and parses a zstd JSON transcript blob.
pub(crate) fn read_transcript_blob(path: &Path) -> Result<Transcript> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading transcript blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing transcript blob {}", path.display()))?;
    Ok(Transcript::from_json(&json)?)
}

/// Reads and parses one still's zstd JSON caption blob.
pub(crate) fn read_caption_blob(path: &Path) -> Result<majestical_core::ports::Caption> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading caption blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing caption blob {}", path.display()))?;
    serde_json::from_slice(&json)
        .with_context(|| format!("parsing caption blob {}", path.display()))
}

/// Parses a video captions blob written by `run::video_captions_json` back
/// into its `(ts_ms, text)` rows.
///
/// # Errors
/// Returns an error naming the missing/mistyped field on malformed bytes.
pub(crate) fn video_captions_read(json: &[u8]) -> Result<Vec<(u64, String)>> {
    let value: serde_json::Value =
        serde_json::from_slice(json).context("parsing video captions json")?;
    let described = value["described"]
        .as_array()
        .context("video captions missing array field 'described'")?;
    let mut rows = Vec::new();
    for entry in described {
        let ts_ms = entry[0]
            .as_u64()
            .context("video caption timestamp is not a non-negative integer")?;
        let text = entry[1]
            .as_str()
            .context("video caption text is not a string")?;
        rows.push((ts_ms, text.to_string()));
    }
    Ok(rows)
}

/// Reads a video's zstd JSON captions blob into its `(ts_ms, text)` rows.
pub(crate) fn read_video_captions_blob(path: &Path) -> Result<Vec<(u64, String)>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading video captions blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing video captions blob {}", path.display()))?;
    video_captions_read(&json)
}

/// Reads and joins one OCR blob's recognized lines into a single content
/// string (newline-separated, preserving line order).
pub(crate) fn read_ocr_blob_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading ocr blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing ocr blob {}", path.display()))?;
    let result = OcrResult::from_json(&json)?;
    Ok(result
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) fn read_pdf_blob(path: &Path) -> Result<PdfContent> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading pdf text blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing pdf text blob {}", path.display()))?;
    Ok(PdfContent::from_json(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_captions_read_rejects_malformed_json() {
        let err = video_captions_read(b"not json").expect_err("must reject non-json");
        assert!(err.to_string().contains("video captions"), "{err}");
        let err = video_captions_read(b"{\"described\":[[\"x\",1]]}")
            .expect_err("must name the mistyped field");
        assert!(err.to_string().contains("timestamp"), "{err}");
    }
}
