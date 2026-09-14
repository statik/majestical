# Majestical — Phase 7F handoff

Written at the close of Phase 7E. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase7E.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP + GUI, all at parity). The app
ships on macOS; since phase 7C the Rust workspace also builds and its tests
run on Linux, with the Apple-only derivations honestly absent there. Since
phase 7E the app is always-on: a menu-bar tray keeps a power-aware background
indexer running with no window open.

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
- Phase 7E spec (always-on tray, WebDriver e2e, `maj doctor`) + its as-built
  section: `docs/superpowers/specs/2026-08-26-phase7e-alwayson-e2e-doctor-design.md`;
  its mockups are `docs/superpowers/specs/mockups/2026-08-26-phase7e/*.html`
- Phase 7E implementation plan:
  `docs/superpowers/plans/2026-08-26-phase7e-alwayson-e2e-doctor.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main after PR #122, this closing PR pending)

**Shipped and working**:

- Phases 1-7D (see `HANDOFF-phase7E.md` for the detail): catalog
  init/scan/tag/meta/volumes/para, verified multi-destination ingest,
  unified search, blob store + thumbnails + diff-as-queue indexing, four
  conformance-gated model backends, scene keyframes + keyframe images,
  describer backends, OCR/PDF, layered text search, multi-location sync,
  inbox contributions; `crates/services` + `maj mcp`; the Tauri 2 +
  Svelte 5 desktop app with Search/Volumes/Browse/Organize/Ingest
  surfaces, notices rendering, an armed updater, the pinned TS wire
  layer, target-gated Apple seams with a `{macos, ubuntu}` Rust CI matrix.
- Phase 7E, seven chunk PRs:
  - **`maj doctor`** (#110, plus its mutants-closing follow-up #117).
    `crates/services/src/doctor.rs`'s seven checks — `ffmpeg`,
    `imagemagick`, `models`, `state_dir`, `catalog`, `blob_residue`,
    `platform` — at CLI (`maj doctor [--json]`), MCP (the `doctor` read
    tool), and, from #121, the GUI (Settings' health panel).
  - **The WebDriver e2e harness** (#112). Debug-only `tauri-plugin-wdio`
    + `tauri-plugin-wdio-webdriver`, the `apps/desktop/e2e/` WebdriverIO
    project against the embedded provider, a fixture catalog seeded by
    shelling out to the debug `maj` binary, and the `gui-e2e` CI job.
  - **Per-surface e2e flows** (#113). Search, Volumes, Browse, Organize —
    one real flow each against the fixture catalog. Ingest's flow was
    dropped (native dialogs, no test bridge); see the deferrals list.
  - **The doctor GUI panel** (#121). `doctor_report` command, wire
    fixtures, a whole-document `tauri_parity` row, and `SettingsView
    .svelte`'s health panel.
  - **Autopilot policy + power probe + scheduler loop** (#116).
    `crates/services/src/autopilot.rs`'s pure `autopilot_decision`;
    `apps/desktop/src-tauri/src/power.rs`'s `pmset` probe; `indexer.rs`'s
    background loop with the no-progress hold rule, `update_failure_
    report`, and `catch_unwind` guard; the `scheduler_state`/
    `set_throttle` commands.
  - **Tray, hide-to-tray, autostart, Always-on section** (#122). Menu-bar
    tray with a pure `menu_model` function; hide-to-tray on window close
    with a macOS activation-policy switch; `tauri-plugin-autostart` behind
    a Settings toggle, default off; `AlwaysOnSection.svelte`.
  - **This closing PR** — the phase 7E deferrals and cargo-mutants triage
    in `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, the
    spec's `## As-built (phase 7E)` section, and this handoff.
- Full deviation detail (AMENDED notes, per-PR summaries, the review-loop
  shape) is in the spec's own as-built section — read it, not just this
  handoff, before touching any phase 7E code.

