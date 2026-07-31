# Majestical Phase 4 — In-process embeddings + layered search

Date: 2026-07-30
Status: Approved section-by-section in design session; pending written-spec review.
Parent spec: `2026-07-28-majestical-design.md` (§2 data model, §4 AI indexing, §5 sync).
Handoff: `docs/superpowers/HANDOFF-phase4.md`.
Research: `docs/research/ai-stack.md` (encoder split, preprocessing pitfalls,
vector-store benchmarks).

## Scope decisions (from design session)

1. **All four boundary items are in**: thumbnails, video keyframes (scene
   detection pulled forward from the Describer phase), the local-state vs
   sync-root split, and incremental SQLite apply.
2. **Model delivery is explicit** — `maj model fetch` downloads pinned
   artifacts verified by content hash; indexing commands error with a clear
   "run maj model fetch" message when the model is absent. No auto-download.
3. **Unified query language** — one query string parsed into semantic + FTS
   terms and `key:value` hard filters; shared later by GUI omnibox and MCP.
4. **Foreground runner** — `maj index run` works a durable (recomputable)
   queue in the foreground; no daemon this phase.
5. **ffmpeg/ffprobe are external binaries** detected on PATH; video work
   degrades visibly when they are missing.
6. **Derived data lives in sync-root `blobs/`** (spec §5 layout); all local
   index structures are disposable projections of blobs + events.
7. **LanceDB now for vectors** (deviation from parent spec §4's sqlite-vec
   default, user decision at design session): scale headroom, native hybrid
   BM25+vector for phase 5 captions/transcripts, Arrow/DuckDB analytics
   interop. SQLite keeps everything relational; blobs keep the engine
   swappable.

Out of scope this phase (per parent spec build order): Describer backends
(Ollama/LM Studio/OpenRouter), captions, open-vocabulary tag suggestion,
transcription, OCR, PDF/design-file extraction, blob push/pull commands
(blobs travel with whatever syncs the folder), MCP server, GUI.

## Architecture

New crate `crates/index`: blob store, model cache + encoder, thumbnailer,
video keyframe pipeline, work planner ("queue-as-diff"), Lance vector store.
Depends on `core` ports only; the CLI injects adapters. Events remain truth;
every local structure remains disposable.

### Storage layout — the split

Sync root (NAS share / Dropbox folder / shuttle drive) holds only dumb,
convergent files:

```
<sync-root>/
├── events/<machine-id>/NNNN.jsonl          # unchanged
└── blobs/<aa>/<asset-hash>/                # aa = first 2 hex of asset hash
    ├── thumb-320.webp                      # 320px longest edge
    └── siglip2-b16-v1/
        ├── image.f32le.zst                 # 768-d f32 LE, zstd
        └── kf-<timestamp-ms>.f32le.zst     # one per video keyframe
```

