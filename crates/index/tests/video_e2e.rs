#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! End-to-end video pipeline test against a real ffmpeg-generated clip.
//!
//!     cargo test -p majestical-index --test video_e2e -- --ignored

use std::process::Command;

use majestical_index::video::{self, ffmpeg_available};

/// Builds a 9s, 320x180, yuv420p clip: three 3s lavfi color segments
/// (red, green, blue) concatenated, at 25fps.
fn generate_test_clip(path: &std::path::Path) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:d=3:r=25,format=yuv420p",
        ])
        .args(["-filter_complex", "[0:v][1:v][2:v]concat=n=3:v=1:a=0[outv]"])
        .args(["-map", "[outv]", "-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .expect("running ffmpeg to generate the test clip");
    assert!(status.success(), "ffmpeg clip generation failed");
}

#[test]
#[ignore = "needs ffmpeg on PATH"]
fn probe_frames_and_scene_detection_agree_on_a_real_clip() {
    assert!(
        ffmpeg_available(),
        "ffmpeg/ffprobe must be on PATH for this test"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let clip_path = dir.path().join("clip.mp4");
    generate_test_clip(&clip_path);

    let info = video::probe(&clip_path).expect("probe");
    assert!(
        (8500..=9500).contains(&info.duration_ms),
        "expected ~9000ms duration, got {}",
        info.duration_ms
    );

    let frames = video::analysis_frames(&clip_path).expect("analysis_frames");
    assert!(
        frames.len() >= 17,
        "expected at least 17 analysis frames, got {}",
        frames.len()
    );

    let keyframes = video::detect_scenes(&frames, 2000, info.duration_ms);
    assert_eq!(
        keyframes.len(),
        3,
        "expected 3 scenes (red/green/blue), got {keyframes:?}"
    );

    let frame = video::extract_frame(&clip_path, 4500).expect("extract_frame");
    assert_eq!((frame.width(), frame.height()), (320, 180));
}
