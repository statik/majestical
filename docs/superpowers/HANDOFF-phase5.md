# Majestical — Phase 5 handoff

Written 2026-07-31 at the close of Phase 4. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase4.md` (kept for history).

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
- Competitive/technical research: `docs/research/` — `ai-stack.md` §1-3 cover
  the Ollama/LM Studio/OpenRouter server landscape phase 5 needs
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #40, this closing PR pending)

**Shipped and working** (`maj` CLI, exercised end to end on a real machine):

- Phases 1-3 (see `HANDOFF-phase4.md` for detail): `catalog init`, `scan`,
  `tag add|rm`, `meta set|get`, `volumes list`, `para add|list|rename|archive`,
  `maj ingest` (multi-destination verified copy), `maj verify` (ASC MHL,
  conformance-gated against the Python reference both directions).
- Phase 4 (PRs #32-#40 + this closing PR):
  - `maj catalog init`/every command now resolves local state (SQLite,
    Lance, journals) into a per-machine, per-catalog Application Support
    directory instead of the sync root; legacy layouts migrate on first
    open (PR #33).
  - Incremental catalog apply: `open_synced` resumes from a saved
    projection snapshot + per-segment cursors instead of replaying the
    whole event log on every open, with a property test asserting
    incremental apply ≡ full rebuild (PR #34).
  - `maj search "<query>"`: a unified query language (bare terms → FTS5
    basename word-prefix + bm25; `key:value` hard filters — `tag:`,
    `vol:`/`volume:`, `para:`, `kind:`, `online:`, `before:`/`after:`;
    `-` negation) replaced `--name`/`--tag` outright and closed the
    ASCII-only case-insensitivity watchlist item (PR #35).
  - `maj search --save/--saved`, `maj searches list|rm` — saved searches as
    CRDT ops (`SavedSearchSet`/`SavedSearchRemove`), synced like every other
    piece of organizational opinion (PR #36).
  - `crates/index`: content-addressed blob store (idempotent temp+rename,
    zstd), thumbnailer (320px WebP), and `maj index run|status` working a
    diff-as-queue — no stored queue, just (required derivations) minus
    (blobs present) (PR #37).
  - `maj model fetch` + the SigLIP 2 B/16 encoder (image tower on the
    CoreML/ANE execution provider, text tower on CPU) behind a conformance
    CI gate against a pinned `transformers` reference — measured cosine
    floors 0.999 (vision, CPU EP), 0.99 (vision, CoreML EP), 0.995 (text)
    (PR #38).
  - LanceDB vector store + the semantic layer merged into `maj search` via
    reciprocal-rank fusion (`fuse_ranked`), with `catch_unwind`-wrapped
    corruption recovery for a verified Lance 9.0.0 manifest-parsing panic
    (PR #39).
  - Video: ffprobe/ffmpeg detection, scene-detected keyframes
    (adaptive HSV-delta, 2s minimum scene length, uniform-sampling
    fallback on zero cuts, ~150-frame cap), keyframe embeddings carrying
    `(asset_hash, timestamp_ms)`, timestamped search hits (PR #40).
  - Closing: cucumber acceptance for the layered search flows, symmetric
    per-kind index outcomes, cargo-mutants triage (see the watchlist's new
    "cargo-mutants triage (phase 4)" subsection — filled by the separate
    mutants-triage commit), this handoff.

**e2e proof points** (real model, real ffmpeg, `--ignored`):
- `matching_pair_scores_higher_than_mismatched_pair`
  (`crates/index/tests/encoder_gated.rs`) — a solid-blue image scores higher
  against the text "a solid blue square" than a solid-green image does,
  proving the encoder's ranking discriminates, not just runs.
- `keyframe_search_resolves_the_correct_segment_and_timestamp`
  (`crates/cli/tests/index_smoke.rs:581`) — a real three-segment
  (red/green/blue) clip, scanned and `index run` through the real CLI,
  resolves `search "solid red"`/`search "solid blue color"` to the correct
  keyframe timestamps (`@0m01s`, `@0m07s`) inside each segment.

**Architecture** (Cargo workspace, edition 2024, strict clippy; `just ci` =
the CI gate, `just conformance` = the MHL oracle gate, `just
encoder-conformance` = the encoder oracle gate):

- `crates/core` — hexagon: clock.rs, event.rs (op variants + golden wire
  tests, now including `SavedSearchSet`/`SavedSearchRemove`), projection.rs
  (CRDT projection, `Touched` reporting for incremental apply), ports.rs,
  media_kind.rs.
- `crates/sync` — file event log; `read_all_reporting`/`read_since_reporting`
  (unification deferred, see watchlist).
- `crates/catalog-sqlite` — split into schema.rs/apply.rs/query.rs
  (PR #34's first commit); disposable projection plus `apply_snapshot`/
  `apply_cursors` (incremental apply state) and `names_fts` (FTS5).
- `crates/ingest` — unchanged this phase.
- `crates/index` — new this phase: `blob.rs` (content-addressed store),
  `thumbs.rs`, `resize.rs`, `preprocess.rs` (encoder image/text
  preprocessing), `model.rs` (fetch + cache), `encoder.rs` (SigLIP 2 via
  `ort`), `vector_store.rs` (LanceDB wrapper + corruption recovery),
  `video.rs` (ffprobe/ffmpeg, scene detection), `work.rs` (queue-as-diff
  planner), `error.rs`.
- `crates/cli` — `maj`; `search.rs` (query parsing, fusion, output),
  `index_cmd.rs` (`index run|status`, `model fetch`), `state_dir.rs`
  (local-state resolution + legacy migration), `volume_identity.rs`
  (`mounted_volumes`).
- CI: SHA-pinned actions, actionlint+zizmor, three conformance-adjacent
  jobs (rust checks, mhl-conformance, encoder-conformance), prek hook =
  `just check`.

**State-dir layout** (per-machine, per-catalog, moved out of the sync root
this phase): `~/Library/Application Support/majestical/catalogs/
<catalog-key>/` where `catalog-key` = xxh3-128 hex of the canonicalized
sync-root path (`MAJ_STATE_DIR` overrides the base for tests/CI). Contents:
`catalog.db` (SQLite projection + FTS5 + `apply_state`), `lance/` (vector
dataset), `runs/<run-id>.jsonl` (ingest journals, migrated from the sync
root on first open).

**Blobs layout** (sync root, dumb convergent files): `<sync-root>/blobs/
<aa>/<asset-hash>/thumb-320.webp` plus `<aa>/<asset-hash>/siglip2-b16-v1/
image.f32le.zst` and `kf-<timestamp-ms>.f32le.zst` per video keyframe.
Addressed by derivation key (asset hash + kind + model tag), not content
hash, so writes are idempotent and two machines deriving the same asset
converge by construction.

**SQLite + Lance pairing**: SQLite stays the relational projection plus
filename FTS; LanceDB is vectors only. LanceDB's per-table ACID model and
lack of multi-table joins make it wrong for the projection; SQLite's
brute-force vector story is exactly what LanceDB replaces. Lance is
per-machine local because the sync transport is dumb files — two writers
through Dropbox would corrupt a Lance dataset directory; blobs are the
actual exchange format, and both SQLite and Lance are disposable
projections rebuilt from them.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` — open items plus
"Phase 3 deferrals", a new "Phase 4 deferrals" section (33 items: state-dir
migration edges, sync's dual read-path divergence, several catalog-sqlite
mutants-review findings, the two competing "online" definitions, Lance
corruption/overflow/index-scale notes, video's memory/timeout/hue-scale
notes), a "Done in phase 4" section, and a `cargo-mutants triage (phase 4)`
section with the full per-package category breakdown.

