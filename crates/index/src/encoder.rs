//! `SigLIP` 2 dual-tower encoder via ONNX Runtime. Vision on the `CoreML` EP
//! (Apple Neural Engine), text on CPU (`CoreML` mishandles the text tower's
//! shapes; the fp16 text model is fast enough for query-time encoding). Both
//! towers emit `pooler_output`, L2-normalized here so dot product == cosine.

use std::path::{Path, PathBuf};

use ort::ep::CoreML;
use ort::ep::coreml::{ComputeUnits, ModelFormat};
use ort::session::{Session, SessionOutputs};
use ort::value::Tensor;
use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationDirection,
    TruncationParams, TruncationStrategy,
};

use crate::error::IndexError;
use crate::preprocess::{EDGE, preprocess_rgb};

pub const EMBED_DIM: usize = 768;
const TEXT_LEN: usize = 64;

/// Options for [`Encoder::load`].
pub struct EncoderOptions {
    /// Run the vision tower on the `CoreML` execution provider (Apple Neural
    /// Engine) instead of CPU.
    pub coreml: bool,
    /// Directory where `CoreML` caches its compiled model graph. Without this,
    /// `CoreML` recompiles the vision tower's graph on every session load,
    /// adding several seconds of startup latency.
    pub coreml_cache: Option<PathBuf>,
}

/// A loaded `SigLIP` 2 encoder. `vision` is `None` for text-only encoders
/// (e.g. query-time text embedding), so loading skips the 372 MB vision
/// tower entirely.
#[derive(Debug)]
pub struct Encoder {
    vision: Option<Session>,
    text: Session,
    tokenizer: Tokenizer,
}

impl Encoder {
    /// Loads both towers plus the tokenizer from `model_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Encoder`] if either ONNX model or the tokenizer
    /// fails to load.
    pub fn load(model_dir: &Path, options: &EncoderOptions) -> Result<Self, IndexError> {
        let vision = build_vision_session(&model_dir.join("vision_model.onnx"), options)?;
        let text = build_text_session(&model_dir.join("text_model_fp16.onnx"))?;
        let tokenizer = load_tokenizer(&model_dir.join("tokenizer.json"))?;
        Ok(Self {
            vision: Some(vision),
            text,
            tokenizer,
        })
    }

    /// Loads only the text tower and tokenizer, skipping the vision tower.
    /// Used for query-time text embedding, where loading the 372 MB vision
    /// model would be pure waste.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Encoder`] if the text model or tokenizer fails
    /// to load.
    pub fn load_text_only(model_dir: &Path) -> Result<Self, IndexError> {
        let text = build_text_session(&model_dir.join("text_model_fp16.onnx"))?;
        let tokenizer = load_tokenizer(&model_dir.join("tokenizer.json"))?;
        Ok(Self {
            vision: None,
            text,
            tokenizer,
        })
    }

    /// Preprocesses `image` and runs the vision tower, returning an
    /// L2-normalized 768-dim embedding.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Encoder`] if this encoder has no vision tower
    /// loaded (see [`Encoder::load_text_only`]), or if preprocessing or
    /// inference fails.
    pub fn embed_image(&mut self, image: &image::RgbImage) -> Result<Vec<f32>, IndexError> {
        let Some(vision) = self.vision.as_mut() else {
            return Err(IndexError::Encoder(
                "text-only encoder: vision tower not loaded".to_string(),
            ));
        };
        let pixels = preprocess_rgb(image)?;
        let edge = i64::from(EDGE);
        let tensor = Tensor::from_array(([1_i64, 3, edge, edge], pixels))
            .map_err(|e| IndexError::Encoder(format!("building pixel_values tensor: {e}")))?;
        let outputs = vision
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e| IndexError::Encoder(format!("running vision tower: {e}")))?;
        pooled(&outputs)
    }

    /// Tokenizes and runs the text tower on `text`, returning an
    /// L2-normalized 768-dim embedding.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Encoder`] if tokenization or inference fails.
    pub fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, IndexError> {
        let ids = self.token_ids(text)?;
        let text_len = i64::try_from(TEXT_LEN)
            .map_err(|e| IndexError::Encoder(format!("TEXT_LEN overflow: {e}")))?;
        let tensor = Tensor::from_array(([1_i64, text_len], ids))
            .map_err(|e| IndexError::Encoder(format!("building input_ids tensor: {e}")))?;
        let outputs = self
            .text
            .run(ort::inputs!["input_ids" => tensor])
            .map_err(|e| IndexError::Encoder(format!("running text tower: {e}")))?;
        pooled(&outputs)
    }

    /// Tokenizes `text` and returns the fixed-length `64`-token id sequence
    /// without running the model. Used by the golden-token conformance test.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Encoder`] if tokenization fails, or if the
    /// resulting sequence isn't exactly [`TEXT_LEN`] tokens long — with the
    /// tokenizer's padding/truncation configured as `Encoder::load` does,
    /// this should never happen; a mismatch means that configuration broke.
    pub fn token_ids(&self, text: &str) -> Result<Vec<i64>, IndexError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| IndexError::Encoder(format!("tokenizing {text:?}: {e}")))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
        if ids.len() != TEXT_LEN {
            return Err(IndexError::Encoder(format!(
                "tokenizer produced {} ids for {text:?}, expected fixed length {TEXT_LEN}",
                ids.len()
            )));
        }
        Ok(ids)
    }
}

