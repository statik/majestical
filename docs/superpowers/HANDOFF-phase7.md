# Majestical — Phase 7 handoff

Written 2026-08-02 at the close of Phase 6. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase6.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP with full GUI parity).

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-5 specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 6 spec + as-built deviations:
  `docs/superpowers/specs/2026-08-01-phase6-sync-design.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #62, closing PR pending)

**Shipped and working** (`maj` CLI, exercised end to end):

- Phases 1-5 (see `HANDOFF-phase6.md` for detail): catalog init/scan/tag/meta/
  volumes/para, verified multi-destination ingest + ASC MHL verify, unified
  search with saved searches, blob store + thumbnails + diff-as-queue
  indexing, SigLIP 2 + MiniLM + whisper behind four conformance gates, scene
  keyframes, describer backends, OCR/PDF, layered text search.
- Phase 6 (PRs #55-#62 + the closing PR):
  - **Multi-location sync** (spec §Sync model): per-machine `sync.toml`
    (state dir; unknown keys survive rewrites; atomic store) managed by
    `maj sync location add|list|rm` (canonicalized, validated, skeleton
    init). The transfer engine (`crates/sync/src/transfer.rs`, ~1040 lines
    with tests) is a stateless set-union diff of two sync roots: segments
    longer-wins whole-file via temp+rename with cross-machine-unique temp
    names, blobs presence+size diff in the priority ladder (thumbs →
    metadata → vectors → transcripts). Source-side read errors propagate
    (NotFound = valid empty peer); destination unreadables self-correct
    through the copy. Per-file failures are recorded in the outcome and the
    transfer continues — one bad blob cannot wedge a sync. File symlinks
    followed; symlinked dirs a documented non-goal.
  - `maj sync push` (readonly refusal naming the setting/file/remedy;
    `--location`, `--only`; per-location outcome/skipped/failed rows in
    text + JSON), `maj sync pull` (same shape; ends with the incremental
    apply — ordering pinned so events apply even when blob failures
    occurred — and a `maj index run` remedy notice; one JSON document),
    `maj sync status` (both directions planned read-only; per-machine
    segment counts + per-class blob counts; unreachable and failed rows;
    healthy locations collapse to `in sync`; exit 0 — status reports,
    push/pull carry the exit policy: nonzero when all locations failed OR
    any per-file failures occurred).
  - **Segment rotation**: `append` rotates at 4 MiB to the next zero-padded
    `NNNN.jsonl` (width-4 enforced, 9999 overflow is a hard error), plus
    the watchlist cleanups (unified read walk, `LogError::io`).
  - **Inbox contributions** (spec §Inbox): `crates/cli/src/inbox_manifest.rs`
    is the trust boundary — versioned `contribution.json` with fail-closed
    validation of EVERY contributor-controlled string (traversal, symlinks,
    duplicates, degenerate names, malformed hashes; `para_target` uses the
    lowercase `kind/name` form), presence+size readiness with refusals as
    values. `maj inbox process` (in `inbox_cmd.rs`): per-inbox
    fingerprint-keyed failure markers (manifest + listed-file mtime/size;
    atomic store, degrade-to-empty load), cheap-first gates (markers →
    presence → refusals → routing → xxh64 hash), verified ingest of exactly
    the listed files via the extracted `run_ingest` (proven byte-identical
    to `maj ingest`), provenance tags covering placed AND dedupe-skipped
    assets, atomic `.processed/` moves (`--keep`), per-contribution failure
    rows with pass-fatal reserved for real I/O.
  - **Manifest-less triage**: quiescence gate (5 min default,
    `MAJ_INBOX_QUIESCENCE_MS` override, iterative walk, unreadable = not
    ready), `--triage-target` ingest tagged exactly `source/inbox`, loose
    files via `plan_source_filtered` (new in `crates/ingest/src/plan.rs` —
    the walk never enters `.processed/` or contribution folders) with
    partial-drain semantics: placed + duplicate files move to
    `.processed/`, failures stay in place named on stderr per row.
  - **Acceptance**: the convergence proptest
    (`crates/sync/tests/convergence.rs`, 64 cases: 3 machines × 2
    locations, random append/write-blob/push/pull scripts, final
    push-all-then-pull-all round proven sufficient as a two-phase set-union
    argument; event-ID sets, counts, and size-aware blob sets converge
    across all five roots) and the shuttle e2e (site A = two machines + a
    NAS, site B reached only by the traveling drive; the gossip hop —
    A2's work reaching B through A1's NAS pull — pinned through the real
    binary; every machine at both sites converges). Inbox cucumber:
    `crates/cli/tests/features/inbox/inbox.feature`, five scenarios,
    per-suite feature directories, `fail_on_skipped` on both cli cucumber
    mains.

**Architecture** (Cargo workspace, edition 2024, strict clippy; `just ci` =
the CI gate, the four conformance jobs unchanged from phase 5):

- `crates/core` — hexagon, unchanged except the `sample_ops()` doc now
  records that phases 5 AND 6 added zero `Op` variants (asserted:
  `event.rs` has an empty diff across the whole phase).
- `crates/sync` — `lib.rs` (rotation, unified read walk, `machine` field on
  `FileEventLog`, single-writer + torn-tail semantics documented honestly)
  + **new `transfer.rs`** (the engine; module doc is the normative
  statement of skip rules and partial-failure semantics) + the convergence
  proptest. `ulid` is now a real dependency (temp-name uniqueness).
- `crates/catalog-sqlite` — untouched this phase; `rusqlite` now a single
  workspace pin (`links = "sqlite3"` lockstep).
- `crates/ingest` — `plan.rs` gained `plan_source_filtered` (walkdir
  filter_entry; rejected dirs never descended); engine untouched.
- `crates/describe`, `crates/index` — untouched this phase.
- `crates/cli` — new `sync_cmd.rs` (~1130), `inbox_cmd.rs` (~1310, pure
  orchestration), `inbox_manifest.rs` (~715, the validation half);
  `commands.rs` refactor: `run_ingest`/`ExecuteIngest`/`IngestRun` +
  `IngestReport { Text, Json, Silent }` extracted from `cmd_ingest`
  (byte-identical behavior, verified against pre-refactor binaries).
  Test surface: `sync_smoke.rs` (~30 tests), `inbox_smoke.rs` (~30),
  `inbox_acceptance.rs` (5 cucumber scenarios), all with a shared
  `Fixture`/`Setup` idiom.

**Sync-root layout** unchanged from phase 5 (events/ + blobs/ + the blob
table in `HANDOFF-phase6.md`); the only addition is a transient `tmp/`
sibling used as sync staging (ignored by all readers; stale temps swept
after 1h). **State-dir additions**: `sync.toml` (locations + `readonly`),
`inbox-failures.json` (per-inbox fingerprint-keyed markers).

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` — now carries a
"Phase 6 deferrals" section (the spec's deferred list — SyncTransport port,
segment divergence detection, share-sheet Shortcut, resident watcher,
auto-index-on-pull — plus execution findings with attribution: truncated
segment tails invisible to readers pending a `maj doctor`, NFC/NFD and
case-folding normalization in unlisted-file comparison, Windows-authored
manifest guards, marker pruning, stuck-contribution re-hash cost,
`fail_on_skipped` for the core/ingest cucumber mains, `read_all` hot-path
cost once rotation multiplies segments), a "cargo-mutants triage (phase 6)"
section (dispositions for the surviving mutants: trait-forwarding covered
only by the cli suite, a sweep-boundary needing `filetime`, a
timeout-not-missed infinite loop), and "Done in phase 6" (segment rotation;
the read-path divergence).

