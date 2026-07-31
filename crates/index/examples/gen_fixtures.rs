//! Regenerate the conformance fixture images (deterministic, no randomness):
//!   cargo run -p majestical-index --example `gen_fixtures`

use std::path::Path;

fn gradient() -> Result<image::RgbImage, std::num::TryFromIntError> {
    let (width, height) = (300u32, 200u32);
    let mut img = image::RgbImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        // x < 300, so x*255/299 <= 255, and y < 200, so y*255/199 <= 255 —
        // both already fit in u8 with no wraparound needed. (x+y) can reach
        // 498, so it still needs `% 256` to fit.
        let red = x * 255 / 299;
        let green = y * 255 / 199;
        let blue = (x + y) % 256;
        *px = image::Rgb([
            u8::try_from(red)?,
            u8::try_from(green)?,
            u8::try_from(blue)?,
        ]);
    }
    Ok(img)
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

fn wide() -> Result<image::RgbImage, std::num::TryFromIntError> {
    // 500x61: non-integer resize ratios on both axes against the 256x256
    // target (unlike the old 512x64, an exact 2x downscale most filters
    // agree on) — this is what actually pins the resize filter choice.
    let (width, height) = (500u32, 61u32);
    let mut img = image::RgbImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let red = x % 256;
        let green = (y * 4) % 256;
        *px = image::Rgb([u8::try_from(red)?, u8::try_from(green)?, 90]);
    }
    Ok(img)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir)?;
    gradient()?.save(dir.join("gradient.png"))?;
    blocks().save(dir.join("blocks.png"))?;
    wide()?.save(dir.join("wide.png"))?;
    Ok(())
}
