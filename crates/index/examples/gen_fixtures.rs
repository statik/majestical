//! Regenerate the conformance fixture images (deterministic, no randomness):
//!   cargo run -p majestical-index --example `gen_fixtures`

use std::path::Path;

fn gradient() -> image::RgbImage {
    let (width, height) = (300u32, 200u32);
    let mut img = image::RgbImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let red = (x * 255 / 299) % 256;
        let green = (y * 255 / 199) % 256;
        let blue = (x + y) % 256;
        *px = image::Rgb([
            u8::try_from(red).unwrap_or(u8::MAX),
            u8::try_from(green).unwrap_or(u8::MAX),
            u8::try_from(blue).unwrap_or(u8::MAX),
        ]);
    }
    img
}

fn blocks() -> image::RgbImage {
    let (width, height) = (256u32, 256u32);
    let mut img = image::RgbImage::new(width, height);
    let (half_w, half_h) = (width / 2, height / 2);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb(match (x < half_w, y < half_h) {
            (true, true) => [220, 30, 30],
            (false, true) => [30, 220, 30],
            (true, false) => [30, 30, 220],
            (false, false) => [220, 220, 30],
        });
    }
    img
}

fn wide() -> image::RgbImage {
    let (width, height) = (512u32, 64u32);
    let mut img = image::RgbImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let red = x % 256;
        let green = (y * 4) % 256;
        *px = image::Rgb([
            u8::try_from(red).unwrap_or(u8::MAX),
            u8::try_from(green).unwrap_or(u8::MAX),
            90,
        ]);
    }
    img
}

// clippy::print_stdout/print_stderr are workspace-denied and allow_attributes
// forbids a local #[allow], so failures abort silently rather than printing
// a diagnostic — acceptable for a one-shot fixture regenerator run by hand.
fn save(img: &image::RgbImage, dir: &Path, name: &str) {
    let path = dir.join(name);
    img.save(&path).unwrap_or_else(|_| std::process::abort());
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap_or_else(|_| std::process::abort());
    save(&gradient(), &dir, "gradient.png");
    save(&blocks(), &dir, "blocks.png");
    save(&wide(), &dir, "wide.png");
}