## Phase 7 recommendation

Per the parent spec's build order, step 7: **Agent surface (`maj mcp`) and
the GUI shell** (parent spec §6-§7). Suggested scope for the design session:

- **MCP server first** (`maj mcp`, stdio): tools mirroring the CLI verbs —
  `search_assets`, `ingest_source`, `tag_assets`, `move_para`,
  `verify_volume`, `get_asset`, and now `sync_push/pull/status` and
  `inbox_process` — thin mappings over the same handlers the CLI dispatches
  to. The phase 6 code was shaped for this: every command already has a
  JSON row contract pinned by tests (search results, sync location rows,
  pull summaries, inbox report rows), and mutating flows have dry-run
  hooks. Thumbnail/keyframe resources so agents can see results; stable
  asset ids + timestamps for chaining; mutating tools take `confirm`
  defaulting to dry-run diff (parent spec §6).
- **Expect a service-layer extraction.** `main.rs` dispatches straight to
  `cmd_*` functions that print. MCP needs the same operations returning
  structured values. The `IngestReport::Silent` + `run_ingest`-returns-
  `IngestRun` shape from phase 6 is the pattern: split "do the operation,
  return the outcome" from "render it" per command, then both the CLI and
  MCP wrap one core. Budget this as the phase's main refactor and do it
  incrementally per verb, not big-bang.
- **GUI shell (Tauri + Svelte) can follow or land as a thin start** —
  parent spec §7 names five surfaces; a first slice (Search + Volumes over
  the same service layer) proves the parity-by-construction claim without
  boiling the ocean. The version-sync check across
  Cargo.toml/tauri.conf.json/package.json (parent spec §7's delivery
  notes) arrives with the first Tauri commit.
