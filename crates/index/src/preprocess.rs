//! `SigLIP` 2 image preprocessing. Every constant here is pinned by the
//! conformance gate: squash-resize to 256×256 (no crop, no aspect
//! preservation) with antialiased bilinear, then (px/127.5 − 1) into NCHW.

use crate::error::IndexError;
use crate::resize::resize_rgb;

pub const EDGE: u32 = 256;
const EDGE_USIZE: usize = 256;

/// Resizes `rgb` to [`EDGE`]×[`EDGE`] (squashing aspect ratio, no crop) and
/// normalizes to `[-1, 1]` per channel, returning a flat NCHW `f32` buffer of
/// length `3 * EDGE * EDGE`.
///
/// # Errors
///
/// Returns [`IndexError::Resize`] if the resize step fails.
pub fn preprocess_rgb(rgb: &image::RgbImage) -> Result<Vec<f32>, IndexError> {
    let resized = if (rgb.width(), rgb.height()) == (EDGE, EDGE) {
        rgb.clone()
    } else {
        resize_rgb(rgb, EDGE, EDGE)?
    };
    let raw = resized.into_raw(); // HWC, RGB
    let plane = EDGE_USIZE * EDGE_USIZE;
    let mut out = vec![0f32; 3 * plane];
    for (i, px) in raw.chunks_exact(3).enumerate() {
        out[i] = f32::from(px[0]) / 127.5 - 1.0;
        out[plane + i] = f32::from(px[1]) / 127.5 - 1.0;
        out[2 * plane + i] = f32::from(px[2]) / 127.5 - 1.0;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_constants_agree() {
        assert_eq!(EDGE as usize, EDGE_USIZE);
    }

    #[test]
    fn uniform_color_maps_exactly_and_shape_is_nchw() {
        let mut img = image::RgbImage::new(100, 50);
        for px in img.pixels_mut() {
            *px = image::Rgb([255, 0, 128]);
        }
        let out = preprocess_rgb(&img).expect("preprocess");
        assert_eq!(out.len(), 3 * 256 * 256);
        let n = 256 * 256;
        assert!((out[0] - 1.0).abs() < 1e-6, "R plane first (NCHW)");
        assert!((out[n] - (-1.0)).abs() < 1e-6, "G plane second");
        assert!(
            (out[2 * n] - (128.0 / 127.5 - 1.0)).abs() < 1e-6,
            "B plane third"
        );
    }

    #[test]
    fn preprocess_is_deterministic() {
        let mut img = image::RgbImage::new(300, 200);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([
                u8::try_from(x % 256).expect("fits"),
                u8::try_from(y % 256).expect("fits"),
                u8::try_from((x + y) % 256).expect("fits"),
            ]);
        }
        assert_eq!(
            preprocess_rgb(&img).expect("a"),
            preprocess_rgb(&img).expect("b")
        );
    }

    #[test]
    fn an_already_256_square_image_skips_resizing_but_still_normalizes() {
        let mut img = image::RgbImage::new(256, 256);
        for px in img.pixels_mut() {
            *px = image::Rgb([0, 64, 255]);
        }
        let out = preprocess_rgb(&img).expect("preprocess");
        let n = 256 * 256;
        assert!((out[0] - (-1.0)).abs() < 1e-6, "R plane");
        assert!((out[n] - (64.0 / 127.5 - 1.0)).abs() < 1e-6, "G plane");
        assert!((out[2 * n] - 1.0).abs() < 1e-6, "B plane");
    }

    /// Pins the invariant the `(width, height) == (EDGE, EDGE)` early
    /// return in `preprocess_rgb` depends on: resizing to the *same*
    /// dimensions is byte-identical to not resizing at all, even for a
    /// non-uniform image, so skipping the resize changes nothing observable
    /// — confirmed here for a full gradient (not just a solid color, which
    /// can't tell a real resize apart from a skip since every pixel is the
    /// same either way). If a future resize algorithm change breaks this,
    /// this test — not a mutant hunt — is what should catch it.
    #[test]
    fn resize_to_matching_dimensions_is_the_identity_even_for_a_non_uniform_image() {
        use crate::resize::resize_rgb;

        let mut img = image::RgbImage::new(256, 256);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([
                u8::try_from(x % 256).expect("fits"),
                u8::try_from(y % 256).expect("fits"),
                u8::try_from((x + y) % 256).expect("fits"),
            ]);
        }
        let resized = resize_rgb(&img, 256, 256).expect("resize");
        assert_eq!(
            resized.as_raw(),
            img.as_raw(),
            "resizing to the same dimensions must be exactly the identity"
        );
    }

    /// The NCHW plane-offset math (`out[plane + i]`, `out[2 * plane + i]`)
    /// must land at the right index for a pixel other than the first: at
    /// `i == 0`, `plane + i` and `plane - i` are the same index, and
    /// `2 * plane + i` and `2 + plane + i` only coincide with `2 * plane +
    /// i` by accident of a uniform-color image — neither bug shows up
    /// unless a later pixel with distinct per-channel colors is checked
    /// directly.
    #[test]
    fn plane_offsets_are_correct_for_a_pixel_past_the_first() {
        let mut img = image::RgbImage::new(256, 256);
        img.put_pixel(0, 0, image::Rgb([10, 20, 30]));
        img.put_pixel(1, 0, image::Rgb([110, 120, 130]));
        let out = preprocess_rgb(&img).expect("preprocess");
        let n = 256 * 256;
        let norm = |v: u8| f32::from(v) / 127.5 - 1.0;

        assert!((out[1] - norm(110)).abs() < 1e-6, "R plane, pixel 1");
        assert!(
            (out[n + 1] - norm(120)).abs() < 1e-6,
            "G plane, pixel 1 must land at plane + i"
        );
        assert!(
            (out[2 * n + 1] - norm(130)).abs() < 1e-6,
            "B plane, pixel 1 must land at 2 * plane + i"
        );
    }
}
