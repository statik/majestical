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

# `tauri_parity`'s cross-binary test compares the GUI's search rows against
# `maj search --json`, so build a `maj` first and point MAJ_BIN at it —
# without one that test skips (loudly) and the rest still run.
gui-test:
    cargo build -p majestical-cli
    MAJ_BIN="{{justfile_directory()}}/target/debug/maj" \
        cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml

# The GUI workspace carries its own copy of the root lint table (a standalone
# workspace cannot inherit one), so it needs its own gate — `just check` above
# only ever sees the headless workspace.
gui-lint:
    cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml --all -- --check
    cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets -- -D warnings

# WebDriver launch smoke, against a debug .app bundle. macOS only (the
# embedded WebDriver provider's native support). Builds `maj` (the fixture
# catalog setup shells out to it) and the debug bundle before running.
gui-e2e:
    cargo build -p majestical-cli
    cd apps/desktop && pnpm tauri build --debug -b app --config src-tauri/tauri.e2e.conf.json
    cd apps/desktop/e2e && pnpm install --frozen-lockfile && pnpm check && pnpm test

# Regenerates the tray's four macOS template icons (monochrome, alpha-only —
# `tray.rs` loads them with `icon_as_template(true)`, so only the shape
# matters and RGB is ignored). `magick` the same way `phase5_e2e.rs` and
# `crates/index/tests/fixtures/ocr-hello.png` render text fixtures: this
# machine's ffmpeg has no `drawtext` support. Run whenever a glyph changes;
# the PNGs are committed, so this is not part of any build or CI job. Byte-
# stable: `png:exclude-chunks=date,time` strips the only per-run difference
# ImageMagick otherwise writes, so running this twice with no glyph change
# leaves `git status` clean (verified: two runs, `cmp` identical).
TRAY_ICON_FONT := "/System/Library/Fonts/Supplemental/Arial Bold.ttf"
tray-icons:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="apps/desktop/src-tauri/icons/tray"
    mkdir -p "$dir"
    font="{{TRAY_ICON_FONT}}"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    strip=(-define png:exclude-chunks=date,time)
    render() {
        # size stroke rect_from rect_to radius pointsize outfile
        local size=$1 stroke=$2 from=$3 to=$4 radius=$5 pointsize=$6 out=$7
        magick -size "${size}x${size}" xc:none \
            -fill none -stroke black -strokewidth "$stroke" \
            -draw "roundrectangle $from $to $radius,$radius" \
            -fill black -stroke none -font "$font" -pointsize "$pointsize" \
            -gravity center -annotate +0+0 "M" \
            "${strip[@]}" "$dir/$out"
    }
    render_dashed() {
        local size=$1 stroke=$2 from=$3 to=$4 radius=$5 dash=$6 pointsize=$7 out=$8
        magick -size "${size}x${size}" xc:none \
            -fill none -stroke black -strokewidth "$stroke" \
            -draw "stroke-dasharray $dash roundrectangle $from $to $radius,$radius" \
            -fill black -stroke none -font "$font" -pointsize "$pointsize" \
            -gravity center -annotate +0+0 "M" \
            "${strip[@]}" "$dir/$out"
    }
    render_knockout() {
        # A filled rounded square with the glyph cut out (`DstOut`): the
        # glyph area is fully transparent, not just white — a template
        # icon's RGB is ignored, only alpha marks the shape. The two
        # intermediate layers live in $tmp, not $dir, so a failed run
        # never leaves scratch files next to the committed PNGs.
        local size=$1 from=$2 to=$3 radius=$4 pointsize=$5 out=$6
        magick -size "${size}x${size}" xc:none -fill black -stroke none \
            -draw "roundrectangle $from $to $radius,$radius" \
            "${strip[@]}" "$tmp/shape.png"
        magick -size "${size}x${size}" xc:none -fill black -stroke none \
            -font "$font" -pointsize "$pointsize" -gravity center \
            -annotate +0+0 "M" "${strip[@]}" "$tmp/glyph.png"
        magick "$tmp/shape.png" "$tmp/glyph.png" -compose DstOut -composite \
            "${strip[@]}" "$dir/$out"
    }
    render_attention() {
        # The idle look plus a small solid dot badge at the top right —
        # template icons are monochrome, so this stands in for the
        # mockup's amber tint (see tray.rs's as-built note).
        local size=$1 stroke=$2 from=$3 to=$4 radius=$5 pointsize=$6 cx=$7 cy=$8 r=$9 out=${10}
        magick -size "${size}x${size}" xc:none \
            -fill none -stroke black -strokewidth "$stroke" \
            -draw "roundrectangle $from $to $radius,$radius" \
            -fill black -stroke none -font "$font" -pointsize "$pointsize" \
            -gravity center -annotate +0+0 "M" \
            -fill black -stroke none -draw "circle $cx,$cy $((cx + r)),$cy" \
            "${strip[@]}" "$dir/$out"
    }
    render         22 1.5 "2,2" "19,19" 4 11 idle.png
    render         44 3   "4,4" "39,39" 8 22 idle@2x.png
    render_dashed  22 1.5 "2,2" "19,19" 4 "2,2" 11 paused.png
    render_dashed  44 3   "4,4" "39,39" 8 "4,4" 22 paused@2x.png
    render_knockout 22 "2,2" "19,19" 4 11 indexing.png
    render_knockout 44 "4,4" "39,39" 8 22 indexing@2x.png
    render_attention 22 1.5 "2,2" "19,19" 4 11 17 4 2 attention.png
    render_attention 44 3   "4,4" "39,39" 8 22 34 8 4 attention@2x.png

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

# Whisper conformance: same committed speech fixture through pinned
# faster-whisper (reference) and our whisper-rs, compared on WER + boundary
# drift. CI never synthesizes audio — see `whisper-fixture` below for that.
whisper-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch --only whisper-large-v3-turbo-q5-v1
    mkdir -p target
    uv run conformance/whisper/golden.py \
        --revision {{WHISPER_TORCH_REVISION}} \
        --audio conformance/whisper/fixture.wav --out target/whisper-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_AUDIO="{{justfile_directory()}}/conformance/whisper/fixture.wav" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/whisper-golden.json" \
        cargo test -p majestical-index --test whisper_conformance --test whisper_gated -- --ignored --nocapture

# Regenerates the committed whisper fixture from macOS `say`. Refuses a
# silent result (a flake `say` produces on headless runners — CI runs
# 34882576400 and 34923609907 are the recorded instances), so the
# committed file can never be the silent one. CI never runs this; it
# reads the committed file.
whisper-fixture:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    say -o "$tmp/fixture.aiff" "The quick brown fox jumps over the lazy dog. \
        We reviewed the quarterly budget on Tuesday and shipped the release candidate."
    # 2s leading silence — see whisper_conformance.rs's module doc.
    ffmpeg -y -v error -i "$tmp/fixture.aiff" -af "adelay=2000:all=1" -ar 16000 -ac 1 "$tmp/fixture.wav"
    peak=$(ffmpeg -v info -i "$tmp/fixture.wav" -af volumedetect -f null - 2>&1 \
        | sed -n 's/.*max_volume: \(-\{0,1\}[0-9.]*\) dB.*/\1/p')
    if [ -z "$peak" ] || awk -v p="$peak" 'BEGIN { exit !(p < -60) }'; then
        echo "whisper-fixture: synthesized audio is silent (peak ${peak:-unknown} dB) — not written" >&2
        exit 1
    fi
    mv "$tmp/fixture.wav" conformance/whisper/fixture.wav
    echo "wrote conformance/whisper/fixture.wav (peak ${peak} dB)"
