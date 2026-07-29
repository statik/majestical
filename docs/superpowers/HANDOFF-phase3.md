# Majestical — Phase 3 handoff

Written 2026-07-29 at the close of Phase 2. Read this first; everything else is
linked from here.

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP with full GUI parity).

- Spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Competitive research: `docs/research/` (OffShoot, Kyno, Shade, NeoFinder,
  Peakto, ASC MHL, local-AI stack, cuesheet + hyperdeck-adapter patterns)
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters (see research).

## State at handoff (main @ PR #19, all CI green)

**Shipped and working** (`maj` CLI, exercised end to end on a real machine):

- `catalog init` (load-bearing; strict — commands error on uninitialized roots)
- `scan [--volume ID] <dir>` — streaming xxh3-128 content hashing, volume
  identity auto-detected via diskutil VolumeUUID with documented fallbacks,
  emits VolumeSeen + AssetSeen events
- `tag add|rm` (add-wins OR-Set; removes cite observed add ids; unknown assets
  rejected), `meta set|get` (HLC-LWW fields), `search --name|--tag [--json]`,
  `volumes list [--json]` (the shelf: label, last-seen ISO-8601, online
  heuristic, clock-suspect flagging)
- Two machines sharing a catalog folder converge (e2e-proven, including
  cross-machine removes and LWW)
- v0.1.0 released: tag → universal binary → published release, verified

**Architecture** (Cargo workspace, edition 2024, strict clippy + clippy.toml
test exemptions; `just ci` = the CI gate exactly):

- `crates/core` — hexagon: clock.rs (HLC, 24h drift clamp, ObserveOutcome),
  event.rs (Event/Op enum, golden wire-format tests), projection.rs (CRDT:
  OR-Set tags with global tombstone eviction, LWW fields + volume state,
  idempotent order-independent apply), ports.rs (EventLog, CatalogStore,
  PortError). Tests: unit + proptest (order independence) + cucumber
  acceptance (5 scenarios, mutation-hardened).
- `crates/sync` — file event log `events/<machine>/0001.jsonl`, corrupt-line
  + stray-file tolerant, NotInitialized variant, batched single-write appends.
- `crates/catalog-sqlite` — disposable projection (open + atomic rebuild),
  tag/name search (LIKE-escaped), volumes tables. rusqlite PINNED 0.37
  (0.38+ needs unstable cfg_select; 0.40 dropped u64 ToSql) — Dependabot
  ignore in place.
- `crates/cli` — `maj` binary; main.rs ~600 lines (extraction to a commands
  module is queued for the next command added); volume_identity.rs, iso8601.rs.
- `site/` — Vite/TS marketing site, photo hero (Unsplash-credited), deploys to
  Pages on push via pages.yml.
- CI: SHA-pinned actions, actionlint+zizmor (pinned), prek hook = `just check`,
  Dependabot grouped w/ 7-day cooldown. Release workflow builds universal maj.

**Backlog (triaged)**: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`
— open items include segment rotation, incremental SQLite apply, local-state vs
sync-root split, non-UTF-8 paths (ingest must preserve exact bytes),
cargo-mutants run, CatalogStore port lagging the volumes queries, meta-get
clock-suspect analog, root-volume lumping for internal scans.

## Phase 3 recommendation

Per the spec's build order: **the ingest engine + ASC MHL** (spec §3) — verified
copy (source read + destination read-back, xxHash64), spec-compliant `ascmhl/`
histories (create + verify first; the Python reference implementation is the
conformance oracle in CI), content-hash dedupe before copy, PARA destination
routing, checkpointed resumable transfers. Alternative if search-depth is
preferred: FTS5 + thumbnails. Write a phase 3 plan in the established format
(see below) before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans → subagent-driven
   development. Plans live in `docs/superpowers/plans/YYYY-MM-DD-<name>.md`
   with full TDD steps and code. Each task: fresh implementer subagent → spec
   -compliance reviewer (adversarial, probes empirically, mutation-tests
   claims) → code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green, easy-to-follow titles. Never push to main directly.
3. **NO Claude-Session trailers in commit messages** (user mandate).
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain git).
5. Shared checkout: implementers stage ONLY their files, never `git add -A`.
   For work parallel to an active branch, use a git worktree.
6. Shell: variables do NOT persist across Bash invocations (a `trash` with
   empty vars once moved the whole repo to Trash — recovered). `trash`, never
   `rm -rf`.
7. Git auth: SSH is unavailable in this environment; push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings; verify current versions of deps/actions at execution time
   (plan pins go stale — every SHA in the phase-1 plan was stale by execution).
9. Reviewers' findings get fixed in the same chunk when cheap; deferred items
   go on the watchlist with attribution.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite is disposable.
- Apply is commutative + idempotent (property-tested). New op variants must
  keep order independence and extend the proptest generator.
- Wire format is pinned by golden tests — additive changes only.
- HLC total order: (wall_ms, counter, machine). Clamp semantics are
  acceptance-asserted.
- Tests must discriminate: reviewers mutation-test; write tests whose failure
  modes are loud (two label-LWW tests exist specifically because a tiebreak
  confound survived the whole suite once).
