check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

ci: check test

# The GUI lives in its own cargo workspace (apps/desktop/src-tauri), so none of
# the recipes above ever compile it and none of these compile the headless one.
gui-install:
    cd apps/desktop && pnpm install --frozen-lockfile

# `pnpm build` is here rather than in gui-build because a debug `cargo build`
# never reads frontendDist — nothing else would catch a broken production
# bundle until the release job in phase 7B task 10.
gui-check:
    cd apps/desktop && pnpm check && pnpm lint && pnpm test && pnpm build

gui-build:
    cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml

version-sync:
    ./scripts/version-sync.sh

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

# Pinned commit of sentence-transformers/all-MiniLM-L6-v2 the Python
# reference (golden.py) loads. Must stay in sync with MINILM's revision in
# crates/index/src/model.rs — the reference and our fetch have to load the
# exact same weights for the conformance gate to mean anything.
MINILM_TORCH_REVISION := "1110a243fdf4706b3f48f1d95db1a4f5529b4d41"

# Text-encoder conformance: pinned sentence-transformers reference vs our
# ort MiniLM. Downloads ~90MB of model weights on first run.
text-encoder-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only minilm-l6-v2-v1
    uv run conformance/text-encoder/golden.py \
        --revision {{MINILM_TORCH_REVISION}} --out target/text-encoder-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/text-encoder-golden.json" \
        cargo test -p majestical-index --test text_encoder_conformance --test text_encoder_gated -- --ignored

# Pinned revision of the reference weights golden.py loads: `faster-whisper`'s
# "large-v3-turbo" alias resolves to dropbox-dash/faster-whisper-large-v3-turbo
# (formerly published as mobiuslabsgmbh/faster-whisper-large-v3-turbo, which
# now redirects there). Verified 2026-07-31 via the HF API's `sha` field — must
# match what golden.py loads; bump only after re-verifying.
WHISPER_TORCH_REVISION := "0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf"

# Whisper conformance: same synthesized speech through pinned faster-whisper
# (reference) and our whisper-rs, compared on WER + boundary drift.
whisper-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only whisper-large-v3-turbo-q5-v1
    mkdir -p target
    say -o target/whisper-fixture.aiff "The quick brown fox jumps over the lazy dog. \
        We reviewed the quarterly budget on Tuesday and shipped the release candidate."
    # 2s leading silence: both faster-whisper and whisper.cpp absorb it into
    # the first segment rather than reporting a nonzero start, so this alone
    # does not make the first-boundary assert catch a timestamp-scale bug —
    # see the module doc on whisper_conformance.rs for what actually does.
    ffmpeg -y -v error -i target/whisper-fixture.aiff -af "adelay=2000:all=1" -ar 16000 -ac 1 target/whisper-fixture.wav
    uv run conformance/whisper/golden.py \
        --revision {{WHISPER_TORCH_REVISION}} \
        --audio target/whisper-fixture.wav --out target/whisper-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_AUDIO="{{justfile_directory()}}/target/whisper-fixture.wav" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/whisper-golden.json" \
        cargo test -p majestical-index --test whisper_conformance --test whisper_gated -- --ignored --nocapture
