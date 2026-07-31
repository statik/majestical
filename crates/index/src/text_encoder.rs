//! In-process text embedding for transcript chunks and text queries:
//! all-MiniLM-L6-v2 (384-d) via ort on CPU. L2-normalized at the encoder
//! (same invariant as `encoder.rs`) so Lance `Dot` distance = cosine.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy};

use crate::encoder::l2_normalize;
use crate::error::IndexError;

pub const TEXT_EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 256;

/// A loaded `MiniLM` text encoder: ONNX session plus its tokenizer.
pub struct TextEncoder {
    session: Session,
    tokenizer: Tokenizer,
}

impl TextEncoder {
    /// Load `MiniLM` from `model_dir` (files `model.onnx`, `tokenizer.json`).
    ///
    /// # Errors
    /// Returns [`IndexError::Model`] when files are missing or unloadable.
    pub fn load(model_dir: &Path) -> Result<Self, IndexError> {
        let session = build_session(&model_dir.join("model.onnx"))?;
        let tokenizer = load_tokenizer(&model_dir.join("tokenizer.json"))?;
        Ok(Self { session, tokenizer })
    }

    /// Embed one text into a unit-norm 384-d vector.
    ///
    /// # Errors
    /// Returns [`IndexError::Encoder`] on tokenizer or inference failure.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>, IndexError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| IndexError::Encoder(format!("tokenizing {text:?}: {e}")))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
        let attention_mask = encoding.get_attention_mask();
        let mask_i64: Vec<i64> = attention_mask.iter().map(|&m| i64::from(m)).collect();
        let type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&t| i64::from(t))
            .collect();

        let seq = ids.len();
        let seq_i64 = i64::try_from(seq)
            .map_err(|e| IndexError::Encoder(format!("sequence length {seq} out of range: {e}")))?;

        let ids_tensor = Tensor::from_array(([1_i64, seq_i64], ids))
            .map_err(|e| IndexError::Encoder(format!("building input_ids tensor: {e}")))?;
        let mask_tensor = Tensor::from_array(([1_i64, seq_i64], mask_i64))
            .map_err(|e| IndexError::Encoder(format!("building attention_mask tensor: {e}")))?;
        let type_tensor = Tensor::from_array(([1_i64, seq_i64], type_ids))
            .map_err(|e| IndexError::Encoder(format!("building token_type_ids tensor: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
                "token_type_ids" => type_tensor,
            ])
            .map_err(|e| IndexError::Encoder(format!("running text encoder: {e}")))?;

        let value = outputs.get("last_hidden_state").ok_or_else(|| {
            IndexError::Encoder("missing last_hidden_state in model outputs".to_string())
        })?;
        let (_, hidden) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| IndexError::Encoder(format!("extracting last_hidden_state: {e}")))?;

        let expected = seq * TEXT_EMBED_DIM;
        if hidden.len() != expected {
            return Err(IndexError::Encoder(format!(
                "last_hidden_state has {} elements, expected {expected} (seq {seq} x dim {TEXT_EMBED_DIM})",
                hidden.len()
            )));
        }

        let mut embedding = mean_pool(hidden, attention_mask, TEXT_EMBED_DIM);
        l2_normalize(&mut embedding);
        check_unit_norm(&embedding)?;
        Ok(embedding)
    }
}

fn build_session(path: &Path) -> Result<Session, IndexError> {
    let mut builder = Session::builder()
        .map_err(|e| IndexError::Model(format!("creating minilm session builder: {e}")))?;
    builder
        .commit_from_file(path)
        .map_err(|e| IndexError::Model(format!("loading minilm model {}: {e}", path.display())))
}

