#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! Conformance against the pinned Python `sentence-transformers` reference
//! (the oracle) for the `MiniLM` text encoder. Needs a golden JSON file
//! produced by `conformance/text-encoder/golden.py` plus the fetched ONNX
//! model:
//!
//!   `MAJ_GOLDEN`=/path/to/golden.json `MAJ_MODEL_DIR`=… \
//!     cargo test -p majestical-index --test `text_encoder_conformance` -- --ignored
//!
//! `just text-encoder-conformance` drives the whole pipeline (fetch,
//! golden.py, this test) in one shot.
//!
//! Both sides truncate at 256 tokens: the reference's
//! `model.max_seq_length` (printed by `golden.py`) is 256 for
//! `all-MiniLM-L6-v2` at the pinned revision, matching `MAX_TOKENS` in
//! `text_encoder.rs` — no truncation-length mismatch to reconcile here.
//!
//! The empty-string fixture is kept (not dropped): sentence-transformers'
//! `[CLS][SEP]` pooling over "" produces a real unit-norm vector, and so
//! does our encoder (seq length 2, both tokens attended), so cosine
//! similarity between the two is meaningful rather than a 0/0 degenerate
//! case.

use majestical_index::encoder::cosine;
use majestical_index::model::{self, MINILM};
use majestical_index::text_encoder::TextEncoder;

const COSINE_FLOOR: f32 = 0.999;

#[test]
#[ignore = "needs fetched minilm model and MAJ_GOLDEN from golden.py"]
#[expect(
    clippy::cast_possible_truncation,
    reason = "golden f64 -> encoder f32, matching the model's own precision"
)]
fn rust_encoder_matches_sentence_transformers_reference() {
    let golden_path = std::env::var("MAJ_GOLDEN").expect("MAJ_GOLDEN env var");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("read golden"))
            .expect("parse golden");
    let fixtures = golden["fixtures"].as_array().expect("fixtures");
    let vectors = golden["vectors"].as_array().expect("vectors");
    let dir = model::model_dir_for(&MINILM).expect("model dir");
    let mut encoder = TextEncoder::load(&dir).expect("load");
    for (fixture, reference) in fixtures.iter().zip(vectors) {
        let text = fixture.as_str().expect("fixture text");
        let reference: Vec<f32> = reference
            .as_array()
            .expect("vector")
            .iter()
            .map(|v| v.as_f64().expect("f64") as f32)
            .collect();
        let ours = encoder.embed(text).expect("embed");
        let score = cosine(&ours, &reference);
        assert!(
            score >= COSINE_FLOOR,
            "cosine {score} < {COSINE_FLOOR} for fixture {text:?}"
        );
    }
}
