//! In-process transcription: whisper.cpp via whisper-rs (Metal).
//! Timestamps come back in centiseconds from whisper-rs 0.16 — converted
//! to ms at the boundary and never exposed otherwise.

use std::path::Path;

use crate::error::IndexError;

/// Blob model tag for transcripts — derived from the model registry so a
/// registry bump can never silently diverge from the tag blobs are written
/// under.
pub const WHISPER_MODEL_TAG: &str = crate::model::WHISPER.tag;
/// The single ggml weights file [`Transcriber::load`] opens — derived from
/// the registry for the same no-drift reason as [`WHISPER_MODEL_TAG`].
pub const MODEL_FILE: &str = crate::model::WHISPER.files[0].name;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    pub model_tag: String,
    pub segments: Vec<TranscriptSegment>,
    pub text: String,
}

impl Transcript {
    /// # Errors
    /// Serialization failure (never expected for these plain types).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self)
            .map_err(|error| IndexError::Model(format!("transcript json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes)
            .map_err(|error| IndexError::Model(format!("transcript parse: {error}")))
    }
}

pub(crate) fn centis_to_ms(centis: i64) -> u64 {
    u64::try_from(centis.max(0)).unwrap_or(0) * 10
}

pub(crate) fn full_text(segments: &[TranscriptSegment]) -> String {
    segments
        .iter()
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

pub struct Transcriber {
    context: whisper_rs::WhisperContext,
}

impl Transcriber {
    /// Load the ggml model from `model_dir`.
    ///
    /// # Errors
    /// Returns `IndexError::Model` when the file is missing or unloadable.
    pub fn load(model_dir: &Path) -> Result<Self, IndexError> {
        // whisper.cpp/GGML log straight to stdout/stderr by default; with
        // neither the `log_backend` nor `tracing_backend` feature enabled,
        // installing the hook silences them entirely rather than routing
        // them anywhere — exactly what library code needs (no prints).
        whisper_rs::install_logging_hooks();

        let path = model_dir.join(MODEL_FILE);
        let path_str = path
            .to_str()
            .ok_or_else(|| IndexError::Model(format!("non-utf8 model path {}", path.display())))?;
        let context = whisper_rs::WhisperContext::new_with_params(
            path_str,
            whisper_rs::WhisperContextParameters::default(),
        )
        .map_err(|error| IndexError::Model(format!("whisper load: {error}")))?;
        Ok(Self { context })
    }

    /// Transcribe mono 16 kHz f32 PCM with auto language detection.
    ///
    /// # Errors
    /// Returns `IndexError::Encoder` on inference failure.
    pub fn transcribe(&self, pcm: &[f32]) -> Result<Transcript, IndexError> {
        let mut state = self
            .context
            .create_state()
            .map_err(|error| IndexError::Encoder(format!("whisper state: {error}")))?;
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(None); // auto-detect
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        state
            .full(params, pcm)
            .map_err(|error| IndexError::Encoder(format!("whisper full: {error}")))?;

        let mut segments = Vec::new();
        let count = state.full_n_segments();
        for index in 0..count {
            let Some(segment) = state.get_segment(index) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map_err(|error| IndexError::Encoder(format!("whisper segment text: {error}")))?
                .into_owned();
            segments.push(TranscriptSegment {
                start_ms: centis_to_ms(segment.start_timestamp()),
                end_ms: centis_to_ms(segment.end_timestamp()),
                text,
            });
        }
        let text = full_text(&segments);
        Ok(Transcript {
            model_tag: WHISPER_MODEL_TAG.to_string(),
            segments,
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_json_round_trips() {
        let transcript = Transcript {
            model_tag: WHISPER_MODEL_TAG.to_string(),
            segments: vec![TranscriptSegment {
                start_ms: 0,
                end_ms: 2_500,
                text: " Hello world.".into(),
            }],
            text: "Hello world.".into(),
        };
        let bytes = transcript.to_json().expect("serialize");
        let back = Transcript::from_json(&bytes).expect("parse");
        assert_eq!(back.segments.len(), 1);
        assert_eq!(back.segments[0].end_ms, 2_500);
    }

    #[test]
    fn centiseconds_convert_to_ms() {
        assert_eq!(centis_to_ms(250), 2_500);
        assert_eq!(centis_to_ms(0), 0);
        assert_eq!(centis_to_ms(-5), 0); // negative centis clamp to zero
    }

    #[test]
    fn full_text_joins_trimmed_segments() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 0,
                end_ms: 1,
                text: " Hello".into(),
            },
            TranscriptSegment {
                start_ms: 1,
                end_ms: 2,
                text: " world.".into(),
            },
        ];
        assert_eq!(full_text(&segments), "Hello world.");
    }
}