/// Loads the tokenizer with truncation capped at [`MAX_TOKENS`]. Unlike
/// `encoder.rs`'s fixed-length `SigLIP` text tower, `MiniLM`'s graph has an
/// `attention_mask` input, so the natural (unpadded) sequence length from
/// `encode` is fine — no fixed padding is configured here.
fn load_tokenizer(path: &Path) -> Result<Tokenizer, IndexError> {
    let mut tokenizer = Tokenizer::from_file(path)
        .map_err(|e| IndexError::Model(format!("loading tokenizer {}: {e}", path.display())))?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            direction: TruncationDirection::Right,
            max_length: MAX_TOKENS,
            strategy: TruncationStrategy::LongestFirst,
            stride: 0,
        }))
        .map_err(|e| IndexError::Model(format!("configuring truncation: {e}")))?;
    Ok(tokenizer)
}

/// Mean-pool `[seq, dim]` hidden states over the attention mask.
/// All-masked input yields zeros (callers treat that as "no signal").
fn mean_pool(hidden: &[f32], mask: &[u32], dim: usize) -> Vec<f32> {
    let mut pooled = vec![0.0_f32; dim];
    let mut count = 0.0_f32;
    for (token_index, &m) in mask.iter().enumerate() {
        if m == 0 {
            continue;
        }
        count += 1.0;
        let row = &hidden[token_index * dim..(token_index + 1) * dim];
        for (accumulator, value) in pooled.iter_mut().zip(row) {
            *accumulator += value;
        }
    }
    if count > 0.0 {
        for value in &mut pooled {
            *value /= count;
        }
    }
    pooled
}

/// Validates that `embedding` is either unit-norm (within `1e-3`) or exactly
/// all-zero — the only two shapes `mean_pool` followed by `l2_normalize` can
/// produce (all-masked input pools to zeros, which `l2_normalize` leaves
/// unchanged; everything else should land on the unit sphere). A non-finite
/// value (NaN/inf) fails both checks below since `x == 0.0` and the norm
/// comparison are both false for NaN, so it's rejected explicitly first
/// rather than silently slipping through as "close enough" to 1.0.
///
/// # Errors
/// Returns [`IndexError::Encoder`] if `embedding` contains a non-finite
/// value, or its norm is neither ~1.0 nor exactly 0.
fn check_unit_norm(embedding: &[f32]) -> Result<(), IndexError> {
    if embedding.iter().any(|x| !x.is_finite()) {
        return Err(IndexError::Encoder(
            "text embedding contains a non-finite value (NaN/inf)".to_string(),
        ));
    }
    if embedding.iter().all(|&x| x == 0.0) {
        return Ok(());
    }
    let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if (norm - 1.0).abs() > 1e-3 {
        return Err(IndexError::Encoder(format!(
            "text embedding failed to normalize: norm {norm} not within 1e-3 of 1.0 (and not \
             all-zero) — Lance ranking assumes unit-normalized embeddings"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_unit_norm, mean_pool};
    use crate::error::IndexError;

    #[test]
    fn mean_pool_respects_attention_mask() {
        // 3 tokens, dim 2; third token masked out.
        let hidden = [1.0_f32, 2.0, 3.0, 4.0, 100.0, 100.0];
        let mask = [1_u32, 1, 0];
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled, vec![2.0, 3.0]); // mean of (1,3) and (2,4)
    }

    #[test]
    fn mean_pool_all_masked_returns_zeros_not_nan() {
        let hidden = [1.0_f32, 2.0];
        let mask = [0_u32];
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled, vec![0.0, 0.0]);
    }

    #[test]
    fn check_unit_norm_accepts_a_unit_vector() {
        assert!(check_unit_norm(&[0.6, 0.8]).is_ok());
    }

    #[test]
    fn check_unit_norm_accepts_an_all_zero_vector() {
        assert!(check_unit_norm(&[0.0, 0.0]).is_ok());
    }

    #[test]
    fn check_unit_norm_rejects_a_non_unit_norm_vector() {
        let err = check_unit_norm(&[0.5, 0.0]).expect_err("0.5 norm must fail");
        assert!(matches!(err, IndexError::Encoder(_)));
    }

    #[test]
    fn check_unit_norm_rejects_nan() {
        let err = check_unit_norm(&[f32::NAN, 0.0]).expect_err("NaN must fail, not pass silently");
        assert!(matches!(err, IndexError::Encoder(_)));
    }
}
