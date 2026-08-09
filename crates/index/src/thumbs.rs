//! Thumbnail generation: 320px longest edge, lossy WebP. HEIC/HEIF decode
//! through macOS's `sips` (the `image` crate has no HEIC support).

use std::path::Path;
use std::process::Command;

use crate::error::IndexError;
use crate::resize::resize_rgb;

pub const THUMB_EDGE: u32 = 320;
const WEBP_QUALITY: f32 = 80.0;

/// # Errors
///
/// Returns [`IndexError::Decode`] if the file can't be decoded (including
/// HEIC/HEIF files, when `sips` fails or isn't available).
pub fn decode_image(path: &Path) -> Result<image::RgbImage, IndexError> {
    let is_heic = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif"));

    let image = if is_heic {
        decode_via_sips(path)?
    } else {
        image::open(path).map_err(|e| IndexError::Decode {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?
    };
    Ok(image.to_rgb8())
}

#[cfg(target_os = "macos")]
fn decode_via_sips(path: &Path) -> Result<image::DynamicImage, IndexError> {
    let tmp = tempfile::Builder::new()
        .suffix(".png")
        .tempfile()
        .map_err(|e| IndexError::Decode {
            path: path.to_path_buf(),
            message: format!("creating temp png for sips: {e}"),
        })?;
    let tmp_path = tmp.path();

    let output = Command::new("sips")
        .args([
            "-s",
            "format",
            "png",
            &path.to_string_lossy(),
            "--out",
            &tmp_path.to_string_lossy(),
        ])
        .output()
        .map_err(|e| IndexError::Decode {
            path: path.to_path_buf(),
            message: format!("running sips (macOS-only HEIC decoder): {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(IndexError::Decode {
            path: path.to_path_buf(),
            message: format!("sips failed: {stderr}"),
        });
    }

    image::open(tmp_path).map_err(|e| IndexError::Decode {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// Stub for builds without macOS's `sips` tool. Signature matches the
/// macOS version exactly so `decode_image`'s HEIC branch compiles
/// everywhere; surfaces as an ordinary per-item [`IndexError`], not a panic.
///
/// # Errors
/// Always returns [`IndexError::PlatformUnavailable`] — HEIC decoding has
/// no non-Apple backend.
#[cfg(not(target_os = "macos"))]
fn decode_via_sips(_path: &Path) -> Result<image::DynamicImage, IndexError> {
    Err(IndexError::PlatformUnavailable {
        capability: "HEIC decoding",
        framework: "the macOS `sips` tool",
    })
}

/// # Errors
///
/// Returns [`IndexError::Resize`] if downscaling to the thumbnail edge fails.
pub fn thumbnail_webp(rgb: &image::RgbImage) -> Result<Vec<u8>, IndexError> {
    let (width, height) = (rgb.width(), rgb.height());
    let longest = width.max(height);

    let scaled = if longest <= THUMB_EDGE {
        rgb.clone()
    } else {
        let dst_w = scaled_dimension(width, longest);
        let dst_h = scaled_dimension(height, longest);
        resize_rgb(rgb, dst_w, dst_h)?
    };

    let encoder = webp::Encoder::from_rgb(scaled.as_raw(), scaled.width(), scaled.height());
    Ok(encoder.encode(WEBP_QUALITY).to_vec())
}

/// Scales `edge` so that `longest` maps to `THUMB_EDGE`, rounding to the
/// nearest integer using only integer arithmetic (no float-to-int casts).
fn scaled_dimension(edge: u32, longest: u32) -> u32 {
    let numerator = 2 * u64::from(edge) * u64::from(THUMB_EDGE) + u64::from(longest);
    let denominator = 2 * u64::from(longest);
    let scaled = numerator / denominator;
    u32::try_from(scaled).unwrap_or(THUMB_EDGE).max(1)
}

#[cfg(test)]
mod tests {
    use crate::thumbs::thumbnail_webp;

    #[test]
    fn thumbnail_is_webp_with_320_longest_edge() {
        let mut img = image::RgbImage::new(640, 480);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let r = u8::try_from(x % 256).unwrap_or(0);
            let g = u8::try_from(y % 256).unwrap_or(0);
            *px = image::Rgb([r, g, 128]);
        }
        let bytes = thumbnail_webp(&img).expect("thumb");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let back = image::load_from_memory(&bytes).expect("decode webp");
        assert_eq!((back.width(), back.height()), (320, 240));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_heic_decode_stub_names_the_gap_and_the_framework() {
        let err = crate::thumbs::decode_image(std::path::Path::new("/nonexistent.heic"))
            .expect_err("stub must refuse before touching the filesystem");
        let rendered = err.to_string();
        assert!(rendered.contains("HEIC decoding"));
        assert!(rendered.contains("sips"));
        assert!(rendered.contains("macOS"));
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let img = image::RgbImage::new(100, 60);
        let bytes = thumbnail_webp(&img).expect("thumb");
        let back = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((back.width(), back.height()), (100, 60));
    }
}
