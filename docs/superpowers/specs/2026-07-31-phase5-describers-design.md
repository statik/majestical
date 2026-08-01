# Majestical Phase 5 — Describer backends + transcription, OCR, PDF, text search

Approved in the 2026-07-31 design session. Parent spec:
`2026-07-28-majestical-design.md` §4 (build-order step 5). Predecessor:
`2026-07-30-phase4-search-design.md`. Research grounding:
`docs/research/ai-stack.md` §1-4, §6.

Phase 5 is backend/pipeline only. MCP and GUI stay out of scope until
build-order step 7 (parent spec §6-§7).

## Scope decisions (from design session)

- **Full suggested scope**: describer backends, captions + tag suggestion,
  transcription, per-scene OCR, PDF extraction, and the query-model layer —
  one spec, chunked PRs.
- **Transcription engine**: whisper.cpp in-process via `whisper-rs`
  (Metal), not runtime Python. The parent spec's "faster-whisper" is
  honored as the CI conformance oracle, matching the phase-4 oracle-wins
  pattern. Default model `large-v3-turbo` q5 quant (~574MB), overridable.
- **OCR engine**: Apple Vision `VNRecognizeTextRequest` via `objc2`
  bindings — free, local, no model download. Results are versioned by the
  Vision request revision. OCR covers still images as well as video
  keyframes (session decision extending the handoff's video-only wording:
  same Vision call, and screenshots are a top search target).
- **Tag suggestions are derived blobs, never CRDT ops.** Confirming a
  suggestion emits a plain `TagAdd`. **Phase 5 adds no new op variants**;
  the wire format is untouched.
- **Describer config is per-machine**, in the existing per-catalog state
  dir. API keys never travel through the sync transport.
- **Hosted multimodal embeddings (OpenRouter quality tier) deferred.**
  OpenRouter participates only as a describer backend this phase. Blob
  keys already carry model tags, so nothing forecloses the tier later.
- **PDFs via PDFKit** (`objc2`, zero new runtime deps); PSD/Sketch/AI
  parsing deferred (many `.ai` files are PDF-compatible and open via
  PDFKit for free).
- **Transcript semantic vectors via in-process all-MiniLM-L6-v2** (384-d)
  on the existing ort stack. Captions/OCR/PDF text are FTS-only this
  phase; only transcripts get vectors.
- **Structure (Approach A)**: local derivers (whisper, Vision OCR, PDFKit,
  MiniLM) are new `crates/index` modules beside `encoder.rs`; the
  `Describer` port lives in `crates/core`; one new crate
  `crates/describe` implements it over HTTP.

## Architecture

### Describer port (`crates/core/src/ports.rs`)

Domain-level trait, outcome-shaped — no chat/completions types in core:

```rust
pub trait Describer {
    fn caption(&self, image: &[u8]) -> Result<Caption, DescribeError>;
    fn suggest_tags(
        &self,
        subject: TagSubject<'_>,
        existing_vocab: &[String],
    ) -> Result<Vec<TagSuggestion>, DescribeError>;
}

pub enum TagSubject<'a> {
    Image(&'a [u8]),          // stills: one call per asset
    Captions(&'a [String]),   // video: pooled keyframe captions, text-only call
}
```

`Caption` carries text + model tag. `TagSuggestion` carries tag, confidence,
whether it matched the existing folksonomy or is a new proposal, and model
provenance.

### `crates/describe` — one OpenAI-compatible adapter, three backends

`BackendKind::{Ollama, LmStudio, OpenRouter}` over one HTTP client
(`/v1/chat/completions` with image content), with per-backend quirks
captured explicitly:

- **Ollama**: images base64-only (URLs unsupported); default base URL
  `http://localhost:11434`.
- **LM Studio**: capability discovery via native `GET /api/v1/models`
  (`capabilities.vision` boolean) so `describer test` can prove the
  configured model can see images before any caption work is queued;
  default `http://localhost:1234`.
- **OpenRouter**: `Authorization: Bearer` key, default
  `https://openrouter.ai/api`.