**No manual GUI smoke rule is in force.** PR #112's plugin registration
change was the last hand-run smoke (recorded on that PR); once `gui-e2e`
went green on main, the rule from `HANDOFF-phase7D.md` was retired. See
"The e2e job is the smoke" in Process conventions below.

## Secrets and release state (unchanged from the 7D handoff)

The two signing secrets live ONLY in the `release` GitHub Environment; the
repository-level copies are deleted. The v0.2.0-rc1 dry run proved the
environment-scoped values end to end. Key rotation instructions:
`docs/RELEASING.md`, "The private key". Nothing in 7E touched the release
pipeline.

## Architecture pointers

Everything in the 7D handoff's pointer list still holds (the notices
carrier, `runtime.rs`'s Lance rule, the platform-capability consts, the
updater flow, `docs/RELEASING.md`, the tag alias map, the browse verbs,
the organize verbs, the ingest progress seam, the wire-fixture mechanism,
the parity harnesses). What is new:

- **`maj doctor`** (`crates/services/src/doctor.rs`). Compute-only,
  request struct in, outcome struct out, like every other services verb —
  except it is the one verb that runs with no `App`: `doctor()` builds its
  own `Notices` sink and opens the catalog itself, via `FsApp::open`, only
  for the checks that need one. Exit-code polarity: `Ok(outcome)` even when
  every check fails; findings are rows. `check_blob_residue` resolves the
  blob store root through `BlobStore::root()` and the runs dir through
  `state_dir::catalog_paths` — never a re-derived path — and scans for
  `.tmp-*` and `*.partial` orphans read-only. Reachable at all three heads:
  `maj doctor` (`crates/cli`), the MCP `doctor` read tool, and
  `doctor_report` (`apps/desktop/src-tauri/src/commands.rs:209`, one-liner
  over `doctor_report_impl`), which works before any catalog is selected.
- **The autopilot policy** (`crates/services/src/autopilot.rs`). The
  whole scheduling decision is one pure function,
  `autopilot_decision(power, throttle, pending_items) -> SchedulerDecision`
  (`:57`): `Paused` always holds; an empty queue holds `NoPendingWork`
  regardless of throttle (except `Paused`, which reports itself); `Low`/
  `Full` overrides ignore power entirely; only `Auto` reads
  `PowerState` — Low Power Mode holds, AC runs full, battery or an
  unknown source runs low. No I/O, no timers — every branch is a unit
  test, plus two proptest invariants.
- **The power probe** (`apps/desktop/src-tauri/src/power.rs`).
  `POWER_PROBE_AVAILABLE` (`:11`) is `cfg!(target_os = "macos")`.
  `parse_power_source`/`parse_low_power_mode` are pure functions over
  captured `pmset -g batt`/`pmset -g` text; `read_power_state` is the only
  cfg-gated piece (macOS shells out, everything else returns
  `Unknown`/`false`). The Low Power Mode parser accepts either Apple
  Silicon's `powermode` key (0/2 = false, 1 = true) or Intel's
  `lowpowermode` key (0/1) — no `lowpowermode` line exists on Apple
  Silicon at all.
- **The scheduler loop** (`apps/desktop/src-tauri/src/indexer.rs`). The
  `ingest.rs` pattern: one module owns `SchedulerState`/`SchedulerShared`,
  the loop thread (`spawn_loop`/`run_loop`, spawned in `setup` next to
  `restore_persisted_catalog`), and the two commands. Reads the currently
  selected catalog fresh every tick (`run_tick`, `:270`) rather than
  capturing one at spawn time, so a catalog change re-aims the loop for
  free. `TICK` = 30s, `BATCH_LIMIT` = 25 items PER KIND (ten kinds, so one
  batch can be up to 250 items), `PACE_LOW` = 5s between Low-throttle
  batches. **The no-progress hold rule** (`batch_outcome_pace`, `:205`):
  `index::run` returns `Ok` even when every item in a batch fails
  per-item, and `plan_work` never reads the failure report, so a batch
  that made no progress (`IndexRunOutcome::made_progress()`, a
  `#[must_use]` method on the outcome) holds for a full `TICK` and records
  the failure count in `last_error` — without this the loop would retry a
  permanently failing item forever. `run_batch` (`:229`) also calls
  `index::update_failure_report` after every batch, exactly as the CLI
  and MCP heads do, so `failed_last_run`/doctor stay current; the whole
  batch runs under `catch_unwind` (`ingest.rs`'s `panic_message`, made
  `pub(crate)`), so a panic lands in `last_error` instead of killing the
  thread with `running` stuck `true`. `set_throttle_impl` (`:371`)
  recomputes `last_decision` immediately from the last poll's power and
  pending count, so a throttle change reflects on the tray and in
  Settings at once rather than after up to one `TICK`.
