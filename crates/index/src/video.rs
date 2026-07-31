//! Video probing, analysis-rate frame decoding through ffmpeg, and adaptive
//! scene detection (a Rust port of `PySceneDetect` `AdaptiveDetector`'s
//! field-tested parameters: HSV mean-abs-diff score, rolling-average ratio
//! threshold 3.0, min content 15.0, 2s min scene, 10-sample uniform fallback
//! when zero cuts are detected at all, ~150 keyframe cap).
//!
//! [`analysis_frames`] decodes and buffers the *entire* clip before
//! returning rather than streaming frame-by-frame — at [`ANALYSIS_FPS`]/
//! [`ANALYSIS_W`]x[`ANALYSIS_H`] RGB24 that's roughly 600MB/hour of footage
//! held in memory at once (ffmpeg's raw stdout plus the copied `Frame`
//! vec briefly coexist); switching to a streaming decode is a tracked
//! follow-up, not implemented here. Every ffmpeg/ffprobe call in this
//! module (`probe`, `analysis_frames`, `extract_frame`) is a blocking
//! subprocess call that waits for the child to exit — a video on a stalled
//! or disconnecting volume stalls whichever `index run` pass is working it.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

/// Run a command with a hard timeout, killing the child on expiry.
///
/// std-only: polls `try_wait` at 100ms intervals. Stdout/stderr are drained
/// on a reader thread via piped output, so a chatty child can't deadlock on
/// a full pipe while this thread is busy waiting.
///
/// # Errors
///
/// Returns an error message if spawning fails, waiting fails, the reader
/// thread panics, or the child is still running once `timeout` elapses (in
/// which case it is killed rather than waited on further).
pub(crate) fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<Output, String> {
    use std::io::Read as _;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| format!("spawn: {error}"))?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut stdout);
        }
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        (stdout, stderr)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|error| format!("wait: {error}"))? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let (stdout, stderr) = reader
        .join()
        .map_err(|_| "reader thread panicked".to_string())?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Timeout for audio extraction: a fixed 60s floor plus 1s per source
/// second, so a long clip isn't starved by a floor sized for short ones.
pub(crate) fn audio_timeout(duration_ms: u64) -> Duration {
    Duration::from_secs(60 + duration_ms / 1000)
}

