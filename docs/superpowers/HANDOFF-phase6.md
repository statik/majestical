# Majestical — Phase 6 handoff

Written 2026-08-01 at the close of Phase 5. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase5.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP with full GUI parity).

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3 spec + as-built deviations:
  `docs/superpowers/specs/2026-07-29-phase3-ingest-design.md`
- Phase 4 spec + as-built deviations:
  `docs/superpowers/specs/2026-07-30-phase4-search-design.md`
- Phase 5 spec + as-built deviations:
  `docs/superpowers/specs/2026-07-31-phase5-describers-design.md`
- Competitive/technical research: `docs/research/` — `ai-stack.md` §1-4, §6
  cover the describer-server landscape phase 5 built against
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #53, this closing PR pending)

**Shipped and working** (`maj` CLI, exercised end to end on a real machine):

- Phases 1-4 (see `HANDOFF-phase5.md` for detail): `catalog init`, `scan`,
  `tag add|rm`, `meta set|get`, `volumes list`, `para add|list|rename|archive`,
  `maj ingest` (multi-destination verified copy), `maj verify` (ASC MHL,
  conformance-gated both directions), the per-machine state dir, incremental
  catalog apply, `maj search` with a unified query language + saved searches,
  the blob store + thumbnails + `maj index run|status` diff-as-queue,
  `maj model fetch` + the SigLIP 2 encoder behind a conformance gate, the
  LanceDB vector store + reciprocal-rank fusion, and scene-detected video
  keyframes with timestamped hits.