fn build_vision_session(path: &Path, options: &EncoderOptions) -> Result<Session, IndexError> {
    let builder = Session::builder()
        .map_err(|e| IndexError::Encoder(format!("creating vision session builder: {e}")))?;
    let mut builder = if options.coreml {
        // `ModelFormat::MLProgram` fails to compile this model: ort's
        // ONNX->ML Program converter can't translate the patch embedding
        // Conv node ("Required param 'pad' is missing"). NeuralNetwork is
        // CoreML's older format but still runs on the ANE via
        // `ComputeUnits::CPUAndNeuralEngine`, and matches the reference
        // encoder within 0.9999+ cosine (see the conformance gate).
        let mut ep = CoreML::default()
            .with_model_format(ModelFormat::NeuralNetwork)
            .with_compute_units(ComputeUnits::CPUAndNeuralEngine);
        if let Some(cache) = &options.coreml_cache {
            std::fs::create_dir_all(cache)
                .map_err(|e| IndexError::Encoder(format!("creating CoreML cache dir: {e}")))?;
            ep = ep.with_model_cache_dir(cache.to_string_lossy().into_owned());
        }
        builder
            .with_execution_providers([ep.build()])
            .map_err(|e| IndexError::Encoder(format!("registering CoreML EP: {e}")))?
    } else {
        builder
    };
    builder
        .commit_from_file(path)
        .map_err(|e| IndexError::Encoder(format!("loading vision model {}: {e}", path.display())))
}

fn build_text_session(path: &Path) -> Result<Session, IndexError> {
    let mut builder = Session::builder()
        .map_err(|e| IndexError::Encoder(format!("creating text session builder: {e}")))?;
    builder
        .commit_from_file(path)
        .map_err(|e| IndexError::Encoder(format!("loading text model {}: {e}", path.display())))
}

/// Loads the tokenizer with EXPLICIT padding and truncation. The shipped
/// `tokenizer.json` ships `add_eos: true` (so `encode(_, true)` appends the
/// eos id `1`) and a padding config, but no truncation config — the text
/// tower has no attention-mask input and unconditionally pools position 63,
/// so any sequence not exactly 64 tokens long, right-padded with id `0`,
/// silently produces a degraded (not erroring) embedding.
fn load_tokenizer(path: &Path) -> Result<Tokenizer, IndexError> {
    let mut tokenizer = Tokenizer::from_file(path)
        .map_err(|e| IndexError::Encoder(format!("loading tokenizer {}: {e}", path.display())))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::Fixed(TEXT_LEN),
        direction: PaddingDirection::Right,
        pad_to_multiple_of: None,
        pad_id: 0,
        pad_type_id: 0,
        pad_token: "<pad>".to_string(),
    }));
    tokenizer
        .with_truncation(Some(TruncationParams {
            direction: TruncationDirection::Right,
            max_length: TEXT_LEN,
            strategy: TruncationStrategy::LongestFirst,
            stride: 0,
        }))
        .map_err(|e| IndexError::Encoder(format!("configuring truncation: {e}")))?;
    Ok(tokenizer)
}

fn pooled(outputs: &SessionOutputs<'_>) -> Result<Vec<f32>, IndexError> {
    let value = outputs
        .get("pooler_output")
        .ok_or_else(|| IndexError::Encoder("missing pooler_output in model outputs".to_string()))?;
    let (_, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| IndexError::Encoder(format!("extracting pooler_output: {e}")))?;
    if data.len() != EMBED_DIM {
        return Err(IndexError::Encoder(format!(
            "pooler_output has {} elements, expected {EMBED_DIM}",
            data.len()
        )));
    }
    let mut embedding = data.to_vec();
    l2_normalize(&mut embedding);
    let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if (norm - 1.0).abs() > 1e-3 {
        return Err(IndexError::Encoder(format!(
            "pooler output failed to normalize: norm {norm} not within 1e-3 of 1.0 — \
             PR 7's Lance ranking assumes unit-normalized embeddings"
        )));
    }
    Ok(embedding)
}

/// Normalizes `v` to unit L2 norm in place. A zero vector is left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Dot product of `a` and `b`. Callers are expected to pass L2-normalized
/// vectors, making this equivalent to cosine similarity.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_scales_to_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_leaves_zero_vector_unchanged() {
        let mut v = vec![0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0]);
    }

    #[test]
    fn cosine_of_identical_unit_vectors_is_one() {
        let mut v = vec![1.0, 2.0, 3.0];
        l2_normalize(&mut v);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_unit_vectors_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn loading_from_an_empty_dir_errors_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = Encoder::load_text_only(dir.path()).expect_err("must fail");
        assert!(matches!(err, IndexError::Encoder(_)));
    }
}
