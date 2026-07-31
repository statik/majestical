//! Video probing, analysis-rate frame decoding through ffmpeg, and adaptive
//! scene detection (a Rust port of `PySceneDetect` `AdaptiveDetector`'s
//! field-tested parameters: HSV mean-abs-diff score, rolling-average ratio
//! threshold 3.0, min content 15.0, 2s min scene, uniform fallback below 10
//! scenes, ~150 keyframe cap).

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::IndexError;

/// Frame rate (fps) analysis frames are decoded at.
pub const ANALYSIS_FPS: f64 = 2.0;
/// Width analysis frames are scaled to.
pub const ANALYSIS_W: u32 = 160;
/// Height analysis frames are scaled to.
pub const ANALYSIS_H: u32 = 90;
/// Upper bound on keyframes returned per video, regardless of scene count.
pub const MAX_KEYFRAMES: usize = 150;

/// Content score below this is never a cut, no matter the neighborhood ratio.
const MIN_CONTENT: f32 = 15.0;
/// A cut fires when `score / neighborhood_average >= RATIO_THRESHOLD`.
const RATIO_THRESHOLD: f32 = 3.0;
/// Rolling-average window radius (frames on each side, excluding self).
const NEIGHBORHOOD_WINDOW: usize = 2;
/// Sample count for the no-cuts-detected fallback.
const UNIFORM_FALLBACK_SAMPLES: u64 = 10;

/// One decoded analysis-rate frame: `ANALYSIS_W x ANALYSIS_H` RGB24.
#[derive(Debug, Clone)]
pub struct Frame {
    pub ts_ms: u64,
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
}

/// Container-level facts ffprobe reports about a video file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
}

/// True if both `ffmpeg` and `ffprobe` are on `PATH` and runnable.
#[must_use]
pub fn ffmpeg_available() -> bool {
    binary_runs("ffmpeg") && binary_runs("ffprobe")
}

fn binary_runs(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// # Errors
///
/// Returns [`IndexError::Video`] if ffprobe can't run, exits with a
/// failure status, or its JSON has no video stream / usable duration.
pub fn probe(path: &Path) -> Result<VideoInfo, IndexError> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .map_err(|e| video_err(path, format!("running ffprobe: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(video_err(path, format!("ffprobe failed: {stderr}")));
    }

    parse_probe_json(path, &output.stdout)
}

fn parse_probe_json(path: &Path, stdout: &[u8]) -> Result<VideoInfo, IndexError> {
    let json: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|e| video_err(path, format!("parsing ffprobe json: {e}")))?;

    let video_stream = json["streams"]
        .as_array()
        .and_then(|streams| streams.iter().find(|s| s["codec_type"] == "video"))
        .ok_or_else(|| video_err(path, "no video stream in ffprobe output".to_string()))?;

    let width = video_stream["width"]
        .as_u64()
        .ok_or_else(|| video_err(path, "missing stream width".to_string()))?;
    let height = video_stream["height"]
        .as_u64()
        .ok_or_else(|| video_err(path, "missing stream height".to_string()))?;

    let duration_str = json["format"]["duration"]
        .as_str()
        .ok_or_else(|| video_err(path, "missing format.duration".to_string()))?;
    let duration_s: f64 = duration_str
        .parse()
        .map_err(|e| video_err(path, format!("parsing duration {duration_str}: {e}")))?;

    Ok(VideoInfo {
        duration_ms: seconds_to_ms(duration_s),
        width: u32::try_from(width).unwrap_or(u32::MAX),
        height: u32::try_from(height).unwrap_or(u32::MAX),
    })
}

fn video_err(path: &Path, message: String) -> IndexError {
    IndexError::Video {
        path: path.to_path_buf(),
        message,
    }
}

// ffprobe durations are non-negative and, for any real media file, far below
// u64::MAX milliseconds; `as` on floats saturates rather than wrapping, so
// malformed (negative/NaN) input clamps to 0 instead of misbehaving.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "media durations are non-negative and far below u64::MAX milliseconds"
)]
fn seconds_to_ms(seconds: f64) -> u64 {
    (seconds * 1000.0).round() as u64
}