- **Watchlist items adjacent to this phase**: `PortError` opacity and the
  repeated projection scans in search (both phase 5 deferrals) start to
  matter once MCP multiplies callers; the `maj doctor` residue check is a
  natural MCP-era tool.

Write a phase 7 spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 6 ran eight PRs (#55-#62 +
   closing) on this cadence.
3. **NO Claude-Session trailers in commit messages** (user mandate).
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain git).
5. Shared checkout: implementers stage ONLY their files, never `git add -A`.
   Parallel work needs a git worktree. Do not run reviewers (who mutate
   files empirically) concurrently with implementers.
6. Shell: variables do NOT persist across Bash invocations. `trash`, never
   `rm -rf`.
7. Git auth: SSH unavailable; push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings; verify current versions of deps/actions at execution time.
9. Reviewers' findings get fixed in the same chunk when cheap; deferred
   items go on the watchlist with attribution.
10. **Local setup**: unchanged from phase 5 (`just`, `protoc`, ffmpeg,
    ImageMagick, ~2GB model cache). Phase 6 added no tool or model
    dependencies; sync and inbox tests run on a bare checkout.

## Phase-6 lessons worth carrying

- **The trust boundary must cover every attacker-controlled string, not
  just the obvious ones.** The manifest module validated `files[].name`
  thoroughly while `contributor` — spliced into the destination subdir —
  went unchecked; `Path::join` with an absolute component DISCARDS the
  receiver, so `"contributor": "/tmp/evil"` would have redirected the
  entire ingest. The reviewer found it by tracing every manifest field to
  its use site. When reviewing a validation layer, enumerate the fields
  the OTHER modules consume, not the fields the validator mentions.
- **Sabotage probes and mutation testing kept finding what reads
  couldn't.** The `--only` flag was proven wholly untested by making its
  filter a no-op against a green suite; the pull apply was proven unpinned
  by deleting it outright (search re-applies, masking the loss — the fix
  asserts on `catalog.db` directly between processes); a "walked not
  cached" claim was pinned by deleting a remote blob after push. The
  question "what would still pass if this line vanished?" is the cheapest
  high-yield review move this project has.
- **Stateless + record-and-continue beats clever state.** Every piece of
  sync state that could lie was replaced by walking real files (plans,
  status, event counts measured from what landed at the destination), and
  every batch operation that could wedge on one bad item was converted to
  record-and-continue with loud per-item reporting (transfer failures,
  contribution rows, loose-file partial drain). The two demonstrated
  disasters — the marker-store inbox collision and the duplicate loose
  file growing the replicated event log every cron pass — were both
  cached-claim bugs.
- **Exit-code polarity is a design decision; make it once.** The phase
  settled on: contributor-side faults converge to recorded-notice/exit-0;
  operator-fixable faults fresh-fail nonzero every pass until acted on;
  partial progress is always kept and reported. Cron/agents depend on
  this. It supersedes the original spec wording (which predated per-file
  failures) and is recorded in the spec's as-built section.
- **Reviewers disagree productively when briefed to verify, not trust.**
  Both reviewer roles re-ran implementer claims (byte-identical refactor
  checks against pre-refactor binaries, re-applying claimed-dead mutants,
  formal re-derivation of the convergence sufficiency argument) and each
  found the other's misses. One implementer correctly pushed back on a
  requested "cleanup" with an empirical proof (`#[cfg(test)]` attributes
  are load-bearing for clippy's in-test detection) — briefs should invite
  that.
- **Subagent turns don't keep background children alive.** The closing
  agent twice parked itself "waiting" on cargo-mutants runs that died with
  its turn. Long-running verification belongs in the foreground with split
  scopes (per-file mutants runs), or in the controller's own background
  tasks — never in a subagent's.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. Sync NEVER deletes or truncates in either
  direction; a shorter copy of a machine's segment is always a strict
  prefix (single appender per machine dir).
- Apply is commutative + idempotent (property-tested), incremental apply ≡
  full rebuild, and now: any interleaving of appends/pushes/pulls followed
  by a full sync round converges every root (property-tested). New op
  variants must extend `sample_ops()` and the proptest generator.
- Wire format pinned by golden tests — additive only. Phases 5 AND 6 added
  no op variants; that absence is asserted.
- The inbox validation layer (`inbox_manifest.rs`) is the only constructor
  path for manifest data; the ingest interpolation of `contributor` etc.
  is safe ONLY because `load_manifest` validated it. Never bypass it.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded; a sync or pass that
  could not move everything must not exit 0.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
