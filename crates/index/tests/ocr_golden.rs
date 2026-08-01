//! Golden wiring proof for Apple Vision OCR: recognizes a committed fixture
//! rendered with known text. Not `#[ignore]`d — Vision ships with macOS, so
//! there is no model fetch to gate on.

use majestical_index::ocr;

#[test]
fn recognizes_rendered_text_in_fixture() {
    let image = image::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ocr-hello.png"
    ))
    .expect("fixture")
    .to_rgb8();
    let result = ocr::recognize_text(&image).expect("ocr");
    let joined = result
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    assert!(joined.contains("MAJESTICAL"), "got: {joined}");
    assert!(joined.contains("42"), "got: {joined}");
    assert!(result.lines.iter().all(|l| l.confidence > 0.0));

    // Loose orientation pins on the recognized line: Vision reports
    // normalized [x, y, w, h] with a bottom-left origin, and the fixture's
    // single centered line measures x≈0.04, w≈0.93. An x/y swap would put
    // ≈0.44 in bbox[0]; a w/h swap would put ≈0.11 in bbox[2] — both fail
    // here while staying robust to small drift across macOS versions.
    let line = result
        .lines
        .iter()
        .find(|l| l.text.to_uppercase().contains("MAJESTICAL"))
        .expect("line with fixture text");
    assert!(
        line.bbox.iter().all(|c| (0.0..=1.0).contains(c)),
        "bbox normalized, got: {:?}",
        line.bbox
    );
    assert!(line.bbox[0] < 0.2, "left margin, got bbox: {:?}", line.bbox);
    assert!(
        line.bbox[2] > 0.5,
        "width dominant, got bbox: {:?}",
        line.bbox
    );
}

#[test]
fn blank_image_yields_empty_lines_not_error() {
    let blank = image::RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
    let result = ocr::recognize_text(&blank).expect("ocr");
    assert!(
        result.lines.is_empty(),
        "blank image must produce zero lines"
    );
}

#[test]
fn ocr_result_serializes_round_trip() {
    let result = ocr::OcrResult {
        revision: 3,
        lines: vec![ocr::OcrLine {
            text: "HELLO".into(),
            confidence: 0.98,
            bbox: [0.1, 0.2, 0.5, 0.1],
        }],
    };
    let bytes = result.to_json().expect("serialize");
    let back = ocr::OcrResult::from_json(&bytes).expect("parse");
    assert_eq!(back.lines[0].text, "HELLO");
}
