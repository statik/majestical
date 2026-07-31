#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! Sanity tests requiring the fetched `MiniLM` model:
//!   `MAJ_MODEL_DIR`=… cargo test -p majestical-index --test `text_encoder_gated` -- --ignored

use std::path::PathBuf;

use majestical_index::model::{self, MINILM};
use majestical_index::text_encoder::{TEXT_EMBED_DIM, TextEncoder};

fn require_model_dir() -> PathBuf {
    let dir = model::model_dir_for(&MINILM).expect("model dir");
    assert!(
        dir.join("model.onnx").is_file(),
        "run `maj model fetch --only minilm-l6-v2-v1` first"
    );
    dir
}

#[test]
#[ignore = "needs fetched minilm model"]
fn related_sentences_score_higher_than_unrelated() {
    let mut encoder = TextEncoder::load(&require_model_dir()).expect("load");
    let budget = encoder
        .embed("we discussed the quarterly budget and costs")
        .expect("embed");
    let money = encoder
        .embed("talking about spending money")
        .expect("embed");
    let cats = encoder
        .embed("a fluffy cat sleeping in the sun")
        .expect("embed");
    assert_eq!(budget.len(), TEXT_EMBED_DIM);
    let related = majestical_index::encoder::cosine(&budget, &money);
    let unrelated = majestical_index::encoder::cosine(&budget, &cats);
    assert!(
        related > unrelated + 0.3,
        "related {related} must beat unrelated {unrelated} by a wide margin \
         (measured ~0.905 vs ~0.078; a degenerate encoder must not pass by luck)"
    );
}
