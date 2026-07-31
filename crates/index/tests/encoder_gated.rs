#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

//! Sanity tests requiring the fetched model:
//!   `MAJ_MODEL_DIR`=… cargo test -p majestical-index --test `encoder_gated` -- --ignored

use std::path::PathBuf;

use majestical_index::encoder::{Encoder, EncoderOptions, cosine};
use majestical_index::model;

fn require_model_dir() -> PathBuf {
    let dir = model::model_dir().expect("resolving model dir");
    assert!(
        model::model_present(&dir),
        "model not present at {}; run `maj model fetch`",
        dir.display()
    );
    dir
}

fn solid_color_image(rgb: [u8; 3]) -> image::RgbImage {
    let mut img = image::RgbImage::new(64, 64);
    for px in img.pixels_mut() {
        *px = image::Rgb(rgb);
    }
    img
}

#[test]
#[ignore = "needs fetched model"]
fn text_tokens_are_fixed_64_right_padded_with_eos() {
    let dir = require_model_dir();
    let encoder = Encoder::load_text_only(&dir).expect("load text-only encoder");
    let ids = encoder.token_ids("a photo of a beach").expect("tokenize");

    assert_eq!(ids.len(), 64);
    let last_nonzero = ids
        .iter()
        .rposition(|&id| id != 0)
        .expect("at least one nonzero id");
    assert_eq!(
        ids[last_nonzero], 1,
        "last nonzero token must be eos (id 1)"
    );
    assert!(
        ids[last_nonzero + 1..].iter().all(|&id| id == 0),
        "every token after eos must be pad (id 0)"
    );
}

#[test]
#[ignore = "needs fetched model"]
fn long_text_is_truncated_to_the_fixed_length() {
    let dir = require_model_dir();
    let encoder = Encoder::load_text_only(&dir).expect("load text-only encoder");
    // Without truncation configured, this tokenizes to 242 ids and
    // `token_ids` errors loudly (its fixed-length check) instead of
    // silently degrading — this test pins truncation staying configured.
    let long_text = "a photo of a beach at sunset with palm trees and waves ".repeat(6);

    let ids = encoder
        .token_ids(&long_text)
        .expect("long input must truncate, not error");

    assert_eq!(ids.len(), 64);
}

#[test]
#[ignore = "needs fetched model"]
fn matching_text_and_image_score_higher_than_mismatched() {
    let dir = require_model_dir();
    let mut encoder = Encoder::load(
        &dir,
        &EncoderOptions {
            coreml: false,
            coreml_cache: None,
        },
    )
    .expect("load encoder");

    let blue = solid_color_image([0, 0, 255]);
    let green = solid_color_image([0, 255, 0]);
    let blue_embedding = encoder.embed_image(&blue).expect("embed blue square");
    let green_embedding = encoder.embed_image(&green).expect("embed green square");
    let text_embedding = encoder
        .embed_text("a solid blue square")
        .expect("embed text");

    let blue_score = cosine(&text_embedding, &blue_embedding);
    let green_score = cosine(&text_embedding, &green_embedding);
    assert!(
        blue_score > green_score,
        "matching text/image pair should score higher: blue={blue_score}, green={green_score}"
    );
}

#[test]
#[ignore = "needs fetched model"]
fn text_only_encoder_rejects_images() {
    let dir = require_model_dir();
    let mut encoder = Encoder::load_text_only(&dir).expect("load text-only encoder");
    let img = solid_color_image([128, 128, 128]);

    let err = encoder
        .embed_image(&img)
        .expect_err("text-only encoder must reject image embedding");
    assert!(
        err.to_string().contains("text-only"),
        "error should explain why: {err}"
    );
}