Blobs are addressed by **derivation key** (asset hash + kind + model
id/version — parent spec §2's "keyed by asset hash + model version"), not by
their own content hash: writes are idempotent (skip if present; write temp +
rename otherwise), rebuild is a directory walk, and two machines deriving the
same asset converge by construction. Teammates who sync the folder get
thumbnails and vectors for volumes they never mounted.

Local state moves to a per-machine, per-catalog directory:
`~/Library/Application Support/majestical/catalogs/<catalog-key>/` where
`catalog-key` = xxh3-128 hex of the canonicalized sync-root path.
`MAJ_STATE_DIR` overrides the base for tests. Contents:

- `catalog.db` — the SQLite projection (moved out of the sync root), plus the
  new FTS5 name index and `apply_state` (below).
- `lance/` — LanceDB dataset for vectors. Columns: `asset_hash`, `kind`
  (`image`|`keyframe`), `timestamp_ms` (nullable), `model_tag`, `vector`
  (fixed 768 f32). Per-machine local **because** the sync transport is dumb
  files — a Lance dataset directory with two writers through Dropbox would
  corrupt; blobs are the exchange format.
- `runs/<run-id>.jsonl` — ingest journals move here from the sync root
  (closes phase 3 as-built deviation 9; journals were machine-local in
  practice already).

Engine pairing: **SQLite = relational projection + filename FTS; LanceDB =
vectors only.** LanceDB's per-table ACID, columnar update model, and lack of
multi-table joins make it wrong for the projection; SQLite's brute-force
vector story is what LanceDB replaces. Lance BM25 becomes interesting in
phase 5 when captions/transcripts exist.

Migration: on first open, a legacy `catalog.db` (or `runs/`) found in the
sync root is deleted after the local rebuild succeeds — both are disposable
by invariant; the event log is never touched.

### Incremental apply

`catalog.db` gains `apply_state`: per-segment cursors (machine, segment file,
byte offset) plus a versioned serialized snapshot of the core `Projection`
(full CRDT state — OR-Set internals, HLC timestamps — which the SQL tables
alone cannot rehydrate). Open path, one transaction: load snapshot → scan
`events/` for bytes past each cursor → apply only new events through the
existing core projection (CRDT semantics stay in one place) → core reports
touched entities per op → rewrite only those rows + FTS entries → store new
snapshot + cursors. Snapshot version mismatch, corruption, or a shrunken
segment falls back to full rebuild — the fallback is exactly today's
behavior, so incremental apply can never be worse than the status quo.

Core change: `Projection::apply` (or a wrapper) returns the set of touched
entity keys. Correctness gate: property test asserting incremental apply ≡
full rebuild over random op sequences split at random cursor points.

### New core ops (additive; wire format extended, never changed)

- `SavedSearchSet { name, query }` — HLC-LWW per name.
- `SavedSearchRemove { name }` — LWW tombstone; a later `Set` revives.

Both extend the proptest generator (order-independence obligation) and get
golden wire-format tests. Derived data produces **no events** — blobs are
facts on disk, not operations.

## Encoder

SigLIP 2 B/16-256 (768-d) via the `ort` crate (ONNX Runtime): **image tower
on the Core ML execution provider (ANE), text tower on the CPU EP** — the
proven split (Core ML mishandles the text tower's dynamic shapes; the Gemma2
256K-vocab text tower is ~565MB fp16). Tokenization via the `tokenizers`
crate with the pinned SigLIP 2 `tokenizer.json`. Crate and EP versions
verified current at planning time, not assumed.

`maj model fetch` downloads pinned artifacts (image ONNX, text ONNX,
tokenizer) from exact URLs, verifies expected content hashes before install,
and places them in a shared cache under the majestical Application Support
base (models are catalog-independent). Exact URLs + hashes are pinned in the
implementation plan. Every vector records `model_tag` (`siglip2-b16-v1`) in
both blob path and Lance row; a model upgrade is an incremental re-derive.

**Preprocessing is load-bearing** (oracle-wins, the MHL lesson):

- Image: squash-resize to 256×256 (no center crop, no aspect preservation),
  normalize `(x/255 − 0.5)/0.5` → [−1, 1], NCHW f32.
- Text: tokenize, pad to 64 tokens with pad id 0, **no attention mask**.

Conformance gate `just encoder-conformance` (CI job mirroring
mhl-conformance): a pinned Python reference (uv + pinned `transformers`)
computes golden embeddings for fixture images — including awkward aspect
ratios, where squash-resize errors show first — and fixture text queries.
The Rust path on the CPU EP must match within cosine ≥ 0.999 (target;
measured and recorded at implementation). The Core ML EP gets a separate
looser sanity check (target ≥ 0.99, fp16 variance). Tokenizer output is
pinned with golden token-id tests.

## Thumbnails & decoding

One size this phase: 320px longest edge, WebP, via the `image` crate for
common formats. HEIC decodes through the macOS-native path (`sips` or
CoreGraphics — chosen at planning); undecodable formats (RAW etc.) are
reported by `maj index status` as `unsupported`, never errored. Video
thumbnail = decoded frame at 10% of duration.

## Video keyframes

`ffprobe` → duration/fps/codec. ffmpeg decodes a low-rate analysis stream
(piped frames) → scene detection in Rust with the field-tested parameters:
adaptive luma/HSV-delta detector, 2s minimum scene length, uniform-sampling
fallback when fewer than 10 scenes detected, ~150 frame cap → one keyframe
per scene → image-tower embedding stored with `(asset_hash, timestamp_ms)`
so hits are seekable. Missing ffmpeg/ffprobe → video work queues visibly as
`needs-ffmpeg`; image indexing proceeds.

## Commands & the queue

**The queue is the diff** — no queue storage. Work = (assets in catalog ×
required derivations for their kind × current model) minus (blobs present),
plus (blobs present minus Lance rows) for index loading. Resumable for free
(finished work has blobs), idempotent, self-healing (delete a blob, it gets
remade). An asset with no instance on an online volume is `offline` —
pending, visible, not an error; any online instance may be read.

- `maj index run [--watch] [--threads N] [--limit N]
  [--kinds thumbs,embeddings,keyframes] [--json]` — works the diff in
  priority order thumbnails → image embeddings → keyframes. Blob written
  first, then Lance row upserted. Small default thread count is the
  throttle; `--watch` re-diffs on an interval.
- `maj index status [--json]` — per-kind counts: done / pending / offline /
  unsupported / needs-ffmpeg / needs-model; model cache state.
- `maj model fetch [--json]` — as above.

## Query model

`maj search "<query>" [--limit N] [--json]`:

```
maj search "golden retriever beach -tag:rejected vol:Media2024 para:projects/reel kind:video after:2026-01-01"
```

- Bare terms → semantic layer (text-tower encode → Lance cosine search over
  image + keyframe vectors) AND filename FTS5, merged by reciprocal rank
  fusion. Keyframe hits carry timestamps.
- `key:value` → hard filters resolved in SQLite to an allowed asset set that
  candidates are intersected against: `tag:`, `vol:`/`volume:`, `para:`,
  `kind:` (`image`|`video`|`other`, classified from file extension — the
  same classification the index planner uses to pick derivations),
  `online:` (`yes`|`no`), `before:`/`after:` (file mtime, `YYYY-MM-DD`).
  `-` negates where meaningful (`-tag:x`).
- Output: ranked assets — score, name, volume + online/offline, tags, PARA
  node, timestamp for keyframe hits — plus a result-count line.
- The existing `--name`/`--tag` flags are **replaced** by the query language
  (replace, don't deprecate); FTS5 also retires the ASCII-only
  case-insensitivity limitation (watchlist item closed).
- **Degrade, never error** (invariant): no model → FTS + filters with a
  notice; partial index → results plus coverage ("semantic index: N of M
  eligible assets"). Offline volumes remain fully searchable — that is the
  point.

Saved searches: `maj search --save <name> "<query>"` emits `SavedSearchSet`;
`maj search --saved <name>` runs one; `maj searches list [--json]`
enumerates; `maj searches rm <name>` emits `SavedSearchRemove`. Synced via
CRDT like all organizational opinion.

## Error handling

Typed `thiserror` errors carrying operation + path + suggested fix, per house
style. Model-absent and ffmpeg-absent are *statuses* in indexing and search
degradation paths, but hard errors where the user explicitly asked for the
thing (`maj model fetch` network failure; `--kinds keyframes` with no
ffmpeg). Blob writes are temp + rename — a crashed `index run` never leaves
a partial blob at a final path. Lance dataset corruption → rebuild from
blobs (logged, not fatal). Verification-style honesty applies: coverage
numbers in search output come from counting real rows/blobs, never cached
claims.

## Testing

- **Encoder conformance CI gate** (pinned Python `transformers` reference;
  cosine tolerances as above) — the phase's oracle.
- Golden preprocessing tests (exact pixel values for a fixture after
  squash-resize + normalize; golden token ids).
- **Incremental ≡ full rebuild property test** (random op sequences, random
  cursor split points).
- Scene detection against synthetic ffmpeg-generated fixture videos (colored
  segments with hard cuts → known boundaries; a no-cut fixture → uniform
  fallback).
- Queue idempotency: run twice → second run does nothing; delete a blob →
  exactly that work reappears.
- Degradation acceptance: missing model, missing ffmpeg, partial index,
  offline-only assets — search still answers.
- Query-parser unit tests (operators, negation, quoting, malformed input).
- Cucumber acceptance for the search flows at the hexagon boundary, in the
  established style; CRDT generator extended over the saved-search ops.
- cargo-mutants over `crates/index` and the query layer as the closing task;
  survivors triaged onto the watchlist.
- Zero warnings; the strict clippy table is copied into `crates/index`
  (known lint-table drift caveat).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. Local-state/sync-root split: state dir + `MAJ_STATE_DIR`, catalog.db and
   journals move, legacy cleanup, docs.
2. Incremental apply: snapshot + cursors + touched-entity reporting in core +
   equivalence proptest.
3. FTS5 name index + query-language parser + `maj search` rework (FTS +
   filters only; semantic layer arrives in PR 7).
4. Saved-search ops in core + CLI surface.
5. `crates/index` scaffold: blob store (idempotent temp+rename, zstd),
   thumbnailer, queue-as-diff, `maj index run/status` (thumbnails only).
6. `maj model fetch` + encoder (preprocessing, both towers) + conformance CI
   gate.
7. Lance store + embedding pipeline in `index run` + semantic layer merged
   into `maj search` (RRF).
8. Video: ffprobe/ffmpeg detection, scene detection, keyframe embeddings,
   timestamped results.
9. Closing: cargo-mutants, degradation acceptance sweep, watchlist + handoff
   updates.

Each task runs the mandated loop: fresh implementer subagent → adversarial
spec-compliance reviewer → code-quality reviewer → fix rounds until APPROVED.

## As-built deviations (recorded 2026-07-31, phase complete)

Where the shipped implementation departs from the text above. Each was a
reviewed decision; deferrals carry watchlist entries with attribution.

1. `open_synced` gained an `on_bad_line` callback parameter
   (`crates/catalog-sqlite/src/apply.rs:46-50`) — the plan dropped bad-line
   surfacing; the CLI's corrupt-line warning
   (`crates/cli/src/commands.rs:27-29`, `crates/cli/src/app.rs:33-40`)
   depends on it.
2. The `instances` table's `PRIMARY KEY` briefly widened to `(asset, volume,
   path, size)` in PR 2 (`c4ec4f4`) — a workaround for the then-set-based
   projection legitimately holding two same-path, different-size facts at
   once, which otherwise hit a `UNIQUE` violation. PR 3's instance-LWW
   change (`dfdede5`) re-narrowed it to `(asset, volume, path)` once
   instances became an HLC-LWW map keyed on `(volume, path)`, making a
   same-path-different-size row impossible by construction; the schema
   comment at the PK explains the narrowing.
3. The plan's `Snapshot` wrapper struct was dropped — snapshots serialize
   the `Projection` directly (`serde_json::to_string(projection)`,
   `crates/catalog-sqlite/src/apply.rs:209`); the version lives in its own
   `apply_snapshot.version` column, read separately (`:169-172`).
4. The tuple-keyed `instances` map needs a serde adapter —
   `serde_json` rejects non-string map keys, so `AssetState::instances` uses
   a custom `with` module (`crates/core/src/projection.rs:55-56`) that
   (de)serializes it as a JSON array of `{volume, path, ...}` entries; the
   plan missed this.
5. `--name`/`--tag` semantics were deliberately replaced by the FTS switch
   mid-phase — basename word-prefix matching plus bm25 ranking
   (`crates/catalog-sqlite/src/query.rs:19-41`), not path substring
   matching.
6. The `Volume` filter uses a `LEFT JOIN` plus instance-id match
   (`crates/catalog-sqlite/src/query.rs:90-98`), reaching "ghost" volumes
   with no `VolumeSeen` row — an improvement on the plan's inner join.
7. The plan's `limit*4` prefetch starved filtered results — replaced with
   fetch-everything-then-intersect whenever a hard filter is present
   (`crates/cli/src/search.rs:176-186`); the `limit*4` window survives only
   for the no-filter case.
8. `--save` emits *after* a successful run (`crates/cli/src/search.rs:52-75`)
   — the plan's emit-first would have persisted invalid queries to the
   replicated event log.
9. `arrow-array`/`arrow-schema` are pinned exactly to `58.3.0`
   (`Cargo.toml:35-36`), in lockstep with lancedb's own arrow requirement.
10. The semantic/FTS/filter fusion is a pure `fuse_ranked` function
    (`crates/cli/src/search.rs:208-222`) — the plan's inline intersection was
    untestable without the encoder model; a reviewer extracted it as a
    standalone, model-free unit.
11. Search's read path uses `VectorStore::open_existing`
    (`crates/index/src/vector_store.rs:101`), which never creates a
    dataset — the plan's `open()` created one on read.
12. The semantic limit sentinel `usize::MAX >> 1`
    (`crates/cli/src/search.rs:181-188`) pins lancedb 0.33.0's raw `as i64`
    cast of the query limit, where `usize::MAX >> 1 == i64::MAX` on a
    64-bit target.
13. `detect_scenes`' merged-to-nothing flicker case (all raw cuts removed by
    the minimum-scene-length merge) yields a single whole-span midpoint
    (`crates/index/src/video.rs:256-266,358-364`) — distinct from the
    zero-raw-cuts uniform fallback.
14. The uniform sampling fallback fires on zero raw cuts only
    (`crates/index/src/video.rs:260-262`) — the plan's below-10-scenes
    branches were byte-identical, so the threshold constant was dropped.
15. `ModelFormat::NeuralNetwork`, not `MLProgram`
    (`crates/index/src/encoder.rs:151-158`) — ort's CoreML converter fails
    on the patch-embed `Conv` under `MLProgram`; conformance floors prove
    ANE execution still holds under `NeuralNetwork`.
16. `maj model fetch` shells out to system `curl`
    (`crates/index/src/model.rs:166`) rather than pulling in an HTTP client
    dependency; the vision tower is fp32, the text tower fp16
    (`crates/index/src/model.rs:23-24,30,36`).
17. `tokenizers` uses the `fancy-regex` backend, not `onig`
    (`Cargo.toml:33`, `default-features = false, features =
    ["fancy-regex"]`) — avoids onig's C++ build; exact golden-token parity
    is proven by conformance.
18. CI installs `just` on macOS runners and calls justfile recipes
    throughout (`.github/workflows/ci.yml:27-28,40-41,67-68`) — a user
    directive for a single command source.
19. `maj model fetch` has no `--json` flag — only `--verify`
    (`crates/cli/src/main.rs:225-230`) — despite this spec's `maj model
    fetch [--json]` (line 183). The phase 4 plan's own Task 13 code baked
    in `--verify` without ever specifying `--json`, so the gap traces to
    the plan, not a later implementation drift; not caught until this
    review.
20. `maj index status` prints no model-cache-state line
    (`crates/cli/src/index_cmd.rs:887-907`), despite this spec's "per-kind
    counts... model cache state" (line 181) — the model cache path only
    ever prints from `maj model fetch`
    (`crates/cli/src/index_cmd.rs:918-919`). Same root cause as #19: the
    plan's own `cmd_index_status` sketch never included it.
21. Task 20's cucumber suite shipped a "kind filter selects by media class"
    scenario in place of the plan's "Filter-only search over an offline
    volume still answers" scenario
    (`crates/cli/tests/features/search.feature`), with no record of the
    swap at the time. Restored: the offline-volume scenario is back
    (`crates/cli/tests/features/search.feature:33-36`,
    `crates/cli/tests/acceptance.rs`'s `catalog_with_an_offline_asset`
    step) — an asset scanned under an explicit `--volume` id that
    `volume_identity::mounted_volumes` never resolves is offline by
    construction, no mount-faking or file-deletion needed, exactly as the
    plan's own note anticipated. The kind-filter scenario stays too; both
    now run.
