#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! Conformance against the pinned Python `transformers` reference (the
//! oracle). Needs a golden JSON file produced by
//! `conformance/encoder/golden.py` plus the fetched ONNX model:
//!
//!   `MAJ_GOLDEN`=/path/to/golden.json `MAJ_MODEL_DIR`=… \
//!     cargo test -p majestical-index --test `encoder_conformance` -- --ignored
//!
//! `just encoder-conformance` drives the whole pipeline (fetch, golden.py,
//! this test) in one shot.

use std::collections::BTreeMap;
use std::path::PathBuf;

use majestical_index::encoder::{Encoder, EncoderOptions, cosine};
use majestical_index::model;
use majestical_index::thumbs::decode_image;

/// Measured cosine floors are well above these; if a fresh run dips below,
/// that's a real preprocessing regression to investigate, not a constant to
/// relax. See the golden-run report for the measured values.
const VISION_CPU_MIN_COSINE: f32 = 0.999;
const VISION_COREML_MIN_COSINE: f32 = 0.99;
const TEXT_MIN_COSINE: f32 = 0.995;

struct Golden {
    images: BTreeMap<String, Vec<f32>>,
    texts: BTreeMap<String, Vec<f32>>,
    token_ids: BTreeMap<String, Vec<i64>>,
}

fn require_golden() -> Golden {
    let path = std::env::var("MAJ_GOLDEN")
        .expect("MAJ_GOLDEN must point to golden.json produced by conformance/encoder/golden.py");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("reading golden json at {path}: {e}");
    });
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parsing golden json");

    let images = json_vector_map(&value, "images");
    let texts = json_vector_map(&value, "texts");

    let token_ids = value["token_ids"]
        .as_object()
        .expect("token_ids must be an object")
        .iter()
        .map(|(text, ids)| {
            let ids = ids
                .as_array()
                .unwrap_or_else(|| panic!("token_ids[{text:?}] must be an array"))
                .iter()
                .map(|v| {
                    v.as_i64()
                        .unwrap_or_else(|| panic!("non-integer token id for {text:?}"))
                })
                .collect();
            (text.clone(), ids)
        })
        .collect();

    Golden {
        images,
        texts,
        token_ids,
    }
}

/// The golden JSON stores embedding components as `f64`; the model and our
/// encoder both operate in `f32`, so this narrowing is expected precision
/// loss, not a bug — cosine similarity at the tolerances this gate checks
/// (0.99+) is insensitive to it.
#[expect(
    clippy::cast_possible_truncation,
    reason = "golden f64 -> encoder f32, matching the model's own precision"
)]
fn json_f32(v: &serde_json::Value, context: &str) -> f32 {
    v.as_f64()
        .unwrap_or_else(|| panic!("non-numeric value in {context}")) as f32
}

fn json_vector_map(value: &serde_json::Value, key: &str) -> BTreeMap<String, Vec<f32>> {
    value[key]
        .as_object()
        .unwrap_or_else(|| panic!("{key} must be an object"))
        .iter()
        .map(|(name, vec)| {
            let floats = vec
                .as_array()
                .unwrap_or_else(|| panic!("{key}[{name:?}] must be an array"))
                .iter()
                .map(|v| json_f32(v, &format!("{key}[{name:?}]")))
                .collect();
            (name.clone(), floats)
        })
        .collect()
}

fn require_model_dir() -> PathBuf {
    let dir = model::model_dir().expect("resolving model dir");
    assert!(
        model::model_present(&dir),
        "model not present at {}; run `maj model fetch`",
        dir.display()
    );
    dir
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn tokenizer_matches_reference_exactly() {
    let golden = require_golden();
    let dir = require_model_dir();
    let encoder = Encoder::load_text_only(&dir).expect("load text-only encoder");

    for (text, expected_ids) in &golden.token_ids {
        let ids = encoder.token_ids(text).expect("tokenize");
        assert_eq!(
            &ids, expected_ids,
            "token ids for {text:?} diverge from the reference tokenizer"
        );
    }
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn cpu_embeddings_match_reference() {
    let golden = require_golden();
    let dir = require_model_dir();
    let mut encoder = Encoder::load(
        &dir,
        &EncoderOptions {
            coreml: false,
            coreml_cache: None,
        },
    )
    .expect("load encoder");

    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut worst_vision = f32::MAX;
    for (name, expected) in &golden.images {
        let path = fixtures.join(name);
        let img = decode_image(&path).unwrap_or_else(|e| panic!("decoding {name}: {e}"));
        let embedding = encoder.embed_image(&img).expect("embed image");
        let score = cosine(&embedding, expected);
        worst_vision = worst_vision.min(score);
        assert!(
            score >= VISION_CPU_MIN_COSINE,
            "{name}: cosine {score} below floor {VISION_CPU_MIN_COSINE}"
        );
    }

    let mut worst_text = f32::MAX;
    for (text, expected) in &golden.texts {
        let embedding = encoder.embed_text(text).expect("embed text");
        let score = cosine(&embedding, expected);
        worst_text = worst_text.min(score);
        assert!(
            score >= TEXT_MIN_COSINE,
            "{text:?}: cosine {score} below floor {TEXT_MIN_COSINE}"
        );
    }

    report_floors(&[("vision (CPU)", worst_vision), ("text", worst_text)]);
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn coreml_vision_is_close_to_reference() {
    let golden = require_golden();
    let dir = require_model_dir();
    let cache = std::env::temp_dir().join(format!("maj-coreml-conformance-{}", std::process::id()));
    let mut encoder = Encoder::load(
        &dir,
        &EncoderOptions {
            coreml: true,
            coreml_cache: Some(cache),
        },
    )
    .expect("load encoder with CoreML vision tower");

    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut worst = f32::MAX;
    for (name, expected) in &golden.images {
        let path = fixtures.join(name);
        let img = decode_image(&path).unwrap_or_else(|e| panic!("decoding {name}: {e}"));
        let embedding = encoder.embed_image(&img).expect("embed image");
        let score = cosine(&embedding, expected);
        worst = worst.min(score);
        assert!(
            score >= VISION_COREML_MIN_COSINE,
            "{name}: CoreML cosine {score} below floor {VISION_COREML_MIN_COSINE}"
        );
    }

    report_floors(&[("vision (CoreML)", worst)]);
}

#[expect(
    clippy::print_stdout,
    reason = "conformance gate: measured floors are part of the run's evidence, \
              printed only under --ignored on an explicit conformance invocation"
)]
fn report_floors(floors: &[(&str, f32)]) {
    for (label, score) in floors {
        println!("conformance floor [{label}]: {score}");
    }
}