- **The tray** (`apps/desktop/src-tauri/src/tray.rs`). `menu_model(state,
  catalog_selected) -> MenuModel` (`:178`) is the whole logic, pure and
  fully unit-tested against the pinned status strings; `build_menu` and
  `refresh` (`:205`, `:351`) are thin, untested Tauri glue built FROM the
  model, the same "logic stays testable, the glue does not" split
  `commands.rs`'s `*_impl` functions follow. `catalog_selected` is a
  second input read from `AppState` at rebuild time, not a wire field —
  `SchedulerStateOutcome` cannot distinguish "no catalog" from "not
  ticked yet" on its own. Four icon states (`TrayLook`, `:56`): Idle,
  Indexing, Paused, and Attention (a badge dot on the idle look — macOS
  template icons render alpha-only, so no tint color survives; `last_
  error.is_some()` sets it, overriding whichever of the other three a
  decision would otherwise pick). Only the `@1x` (22×22) PNGs load at
  runtime; `@2x` files are generated and committed by the root `just
  tray-icons` recipe (ImageMagick, byte-stable output) for a future
  HiDPI pass. Hide-to-tray and the activation-policy switch live in
  `lib.rs`'s `on_window_event`, not in this module. Quit is an immediate
  `app.exit(0)` — the scheduler never holds a transaction open across a
  tick, so nothing tears.
- **Autostart + the Always-on section**
  (`apps/desktop/src/lib/AlwaysOnSection.svelte`,
  `apps/desktop/src/lib/autostart.ts`,
  `apps/desktop/src/lib/scheduler-status.ts`). A sibling component to
  `SettingsView.svelte`, not folded into it — the scheduler command and
  the `tauri-plugin-autostart` JS API are two unrelated backends.
  `scheduler-status.ts`'s `statusLine` is written so an unhandled
  `SchedulerDecision` mode or `HoldReason` fails to typecheck (TS2366 on
  the function's explicit `string` return, and `HOLD_LINES` typed as
  `Record<HoldReason, string>`) — deliberately NOT a port of `tray.rs`'s
  `menu_model`: it omits the power-source second line and the Low-Power-
  Mode pending-count refinement, tray-only polish rather than information
  this line claims to give. `autostart.ts`'s read (`isEnabled`) is
  best-effort — a probe failure reads as "off"; its write (`enable`/
  `disable`) is not — a rejection is exactly what the section shows the
  user.