/// Decodes the whole file at [`ANALYSIS_FPS`]/[`ANALYSIS_W`]x[`ANALYSIS_H`]
/// RGB24 through ffmpeg's raw pipe.
///
/// # Errors
///
/// Returns [`IndexError::Video`] if ffmpeg can't run, exits with a failure
/// status, or its stdout isn't an exact multiple of one frame's byte size.
pub fn analysis_frames(path: &Path) -> Result<Vec<Frame>, IndexError> {
    let filter = format!("fps={ANALYSIS_FPS},scale={ANALYSIS_W}:{ANALYSIS_H}");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .map_err(|e| video_err(path, format!("running ffmpeg: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(video_err(path, format!("ffmpeg failed: {stderr}")));
    }

    chunk_frames(path, &output.stdout)
}

fn chunk_frames(path: &Path, raw: &[u8]) -> Result<Vec<Frame>, IndexError> {
    let frame_bytes = usize::try_from(ANALYSIS_W * ANALYSIS_H * 3).unwrap_or(usize::MAX);
    if frame_bytes == 0 || !raw.len().is_multiple_of(frame_bytes) {
        return Err(video_err(
            path,
            format!(
                "ffmpeg output length {} is not a multiple of frame size {frame_bytes}",
                raw.len()
            ),
        ));
    }

    let frame_count = raw.len() / frame_bytes;
    let mut frames = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let start = i * frame_bytes;
        frames.push(Frame {
            ts_ms: frame_timestamp_ms(i),
            w: ANALYSIS_W,
            h: ANALYSIS_H,
            rgb: raw[start..start + frame_bytes].to_vec(),
        });
    }
    Ok(frames)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "frame indices for analysis-rate video stay far below f64's 53-bit mantissa"
)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "timestamps stay far below u64::MAX milliseconds for real media"
)]
fn frame_timestamp_ms(index: usize) -> u64 {
    ((index as f64) * 1000.0 / ANALYSIS_FPS) as u64
}

/// Extracts a single full-resolution frame at `ts_ms` as a decoded PNG.
///
/// # Errors
///
/// Returns [`IndexError::Video`] if ffmpeg can't run, exits with a failure
/// status, produces no output, or the output doesn't decode as an image.
pub fn extract_frame(path: &Path, ts_ms: u64) -> Result<image::RgbImage, IndexError> {
    let ts_arg = format_timestamp(ts_ms);
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &ts_arg, "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .map_err(|e| video_err(path, format!("running ffmpeg: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(video_err(path, format!("ffmpeg failed: {stderr}")));
    }
    if output.stdout.is_empty() {
        return Err(video_err(
            path,
            format!("ffmpeg produced no frame at {ts_ms}ms"),
        ));
    }

    image::load_from_memory(&output.stdout)
        .map_err(|e| video_err(path, format!("decoding extracted frame: {e}")))
        .map(|img| img.to_rgb8())
}

fn format_timestamp(ts_ms: u64) -> String {
    let whole_seconds = ts_ms / 1000;
    let millis = ts_ms % 1000;
    format!("{whole_seconds}.{millis:03}")
}

/// Adaptive scene detection over pre-decoded analysis frames.
///
/// Returns keyframe timestamps (ms): one per detected scene's midpoint, or,
/// when no cut is ever found (continuous/gradual footage), 10 timestamps
/// uniformly spread across the duration. A candidate cut whose min-scene-length
/// enforcement removes every real cut collapses to a single midpoint for the
/// whole span, rather than falling back to uniform sampling — see
/// `enforce_min_scene_length`. The result is thinned to [`MAX_KEYFRAMES`].
#[must_use]
pub fn detect_scenes(frames: &[Frame], min_scene_ms: u64, duration_ms: u64) -> Vec<u64> {
    if frames.len() < 2 {
        return Vec::new();
    }
    let raw_cuts = raw_candidate_cuts(frames);
    if raw_cuts.is_empty() {
        return uniform_fallback(duration_ms);
    }
    let accepted = enforce_min_scene_length(&raw_cuts, min_scene_ms, duration_ms);
    thin_to_cap(scene_midpoints(&accepted, duration_ms))
}

fn raw_candidate_cuts(frames: &[Frame]) -> Vec<u64> {
    let scores: Vec<f32> = frames
        .windows(2)
        .map(|w| content_score(&w[0], &w[1]))
        .collect();
    let mut cuts = Vec::new();
    for (i, &score) in scores.iter().enumerate() {
        if score < MIN_CONTENT {
            continue;
        }
        let avg = neighborhood_average(&scores, i);
        // A near-zero neighborhood average makes the ratio undefined
        // (effectively infinite); score clearing MIN_CONTENT is enough on
        // its own in that case.
        if avg <= f32::EPSILON || score / avg >= RATIO_THRESHOLD {
            cuts.push(frames[i + 1].ts_ms);
        }
    }
    cuts
}

