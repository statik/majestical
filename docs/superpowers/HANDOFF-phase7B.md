# Majestical — Phase 7B handoff

Written 2026-08-03 at the close of Phase 7A. Read this first; everything else
is linked from here. Supersedes `HANDOFF-phase7.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP with full GUI parity).

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-6 specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 7 spec (services + MCP + GUI, phase 7A implements the first two
  thirds) + as-built deviations:
  `docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md`
- Implementation plan: `docs/superpowers/plans/2026-08-02-phase7a-services-mcp.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #69, this closing PR pending)

**Shipped and working** (`maj` CLI, exercised end to end):

- Phases 1-6 (see `HANDOFF-phase7.md` for detail): catalog init/scan/tag/meta/
  volumes/para, verified multi-destination ingest + ASC MHL verify, unified
  search with saved searches, blob store + thumbnails + diff-as-queue
  indexing, SigLIP 2 + MiniLM + whisper behind four conformance gates, scene
  keyframes, describer backends, OCR/PDF, layered text search, multi-location
  sync (push/pull/status), inbox contributions (manifest + manifest-less
  triage).
- Phase 7A (PRs #64-#69 + this closing PR):
  - **`crates/services`** (new workspace crate, spec's "one function per
    verb" design): every CLI verb's compute moved here as a request-in/
    outcome-struct-out/`ServiceError`-out function, extracted incrementally
    across PR1-PR3 (#65-#67) with the phase-6 refactor-proof technique —
    each moved verb's CLI output stays byte-identical, proven against a
    pre-extraction reference binary built at each chunk's start (see
    "Architecture pointers" below for the harness mechanics). `crates/cli`'s
    `cmd_*` functions are now parse-args → build-request → call-service →
    render only; no operation logic lives in `crates/cli` anymore.
  - **`maj mcp`** (`crates/cli/src/mcp_cmd/`, new subcommand, PR4-PR5
    (#68-#69)): a stdio JSON-RPC server built on the official `rmcp` SDK,
    serving 10 read-only tools, 16 confirm-gated mutating tools, and 2
    resources (`majestical://thumb/{asset_id}`,
    `majestical://keyframes/{asset_id}`) — full CLI parity per the spec's
    scope decision, `catalog_init` included. Every mutating tool defaults to
    a dry-run preview; `confirm: true` executes. Only this subcommand opens
    a tokio runtime (`enable_io()` + `enable_time()`, the latter required
    for a clean shutdown on client disconnect — see lessons below); every
    other `maj` verb stays synchronous.
  - **The GUI slice, release pipeline, and version-sync check are NOT part
    of phase 7A** — the spec's third architecture section (GUI) and its
    whole "Release pipeline & CI" section are unbuilt. This is Plan B/C
    below, not a phase-7A gap.
  - **Tests**: 36 `mcp_smoke` tests (real `maj mcp` over stdio via an rmcp
    client — tool-list snapshot, per-tool dry-run/confirm pairs, wire-shape
    pins) + 39 `services_parity` tests (byte-identical CLI output through
    the extraction, diffed against the pre-extraction reference binary).
    `just ci`'s four conformance jobs are unchanged from phase 6.

**Architecture pointers**:

- `crates/services/src/` — one file per verb family: `search.rs`,
  `catalog.rs` (get_asset, volumes, catalog_init), `scan.rs`, `verify.rs`,
  `meta.rs`, `tags.rs`, `para.rs`, `ingest.rs`, `sync.rs`, `inbox.rs`,
  `inbox_manifest.rs`, `index/` (`mod.rs`, `run.rs`, `heal.rs`,
  `blob_read.rs`), `describer_config.rs`, plus shared plumbing (`app.rs`'s
  `FsApp`, `error.rs`'s `ServiceError`, `state_dir.rs`, `capability.rs`,
  `iso8601.rs`, `volume_identity.rs`, `query.rs`). `crates/cli` and (later)
  the Tauri backend both depend on this crate; it depends on
  core/catalog-sqlite/ingest/sync/index/describe — never the reverse.
- `crates/cli/src/mcp_cmd/` — `mod.rs` (the `MajServer` struct,
  `open_app`/`ensure_catalog` guards, `tool_error`/`structured_ok` result
  builders, `run_off_tokio_runtime`, `serve`), `read_tools.rs` (10 read
  tools), `write_tools.rs` (16 mutating tools + the `confirm_gate`/
  `inject_executed` dry-run/execute plumbing), `resources.rs` (the two
  `majestical://` resources). Read each module's own doc comment before
  touching it — the wire-contract and confirm-semantics rules are recorded
  there, not just in the spec.
- **The reference-binary parity harness is the extraction-refactor tool of
  record** (`crates/cli/tests/services_parity.rs`): each test runs the SAME
  args against the just-built `maj` and a pre-extraction reference binary
  at `/tmp/maj-ref`, then asserts stdout, stderr, AND exit code are
  identical. Locally the reference is opt-in (tests skip with a loud
  message if `/tmp/maj-ref` is absent); in CI, `.github/workflows/ci.yml`
  builds it automatically every run — `git merge-base origin/main HEAD`,
  detach to that commit, `cargo build -p majestical-cli`, copy the binary
  to `/tmp/maj-ref`, detach back — so every PR's parity tests run for
  real, not skipped, without a human ever building the reference by hand.
  `diff_against_ref` shares one catalog root between both binaries;
  `diff_against_ref_independent` gives each its own root for verbs whose
  second application legitimately differs from its first (e.g. `tag rm`
  is a tombstone). Reuse this harness verbatim for any future extraction
  work (the GUI's Tauri commands will want the same proof against the CLI).

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` now carries a
"Phase 7A deferrals" section: keyframe-image extraction (the resource
serves the manifest, not images); a CLI-vs-MCP divergence on
`sync location list`/`list_sync_locations` against a missing catalog
(unresolved); four MCP params that are enum-shaped strings with no
schemars enum (`tag_assets`/`move_para`'s `op`, `ingest_source`'s `dedupe`,
`set_describer`'s `backend`); two dry-run previews (`set_metadata`,
`tag_assets`) that don't validate the asset exists, unlike their real
execution paths; `scan_volume`'s dry-run count silently dropping walk
errors; the `"ascmhl"` literal repeated ~8 places instead of a shared
const; 28 MCP-invisible stderr diagnostics in `crates/services` behind
per-site `#[expect(clippy::print_stderr)]`, reachable from most tools;
`IngestRun.outcome`'s name colliding in spirit with the service layer's own
`*Outcome` convention; and — found during this closing task's mutants
triage — 8 of 16 mutating MCP tools with no functional `mcp_smoke.rs` test
beyond the roster/schema checks (`add_sync_location`, `rm_sync_location`,
`scan_volume`, `set_describer`, `set_metadata`, `sync_pull`,
`test_describer`, `inbox_process`), plus a general lesson: a tool tested
only on its `confirm: true` path can still hide an inverted dry-run guard,
since `confirm_gate`/`inject_executed` reports `executed` from the request
value, not from which branch ran. A "cargo-mutants triage (phase 7A)"
section records
the scoped mutants runs against `crates/services/src/search.rs`,
`crates/services/src/catalog.rs`, and `crates/cli/src/mcp_cmd/
write_tools.rs` and their survivor dispositions.

## Phase 7B recommendation

Per the phase 7 spec's remaining architecture/delivery items (Plan B: the
GUI slice; Plan C: the release pipeline), scope for the next design session:

- **Plan B — GUI slice** (spec's GUI architecture section): `apps/desktop`,
  Tauri 2 + Svelte 5 + Vite, no SvelteKit. Layout C (three-pane with
  inspector). `src-tauri` lives in a separate GUI workspace so headless CI
  never compiles the Tauri tree. One `#[tauri::command]` per verb the
  slice uses (`search_assets`, `get_asset`, `list_volumes`,
  `list_saved_searches`, `run_saved_search`, `catalog_init`), each a thin
  wrapper over `crates/services` returning the outcome struct as-is —
  parity by construction, same as MCP. First-run flow: no catalog required
  to reach a working app; a welcome screen calls `catalog_init` directly.
  Reuse `services_parity.rs`'s harness shape for Tauri-command-vs-CLI
  proof.
- **Plan C — release pipeline + CI matrix**: tag-triggered `tauri-action`
  draft release (macOS aarch64 + x86_64), auto-update armed from day one
  (updater keypair + `latest.json`), `cargo-about` license bundle,
  version-sync check across `Cargo.toml`/`tauri.conf.json`/`package.json`,
  and the GUI workspace building on macOS/Windows/Linux in CI from the
  first Tauri commit (release artifacts stay macOS-only; the matrix is
  feedback, not distribution).
- **Before writing GUI code**, migrate the stderr-diagnostics watchlist item
  (above) — the GUI faces the identical "stderr is invisible to this head"
  problem MCP already has; fixing it once as outcome-struct fields benefits
  both heads instead of getting re-discovered.
- Watchlist items adjacent to this phase: the four schemars-enum params (a
  GUI dropdown wants the same enum the MCP schema wants); the two
  under-validating dry-run previews (a GUI form filling in a stale asset id
  would hit the identical silent-success illusion).

Write a phase 7B spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 7A ran six PRs (#64-#69 +
   this closing PR) on this cadence.
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
10. **Local setup**: unchanged from phase 6 (`just`, `protoc`, ffmpeg,
    ImageMagick, ~2GB model cache). Phase 7A added no tool or model
    dependency; `rmcp`/`schemars`/`base64` are new Rust crate deps only.

## Phase-7A lessons worth carrying

- **Subagents park on background children that die with their turn — this
  is a recurring failure mode, not a one-off.** Phase 6's closing agent
  twice lost a `cargo-mutants` run this way; phase 7A's closing task
  therefore opened with an explicit foreground mandate (split per-file
  scopes, one at a time, no `run_in_background`, no monitors) and treated
  any tool-level auto-backgrounding (a command outliving its foreground
  timeout) as something to let finish via its own notification, never
  something to re-wrap in a monitor or sleep-poll loop. Any future brief
  handing off a long-running verification step must open with this
  mandate; a controller that notices a subagent has silently stopped
  reporting progress on one should nudge once, then replace the subagent
  rather than continuing to wait indefinitely.
- **Test the lifecycle, not just the call.** The `enable_time()` gap (a
  clean client disconnect panicking the server's worker thread instead of
  exiting 0) hid behind tests that only ever checked a tool call's
  response, never a real client's connect-call-disconnect sequence ending
  in `kill()`-style teardown. `read_tool_then_clean_stdin_close_exits_
  success` (`mcp_smoke.rs`) exists specifically because "does the call
  work" and "does the session end cleanly" are different questions with
  different failure modes.
- **The reference-binary parity harness (stdout + stderr + exit code,
  built from a CI-computed merge-base) is the extraction-refactor tool of
  record.** It caught byte-for-byte drift across three incremental
  extraction PRs without a single hand-written before/after fixture, and
  the CI merge-base step means every PR gets a real (not skipped) parity
  run without a human maintaining the reference binary. Any future
  extraction (the GUI's Tauri commands, a hypothetical phase-8 refactor)
  should reuse this shape rather than inventing golden-file fixtures.
- **The scoped-thread rule: anything that can reach a Lance
  `VectorStore`/`TextVectorStore` must run off the server's own tokio
  runtime.** Lance's store-open builds and enters its own tokio runtime
  internally; doing that from inside a thread that already has one active
  panics regardless of whether it's the same `Runtime` value. This bit
  `search_assets`/`run_saved_search` (the image-semantic layer) and
  `index_run`'s real pass; `run_off_tokio_runtime`
  (`crates/cli/src/mcp_cmd/mod.rs`) is the fix, and it is a rule for
  future tools, not a one-time patch — anything routing through
  `majestical_index`'s vector stores from inside the MCP server needs it.
- **macOS Gatekeeper makes a fresh, unsigned test binary look hung, not
  slow.** A freshly built (never-before-run) test binary can sit at ~0%
  CPU for 25-30 seconds before its first line of output while Gatekeeper's
  first-launch check runs — indistinguishable from a real deadlock by
  wall-clock alone. Before diagnosing a "hang" in a newly compiled binary
  on this machine, rule this out first (a second run of the same binary is
  instant).

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it never
  prints, and every head (CLI, MCP, future GUI) renders the same outcome
  structs rather than re-deriving behavior.
- Exit-code/error polarity is decided once in the outcome structs (phase-6
  doctrine, carried through unchanged): per-item failures are recorded
  rows inside a successful result; only operator-fixable or total failures
  become hard errors (CLI nonzero exit, MCP `isError: true`) — always with
  partial progress attached, never discarded.
- `sample_ops()` in `crates/core/src/projection.rs` is the op-variant
  absence assertion; phase 7A added zero `Op` variants (asserted: an empty
  `git diff` on `event.rs` across the whole phase). Any future phase that
  adds a variant must extend `sample_ops()` and the proptest generator.
- MCP mutating tools all default to dry-run; `confirm: true` executes.
  Read tools take no `confirm` parameter. A dry-run preview must describe
  REAL state it read, never a guess — but see the watchlist for the two
  known gaps where a preview doesn't validate as much as its real
  execution path does.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
