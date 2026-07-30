# Majestical — Phase 4 handoff

Written 2026-07-30 at the close of Phase 3. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase3.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP with full GUI parity).

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3 spec + as-built deviations:
  `docs/superpowers/specs/2026-07-29-phase3-ingest-design.md`
- Competitive research: `docs/research/` (esp. `ai-stack.md` for phase 4 —
  documented preprocessing pitfalls for the image-encoder split)
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #30, all CI green)

**Shipped and working** (`maj` CLI, exercised end to end on a real machine):

- Phases 1-2: `catalog init` (strict), `scan` (streaming xxh3-128, volume
  identity via diskutil), `tag add|rm` (OR-Set), `meta set|get` (HLC-LWW),
  `search --name|--tag`, `volumes list` (the shelf). Two machines converge
  through a shared catalog folder. v0.1.0 released.
- Phase 3 (PRs #21-#30): `para add|list|rename|archive` (full PARA CRDT;
  archive moves materialized dirs, converges on re-run), `maj ingest`
  (multi-destination verified copy — single-pass xxh64+xxh3-128 fan-out,
  temp-name quarantine, independent per-destination read-back before rename,
  end-of-run sweep, size-prefiltered content-hash dedupe, `{date}/
  {source-label}` templates, dry-run, fsynced JSONL journal with resume),
  `maj verify` (appends ASC MHL generations), ASC MHL create/verify
  conformance-gated BOTH directions against the Python reference (`ascmhl`
  1.2) locally (`just conformance`) and in CI. cargo-mutants ran over
  ingest+core (16 discriminating tests added; survivors triaged).

**Architecture** (Cargo workspace, edition 2024, strict clippy; `just ci` =
the CI gate, `just conformance` = the MHL oracle gate):

- `crates/core` — hexagon: clock.rs (HLC, 24h clamp), event.rs (11 op
  variants, golden wire tests), projection.rs (CRDT: OR-Set tags, LWW fields/
  volumes/PARA, grow-only verification+manifest sets), ports.rs (EventLog,
  CatalogStore).
- `crates/sync` — file event log `events/<machine>/0001.jsonl`.
- `crates/catalog-sqlite` — disposable projection; tables for assets/
  instances/tags/volumes/para_nodes/asset_para/verifications/manifests.
  rusqlite PINNED 0.37 (Dependabot ignore in place).
- `crates/ingest` — planner (dedupe, templates), engine (file-parallel
  verified copy, Sink/SinkFactory fault-injection seam), journal, mhl (ASC
  MHL + c4 chain hashing), hashing (shared streaming helpers).
- `crates/cli` — `maj`; main.rs = clap+dispatch, commands.rs = handlers,
  app.rs = adapter wiring. Cucumber acceptance in core and ingest.
- CI: SHA-pinned actions, actionlint+zizmor, mhl-conformance job (uv +
  pinned ascmhl), prek hook = `just check`.

**Backlog (triaged)**: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`
— open items + a "Phase 3 deferrals" section (dedupe link mode, per-dest
failure attribution, symlink/junk-file policy, cross-day resume `{date}`
re-render, journal abort-path seam, chain c4 cross-check, local-state vs
sync-root split, incremental SQLite apply, segment rotation, FTS-dependent
ASCII-only search, macOS-only test CI, full mutants triage).

## Phase 4 recommendation

Per the parent spec's build order: **in-process embeddings + layered search**
(spec §4) — the differentiator. Suggested scope for the design session:

- In-process image/text encoder: SigLIP 2 B/16 (768-d) via ONNX Runtime with
  the Core ML execution provider (image tower on ANE, text tower on CPU — the
  proven split; read `docs/research/ai-stack.md` for the preprocessing
  pitfalls before writing any encoder code). MobileCLIP2 is the documented
  lighter alternative. Model id+version stored per vector.
- Storage: sqlite-vec + FTS5 in the catalog SQLite (documented LanceDB
  migration path; not a day-one dependency). FTS5 also resolves the
  ASCII-only case-insensitive search watchlist item.
- Query model: layered — semantic + keyword FTS (names/captions/OCR later) +
  hard filters (volume, PARA, tags, online/offline), result counts, negative
  operators, saved searches.
- Background index queue with visible progress/throttle; thumbnails first.
  Derived data is content-addressed and disposable (spec §2).
- Phase-boundary decisions to make during brainstorming: thumbnails in or
  out; video keyframes in or out (spec puts scene detection in the Describer
  phase); whether incremental SQLite apply and the local-state/sync-root
  split must land first (index rebuild cost makes both more pressing).
- Describer backends (Ollama/LM Studio/OpenRouter), transcription, and the
  video pipeline are phase 5 — keep them out.

Write a phase 4 spec + plan in the established format before any code.

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
   CI's clippy can be NEWER than local — a 1.97-only lint broke a
   green-local PR once; expect drift and fix forward.
9. Reviewers' findings get fixed in the same chunk when cheap; deferred items
   go on the watchlist with attribution.

## Phase-3 lessons worth carrying

- **The oracle-wins pattern worked.** For any external format, install the
  reference tool first, study its real output, and let conformance tests
  drive the implementation (ASC MHL's c4 chain requirement was undiscoverable
  from the spec sketch). Phase 4 analog: validate encoder preprocessing
  against the reference model implementation's outputs early.
- **Verify tests discriminate against the PRODUCTION line** — two "verified
  failing" claims collapsed under reviewer probes because the test exercised
  a copy of the pattern, not the code. Break the real line, watch the real
  test fail.
- clippy.toml's test exemptions key on literal `#[cfg(test)]`, so
  integration-test files need the attribute (with a comment) or no
  unwrap/expect.
- cargo-mutants runs take 20-40 min per package — background them and keep
  working; scoped `--file` re-runs verify fixes cheaply.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite is disposable. Derived data
  (vectors, thumbnails) must also be disposable and content-addressed.
- Apply is commutative + idempotent (property-tested). New op variants must
  keep order independence and extend the proptest generator.
- Wire format is pinned by golden tests — additive changes only.
- HLC total order: (wall_ms, counter, machine); 24h clamp is
  acceptance-asserted.
- Never lie about data safety: verification claims must be backed by bytes
  read from the destination; search must degrade, never error, when indexes
  are incomplete.
- Tests must discriminate: reviewers mutation-test; write tests whose
  failure modes are loud.
