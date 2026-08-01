//! Golden tests against real `PDFKit` (ships with macOS — not ignored).

use majestical_index::pdf;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fixture.pdf");

#[test]
fn extracts_per_page_text() {
    let content = pdf::extract_text(std::path::Path::new(FIXTURE)).expect("extract");
    assert!(!content.pages.is_empty());
    let all = content.pages.join(" ");
    assert!(all.contains("Majestical fixture"), "got: {all}");
    assert!(all.contains("7734"), "got: {all}");
}

#[test]
fn renders_first_page_to_rgb() {
    let rendered = pdf::render_first_page(std::path::Path::new(FIXTURE), 1024).expect("render");
    assert_eq!(rendered.width().max(rendered.height()), 1024);
    // A rendered text page is mostly white but not uniform.
    let first = *rendered.get_pixel(0, 0);
    assert!(
        rendered.pixels().any(|p| *p != first),
        "render must not be a flat color"
    );
}

/// A portrait `MediaBox` with `/Rotate 90` renders landscape — the longest
/// edge of the *post-rotation* orientation must land at the requested edge.
#[test]
fn rotated_page_renders_landscape_with_exact_longest_edge() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fixture-rotated.pdf"
    );
    let rendered = pdf::render_first_page(std::path::Path::new(fixture), 1024).expect("render");
    assert_eq!(rendered.width().max(rendered.height()), 1024);
    assert!(
        rendered.width() > rendered.height(),
        "rotated portrait page must render landscape, got {}x{}",
        rendered.width(),
        rendered.height()
    );
}

#[test]
fn missing_file_is_a_decode_error() {
    assert!(pdf::extract_text(std::path::Path::new("/nonexistent.pdf")).is_err());
}

#[test]
fn pdf_content_serializes_round_trip() {
    let content = pdf::PdfContent {
        pages: vec!["page one text".into(), String::new()],
    };
    let bytes = content.to_json().expect("serialize");
    let back = pdf::PdfContent::from_json(&bytes).expect("parse");
    assert_eq!(back.pages.len(), 2);
    assert_eq!(back.pages[0], "page one text");
}
