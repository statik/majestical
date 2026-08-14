//! Keyframe-image extraction: one thumb-scale WebP per manifest timestamp.
//! Pure composition — ffmpeg frame decode (`crate::video::extract_frame`)
//! into the thumbnail encoder (`crate::thumbs::thumbnail_webp`).

use std::path::Path;

use crate::error::IndexError;

/// Extracts the frame at `ts_ms` and encodes it at thumbnail scale.
///
/// # Errors
/// Returns [`IndexError::Video`] if ffmpeg fails or produces no frame, or
/// [`IndexError::Resize`] if downscaling fails — ordinary per-item failures.
pub fn extract_keyframe_webp(path: &Path, ts_ms: u64) -> Result<Vec<u8>, IndexError> {
    let frame = crate::video::extract_frame(path, ts_ms)?;
    crate::thumbs::thumbnail_webp(&frame)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use crate::error::IndexError;
    use crate::video::ffmpeg_available;

    /// Same synthesis `crates/index/tests/video_e2e.rs::generate_test_clip`
    /// uses, at double [`crate::thumbs::THUMB_EDGE`] (640x360, not 320x180)
    /// so `thumbnail_webp` actually exercises its resize branch instead of
    /// passing the frame through unchanged: three 3s lavfi color segments
    /// (red, green, blue) concatenated, at 25fps.
    fn generate_test_clip(path: &Path) {
        let status = Command::new("ffmpeg")
            .args(["-y", "-v", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=640x360:d=3:r=25,format=yuv420p",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=640x360:d=3:r=25,format=yuv420p",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=640x360:d=3:r=25,format=yuv420p",
            ])
            .args(["-filter_complex", "[0:v][1:v][2:v]concat=n=3:v=1:a=0[outv]"])
            .args(["-map", "[outv]", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .expect("running ffmpeg to generate the test clip");
        assert!(status.success(), "ffmpeg clip generation failed");
    }

    #[test]
    fn extracted_frame_is_webp_at_thumb_scale() {
        if !ffmpeg_available() {
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let video_path = dir.path().join("clip.mp4");
        generate_test_clip(&video_path);

        // 4500ms lands in the middle of the 3-6s green segment (video_e2e.rs
        // extracts at the same timestamp for the same reason): a `ts_ms`
        // that got silently swapped for 0 would instead decode the red
        // segment, which the color-dominance assertion below catches.
        let bytes = super::extract_keyframe_webp(&video_path, 4500).expect("frame at 4500ms");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let img = image::load_from_memory(&bytes).expect("decode");
        // 640x360 source at THUMB_EDGE (320) longest-edge scale must land on
        // exact output dims, not just "under the cap" — the same convention
        // thumbs.rs's own test uses.
        assert_eq!((img.width(), img.height()), (320, 180));

        let rgb = img.to_rgb8();
        let center = rgb.get_pixel(rgb.width() / 2, rgb.height() / 2);
        let (r, g, b) = (
            u16::from(center[0]),
            u16::from(center[1]),
            u16::from(center[2]),
        );
        assert!(
            g > r + 50 && g > b + 50,
            "expected the 4500ms frame's center pixel to be green-dominant, got {center:?}"
        );
    }

    #[test]
    fn nonexistent_path_is_an_ordinary_error_not_a_panic() {
        if !ffmpeg_available() {
            return;
        }

        let path = Path::new("/nonexistent/definitely-not-a-real-clip-9f3c2.mp4");
        let error = super::extract_keyframe_webp(path, 0).expect_err("nonexistent input must fail");

        assert!(
            matches!(error, IndexError::Video { .. }),
            "expected IndexError::Video, got {error:?}"
        );
    }
}