fn neighborhood_average(scores: &[f32], i: usize) -> f32 {
    let lo = i.saturating_sub(NEIGHBORHOOD_WINDOW);
    let hi = (i + NEIGHBORHOOD_WINDOW + 1).min(scores.len());
    let mut sum = 0.0_f32;
    let mut count: u32 = 0;
    for (offset, &score) in scores[lo..hi].iter().enumerate() {
        if lo + offset != i {
            sum += score;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "count is at most 2 * NEIGHBORHOOD_WINDOW (4)"
    )]
    let denom = count as f32;
    sum / denom
}

/// Drops cuts that would bound a scene shorter than `min_scene_ms`.
///
/// A too-short scene has both its bounding cuts removed (not just one),
/// merging it into whatever comes before *and* after: a single-frame
/// flicker shouldn't split a video into three scenes just because the
/// blip itself can't stand alone. Repeats until every remaining scene
/// clears the minimum, or no cut is left to remove.
fn enforce_min_scene_length(raw_cuts: &[u64], min_scene_ms: u64, duration_ms: u64) -> Vec<u64> {
    let mut cuts: Vec<u64> = raw_cuts.to_vec();
    loop {
        let boundaries = scene_boundaries(&cuts, duration_ms);
        let Some(short) =
            (0..boundaries.len() - 1).find(|&s| boundaries[s + 1] - boundaries[s] < min_scene_ms)
        else {
            return cuts;
        };

        let next: Vec<u64> = cuts
            .iter()
            .enumerate()
            .filter(|&(i, _)| {
                let is_left_bound = short > 0 && i == short - 1;
                let is_right_bound = short < cuts.len() && i == short;
                !is_left_bound && !is_right_bound
            })
            .map(|(_, &c)| c)
            .collect();

        if next.len() == cuts.len() {
            // The short scene only touches the virtual start/end, with no
            // real cut left to remove.
            return cuts;
        }
        cuts = next;
    }
}

fn scene_boundaries(cuts: &[u64], duration_ms: u64) -> Vec<u64> {
    let mut boundaries = Vec::with_capacity(cuts.len() + 2);
    boundaries.push(0);
    boundaries.extend_from_slice(cuts);
    boundaries.push(duration_ms);
    boundaries
}

fn scene_midpoints(cuts: &[u64], duration_ms: u64) -> Vec<u64> {
    let boundaries = scene_boundaries(cuts, duration_ms);
    boundaries
        .windows(2)
        .map(|w| u64::midpoint(w[0], w[1]))
        .collect()
}

fn uniform_fallback(duration_ms: u64) -> Vec<u64> {
    (0..UNIFORM_FALLBACK_SAMPLES)
        .map(|k| duration_ms * (2 * k + 1) / (2 * UNIFORM_FALLBACK_SAMPLES))
        .collect()
}

fn thin_to_cap(mut keyframes: Vec<u64>) -> Vec<u64> {
    keyframes.sort_unstable();
    keyframes.dedup();
    if keyframes.len() <= MAX_KEYFRAMES {
        return keyframes;
    }
    let total = keyframes.len();
    (0..MAX_KEYFRAMES)
        .map(|i| keyframes[i * total / MAX_KEYFRAMES])
        .collect()
}

fn content_score(a: &Frame, b: &Frame) -> f32 {
    let pixel_count = a.rgb.len() / 3;
    if pixel_count == 0 {
        return 0.0;
    }
    let mut total_diff: u64 = 0;
    for px in 0..pixel_count {
        let i = px * 3;
        let (ah, asat, aval) = rgb_to_hsv_u8(a.rgb[i], a.rgb[i + 1], a.rgb[i + 2]);
        let (bh, bsat, bval) = rgb_to_hsv_u8(b.rgb[i], b.rgb[i + 1], b.rgb[i + 2]);
        total_diff += u64::from(ah.abs_diff(bh))
            + u64::from(asat.abs_diff(bsat))
            + u64::from(aval.abs_diff(bval));
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "frame pixel-component counts (tens of thousands) are far below f32's 24-bit mantissa"
    )]
    let denom = (pixel_count * 3) as f32;
    #[expect(
        clippy::cast_precision_loss,
        reason = "summed u8 abs diffs over one frame stay far below f32's 24-bit mantissa"
    )]
    let total = total_diff as f32;
    total / denom
}