- **The e2e harness** (`apps/desktop/e2e/`). WebdriverIO with `@wdio/
  tauri-service`'s embedded provider (`wdio.conf.ts`) — no external
  driver, no paid key, and (deliberately) no `browser.tauri.*` bridge:
  the suite only needs plain WebDriver element queries, so going without
  the bridge means the service's window-focus recovery can't run either,
  suppressed the standard way in each spec's `before()` hook.
  `onPrepare` seeds one fixture catalog (`setup/fixture-catalog.ts`,
  shelling out to the debug `maj` binary — the same one `crates/cli`'s
  own integration tests drive) for the whole `wdio run`, not per spec
  file, and hands the app `MAJ_DESKTOP_CONFIG_DIR`/`MAJ_STATE_DIR` via
  the tauri-service's per-capability env override; `onComplete` removes
  it. One spec file per surface (`smoke.e2e.ts` plus `search`/`volumes`/
  `browse`/`organize`/`settings.e2e.ts`) — the smoke spec's `describe`
  callback sat at the project's 50-line function-length cap, which is
  why Settings coverage is its own file rather than an addition to the
  smoke loop. **"The e2e job is the smoke"**: the `gui-e2e` CI job
  (`.github/workflows/ci.yml:233`) — build the debug `maj` CLI, `pnpm
  tauri build --debug -b app --config src-tauri/tauri.e2e.conf.json`
  (the overlay config disables `createUpdaterArtifacts`, which otherwise
  makes the build fail trying to sign an updater tarball with no debug
  key), then `pnpm test` — replaces the hand-run smoke rule entirely,
  required for merge like every other job. Locally, the `tauri build
  --debug` step is the slow part (roughly 25 minutes on a cold cache);
  `wdio.conf.ts` points at a fixed bundle path
  (`src-tauri/target/debug/bundle/macos/Majestical.app/…`), so once that
  build exists, editing only spec files and re-running `pnpm test` in
  `apps/desktop/e2e` picks up the change without rebuilding the app —
  rebuild only after a Rust or Svelte change.
