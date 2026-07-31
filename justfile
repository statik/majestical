check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

ci: check test

# Two-way ASC MHL conformance against the Python reference implementation.
# `uv venv` doesn't install a `pip` binary — install via `uv pip` targeting
# the venv's interpreter instead. Paths must be absolute: `cargo test` runs
# test binaries with the package directory (crates/ingest) as the working
# directory, not the workspace root the venv was created in.
conformance:
    uv venv --allow-existing .ascmhl-venv
    uv pip install --quiet --python .ascmhl-venv/bin/python ascmhl==1.2
    ASCMHL_BIN="{{justfile_directory()}}/.ascmhl-venv/bin/ascmhl" \
        ASCMHL_DEBUG_BIN="{{justfile_directory()}}/.ascmhl-venv/bin/ascmhl-debug" \
        cargo test -p majestical-ingest --test conformance -- --ignored

# Pinned commit of google/siglip2-base-patch16-256 the Python reference
# (golden.py) loads. Verified 2026-07-30 via the HF API's `sha` field — bump
# only after re-verifying, since it's the oracle every Rust encoder change
# is checked against.
SIGLIP2_TORCH_REVISION := "3f9f96cb90da5dbc758b01813f2f6f1aee24c1ab"

# Encoder conformance: fetches the pinned ONNX model, runs the pinned
# `transformers` reference to produce golden embeddings, then checks our
# Rust encoder (tokenizer, CPU vision/text towers, CoreML vision tower)
# against them. Downloads ~1GB of model weights on first run.
encoder-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only siglip2-b16-v1
    uv run conformance/encoder/golden.py \
        --revision {{SIGLIP2_TORCH_REVISION}} --out target/encoder-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/encoder-golden.json" \
        cargo test -p majestical-index --test encoder_conformance --test encoder_gated -- --ignored