/// Weights hue, saturation, and value equally (edges are not scored: `PySceneDetect`'s
/// `AdaptiveDetector` weight for them is 0 in the field-tested preset this ports).
fn rgb_to_hsv_u8(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let rf = f32::from(r) / 255.0;
    let gf = f32::from(g) / 255.0;
    let bf = f32::from(b) / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;

    let hue_deg = if delta <= f32::EPSILON {
        0.0
    } else if (max - rf).abs() <= f32::EPSILON {
        60.0 * ((gf - bf) / delta).rem_euclid(6.0)
    } else if (max - gf).abs() <= f32::EPSILON {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    let sat = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };

    unit_fractions_to_u8(hue_deg / 360.0, sat, max)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each fraction is clamped to 0.0..=255.0 before the cast"
)]
fn unit_fractions_to_u8(hue_frac: f32, sat_frac: f32, val_frac: f32) -> (u8, u8, u8) {
    let scale = |f: f32| (f * 255.0).round().clamp(0.0, 255.0) as u8;
    (scale(hue_frac), scale(sat_frac), scale(val_frac))
}

#[cfg(test)]
mod tests {
    use super::{Frame, detect_scenes};

    fn solid(ts_ms: u64, rgb: [u8; 3]) -> Frame {
        let w = 16;
        let h = 9;
        let mut pixels = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            pixels.extend_from_slice(&rgb);
        }
        Frame {
            ts_ms,
            w,
            h,
            rgb: pixels,
        }
    }

    #[test]
    fn hard_cuts_at_2fps_are_found_and_midpoints_returned() {
        // 0-3s red, 3-6s green, 6-9s blue at 2fps (18 frames, ts = i*500).
        let mut frames = Vec::new();
        for i in 0..18u64 {
            let ts = i * 500;
            let color = if ts < 3000 {
                [255, 0, 0]
            } else if ts < 6000 {
                [0, 255, 0]
            } else {
                [0, 0, 255]
            };
            frames.push(solid(ts, color));
        }

        let keyframes = detect_scenes(&frames, 2000, 9000);

        assert_eq!(keyframes.len(), 3, "expected 3 scenes, got {keyframes:?}");
        assert!(
            keyframes[0] < 3000,
            "first keyframe should be in the red scene: {keyframes:?}"
        );
        assert!(
            (3000..6000).contains(&keyframes[1]),
            "second keyframe should be in the green scene: {keyframes:?}"
        );
        assert!(
            keyframes[2] >= 6000,
            "third keyframe should be in the blue scene: {keyframes:?}"
        );
    }

    #[test]
    fn single_frame_flicker_shorter_than_min_scene_is_ignored() {
        // 12 frames dark blue-grey, frame 5 pure white.
        let dark = [30, 30, 45];
        let white = [255, 255, 255];
        let mut frames = Vec::new();
        for i in 0..12u64 {
            let color = if i == 5 { white } else { dark };
            frames.push(solid(i * 500, color));
        }

        let keyframes = detect_scenes(&frames, 2000, 6000);

        assert_eq!(
            keyframes.len(),
            1,
            "flicker should not split the scene: {keyframes:?}"
        );
    }

    #[test]
    fn continuous_footage_falls_back_to_uniform_sampling() {
        // 120 frames slow red-channel gradient (i%256 stepping), no cut fires.
        let mut frames = Vec::new();
        for i in 0..120u64 {
            #[expect(clippy::cast_possible_truncation, reason = "i % 256 always fits u8")]
            let r = (i % 256) as u8;
            frames.push(solid(i * 500, [r, 128, 128]));
        }

        let keyframes = detect_scenes(&frames, 2000, 60_000);

        assert_eq!(
            keyframes.len(),
            10,
            "expected uniform fallback: {keyframes:?}"
        );
        assert!(
            keyframes.windows(2).all(|w| w[0] < w[1]),
            "fallback samples must be strictly increasing: {keyframes:?}"
        );
        assert!(*keyframes.first().unwrap() > 0);
        assert!(*keyframes.last().unwrap() < 60_000);
    }

    #[test]
    fn keyframes_are_capped_at_150() {
        // 200 scenes x 4 frames each (2s per scene at 2fps), hue stepped per
        // scene so every boundary is a hard cut.
        let mut frames = Vec::new();
        for scene in 0..200u64 {
            let hue_step = (scene % 6) as u8;
            let color = match hue_step {
                0 => [255, 0, 0],
                1 => [255, 255, 0],
                2 => [0, 255, 0],
                3 => [0, 255, 255],
                4 => [0, 0, 255],
                _ => [255, 0, 255],
            };
            for f in 0..4u64 {
                let ts = scene * 2000 + f * 500;
                frames.push(solid(ts, color));
            }
        }

        let keyframes = detect_scenes(&frames, 2000, 200 * 2000);

        assert!(
            keyframes.len() <= 150,
            "expected cap at 150, got {}",
            keyframes.len()
        );
    }

    #[test]
    fn empty_frames_yield_no_keyframes() {
        let keyframes = detect_scenes(&[], 2000, 10_000);
        assert!(keyframes.is_empty());
    }
}