/// Extracts mono 16kHz `f32` PCM (whisper's native input format) from any
/// av file's audio track through ffmpeg, hard-timed out by [`audio_timeout`]
/// so a stalled/hung ffmpeg process can't block an `index run` pass
/// indefinitely the way the other ffmpeg calls in this module can (see the
/// module doc).
///
/// # Errors
///
/// Returns [`IndexError::Video`] if ffmpeg can't run, times out, or exits
/// with a failure status.
pub fn extract_audio_pcm(path: &Path, duration_ms: u64) -> Result<Vec<f32>, IndexError> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ar", "16000", "-ac", "1", "-f", "f32le", "-"]);

    let output = run_with_timeout(command, audio_timeout(duration_ms))
        .map_err(|message| video_err(path, format!("audio extract: {message}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(video_err(
            path,
            format!("ffmpeg audio extract failed: {stderr}"),
        ));
    }

    let mut pcm = Vec::with_capacity(output.stdout.len() / 4);
    for chunk in output.stdout.chunks_exact(4) {
        pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(pcm)
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
        // A zero (or near-zero) neighborhood average needs no special case:
        // IEEE float division by 0.0 yields +inf, which clears
        // RATIO_THRESHOLD on its own — `score` is already >0.0 here (it
        // cleared MIN_CONTENT above), so `score / avg` can never be the
        // 0.0/0.0 NaN case.
        if score / avg >= RATIO_THRESHOLD {
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
///
/// Hue is packed onto the same 0-255 `u8` scale as saturation and value —
/// not `OpenCV`/`PySceneDetect`'s reference 0-179 scale — so a hue delta here
/// weighs about 1.42x (255/179) what the same angular difference weighs in
/// the reference implementation, against the same `MIN_CONTENT`/`RATIO_THRESHOLD`
/// ported from it rather than re-tuned for this scale; the module's e2e
/// tests (real ffmpeg clips) validate scene detection still holds up in
/// practice at this scale. Hue is also circular (0 and 255 are the same
/// angle) but `content_score`'s `u8::abs_diff` on it doesn't wrap — matching
/// the reference implementation's own unwrapped hue diff, not a bug unique
/// to this port.
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
    use super::{
        ANALYSIS_H, ANALYSIS_W, Frame, MAX_KEYFRAMES, VideoInfo, audio_timeout, binary_runs,
        chunk_frames, detect_scenes, enforce_min_scene_length, extract_audio_pcm, ffmpeg_available,
        format_timestamp, frame_timestamp_ms, parse_probe_json, raw_candidate_cuts, rgb_to_hsv_u8,
        run_with_timeout, scene_midpoints, seconds_to_ms,
    };
    use std::path::Path;

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

        // Exact midpoints, not just "somewhere in the right scene": a range
        // check here would pass just as well on the scene START timestamps
        // (0, 3000, 6000), so it can't tell a real midpoint bug apart from a
        // correct result.
        assert_eq!(keyframes, vec![1500, 4500, 7500]);
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

    /// 200 scenes x 4 frames each (2s per scene at 2fps), hue stepped by 71
    /// (coprime with 255, so consecutive scenes never repeat or land close
    /// together within a short run) mod 255 per scene, so every scene
    /// boundary is a genuine hard cut — unlike the old fixed 6-color cycle,
    /// whose adjacent-color deltas mostly stayed under `MIN_CONTENT` and
    /// left the test passing on ~34 real keyframes, never actually
    /// exercising `thin_to_cap`.
    fn stepped_hue_scenes(scene_count: u64) -> Vec<Frame> {
        let mut frames = Vec::new();
        for scene in 0..scene_count {
            // `% 255` bounds the result to 0..=254, which clippy's range
            // analysis recognizes as always fitting u8 — no truncation
            // `#[expect]` needed here.
            let hue = ((scene * 71) % 255) as u8;
            let color = [hue, 255 - hue, 128];
            for f in 0..4u64 {
                let ts = scene * 2000 + f * 500;
                frames.push(solid(ts, color));
            }
        }
        frames
    }

    #[test]
    fn keyframes_are_capped_at_150() {
        let scene_count = 200u64;
        let duration_ms = scene_count * 2000;
        let frames = stepped_hue_scenes(scene_count);

        // Pin the fixture actually produces more real scenes than the cap,
        // not just fewer than or equal to it — a fixture that happened to
        // detect <=150 scenes would make `keyframes.len() <= 150` pass
        // without `thin_to_cap` ever running.
        let raw_cuts = raw_candidate_cuts(&frames);
        let accepted = enforce_min_scene_length(&raw_cuts, 2000, duration_ms);
        let pre_cap = scene_midpoints(&accepted, duration_ms).len();
        assert!(
            pre_cap > MAX_KEYFRAMES,
            "fixture must out-scene the cap to exercise thinning, got {pre_cap} pre-cap scenes"
        );

        let keyframes = detect_scenes(&frames, 2000, duration_ms);

        // Exact, not `<=`: pins the cap actually binds, discriminating a
        // `thin_to_cap` regression from a fixture that just happens to clear
        // 150 on its own.
        assert_eq!(
            keyframes.len(),
            MAX_KEYFRAMES,
            "expected the cap to bind exactly, got {}",
            keyframes.len()
        );
    }

    #[test]
    fn empty_frames_yield_no_keyframes() {
        let keyframes = detect_scenes(&[], 2000, 10_000);
        assert!(keyframes.is_empty());
    }

    /// `frames.len() < 2` is the "too few frames to compare" guard — exactly
    /// 2 frames is the minimum that must still run detection, not be turned
    /// away by it. A `<` -> `<=` mutation on that guard would treat 2 frames
    /// the same as 0 or 1.
    #[test]
    fn exactly_two_frames_is_enough_to_run_detection() {
        let frames = vec![solid(0, [255, 0, 0]), solid(2000, [0, 0, 255])];
        let keyframes = detect_scenes(&frames, 500, 4000);
        assert_eq!(
            keyframes,
            vec![1000, 3000],
            "2 frames with an obvious color change must still detect the cut between them"
        );
    }

    #[test]
    fn seconds_to_ms_rounds_to_the_nearest_millisecond() {
        assert_eq!(seconds_to_ms(0.0), 0);
        assert_eq!(seconds_to_ms(9.125), 9125);
    }

    #[test]
    fn frame_timestamp_ms_scales_index_by_the_analysis_frame_rate() {
        // At ANALYSIS_FPS (2.0), index 1 is 500ms and index 3 is 1500ms —
        // two data points, since a `*` -> `+` mutation on the numerator
        // happens to agree with the correct answer at index 1 alone.
        assert_eq!(frame_timestamp_ms(0), 0);
        assert_eq!(frame_timestamp_ms(1), 500);
        assert_eq!(frame_timestamp_ms(3), 1500);
    }

    #[test]
    fn format_timestamp_splits_whole_seconds_and_millis() {
        assert_eq!(format_timestamp(0), "0.000");
        assert_eq!(format_timestamp(1234), "1.234");
        assert_eq!(format_timestamp(60_500), "60.500");
    }

    #[test]
    fn chunk_frames_slices_ts_and_bytes_per_frame_without_overlap() {
        let frame_bytes = usize::try_from(ANALYSIS_W * ANALYSIS_H * 3).expect("fits");
        let mut raw = vec![0u8; frame_bytes * 2];
        // Frame 1 is filled with a distinct byte so a slicing-offset bug
        // (wrong `start`, or `start..start+frame_bytes` reading into frame
        // 0's region) shows up as a byte-value mismatch, not just a length
        // mismatch.
        for b in &mut raw[frame_bytes..] {
            *b = 7;
        }
        let frames = chunk_frames(Path::new("clip.mp4"), &raw).expect("chunk");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].ts_ms, 0);
        assert_eq!(
            frames[1].ts_ms, 500,
            "frame 1 at 2fps analysis rate lands at 500ms"
        );
        assert!(frames[0].rgb.iter().all(|&b| b == 0));
        assert!(
            frames[1].rgb.iter().all(|&b| b == 7),
            "frame 1 must slice the second chunk, not overlap frame 0"
        );
    }

    #[test]
    fn chunk_frames_rejects_a_length_not_a_multiple_of_the_frame_size() {
        let frame_bytes = usize::try_from(ANALYSIS_W * ANALYSIS_H * 3).expect("fits");
        let raw = vec![0u8; frame_bytes + 1];
        assert!(chunk_frames(Path::new("clip.mp4"), &raw).is_err());
    }

    #[test]
    fn parse_probe_json_finds_the_video_stream_and_ignores_others() {
        let json = br#"{
            "streams": [
                {"codec_type": "audio", "width": 999, "height": 999},
                {"codec_type": "video", "width": 320, "height": 180}
            ],
            "format": {"duration": "9.500"}
        }"#;
        let info = parse_probe_json(Path::new("clip.mp4"), json).expect("parse");
        assert_eq!(
            info,
            VideoInfo {
                duration_ms: 9500,
                width: 320,
                height: 180
            },
            "must pick the video stream's dimensions, not the audio stream's"
        );
    }

    /// Hue/saturation/value are all packed onto the same 0-255 `u8` scale
    /// (not the reference 0-179 hue scale — see the module doc on
    /// `rgb_to_hsv_u8`). Six known colors pin every branch: the three
    /// `hue_deg` cases (red takes the `max == r` branch, green `max == g`,
    /// blue the `else` branch), the `delta <= EPSILON` zero-hue case (white,
    /// black, grey), and the `max <= EPSILON` zero-saturation case (black).
    #[test]
    fn rgb_to_hsv_u8_matches_known_color_conversions() {
        assert_eq!(rgb_to_hsv_u8(255, 0, 0), (0, 255, 255), "pure red");
        assert_eq!(rgb_to_hsv_u8(0, 255, 0), (85, 255, 255), "pure green");
        assert_eq!(rgb_to_hsv_u8(0, 0, 255), (170, 255, 255), "pure blue");
        assert_eq!(rgb_to_hsv_u8(255, 255, 255), (0, 0, 255), "white");
        assert_eq!(rgb_to_hsv_u8(0, 0, 0), (0, 0, 0), "black");
        assert_eq!(rgb_to_hsv_u8(128, 128, 128), (0, 0, 128), "mid grey");
    }

    #[cfg(unix)]
    #[test]
    fn binary_runs_reports_true_only_for_an_actually_runnable_binary() {
        assert!(
            binary_runs("true"),
            "the POSIX `true` utility must be runnable via PATH"
        );
        assert!(
            !binary_runs("definitely-not-a-real-binary-9f3c2"),
            "a nonexistent binary must report false, not true"
        );
    }

    /// Every adjacent frame pair here scores content ~16.67 (comfortably
    /// above `MIN_CONTENT`'s 15.0) but *identically* across the whole clip,
    /// so a real neighborhood average also settles near 16.67 and the fire
    /// ratio stays near 1.0 — well under `RATIO_THRESHOLD` (3.0). No cut
    /// should ever fire; the clip must fall back to uniform sampling.
    /// `neighborhood_average` returning a bogus `0.0` (e.g. a dropped
    /// divide-by-count guard) turns every one of these above-floor pairs
    /// into `score / 0.0 == +inf`, which clears the ratio threshold
    /// unconditionally — this is the regression that guards against that.
    #[test]
    fn sustained_above_threshold_motion_does_not_fire_a_cut_on_its_own() {
        let mut frames = Vec::new();
        let mut val: i32 = 0;
        let mut step: i32 = 50;
        for i in 0..40u64 {
            let v = u8::try_from(val).expect("stays within 0..=255 by construction");
            frames.push(solid(i * 500, [v, v, v]));
            if val + step > 255 || val + step < 0 {
                step = -step;
            }
            val += step;
        }

        let keyframes = detect_scenes(&frames, 2000, 20_000);

        assert_eq!(
            keyframes.len(),
            10,
            "sustained above-threshold motion with a flat neighborhood must fall back to \
             uniform sampling, not fire a cut at every frame: {keyframes:?}"
        );
    }

    /// Isolates the `<` in `raw_candidate_cuts`'s `MIN_CONTENT` gate from
    /// the ratio check that follows it: five flat frames (content score 0
    /// for every pair) surround one jump whose score lands exactly on
    /// `MIN_CONTENT` (a single gray pixel, val jumping by 45 — hue and
    /// saturation are both 0 for grayscale, so `45 / 3 == 15.0` exactly).
    /// The near-zero neighborhood average on both sides means the ratio
    /// clears `RATIO_THRESHOLD` trivially once the score clears the gate at
    /// all, so whether a cut fires here depends only on the gate itself.
    #[test]
    fn min_content_gate_is_exclusive_a_score_at_the_floor_still_cuts() {
        let gray = |ts_ms: u64, v: u8| Frame {
            ts_ms,
            w: 1,
            h: 1,
            rgb: vec![v, v, v],
        };
        let frames = vec![
            gray(0, 100),
            gray(500, 100),
            gray(1000, 100),
            gray(1500, 145), // |145-100| = 45 -> content_score 45/3 = 15.0 exactly
            gray(2000, 145),
            gray(2500, 145),
        ];

        let cuts = raw_candidate_cuts(&frames);

        assert_eq!(
            cuts,
            vec![1500],
            "a content score exactly at MIN_CONTENT must still be treated as clearing it"
        );
    }

    #[test]
    fn run_with_timeout_kills_a_hung_process() {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        let started = std::time::Instant::now();

        let result = run_with_timeout(command, std::time::Duration::from_millis(300));

        assert!(result.is_err(), "hung process must error");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "must not wait for sleep 30"
        );
    }

    #[test]
    fn run_with_timeout_returns_output_of_fast_process() {
        let mut command = std::process::Command::new("echo");
        command.arg("ok");

        let output = run_with_timeout(command, std::time::Duration::from_secs(5)).expect("fast");

        assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));
    }

    #[test]
    fn audio_timeout_scales_with_duration() {
        assert_eq!(audio_timeout(0), std::time::Duration::from_mins(1));
        assert_eq!(audio_timeout(3_600_000), std::time::Duration::from_mins(61));
    }

    /// No ffmpeg fixture needed: a nonexistent input path fails either way
    /// ffmpeg can fail on it — a missing `ffmpeg` binary trips
    /// `run_with_timeout`'s spawn error (message prefixed "audio extract:"
    /// by `extract_audio_pcm`), while an installed ffmpeg instead runs and
    /// exits non-zero over the missing input (message prefixed "ffmpeg
    /// audio extract failed:") — both land in the same `Err` and both
    /// mention ffmpeg or the extraction step, which is all this test pins.
    #[test]
    fn extract_audio_pcm_reports_ffmpeg_or_audio_extract_failure() {
        if !ffmpeg_available() {
            return;
        }

        let path = Path::new("/nonexistent/definitely-not-a-real-clip-9f3c2.mp4");
        let error = extract_audio_pcm(path, 0).expect_err("nonexistent input must fail");

        let message = error.to_string();
        assert!(
            message.contains("audio extract") || message.contains("ffmpeg"),
            "error should mention audio extraction or ffmpeg: {message}"
        );
    }
}