## Phase 5 recommendation

Per the parent spec's build order, step 5: **Describer backends + video's
remaining pipeline**. Suggested scope for the design session:

- Describer backends (Ollama / LM Studio / OpenRouter) behind one
  OpenAI-compatible adapter, configured per catalog. `docs/research/
  ai-stack.md` §1-3 already survey the server landscape (Ollama/LM Studio
  are text-only for embeddings, which is why phase 4 kept the in-process
  encoder; OpenRouter is the one opt-in path to hosted multimodal
  embeddings as a quality tier — re-read §4 before assuming an in-process
  encoder is still the only path).
- Captions and open-vocabulary tag suggestion, classified into the
  existing folksonomy (`tag add`'s OR-Set) rather than a new tag namespace.
- Local `faster-whisper` transcription with timecodes; per-scene OCR using
  phase 4's scene detection, not a new pass.
- PDFs/design files: text extraction + preview embedding.
- Query model grows another layer (transcript/caption/OCR text into FTS,
  a separate transcript vector index merged at query time per parent spec
  §4) — don't collapse it into the phase 4 image/keyframe index.
- Keep MCP and GUI phasing per parent spec §6-§7 — both remain explicitly
  out of scope until build-order step 7; phase 5 is backend/pipeline only.

Write a phase 5 spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial spec-compliance
   reviewer (probes empirically, mutation-tests claims) → code-quality
   reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly.
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
10. **Local setup for phase 5**: install `just`, `protoc` (lance's build
    dependency — `brew install protobuf`), and `ffmpeg`/`ffprobe` on PATH
    before touching `crates/index`. Model artifacts follow the
    `MAJ_MODEL_DIR`/`.model-cache` pattern (`justfile`'s
    `encoder-conformance` recipe): a repo-local cache directory, ~1GB on
    first fetch, keyed in CI on a fixed model-id string so it survives
    across runs. Expect the encoder-conformance and mhl-conformance CI jobs
    to be slow (cold-cache) on the first run after a dependency bump —
    they download a pinned `transformers`/`ascmhl` reference plus model
    weights, not just compile Rust.

## Phase-4 lessons worth carrying

- **The oracle-wins pattern extends past file formats to model
  preprocessing.** The encoder conformance gate (pinned `transformers`
  reference, cosine floors 0.999/0.99/0.995) caught preprocessing bugs
  exactly where the spec predicted — awkward-aspect-ratio fixtures, not the
  square ones. For phase 5's Describer backends, treat each provider's own
  reference client output as the oracle before hand-rolling request
  shaping.
- **Adversarial review earned its keep against real defect classes this
  phase**, not just style nits: a clap `allow_hyphen_values` gap that broke
  `-tag:x` queries as the leading token, a `limit*4` prefetch that starved
  filtered semantic results (fixed with fetch-everything-when-filtered), a
  Lance 9.0.0 panic on a corrupted manifest (wrapped in `catch_unwind`
  rather than left to crash the process), and a hard-filter leak through
  the semantic fusion step (a BLOCKER-severity fix, `fuse_ranked` rewritten
  to intersect against the filter set on both ranked lists). None of these
  would have surfaced from a happy-path read-through.
- **`catch_unwind` and linker flags can conflict in non-obvious ways.** The
  `__eh_frame section too large` warning from Lance's debug build graph has
  an obvious-looking fix (`-no_compact_unwind`) that turns out to make real
  panics abort the process instead of unwind — which would silently defeat
  the corruption-recovery wrapping built for the Lance panic above. Recorded
  `DO NOT USE` in `Cargo.toml` rather than applied.
- **Review throughput under rate limits favors inline, same-diff review
  comments over a separate report-then-fix pass** — reviewers annotating
  the actual patch (spec-compliance and code-quality passes both against
  the same diff) closed rounds faster than a written report requiring a
  second reconstruction step. Phase 5's larger fan-out (three Describer
  backends behind one adapter) will want the same discipline.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite is disposable. Derived data
  (vectors, thumbnails, keyframes) must also be disposable and
  content-addressed — blobs are the actual exchange format; Lance and
  SQLite are both rebuildable projections of blobs + events.
- Apply is commutative + idempotent (property-tested), now including
  incremental apply ≡ full rebuild over random cursor splits. New op
  variants must keep order independence and extend the proptest generator.
- Wire format is pinned by golden tests — additive changes only.
- Vectors are L2-normalized at the encoder (`crates/index/src/encoder.rs`)
  so Lance's `Dot` distance equals cosine similarity — never store a
  non-normalized vector.
- Never lie about data safety or index completeness: search must degrade,
  never error, when a model or ffmpeg is absent, or an index is partial —
  and the degradation notice must name the specific gap (which coverage
  count, which missing tool), never a generic "unavailable."
- Never silently truncate: a result-count line accompanies every truncated
  output; coverage numbers come from counting real rows/blobs, never cached
  claims.
- Tests must discriminate: reviewers mutation-test; write tests whose
  failure modes are loud.
