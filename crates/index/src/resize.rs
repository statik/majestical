//! Antialiased bilinear resize. The encoder conformance gate (later task)
//! depends on this exact algorithm (transformers v5 resizes with torchvision
//! antialias=True); change the filter only together with the conformance run.

use fast_image_resize as fr;

use crate::error::IndexError;

/// # Errors
///
/// Returns [`IndexError::Resize`] if the source buffer doesn't match its
/// declared dimensions, the resize itself fails, or the output buffer size
/// doesn't match `dst_w * dst_h`.
pub fn resize_rgb(
    src: &image::RgbImage,
    dst_w: u32,
    dst_h: u32,
) -> Result<image::RgbImage, IndexError> {
    // A borrowed view (`ImageRef`), not `Image::from_vec_u8` — the source
    // only needs read access, so this avoids copying the whole decoded
    // buffer (tens to hundreds of MB for a large still).
    let src_img =
        fr::images::ImageRef::new(src.width(), src.height(), src.as_raw(), fr::PixelType::U8x3)
            .map_err(|e| IndexError::Resize(e.to_string()))?;
    let mut dst_img = fr::images::Image::new(dst_w, dst_h, fr::PixelType::U8x3);
    let mut resizer = fr::Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &fr::ResizeOptions::new()
                .resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Bilinear)),
        )
        .map_err(|e| IndexError::Resize(e.to_string()))?;
    image::RgbImage::from_raw(dst_w, dst_h, dst_img.into_vec())
        .ok_or_else(|| IndexError::Resize("buffer size mismatch after resize".into()))
}