- **Cap rules, ratcheted again this phase**: `api.ts` sits at 640 lines
  (`chore: raise the api.ts line cap`, PR #116) with its next split named
  in the comment — the always-on/health wire-type subject moves to a
  sibling module before the number moves again. `App.test.ts`/
  `IngestView.test.ts`'s shared cap rose 360→385 (PR #122), naming
  `App.settings.test.ts` as the next split.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` now carries a
"Phase 7E deferrals" section (attributed to #109-#113, #116, #117, #121,
#122 and one infra item) and a "cargo-mutants triage (phase 7E)" section
recording three scoped runs and every survivor's disposition. Every item
the phase 7D watchlist carried forward — MCP progress notifications, CLI
ingest progress rendering, the ingest queue, Windows/Linux release
artifacts, localization — is carried again unchanged; 7E added no new
seam that would have closed any of them.

## Phase 7F recommendation

From the phase 7E deferrals, roughly in order of value:

- **`plan_work` reading `failed_last_run` before re-queuing an item.**
  The scheduler's no-progress hold keeps a permanently failing item from
  hot-looping, but it still gets re-planned and re-tried at every head —
  CLI, MCP, and every scheduler tick — forever. This is the cheapest fix
  on the list (one function's input changes) and it closes a real,
  user-visible symptom: an unfixable item (bad codec, a describer with no
  key) keeps showing up as "batch made no progress" instead of a named,
  stable Warn row doctor or status could report once and stop repeating.
- **The `whisper_gated` "MAJ_AUDIO fixture is silent" flake.** Recurring
  2 of 3 runs on `macos-latest` during phase 7E, not reproduced locally,
  currently quarantined by re-run rather than understood. A flake that
  recurs at that rate on the required matrix leg is worth a dedicated
  investigation before it either blocks a real change or gets ignored
  into normalcy.
- **The type-a-path `IngestView` affordance, and its e2e flow.** Declared
  as a gap in phase 7E rather than worked around with an invented test
  backdoor. It is a legitimate product feature on its own (an agent or a
  power user typing a path is normal, not a testing hack) that would
  incidentally close the one surface e2e still does not cover. Needs a
  mockup first, per the mockup-review convention.
- **HiDPI tray icons.** The `@2x` PNGs already exist and are committed;
  what's missing is the runtime plumbing (`tray_icon`/Tauri API support
  for a HiDPI-aware `NSImage`) to actually load them. Low risk, visible
  payoff on any Retina display, and unblocked by nothing else on this
  list.
- **Windows/Linux release artifacts** (carried since phase 7). The CI
  matrix builds and tests on Linux; the release pipeline remains
  macOS-only.
- **`api.ts` split or codegen.** The hand-written wire layer has been
  ratcheted three times now (7D) plus once more in 7E (640 lines), each
  time naming its next split truthfully rather than just raising the
  number again. A phase that either performs the always-on/health module
  split the current comment promises, or replaces the hand-written file
  with codegen from the Rust wire types, closes a debt that every
  GUI-touching phase since 7B has paid interest on.

Brainstorm fresh rather than picking from this list as written — it is
input to a 7F design session, not its output. **The mockup-review
convention worked for a third phase in a row**: both phase 7E mockups
(`health-panel.html`, `tray-menu.html`) were approved by the user before
code, and the tray mockup's approved strings were pinned byte-for-byte in
`menu_model`'s tests. Keep budgeting for mockups and the wire-gap ledger
they produce — this phase needed no new WIRE GAP entries, because
`menu_model`'s `catalog_selected` input resolved its one gap without
touching the wire struct, but the ledger convention is still worth
keeping in force for whatever 7F designs.

Write a phase 7F spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

Carried verbatim from `HANDOFF-phase7E.md`:

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly.
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
    before the next starts.** A controller that notices a subagent has
    stopped reporting progress on one should nudge once, then replace the
    subagent rather than wait indefinitely.
11. **Subagents report through `SendMessage`, not through prose.** Text a
    subagent writes outside a tool call does not reach the controller. A
    task that ends without a `SendMessage` report reads to the controller
    as a task that never finished.
12. **`origin/main` goes stale in a long session.** `git fetch origin`
    before reviewing or rebasing; a reviewer working from a stale ref will
    report conflicts and missing commits that do not exist.
13. **Local setup**: `just`, `protoc`, ffmpeg, ImageMagick, ~2GB model
    cache, Node 22, pnpm 11 for `apps/desktop`. `just gui-install` before
    any GUI recipe.

Added this phase:

14. **Local GUI verification is a four-command line, not three**: `pnpm
    check && pnpm lint && pnpm test && pnpm build`. Omitting `pnpm lint`
    cost a phase 7E chunk a CI run over a lint-only failure that local
    verification would have caught.
15. **A subagent's background build dies with its turn.** A build kicked
    off with `run_in_background` inside a subagent's own turn does not
    survive that turn ending, even if the subagent is later resumed — it
    happened twice in phase 7E's Task 9 before the team lead finished the
    task directly. Run a build the subagent must wait on in the
    foreground with a 600-second timeout instead, or hand the wait off to
    the lead.
16. **Never `gh pr merge --auto`.** It merges immediately in this repo
    rather than waiting for checks — watch CI to green, then merge
    explicitly.
17. **The session scratchpad is wiped on date rollover.** Anything a
    closing agent needs to survive past midnight (fact sheets, extracted
    source, notes) belongs in the repo or in memory, not only in the
    scratchpad.
18. **`cargo-mutants` runs foreground, one at a time, with `--in-place`
    on a warm target.** Phase 7D's closing triage copied the tree per run
    to avoid leaving a mutation behind; phase 7E ran `--in-place` instead
    against an already-built target directory, checking `git status`
    clean after each run — faster on a warm cache, with the same
    no-leftover-mutation guarantee the copy approach gave.

## Phase-7E lessons worth carrying

- **A hot-loop guard belongs wherever "success" can mean "no progress."**
  `index::run`'s `Ok` result says the batch ran, not that anything in it
  actually got written — per-item failures live in each kind's own
  `failed` list. The scheduler's naive first draft read `Ok` as license to
  re-tick immediately, which would have spun forever on a permanently
  failing item. `IndexRunOutcome::made_progress()` is now the general
  answer to "did this batch actually do anything," available to any
  future caller of `index::run` that paces itself off the result.
- **A pure decision function plus a thin untested shim scales past one
  use.** `autopilot_decision` and `tray.rs`'s `menu_model` are both this
  shape — total, unit-tested-per-branch policy functions with all the
  Tauri/OS glue built FROM their output in a separate, deliberately
  untested function. Every reviewer round on both files found real gaps
  in the pure function and zero in the glue, which is the split earning
  its keep.
- **A wire gap does not always need a new field.** `menu_model` needed to
  know whether a catalog was selected, which `SchedulerStateOutcome`
  cannot represent on its own (it can't distinguish "no catalog" from
  "not ticked yet"). Rather than adding a field to the outcome struct,
  the fix was a second function parameter read from `AppState` at
  rebuild time — a reminder that "declare it as a WIRE GAP" (the phase 7D
  lesson) is about not inventing a value, not a rule that every gap must
  grow the wire struct.
- **Platform literals should be captured from a real machine, and the gap
  named when they can't be.** The Apple Silicon `powermode` values in
  `power.rs`'s tests are captured output, dated and machine-identified in
  a comment; the Intel `lowpowermode` values are Apple's documented
  format, used because no Intel Mac was available, and that fact is on
  the watchlist rather than left to look like it was also captured.
- **A cheap survivor found during review does not have to wait for the
  close-of-phase triage task.** PR #117 closed a `doctor.rs` mutants
  survivor between chunks 4 and 5, out of the plan's own Task-16-at-close
  order, because it surfaced during #121's review and was cheaper to fix
  immediately than to carry forward. The standing "cargo-mutants at
  close" task is still worth doing — it caught real gaps in `power.rs`/
  `indexer.rs` no earlier review found — but it doesn't own every mutant
  fix in the phase.
- **"The e2e job is the smoke" is only true once it is green on the
  branch that changes what it covers.** The manual-smoke rule stayed in
  force through the one PR (#112) that changed `lib.rs`'s plugin
  registration before the job existed to catch a regression in that same
  change, and was retired only after. A new automated gate replacing a
  manual one needs to prove itself on the exact class of change it's
  meant to replace before the manual step stops.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it never
  prints, and every head (CLI, MCP, GUI) renders the same outcome structs
  rather than re-deriving behavior. `doctor` is the one verb built to run
  without an `App`; every other verb still requires one.
- Warnings ride the outcome, not stderr — on the failure path too, since
  7C. A new diagnostic in `crates/services` goes to the notices sink; a
  verb whose sink is local attaches it on `Err` via `attach_on_err`;
  `print_stderr` is denied in that crate with no per-site exemptions.
- Exit-code/error polarity is decided once in the outcome structs (phase-6
  doctrine, unchanged): per-item failures are recorded rows inside a
  successful result; only operator-fixable or total failures become hard
  errors (CLI nonzero exit, MCP `isError: true`) — always with partial
  progress attached, never discarded. `doctor` is the sharpest instance
  of this: it returns `Ok` even when every check fails, because the
  findings ARE the result.
- Platform selection is `cfg(target_os)`, never a cargo feature — features
  are additive and user-selectable, and a Linux build claiming
  `apple-native` must not be representable. A capability a build lacks is
  named and counted (`AVAILABLE` consts, `PlatformUnavailable`,
  `POWER_PROBE_AVAILABLE`), never silently zero.
- `sample_ops()` in `crates/core/src/projection.rs` is the op-variant
  absence assertion. Phase 7E added no new `Op` variant; any future phase
  that does must extend `sample_ops()` and the proptest generator with it.
- MCP mutating tools all default to dry-run; `confirm: true` executes. Read
  tools take no `confirm` parameter — `doctor` is one of these. A dry-run
  preview must describe REAL state it read, never a guess, and must not
  promise what its own execute path would refuse.
- Tauri commands stay one-liners over `*_impl` functions. Logic in a
  command wrapper is logic no test can reach — `doctor_report`,
  `scheduler_state`, and `set_throttle` all follow this; `tray.rs`'s
  `menu_model`/`build_menu` split is the same rule applied to non-command
  code. A new command's wire shape gets a fixture on both sides or it is
  unpinned.
- A background thread that owns mutable state must never leave that state
  stuck on a panic. `indexer.rs`'s `run_batch` wraps its batch in
  `catch_unwind` for exactly this reason — before that guard, a panic
  inside `index::run` would have left `running` `true` forever with
  nothing left to flip it back.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