Caption and tag-suggestion prompts request structured JSON; responses are
parsed strictly with one retry on malformed output. Request shaping is
pinned by dialect fixtures (see Testing) per the phase-4 lesson: each
provider's reference client output is the oracle before hand-rolling.

### Config — per-machine `describer.toml`

Lives in the per-catalog state dir, written `0600` (OpenRouter key).
Fields: `backend`, `base_url` (defaulted per backend), `model`, `api_key`
(OpenRouter only; `MAJ_OPENROUTER_KEY` env var overrides so the file can
stay keyless). macOS Keychain storage is a watchlist item, not phase 5.

CLI:

- `maj describer set --backend <kind> --model <m> [--base-url …] [--api-key …]`
- `maj describer show` — key redacted
- `maj describer test` — connectivity, model presence, vision capability
  (LM Studio); prints exactly what caption work will and won't run and why

### Local derivers — new `crates/index` modules

- `transcribe.rs` — whisper-rs (whisper.cpp, Metal). Input: mono 16kHz WAV
  extracted by ffmpeg subprocess **under a duration-scaled timeout** (this
  new call does not inherit the watchlist's no-timeout gap). Output:
  word-timestamped segments + full text. Language auto-detect. No
  diarization this phase.
- `ocr.rs` — Vision `VNRecognizeTextRequest`, accurate mode, via `objc2`.
  Empty results are stored ("no text" is an answer; otherwise the planner
  retries forever).
- `pdf.rs` — PDFKit via `objc2`: per-page text extraction + first-page
  1024px render. The render feeds the existing thumbnail + SigLIP
  embedding path unchanged, so PDFs join visual semantic search for free.
- `text_encoder.rs` — all-MiniLM-L6-v2 ONNX (384-d) via ort, L2-normalized
  at the encoder so Lance `Dot` = cosine (same invariant as SigLIP).

Whisper GGML and MiniLM ONNX join `maj model fetch` under the existing
`.model-cache` / `MAJ_MODEL_DIR` pattern, pinned by digest.

### Transcript chunking

Whisper segments group greedily into windows of ≤45 seconds AND ≤120
words, never splitting a segment. Each chunk gets one MiniLM vector.
Property-tested: chunks cover every segment exactly once, respect both
caps, never split a segment.

## Storage

### Blobs (sync root, `<aa>/<asset-hash>/`, zstd, keyed by derivation + model tag)

- `whisper-large-v3-turbo-q5-v1/transcript.json.zst` — segments with
  timecodes + full text
- `minilm-l6-v2-v1/chunk-<start-ms>.f32le.zst` — transcript-chunk vectors
  (mirrors the `kf-<ts>` convention)
- `applevision-r<rev>-v1/image.json.zst` (stills) and `kf-<ts-ms>.json.zst`
  (keyframes) — OCR lines with bounding info
- `pdfkit-v1/text.json.zst` — per-page text
- `<caption-model-tag>/caption.json.zst`, `tags.json.zst` — describer
  output; the tag embeds the backend model name, so captions from
  different machines' different models coexist honestly

Blobs remain the exchange format; both projections below are rebuildable
from blobs + events.

### SQLite

One new FTS5 table: `text_fts(asset_id, source, locator, content)` with
`source ∈ {transcript, caption, ocr, pdf}`. `locator` is a timestamp in ms
for transcript/OCR rows and a page number for PDF rows (named `locator`,
not `ts_ms`, so the schema is honest for both).

### Lance

A second table for 384-d text chunks: `(asset_hash, start_ms, end_ms,
source, vector)` — transcript chunks only this phase. Separate from the
768-d image table by construction: the parent spec's "separate indexes
merged at query time," made literal.

### Rejections (state dir, per-machine)

`tag-rejections.jsonl` — append-only, NOT synced, deliberately outside the
disposable SQLite so projection rebuilds don't resurrect rejected
suggestions. Synced rejections: watchlist.

## Commands & the queue

`work.rs`'s diff-as-queue learns the new derivation kinds, gated by media
kind:

- video/audio → transcript → transcript chunks (vectors)
- video keyframes + stills → OCR
- pdf → text + preview (preview then flows into thumb/embed)
- image assets + keyframes → caption + tags, **only when a describer is
  configured**. Stills: one caption call + one tag call per asset. Video:
  describer work is capped at ≤12 evenly-spaced keyframes per asset (VLM
  calls cost seconds each; 150 calls per clip is not a sane default), each
  captioned with its timestamp; tag suggestion runs once over the pooled
  caption set, not per frame. The cap is a named constant, and the
  per-video blob records which timestamps were described so the gap is
  auditable, not hidden (the keyframe-manifest pattern).

Priority: thumbnails → embeddings → transcription → OCR/PDF → captions.

Keyframe OCR planning diffs the existing keyframe manifest against OCR
blobs and re-extracts only missing frames by timestamp seek at full
resolution — no full-stream decode (sidesteps the watchlist's 600MB/hour
`analysis_frames` buffering for this pass).

`maj index run --kinds` extends to the new kinds; `maj index status`
reports per-derivation coverage from real row/blob counts (never cached
claims).

### Tag suggestion review

- `maj tags suggestions [query]` — pending suggestions with confidence,
  provenance, target asset; already-present and rejected tags filtered out
- `maj tags confirm <asset> <tag>…` — emits plain `TagAdd`; a confirmed
  tag is indistinguishable from a hand-added one in the log (provenance
  stays in the suggestion blob; the folksonomy stays clean)
- `maj tags reject <asset> <tag>…` — appends to `tag-rejections.jsonl`

## Query model

`fuse_ranked` generalizes from 2 to N ranked lists. Four inputs:

1. filename FTS (existing)
2. `text_fts` (new: transcripts/captions/OCR/PDF)
3. image-vector semantic (existing, SigLIP)
4. transcript-vector semantic (new: MiniLM-encoded query against the text
   table)

Hard filters intersect against **every** list — the phase-4 BLOCKER
(filter leak through fusion) is a named regression test from day one.

Syntax: new `in:transcript|caption|ocr|pdf|name` filter restricts which
text sources participate. Everything else (`tag:`, `vol:`, `para:`,
`kind:`, `online:`, `before:`/`after:`, `-` negation, `--save`/`--saved`)
unchanged.

Output: transcript and keyframe hits print `@0m07s`-style timestamps
(phase-4 convention); transcript hits add a one-line snippet of the
matching chunk. Result-count and truncation lines unchanged.

## Error handling

Search degrades, never errors; every degradation names its specific gap
and remedy:

- `captions: no describer configured (run maj describer set)`
- `transcripts: 12/40 videos (whisper model not fetched — run maj model fetch)`
- Vision/PDFKit per-asset failures: logged, counted in `index status`,
  never fatal to the run

Describer HTTP calls get a timeout + one retry with backoff; a mid-run
backend outage skips remaining caption work and reports the skipped count.

Per-asset deriver failures record a failure marker in the run report (not
a blob), so `index status` distinguishes "not yet" from "failed last
time" — and failures re-plan on the next run rather than silently dropping
out (learning from the keyframe-manifest gap on the watchlist).

## Testing

**Oracle-wins CI gates** (cached-model pattern; slow on cold cache):

- `whisper-conformance` — pinned `faster-whisper` reference vs whisper-rs
  on fixture audio; asserted on WER threshold + segment-boundary drift,
  not exact text.
- `text-encoder-conformance` — pinned `sentence-transformers` reference vs
  ort MiniLM; cosine floor ≥0.999 on fixtures including long/truncated
  inputs (phase 4 found bugs in the awkward cases, not the easy ones).
- Describer dialect fixtures — each backend's reference client output
  captured and replayed against a fake OpenAI-compatible server in unit
  tests; a `--ignored` live test runs against a real local Ollama when
  present.

**Deterministic goldens**: Vision OCR against ffmpeg `drawtext`-rendered
fixtures (assert substrings, not layout); PDFKit against small fixture
PDFs; wire-format goldens unchanged (this phase adds no ops — that
absence is itself asserted).

**Property tests**: chunking invariants (above); N-way `fuse_ranked`
(hard-filter intersection holds for every list count; reduces to phase-4
behavior at N=2).

**e2e proof points** (`--ignored`, real models):

- A generated clip with a spoken phrase (macOS `say` + ffmpeg) →
  `index run` → `search "<paraphrase>"` resolves the correct asset and
  timestamp via the semantic text path.
- A drawtext keyframe found via `in:ocr`.

**Close-out**: cucumber acceptance for the new search flows, cargo-mutants
triage, watchlist + handoff updates — the phase-4 closing discipline.

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. `Describer` port + `crates/describe` adapter + dialect fixtures +
   `maj describer set|show|test`
2. Model fetch extensions (whisper GGML, MiniLM ONNX) + `text_encoder.rs`
   + text-encoder-conformance CI job
3. `transcribe.rs` + whisper-conformance CI job + transcript blobs
4. Chunking + Lance text table + `text_fts` projection
5. `ocr.rs` (stills + keyframe re-extraction) + goldens
6. `pdf.rs` + preview-into-existing-pipeline + goldens
7. Queue integration (`work.rs` kinds, priorities, failure markers,
   `index status` coverage)
8. Captions + tag suggestions + `maj tags suggestions|confirm|reject`
9. Query layer: N-way fusion, `in:` filter, snippets, degradation notices
10. Closing: cucumber acceptance, mutants triage, watchlist/handoff/as-built

## Deferred (watchlist items with this spec's attribution)

- Hosted multimodal embeddings (OpenRouter quality tier; parallel vector
  space + cross-space fusion)
- Synced tag-suggestion rejections
- API keys in macOS Keychain
- PSD/Sketch native parsing
- Caption/OCR/PDF text vectors (FTS-only this phase)
- Diarization; translation; language forcing

## As-built deviations

Where execution diverged from the spec above, and why. Everything else
shipped as written.

### Query layer

- **Multi-term FTS is AND, not OR.** The spec did not say which; the
  phase 4 name search joined terms with `OR`. Both name and text search now
  join with `AND` through one shared builder,
  `fts_match_expr` (`crates/catalog-sqlite/src/query.rs:36`, used at `:60`
  and `:128`) — a two-word query returning everything matching *either*
  word is not what anyone means by search, and having the two surfaces
  disagree would have been worse than either choice.
- **Best-per-asset text dedupe happens client-side, not in SQL.** The spec
  implies one ranked row per asset out of `text_fts`. SQLite's `snippet()`
  is not usable under `GROUP BY` (it reads the current match's context,
  which a grouped row no longer has), so the query returns every matching
  row with its own snippet and the CLI keeps the best-ranked row per asset
  (`crates/cli/src/search.rs`, `TextHit` dedupe). The alternative — group in
  SQL and re-query for a snippet — costs a second statement per result.
- **Text score is the raw bm25 rank.** The plan's parenthetical suggested
  `-rank`, which contradicted itself (bm25 in SQLite is already
  negative-is-better). The stored/compared value is bm25's own output,
  matching `search_names_ranked`'s existing convention exactly rather than
  inventing a second one.
- **ANY `in:` restriction disables the image-vector layer**, not just
  `in:name` as the spec's syntax section implies. `image_semantic_enabled`
  (`crates/cli/src/search.rs:322-330`) turns image vectors off for every
  restricted query: the four text sources plus `name` are all *text*
  sources, and there is no `in:image` to ask for the image layer back.
  Reviewed and accepted rather than adding a speculative `in:image` with no
  requester.

### Derivation pipeline

- **Video caption frames reuse the 320px thumbnail edge.** The spec left
  the caption-frame resolution unstated; `caption_video_frame`
  (`crates/cli/src/index_cmd.rs:1676`) re-extracts at the existing
  `THUMB_EDGE`, so the caption path adds no new size constant and no new
  decode profile.
- **Caption/tags completion requires BOTH blobs, not caption-first.** The
  spec's ordering ("one caption call + one tag call per asset") implies a
  caption blob alone marks progress; the planner treats an asset as
  described only when both the `Caption`/`Captions` and `Tags` blobs exist,
  so an interrupted run between the two calls re-plans instead of stalling
  with a caption and no tags forever.
- **A completion-marker family was added that the spec's blob list does not
  mention**: `OcrComplete`, `ChunksComplete`, and `ChunksEmpty`
  (`crates/index/src/blob.rs:70-86`). The spec's per-item blobs
  (`kf-<ts>.json.zst`, `chunk-<start-ms>.f32le.zst`) cannot express "every
  item is done" — a planner would have to diff every keyframe timestamp or
  re-chunk the transcript on every status call, and a legitimately empty
  transcript would re-plan forever. `has_chunk_completion`
  (`crates/index/src/blob.rs:181`) makes the distinction explicit:
  individual chunk blobs are not completion, because they are written one
  at a time before the vector-store add.
- **Text vectors get a blob to Lance heal the spec did not call for.**
  `load_missing_text_vectors_from_blobs` (`crates/cli/src/index_cmd.rs:1184`)
  runs on every pass regardless of `--kinds`, mirroring the image path's
  always-on diff — a teammate's synced chunk vectors index locally without
  re-inference, and a deleted `lance/` directory rebuilds from blobs.

### Models and bindings

- **Model presence is one definition, not two.** The spec assumed the
  phase 4 `model_present` carried over; it was SigLIP-only. It became
  `model_present_for(spec, dir)` (`crates/index/src/model.rs:160`) before
  the whisper/MiniLM consumers could copy the hardcoded version — used by
  `search.rs:654` (SigLIP) and `:802` (MiniLM) and both conformance suites.
- **whisper-rs API adaptations.** `to_str_lossy()` in whisper-rs 0.16
  returns a `Result` rather than a `&str`
  (`crates/index/src/transcribe.rs:115`), and segment timestamps come back
  in centiseconds, not milliseconds — converted at the boundary and pinned
  by `centiseconds_convert_to_ms` (`:155`) so the blob format stays
  millisecond-based like every other locator in the system.
- **MiniLM's ONNX export has no pooled output.** The spec said "384-d via
  ort", implying a pooled vector comes out of the session; the export
  exposes `last_hidden_state` only, so mean-pooling over the attention mask
  is done in Rust (`mean_pool`, `crates/index/src/text_encoder.rs:123`)
  before the L2 normalize. This is exactly what the
  `sentence-transformers` oracle does internally, which is why the
  conformance floor holds.
- **PDFKit renders through a square box.** The spec said "first-page
  1024px render"; `-[PDFPage thumbnailOfSize:forBox:]` sizes to a box, so
  `render_first_page` (`crates/index/src/pdf.rs:102-130`) passes an
  `edge`x`edge` square, which puts the longest post-rotation edge at
  exactly `edge` for any page aspect ratio or rotation.

### Testing

- **OCR fixtures are rendered with ImageMagick, not ffmpeg `drawtext`.**
  The spec named `drawtext`; this machine's ffmpeg is built without
  freetype, so `magick -annotate` renders the text fixtures instead — for
  `crates/index/tests/fixtures/ocr-hello.png` (PR 5) and again for the e2e
  clip generator (`render_text_png`,
  `crates/cli/tests/phase5_e2e.rs:20`). The assertions are unchanged
  (substrings, not layout).
- **Python oracles pin `device="cpu"` explicitly.** The spec did not
  mention device selection. torch auto-selects MPS on Apple silicon, and
  the virtualized CI Metal stack silently corrupts embeddings — the
  text-encoder oracle measured cosine 0.738 against the Rust encoder
  instead of the expected ≥0.999, with no error anywhere. Every
  `SentenceTransformer`-based oracle now pins CPU. (`AutoModel`-based
  oracles such as the SigLIP `golden.py` are CPU-default and were never
  affected; the `faster-whisper` oracle already pinned CPU per plan.)

### CLI surface

- **Tag review is its own verb, `maj tags`, not a subcommand of `tag`.**
  The spec wrote `maj tags suggestions|confirm|reject` but the repo already
  had a singular `maj tag add|rm`; folding review under `tag` would have
  mixed a CRDT-writing verb with a per-machine review workflow that mostly
  writes nothing. `Cmd::Tags` (`crates/cli/src/main.rs:59`) is separate, and
  `tags confirm` emits a plain `TagAdd` exactly as specified.