- Phase 5 (PRs #43-#53 + this closing PR):
  - `Describer` port in `crates/core/src/ports.rs` + the new
    `crates/describe` crate: one OpenAI-compatible HTTP adapter over
    `BackendKind::{Ollama, LmStudio, OpenRouter}`, per-backend quirks
    (Ollama base64-only images, LM Studio `GET /api/v1/models` vision
    capability discovery, OpenRouter bearer key) pinned by replayed dialect
    fixtures, plus `maj describer set|show|test` and a per-machine
    `describer.toml` written 0600 (PR #43).
  - Model registry generalized past SigLIP: whisper GGML and MiniLM ONNX
    join `maj model fetch` under the same `.model-cache`/`MAJ_MODEL_DIR`
    digest-pinned pattern, `crates/index/src/text_encoder.rs` (all-MiniLM-L6-v2,
    384-d, L2-normalized), and a `text-encoder-conformance` CI job against a
    pinned `sentence-transformers` reference (PR #44).
  - `crates/index/src/transcribe.rs`: whisper.cpp in-process via `whisper-rs`
    (Metal), fed by an ffmpeg mono-16kHz extraction under a duration-scaled
    timeout, plus a `whisper-conformance` CI job against a pinned
    `faster-whisper` reference asserted on WER + segment-boundary drift
    (PR #46).
  - `crates/index/src/chunk.rs`: whisper segments group greedily into
    windows of ≤45s AND ≤120 words, never splitting a segment
    (property-tested); each chunk gets one MiniLM vector in a second,
    384-d Lance table, and every text source lands in the new `text_fts`
    SQLite table (PR #47).
  - `crates/index/src/ocr.rs`: Apple Vision `VNRecognizeTextRequest` via
    `objc2`, accurate mode, for stills and video keyframes; empty results
    are stored, because "no text" is an answer (PR #48).
  - `crates/index/src/pdf.rs`: PDFKit via `objc2` — per-page text plus a
    first-page render that feeds the existing thumbnail + SigLIP path, so
    PDFs join visual semantic search for free. `MediaKind` gained `Audio`
    and `Pdf` and became a single extension table (PR #50).
  - Queue integration: `work.rs`'s diff-as-queue learned every new
    derivation kind gated by media kind, the
    thumbnails→embeddings→transcription→OCR/PDF→captions priority,
    per-asset failure markers in `index-failures.json`, and per-derivation
    coverage in `maj index status` counted from real rows and blobs
    (PR #51).
  - Captions + tag suggestions: stills get one caption + one tag call;
    video captions are capped at ≤12 evenly-spaced keyframes with the
    described timestamps recorded in the blob, and tag suggestion runs once
    over the pooled caption set. `maj tags suggestions|confirm|reject` —
    confirm emits a plain `TagAdd` (no new op variants this phase), reject
    appends to the never-synced `tag-rejections.jsonl` (PR #52).
  - Layered text search: `fuse_ranked` generalized from 2 to N ranked lists
    (filename FTS, `text_fts`, image vectors, transcript vectors), the
    `in:transcript|caption|ocr|pdf|name` filter, per-hit snippets and
    `@0m07s`/`p<page>` locators, and per-source degradation notices that
    name the specific gap and its remedy (PR #53).
  - Closing: cucumber acceptance for the layered text flows
    (`crates/cli/tests/features/text_search.feature`), the three e2e proof
    points below, cargo-mutants triage (see the watchlist's
    "cargo-mutants triage (phase 5)" subsection), this handoff.

**e2e proof points** (`crates/cli/tests/phase5_e2e.rs`, `--ignored`, real
models, real ffmpeg, real Vision):

- `semantic_transcript_search_resolves_paraphrase_with_timestamp` (`:52`) —
  macOS `say` synthesizes ~47s of filler followed by a payload sentence about
  "the quarterly budget and the cost overruns"; the clip is scanned,
  transcribed and chunk-embedded through the real CLI, and
  `search "talking about spending money in:transcript"` — a query sharing no
  vocabulary with the transcript — resolves `meeting.wav` with the payload
  snippet at a locator inside the speech, not `@0m00s`. The control rerun
  points `MAJ_MODEL_DIR` at an empty directory and asserts `0 results`,
  proving the hit is load-bearing on the MiniLM path rather than an accidental
  word overlap.
- `keyframe_ocr_text_found_via_in_ocr` (`:165`) — "SCENE 42 TAKE 7" rendered
  into a 4s clip, run through real scene detection and real Vision OCR (no
  hand-planted manifests or blobs), found by `search "scene 42 in:ocr"`.
- `video_captions_describe_real_keyframes` (`:218`) — real keyframe
  extraction and re-extraction through ffmpeg, captioned through a mock
  OpenAI-compatible backend; asserts `captions: 1 written`, exactly one
  `captions.json.zst` blob under the sync root whose `described` rows all
  carry the mocked caption text, and that the mock received at least one real
  caption call. This closes the gap PR 8 left, where the only coverage ran
  against a hand-planted `KeyframeManifest`.

**Architecture** (Cargo workspace, edition 2024, strict clippy; `just ci` =
the CI gate, `just conformance` / `just encoder-conformance` /
`just text-encoder-conformance` / `just whisper-conformance` = the four
oracle gates):

- `crates/core` — hexagon: `clock.rs`, `event.rs` (op variants + golden wire
  tests — **unchanged this phase; phase 5 added no op variants, and that
  absence is asserted**), `projection.rs`, `ports.rs` (gained the
  `Describer` port, `Caption`, `TagSubject`, `TagSuggestion`,
  `DescribeError`), `media_kind.rs` (gained `Audio`/`Pdf` and a single
  `EXTENSIONS` table).
- `crates/sync` — file event log; `read_all_reporting`/`read_since_reporting`
  (unification still deferred, see watchlist). Untouched this phase — and the
  crate phase 6 will live in.
- `crates/catalog-sqlite` — `schema.rs`/`apply.rs`/`query.rs`; disposable
  projection, incremental apply state, `names_fts`, and new this phase
  `text_fts(content, asset, source, locator)` with
  `source ∈ {transcript, caption, ocr, pdf}` plus the shared
  `fts_match_expr` term builder.
- `crates/ingest` — unchanged this phase.
- `crates/describe` — **new this phase**: `client.rs` (one
  `/v1/chat/completions` adapter over three backends, strict JSON parsing
  with one retry, timeout + backoff), `config.rs` (`describer.toml`,
  0600, `MAJ_OPENROUTER_KEY` override).
- `crates/index` — gained `transcribe.rs`, `chunk.rs`, `ocr.rs`, `pdf.rs`,
  `text_encoder.rs`, a `TextVectorStore` alongside the image store in
  `vector_store.rs`, `run_with_timeout`/`audio_timeout` in `video.rs`, and
  ten new `Derivation` variants in `blob.rs`. Existing: `blob.rs`,
  `thumbs.rs`, `resize.rs`, `preprocess.rs`, `model.rs`, `encoder.rs`,
  `vector_store.rs`, `video.rs`, `work.rs`, `error.rs`.
- `crates/cli` — `maj`; new `describer_cmd.rs` (`describer set|show|test`)
  and `tags_cmd.rs` (`tags suggestions|confirm|reject`); `search.rs` grew
  the N-way fusion, `in:` source selection, text snippets/locators and
  per-source coverage notices; `index_cmd.rs` grew every new derivation
  runner, the failure-marker file, and the text blob↔Lance heal.
- CI: SHA-pinned actions, `persist-credentials: false`, and six jobs in
  `.github/workflows/ci.yml` — `rust` (checks and tests),
  `mhl-conformance`, `encoder-conformance`, `text-encoder-conformance`,
  `whisper-conformance`, `actions-lint` (actionlint + zizmor). prek hook =
  `just check`.

**Blobs layout** (sync root, dumb convergent files), `blobs/<aa>/<asset-hex>/`
then, per derivation (`crates/index/src/blob.rs:18-86` is authoritative):

| derivation | path under the asset dir |
| --- | --- |
| `Thumb` | `thumb-320.webp` |
| `ImageEmbedding` | `<model_tag>/image.f32le.zst` |
| `KeyframeEmbedding` | `<model_tag>/kf-<ts-ms>.f32le.zst` |
| `KeyframeManifest` | `<model_tag>/keyframes.json` |
| `Transcript` | `<model_tag>/transcript.json.zst` |
| `TranscriptChunk` | `<model_tag>/chunk-<start-ms>.f32le.zst` |
| `OcrImage` | `<model_tag>/image.json.zst` |
| `OcrKeyframe` | `<model_tag>/kf-<ts-ms>.json.zst` |
| `PdfText` | `<model_tag>/text.json.zst` |
| `Caption` | `<model_tag>/caption.json.zst` |
| `Captions` | `<model_tag>/captions.json.zst` |
| `Tags` | `<model_tag>/tags.json.zst` |
| `OcrComplete` | `<model_tag>/ocr-complete.json` |
| `ChunksEmpty` | `<model_tag>/chunks-empty.json` |
| `ChunksComplete` | `<model_tag>/chunks-complete.json` |

Model tags in use: `siglip2-b16-v1`, `whisper-large-v3-turbo-q5-v1`,
`minilm-l6-v2-v1`, `applevision-r<rev>-v1`, `pdfkit-v1`, and
`describe-<backend-model>` for describer output — so captions from different
machines' different models coexist honestly rather than overwriting each
other. Blobs are addressed by derivation key (asset hash + kind + model tag),
not content hash, so writes are idempotent and two machines deriving the same
asset converge by construction. The last three rows are completion markers,
not data: they exist because per-item blobs cannot express "every item is
done" (see the spec's as-built deviations).

**State-dir layout** (per-machine, per-catalog, outside the sync root):
`~/Library/Application Support/majestical/catalogs/<catalog-key>/` where
`catalog-key` = xxh3-128 hex of the canonicalized sync-root path
(`MAJ_STATE_DIR` overrides the base for tests/CI). Contents: `catalog.db`
(SQLite projection + FTS5 + `text_fts` + `apply_state`), `lance/` (both
vector datasets), `runs/<run-id>.jsonl` (ingest journals), and new this
phase — `describer.toml` (backend config, 0600, never synced),
`tag-rejections.jsonl` (append-only per-machine suppression list,
deliberately outside the disposable SQLite so projection rebuilds cannot
resurrect a rejected suggestion), and `index-failures.json` (per-asset
deriver failure markers, so `index status` distinguishes "not yet" from
"failed last time" and failures re-plan rather than dropping out).

**SQLite + Lance pairing**: SQLite stays the relational projection plus both
FTS tables; LanceDB is vectors only, now two tables — 768-d image/keyframe
and 384-d transcript chunks, separate by construction (the parent spec's
"separate indexes merged at query time", made literal). LanceDB's per-table
ACID model and lack of multi-table joins make it wrong for the projection;
SQLite's brute-force vector story is exactly what LanceDB replaces. Lance is
per-machine local because the sync transport is dumb files — two writers
through Dropbox would corrupt a Lance dataset directory; blobs are the actual
exchange format, and both SQLite and Lance are disposable projections rebuilt
from them.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` — open items plus
"Phase 3 deferrals", "Phase 4 deferrals", a `cargo-mutants triage (phase 4)`
section, a new "Phase 5 deferrals" section (the spec's six explicit deferrals
— hosted multimodal embeddings, synced rejections, Keychain keys, PSD/Sketch
parsing, caption/OCR/PDF vectors, diarization/translation/language forcing —
plus the execution findings: the remaining untimed ffmpeg calls and the
`try_wait` leak, the `whisper_gated` `say` flake and its hardening, the
locked-PDF fixture waiting on `qpdf`, the unimplemented
`tags suggestions [query]` argument, `PortError` opacity, per-item outage
rows, the repeated projection scans in search, the `--json` locator sentinel,
and the stale `semantic_tests` module name), a
`cargo-mutants triage (phase 5)` section, and a "Done in phase 5" section
(the one-place media-kind extension table and its missing extensions, the
ffmpeg timeout for the new audio path only, and the gated video-caption
e2e).

## Phase 6 recommendation

Per the parent spec's build order, step 6: **Sync + inbox contributions**
(parent spec §5). This is the first phase where a second machine exists, and
everything phases 1-5 built was shaped for it — events are immutable and
commutative, blobs are derivation-keyed and idempotent, and SQLite/Lance are
already per-machine and disposable. Suggested scope for the design session:

- **Sync locations** as a configured list (NAS share, Dropbox/iCloud folder,
  or a shuttle drive carrying its own sync root). `crates/sync` already
  reads and writes the segmented event log; what does not exist is the
  push/pull orchestration across multiple locations, the location config
  itself, or `maj sync push|pull|status` (parent spec §6 names all three).
- **Push own segments, pull others', merge idempotently.** The projection is
  already commutative + idempotent and property-tested, including incremental
  apply ≡ full rebuild — reuse those properties as the sync acceptance
  criteria rather than inventing new ones. A shuttle drive plugged in at both
  sites must converge both.
- **Lazy, prioritized blob sync — thumbnails first** — so teammates browse
  and search volumes they never mounted. Phase 5 multiplied the blob
  inventory from 4 derivations to 15, several of them small JSON (captions,
  OCR, tags, the three completion markers) and two of them large
  (transcripts, and vectors at 768-d/384-d per item), so a priority policy
  now has real cost differences to reason about. `index status`'s
  per-derivation coverage counting is the natural place to surface what a
  teammate has versus what you have.
- **Read-only members simply never push** — events already carry author
  identity (`--author`/`MAJ_AUTHOR`, phase 2), so this is a policy on the
  push side, not a new data concept.
- **Inbox contributions**: a contribution = a folder of media plus
  `contribution.json` (contributor, capture context, client-computed
  xxHash64s, intended PARA target). An inbox watcher validates the hashes,
  runs the existing verified ingest (`crates/ingest` is done and MHL
  conformance-gated), files into PARA, and tags provenance
  (`contributor/dana`, `source/iphone`) through the existing OR-Set. Day one
  is a shared iCloud/Dropbox folder plus a share-sheet Shortcut that
  generates the manifest — no app. Manifest-less drops still ingest, hashed
  on arrival and filed to a triage node.
- **Expect to touch the sync-crate watchlist items.** `read_all_reporting`
  and `read_since_reporting` diverge in walk and UTF-8 handling
  (`crates/sync/src/lib.rs:152-169`, `:283-301`), `LogError::Io` is
  hand-built at 13 call sites, and the zero-padded `NNNN` segment-rotation
  constraint becomes load-bearing the moment a second machine's segments
  interleave. Read those three before designing.
- **Keep MCP and GUI phasing per parent spec §6-§7** — both remain out of
  scope until build-order step 7.

Write a phase 6 spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial spec-compliance
   reviewer (probes empirically, mutation-tests claims) → code-quality
   reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 5 ran ten chunk PRs (#43-#53)
   on this cadence.
3. **NO Claude-Session trailers in commit messages** (user mandate).
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain git).
5. Shared checkout: implementers stage ONLY their files, never `git add -A`.
   Parallel work needs a git worktree.
6. Shell: variables do NOT persist across Bash invocations. `trash`, never
   `rm -rf`.
7. Git auth: SSH unavailable; push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings; verify current versions of deps/actions at execution time.
   CI's clippy can be NEWER than local — expect drift and fix forward.
9. Reviewers' findings get fixed in the same chunk when cheap; deferred items
   go on the watchlist with attribution.
10. **Local setup**: `just`, `protoc` (`brew install protobuf`, Lance's build
    dependency), `ffmpeg`/`ffprobe`, and — new for phase 5's fixtures —
    ImageMagick (`magick`) on PATH, because this machine's ffmpeg is built
    without freetype and has no `drawtext`. Model artifacts follow the
    `MAJ_MODEL_DIR`/`.model-cache` pattern; the cache is now ~2GB fetched
    (SigLIP 2 + whisper `large-v3-turbo` q5 at ~574MB + MiniLM), keyed in CI
    on fixed model-id strings so it survives across runs. All four
    conformance jobs are slow on a cold cache — they download pinned
    `ascmhl`/`transformers`/`sentence-transformers`/`faster-whisper`
    references plus weights, not just compile Rust. Vision OCR and PDFKit
    need no download at all.

## Phase-5 lessons worth carrying

- **Pin the device in every Python oracle.** torch auto-selects MPS on Apple
  silicon, and the virtualized CI Metal stack silently corrupts embeddings —
  the text-encoder oracle reported cosine 0.738 against a Rust encoder that
  was in fact correct, with no error, no warning, and a conformance job that
  looked like a real regression. `SentenceTransformer` oracles must pass
  `device="cpu"` explicitly; `AutoModel`-based ones (the SigLIP `golden.py`)
  are CPU-default and were never affected. The general form: an oracle that
  can silently pick a different backend than the thing under test is not an
  oracle. Check what device or execution provider *both* sides ran on before
  believing a conformance failure.
- **Sabotage probes are how you find a vacuous assertion.** Two this phase.
  The e2e paraphrase test's `0 results` control (point `MAJ_MODEL_DIR` at an
  empty directory, rerun the identical query) is the only thing separating
  "the semantic path found this" from "FTS happened to overlap a word" —
  without it the test passes either way and proves nothing. And the vector
  store's empty-add short-circuit could not be discriminated by row count at
  all, because Lance versions every write that reaches the table including an
  empty one; pinning `table.version()` across the call
  (`crates/index/src/vector_store.rs:1150-1157`) is what actually catches a
  deleted guard. Ask of every new assertion: what would still pass it?
- **A multi-artifact derivation needs an explicit completion marker.** Per-item
  blobs (`kf-<ts>.json.zst`, `chunk-<start-ms>.f32le.zst`) cannot express
  "every item is done", and the two failure modes are both bad: a planner
  that re-diffs every timestamp on every status call, or one that treats a
  legitimately empty result as never-done and retries forever.
  `OcrComplete`/`ChunksComplete`/`ChunksEmpty`
  (`crates/index/src/blob.rs:70-86`) solve both, and
  `has_chunk_completion` (`:181`) documents the rule that individual item
  blobs deliberately do *not* count — they are written one at a time before
  the store add, so their presence alone means an interrupted run. Phase 6
  will want the same shape for "this location is fully pushed".
- **Diff-as-queue converges over passes, and that is the design, not a
  bug.** The plan is a snapshot of (required derivations) minus (present
  blobs), built once at the start of a run — so a transcript produced
  mid-run is not re-planned for chunk embedding in the same pass, and a
  keyframe manifest written mid-run does not get OCR'd until the next one.
  Both e2e proof points run two `index run` passes for exactly this reason
  (`crates/cli/tests/phase5_e2e.rs:114-125`, `:189-201`). Do not "fix" it by
  re-planning mid-run; state it, and let the next pass converge. Phase 6 adds
  a third producer of blobs — a teammate — under the same rule.
- **Quarantine `objc2` unsafety behind small safe functions.** Both macOS
  bindings (`crates/index/src/ocr.rs`, `crates/index/src/pdf.rs`) wrap every
  message send in a narrow safe Rust function carrying a `SAFETY` comment
  that names the selector and why the call is sound — e.g. `-[PDFPage
  thumbnailOfSize:forBox:]` rendering offscreen into a fresh bitmap
  (`pdf.rs:130`), `-[PDFDocument isLocked]` reading a BOOL property
  (`pdf.rs:61`). The rest of the crate stays safe Rust, and the entire unsafe
  surface is auditable by grepping two files. Vision and PDFKit cost zero
  runtime dependencies and zero download, which is why this was worth doing
  at all.
- **Adversarial review kept earning its keep against real defect classes.**
  Model-presence was still SigLIP-only when the whisper and MiniLM consumers
  were about to copy it — `model_present_for(spec, dir)`
  (`crates/index/src/model.rs:160`) landed before three call sites
  hardcoded the wrong check. `WHISPER_MODEL_TAG`/`MODEL_FILE` were derived
  from the registry consts (`crates/index/src/transcribe.rs:12-15`) rather
  than re-typed, closing a drift the reviewer named before it drifted.
  Whitespace-only whisper segments were reaching the embedder as empty
  chunks until a review caught it (`crates/cli/src/index_cmd.rs:1065`). The
  vector-store empty-add gap above was found while reviewing the *text*
  store's tests, in code nobody had changed. And the plan's own e2e sketch
  was wrong in two ways only empirical probing found: `maj search` takes one
  quoted positional (the sketch passed `in:transcript` as a second argument,
  which clap rejects), and whisper folds leading silence into segment 1, so
  a silence-led fixture reports `@0m00s` for the payload and the intended
  assertion cannot hold — the fixture became ~47s of filler followed by the
  payload so the chunker starts a new chunk at the payload's own boundary.
  None of these surface from a happy-path read-through.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite is disposable. Derived data
  (vectors, thumbnails, keyframes, transcripts, OCR, captions) must also be
  disposable and derivation-addressed — blobs are the actual exchange format;
  Lance and SQLite are both rebuildable projections of blobs + events.
- Apply is commutative + idempotent (property-tested), including incremental
  apply ≡ full rebuild over random cursor splits. New op variants must keep
  order independence and extend the proptest generator.
- Wire format is pinned by golden tests — additive changes only. Phase 5
  added **no** op variants (tag suggestions are derived blobs; confirming one
  emits a plain `TagAdd`), and that absence is itself asserted.
- Vectors are L2-normalized at the encoder (`crates/index/src/encoder.rs`,
  `crates/index/src/text_encoder.rs`) so Lance's `Dot` distance equals cosine
  similarity — never store a non-normalized vector. The two tables have
  different dimensions (768 and 384) and must never be merged.
- Never lie about data safety or index completeness: search must degrade,
  never error, when a model, a describer, or ffmpeg is absent, or an index is
  partial — and the degradation notice must name the specific gap and its
  remedy (which coverage count, which missing tool, which command to run),
  never a generic "unavailable."
- Never silently truncate: a result-count line accompanies every truncated
  output; coverage numbers come from counting real rows/blobs, never cached
  claims.
- A per-asset deriver failure is recorded, not swallowed, and re-plans on the
  next run — `index status` must distinguish "not yet" from "failed last
  time".
- Tests must discriminate: reviewers mutation-test; write tests whose failure
  modes are loud, and whose passing requires the behavior under test.
