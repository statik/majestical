# Majestical — Phase 7E handoff

Written 2026-08-19 at the close of Phase 7D. Read this first; everything else
is linked from here. Supersedes `HANDOFF-phase7D.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP + GUI, all at parity). The app
ships on macOS; since phase 7C the Rust workspace also builds and its tests
run on Linux, with the Apple-only derivations honestly absent there.

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-6 specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 7 spec (services + MCP + GUI):
  `docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md`
- Phase 7B spec (GUI slice + release pipeline) + its as-built section:
  `docs/superpowers/specs/2026-08-04-phase7b-gui-release-design.md`
- Phase 7C spec (infra: notices-on-error, wire pinning, target-gating) + its
  as-built section: `docs/superpowers/specs/2026-08-09-phase7c-infra-design.md`
- Phase 7D spec (Browse / Organize / Ingest surfaces + keyframe images) + its
  as-built section:
  `docs/superpowers/specs/2026-08-12-phase7d-surfaces-design.md`; its mockups
  are `docs/superpowers/specs/mockups/2026-08-12-phase7d/*.html`
- Phase 7D implementation plan:
  `docs/superpowers/plans/2026-08-12-phase7d-surfaces.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #102, this closing PR pending)

**Shipped and working**:

- Phases 1-7C (see `HANDOFF-phase7D.md` for the detail): catalog
  init/scan/tag/meta/volumes/para, verified multi-destination ingest + ASC
  MHL verify, unified search with saved searches, blob store + thumbnails +
  diff-as-queue indexing, SigLIP 2 + MiniLM + whisper behind four
  conformance gates, scene keyframes, describer backends, OCR/PDF, layered
  text search, multi-location sync, inbox contributions; `crates/services`
  + `maj mcp`; the Tauri 2 + Svelte 5 desktop app with search/volumes/
  inspector surfaces, notices rendering (failures included), an armed
  updater and the tag-triggered release pipeline; the pinned TS wire layer;
  target-gated Apple seams with a `{macos, ubuntu}` Rust CI matrix.
- Phase 7D, six chunk PRs:
  - **Keyframe images** (#97). `Derivation::KeyframeImage` /
    `KeyframeImagesComplete` blobs, an ffmpeg-backed `extract_keyframe_webp`,
    a `plan_keyframe_images` planner pass with its own `index status` counts,
    and serving at every head (MCP image blocks, `thumb://` routing, the
    Inspector strip). Closes the phase-7A keyframe-image deferral.
  - **Browse** (#98). `services::browse` — `browse_tree` (folder trees,
    recursive counts, offline volumes named once) and `browse_list` (scope,
    kind filter, three sorts, limit/offset) — rendered through `search.rs`'s
    `SearchHit` row; `maj browse tree|list`, two MCP read tools, wire
    fixtures, and `BrowseView` + `Filmstrip` with hover-scrub.
  - **The tag CRDT op** (#99). `Op::TagRenamed` plus the projection's alias
    map and resolve rule; `tags_list`, `tag_rename`, `tag_merge`,
    `tags_assign` and `para_file` at all three heads.
  - **Organize** (#100). The two-column surface (PARA tree, tag vocabulary),
    the archive dry-run modal over a new `list_mounted_roots` command, and
    `SelectionBar` in Browse and Search for bulk tag/file from a grid.
  - **The progress seam** (#101). `ProgressEvent` + a cancel flag reach the
    engine as one `RunControl`; the journal gained a `RunStarted` record;
    `ingest_unfinished` lists resume candidates newest-first.
  - **The Ingest surface** (#102). Setup board, live run, completion card,
    plus the resume banner; the run lives in the backend and is rejoined on
    mount.

**The user personally ran the manual GUI smoke on 2026-08-19** (7D close,
recorded on PR #102). The standing rule holds: repeat the smoke by hand
whenever `lib.rs`'s plugin registration, `tauri.conf.json`, or a surface's
mount path changes; no automated test covers app launch end to end.

## Secrets and release state (unchanged from the 7D handoff)

The two signing secrets live ONLY in the `release` GitHub Environment; the
repository-level copies are deleted. The v0.2.0-rc1 dry run proved the
environment-scoped values end to end. Key rotation instructions:
`docs/RELEASING.md`, "The private key". Nothing in 7D touched the release
pipeline.

## Architecture pointers

Everything in the 7D handoff's pointer list still holds (the notices
carrier, `runtime.rs`'s Lance rule, the platform-capability consts, the
updater flow, `docs/RELEASING.md`). What is new:

- **The tag alias map** — `Projection::tag_aliases`
  (`crates/core/src/projection.rs:240`), an LWW map from retired name to
  target, written only by `Op::TagRenamed` (`:322`). The resolve rule is
  `resolve_alias`/`tag_alias_target` (`:482`, `:503`): follow the chain to its
  final target, with a bounded walk so a cycle cannot hang a read. Both
  `tag rename` and `tag merge` emit the SAME op — a merge is a rename onto
  an occupied name — and the difference lives entirely in the guards:
  `rename_plan` (`crates/services/src/tags.rs:532`) refuses an occupied
  target, `merge_plan` (`:561`) requires one. Read those two before touching
  anything tag-shaped. `Touched::Tag` invalidates by rewriting the whole
  `tags` table (`crates/catalog-sqlite/src/apply.rs:161`, `rebuild_tags`),
  because one rename can move every asset's effective tags at once.
- **The keyframe-image derivation chain**: `plan_keyframe_images`
  (`crates/index/src/work.rs:561`) queues one item per manifest timestamp
  that has no image blob yet, gated on ffmpeg; the runner calls
  `extract_keyframe_webp` (`crates/index/src/keyframe_images.rs:14`); when
  every timestamp in the manifest has an image, a
  `KeyframeImagesComplete` marker blob is written so the pass stops
  re-planning it. The chain is manifest → images → marker; the manifest is
  produced by the keyframes pass and remains its own signal.
- **The browse verbs**: `crates/services/src/browse.rs`. `browse_tree`'s
  per-folder `recursive_count` dedupes by asset id, which is why the
  sidebar badge predicts the grid's `count` exactly; `browse_list` picks
  one representative instance per asset (`dedupe_by_asset`/`is_better`:
  highest mtime, tie to the smaller path) and the row's `size`/`mtime_ms`/
  `kind` come from THAT instance while `name` comes from the asset's
  catalog summary. Both verbs use `volumes::volume_is_online`; `search.rs`
  still has its own predicate (watchlist).
- **The organize verbs and their plan functions**: `tags_list`
  (`tags.rs:440`), `tag_rename` (`:604`), `tag_merge` (`:623`),
  `tags_assign` (`:653`), `para_file` (`crates/services/src/para.rs:366`).
  The two `*_plan` functions are public on purpose: they answer "what would
  this do" with no event emitted, which is what the MCP dry-run previews
  call so a preview can never promise what the execute path would refuse.
- **The ingest progress seam**: `ProgressEvent`
  (`crates/ingest/src/engine.rs:129`, seven variants) and `RunControl {
  progress, cancel }` (`:197`). Cancellation is checked between files, so a
  cancelled run's outcome is partial-but-consistent, never torn. Two rules
  that cost the phase a review round each: **`run_stopped` is not the
  outcome** — it says the thread ended, and what it ended WITH comes from a
  separate `ingest_state` poll — and **the run id is parsed out of a notice
  line** (`run_id_from_notice`, `apps/desktop/src-tauri/src/ingest.rs:270`),
  which makes that notice's leading token a wire contract. Reword it and
  the GUI loses the run.
- **The desktop file map** (`apps/desktop/src-tauri/src/`): `commands.rs`
  is the read/organize command surface, still one-liners over `*_impl`;
  `ingest.rs` is the whole ingest subsystem (five commands, the job thread,
  the `BytesThrottle` that keeps the webview from drowning in
  `BytesCopied`, the notice parser); `config.rs` holds the selected
  catalog; `thumb_protocol.rs` serves `thumb://` for thumbs and keyframe
  images. Splitting `ingest.rs` out of `commands.rs` is the pattern for the
  next subsystem: a module per long-running thing, not one growing file.
- **The wire-fixture mechanism grew with the phase**: 22 Rust tests in
  `apps/desktop/src-tauri/tests/wire_fixtures.rs` against 21 committed
  JSON files in `apps/desktop/src/lib/fixtures/`, each assigned to its
  `api.ts` interface by `fixtures.test.ts`. `MAJ_UPDATE_FIXTURES=1 cargo
  test --test wire_fixtures` regenerates. A new command's outcome struct
  gets a fixture on both sides or its wire shape is unpinned.
- **The parity harnesses** (both extended this phase):
  `crates/cli/tests/services_parity.rs` — 52 tests diffing stdout, stderr
  and exit code against `/tmp/maj-ref`, now including browse, `tags list`,
  `tag rename`/`merge`/`assign`, `para file` and `ingest unfinished`.
  Mutating verbs pick one of three helpers by how the verb behaves on a
  second application (`diff_against_ref` for idempotent ones,
  `diff_against_ref_independent` for one-shot ones, `diff_against_ref_
  with_between` when shared filesystem state must be restored) — read the
  helper docs before adding a row. `apps/desktop/src-tauri/tests/
  tauri_parity.rs` compares GUI command payloads against `maj … --json`;
  for the 7D read verbs the WHOLE document is the contract, because those
  verbs print the outcome struct as-is. Both loud-skip when their
  reference is missing.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` now carries a "Phase
7D deferrals" section (23 items, each attributed — the ingest queue, CLI
ingest progress rendering, the carried MCP-progress and menu-bar items,
PARA click-through, grid virtualization, prefetch tuning, seven declared
WIRE GAPS where a mockup asks for a field no Rust row carries, the ffmpeg-
less CI job, `api.ts` codegen with its three ratcheted oxlint caps, and the
rest) and a "cargo-mutants triage (phase 7D)" section recording four scoped
runs and every survivor's disposition. The phase-7A keyframe-image deferral
is marked closed there with PR #97, as is its 7B restatement.

## Phase 7E recommendation

From the phase-7 spec's `## Deferred` list
(`docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md:271`)
and the watchlist, roughly in order of value:

- **Menu-bar indicator with indexing throttle** (carried since phase 7).
  The one piece of the app that runs when no window is open.
- **GUI end-to-end tests via WebDriver.** Every surface now has unit and
  wire coverage; nothing drives the real app. This is also what would
  retire the standing manual-smoke rule.
- **`maj doctor`** — one verb that checks the environment (ffmpeg, models,
  state dir, catalog health) and names each gap with its remedy.
- **Windows/Linux release artifacts** (the CI matrix builds; the release
  pipeline is macOS-only).
- **The ingest queue** (new in 7D's deferrals): multiple pending jobs,
  reordering, persistence across restarts.

Brainstorm fresh rather than picking from this list as written — it is
input to a 7E design session, not its output. **The mockup-review
convention from 7D worked and should be repeated**: the user reviewed
standalone HTML mockups of each surface before any code, and every place
the mockup asked for data the backend could not supply became a declared
WIRE GAP in the plan instead of an invented field. Budget for the mockups
and for the gap ledger they produce.

Write a phase 7E spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 7D ran six chunk PRs plus
   this closing one.
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
10. **`cargo-mutants` runs FOREGROUND, one at a time — no
    `run_in_background`, no monitors, no sleep-polling, each run finishing
    before the next starts.** Carried verbatim from 7A, which carried it
    from phase 6, where a closing agent twice lost a run to a background
    child dying with its turn. A controller that notices a subagent has
    stopped reporting progress on one should nudge once, then replace the
    subagent rather than wait indefinitely. Any future brief handing off a
    long-running verification step opens with this mandate.
11. **Subagents report through `SendMessage`, not through prose.** Text a
    subagent writes outside a tool call does not reach the controller. A
    task that ends without a `SendMessage` report reads to the controller as
    a task that never finished.
12. **`origin/main` goes stale in a long session.** `git fetch origin`
    before reviewing or rebasing; a reviewer working from a stale ref will
    report conflicts and missing commits that do not exist.
13. **Local setup**: unchanged from 7A (`just`, `protoc`, ffmpeg,
    ImageMagick, ~2GB model cache) plus Node 22 and pnpm 11 for
    `apps/desktop`. `just gui-install` before any GUI recipe.

## Phase-7D lessons worth carrying

- **Declare wire gaps in the plan; do not invent fields.** Mockup review
  produced seven places where the picture asked for data no Rust row
  carries (a duration chip, per-node PARA counts, a run's PARA node, a
  failure's destination, free space, run duration, the now-row's verb).
  Each became an AMENDED note naming the gap, a surface that renders
  without it, and a watchlist item — instead of a placeholder value that
  would later read as a bug. The ledger is worth the ceremony: at close it
  is already written.
- **Reviewers' mutation probes catch generator-lucky tests that
  `cargo-mutants` also finds — but earlier and cheaper.** The browse
  representative-instance test asserted the right thing about a fixture
  whose two orderings agreed, so "keep the first instance" passed it. A
  fixture whose orderings are deliberately opposed is the fix, and the
  general rule is: when a test pins a tie-break or a pick rule, make the
  wrong answer differ from the right one on every axis the fixture has.
- **A terminal event is not an outcome.** The GUI treated the run thread's
  `run_stopped` as the result, so a cancelled run and a completed one
  looked the same. Any component with a lifecycle needs the outcome fetched
  from the thing that owns it, not inferred from the last event it emitted.
- **Lint caps are ratchets, and each one names its next split.** Three
  oxlint `max-lines` caps rose this phase; every override in
  `apps/desktop/.oxlintrc.json` sits two or three lines above the file as
  it stands and its comment names the specific extraction that comes next.
  A cap raised without that sentence is just permission to grow.
- **A parsed notice is a wire contract — pin it with a test and say so.**
  The GUI recovers a run id by parsing the leading token of a notice line.
  That is a fine seam, but only because a test pins the format and both
  sides' comments name each other. An unpinned prose format is a silent
  dependency waiting to be reworded.
- **Amend the plan in place.** Five corrections landed as AMENDED notes
  written into the task they correct, not as errata appended at the end.
  Implementers read the correction before the instruction, and the closing
  agent could reconstruct every deviation from the plan alone.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it never
  prints, and every head (CLI, MCP, GUI) renders the same outcome structs
  rather than re-deriving behavior.
- Warnings ride the outcome, not stderr — on the failure path too, since
  7C. A new diagnostic in `crates/services` goes to the notices sink; a
  verb whose sink is local attaches it on `Err` via `attach_on_err`;
  `print_stderr` is denied in that crate with no per-site exemptions.
- Exit-code/error polarity is decided once in the outcome structs (phase-6
  doctrine, unchanged): per-item failures are recorded rows inside a
  successful result; only operator-fixable or total failures become hard
  errors (CLI nonzero exit, MCP `isError: true`) — always with partial
  progress attached, never discarded.
- Platform selection is `cfg(target_os)`, never a cargo feature — features
  are additive and user-selectable, and a Linux build claiming
  `apple-native` must not be representable. A capability a build lacks is
  named and counted (`AVAILABLE` consts, `PlatformUnavailable`,
  planner-exclusion counters), never silently zero.
- `sample_ops()` in `crates/core/src/projection.rs` is the op-variant
  absence assertion. Phase 7D added exactly one `Op` variant
  (`TagRenamed`), and extended `sample_ops()` and the proptest generator
  with it. Any future phase that adds one must do the same.
- MCP mutating tools all default to dry-run; `confirm: true` executes. Read
  tools take no `confirm` parameter. A dry-run preview must describe REAL
  state it read, never a guess — and must not promise what its own execute
  path would refuse (this is why the organize verbs' `*_plan` functions are
  public: the preview calls the same guards the execute path does).
- Tauri commands stay one-liners over `*_impl` functions. Logic in a command
  wrapper is logic no test can reach. A new command's wire shape gets a
  fixture on both sides or it is unpinned.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
