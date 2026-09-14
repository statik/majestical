# Majestical Phase 7E — Always-on tray, WebDriver e2e, `maj doctor`

Written 2026-08-26 from a brainstorming session against
`docs/superpowers/HANDOFF-phase7E.md`. Parent spec:
`docs/superpowers/specs/2026-07-28-majestical-design.md`. Phase-7 spec (the
Deferred list this phase draws from):
`docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md`.

## Scope decisions (from design session)

- Three themes, chosen from the handoff's ranked candidates: **the always-on
  app** (menu-bar tray + power-aware indexing throttle + start at login),
  **GUI end-to-end tests via WebDriver** (smoke + one flow per surface,
  retiring the standing manual-smoke rule), and **`maj doctor`** at all
  three heads.
- Lifecycle model: **tray-mode Tauri app plus start at login**. The existing
  desktop app gains a menu-bar (tray) icon; closing the last window hides to
  tray instead of quitting; a launchd login item (via `tauri-plugin-autostart`)
  is offered behind a Settings toggle, default **off**. No separate daemon
  process — one app, one codebase.
- Throttle model: **power/battery-aware policy** with a manual override.
  Auto (default): AC power → full speed, battery → low, macOS Low Power
  Mode → hold. Manual override Paused / Low / Full always wins over Auto.
- Scheduler placement: **policy in services, loop in the app**. The
  decision logic is a pure, tested function in `crates/services`; the
  desktop app hosts the timer thread in a new module following the
  `ingest.rs` "module per long-running thing" pattern.
- E2E mechanism: **WebdriverIO `@wdio/tauri-service` with the embedded
  provider** (`tauri-plugin-wdio-webdriver` + `tauri-plugin-wdio`), the
  documented macOS path — no external driver, no paid key. Depth: launch
  smoke plus one representative flow per surface.
- `doctor` surfaces at **CLI + MCP + GUI** from day one — full head parity,
  no parity debt.
- Explicitly deferred again (watchlist, this spec's attribution): MCP
  long-running-tool progress notifications, CLI ingest progress rendering,
  the ingest queue, Windows/Linux release artifacts, localization.

## Architecture

### Wave 1 — `doctor`

- `crates/services/src/doctor.rs`: compute-only, request struct in, outcome
  struct out. `DoctorRequest { catalog: Option<PathBuf> }` (catalog checks
  are skipped with a named notice when no catalog is selected/given).
  `DoctorOutcome { checks: Vec<DoctorCheck>, notices: Vec<Notice> }`;
  `DoctorCheck { name, status, detail, remedy }` where `status` is a
  three-valued enum `Ok | Warn | Fail` (serialized as strings, named
  fields — MCP wire doctrine, no tuples).
- Checks (each names its remedy; all read-only, no auto-fix):
  - `ffmpeg` on PATH and runnable (`ffmpeg -version`).
  - ImageMagick present (same probe shape).
  - Model cache: state-dir models present and complete per the four
    conformance gates' expectations; a missing model names the fetch
    command.
  - State dir exists and is writable.
  - Catalog health: opens, projection replays, SQLite present/rebuildable.
  - Blob-store truncated-tail residue check (the phase-7 spec's named
    candidate).
  - Platform capabilities: report the `AVAILABLE` consts so a Linux build
    honestly lists its absent Apple seams as `Warn` with "expected on this
    platform" detail, never `Fail`.
- Exit-code polarity per phase-6 doctrine: `doctor` succeeds (exit 0) when
  it ran its checks, even if checks failed — the findings ARE the result.
  A hard error is reserved for "could not check" (e.g. unreadable state
  dir). CLI human rendering prints one row per check with status glyph and
  remedy; `--json` prints the outcome struct as-is.
- MCP: read tool `doctor`, no `confirm` parameter. GUI: health panel (Wave
  3) renders the same rows.

### Wave 2 — WebDriver e2e harness + smoke suite

- Debug-build-only integration of `tauri-plugin-wdio-webdriver` and
  `tauri-plugin-wdio` (behind `#[cfg(debug_assertions)]` registration so
  release binaries carry no test server).
- `apps/desktop/e2e/`: WebdriverIO project (pnpm workspace member) with
  `wdio.conf.ts` using the embedded provider. Node 22 / pnpm 11, oxlint
  covered like the rest of the TS surface.
- Fixture catalog: test setup drives the built `maj` CLI against tiny
  committed sample media (reusing the repo's existing test assets where
  possible) into a temp state dir + temp catalog; the app is launched with
  env pointing at that catalog. Same philosophy as the parity harnesses:
  the fixture is generated, not committed.
- Suite:
  - Launch smoke: window appears; each of the five surfaces (Search,
    Volumes, Browse, Organize, Ingest) mounts and renders real data
    without console errors.
  - One flow per surface: a search query returning a known hit; a browse
    tree click-through to a grid; a tag assign from the grid via
    SelectionBar; an ingest **dry-run** (setup board → plan preview; no
    real copy in CI); volumes list showing the fixture volume.
- CI: new job on the macOS runner, required for merge like the existing
  jobs. On its first green run the standing manual-GUI-smoke rule is
  retired and replaced by: "the e2e job is the smoke." The rule's trigger
  list (plugin registration, `tauri.conf.json`, mount paths) becomes the
  e2e suite's explicit coverage checklist.

### Wave 3 — doctor GUI panel (mockup-gated)

- Standalone HTML mockup first (7D convention), reviewed by the user
  before code; every field the mockup wants that no Rust row carries
  becomes a declared WIRE GAP, never an invented value.
- A `doctor` Tauri command (one-liner over `doctor_impl`), wire fixture on
  both sides, tauri-parity row. Panel reachable from Settings and from the
  tray menu's "Health…" item (Wave 4 wires the tray entry).

### Wave 4 — tray, policy, autostart

- **Policy** (`crates/services`, pure): `fn autopilot_decision(power:
  PowerState, override_: ThrottleOverride, pending: PendingCounts) ->
  SchedulerDecision`. `PowerState { source: Ac | Battery | Unknown,
  low_power_mode: bool }`; `ThrottleOverride { Auto, Paused, Low, Full }`;
  `SchedulerDecision { RunFull, RunLow, Hold(HoldReason) }`. Every arm
  unit-tested; `HoldReason` names why (paused, low-power, no work) so the
  tray can say it honestly.
- **Power probe**: `cfg(target_os = "macos")` seam wrapping
  `IOPSCopyPowerSourcesInfo` (AC vs battery) and
  `NSProcessInfo.lowPowerModeEnabled`, with an `AVAILABLE` const; the
  non-macOS probe returns `Unknown`/`false` and the capability is named,
  never silently zero. Platform selection by `cfg(target_os)`, never a
  cargo feature.
- **Scheduler loop**: new `apps/desktop/src-tauri/src/indexer.rs` (the
  `ingest.rs` pattern — commands, the loop thread, state, in one module).
  Ticks on a timer; each tick asks the policy, then either holds or runs
  one **small batched** index run via the existing `IndexRunRequest {
  limit, threads }` knobs: Low = `threads: 1` plus inter-batch pacing,
  Full = default threads. Pause/override takes effect at batch
  boundaries — a paused scheduler is partial-but-consistent, never torn
  (the ingest-cancel doctrine applied to indexing without touching the
  engine). Tray progress text comes from the existing `index status`
  counts, polled, not from a new progress seam.
- **Tray**: Tauri 2 tray API. Icon states: idle / indexing / paused. Menu:
  status line ("Indexing — 214 items pending" / "Idle" / "Paused (Low
  Power Mode)"), override radio group (Auto / Paused / Low / Full), "Open
  Majestical", "Health…", "Quit". Closing the last window hides to tray;
  Quit is explicit and stops the scheduler between batches.
- **Autostart**: `tauri-plugin-autostart`, Settings toggle, default off.
- **Mockups first** for the tray menu states (HTML approximation is fine
  for a menu; the point is agreeing on states and wording before code).
- `lib.rs` plugin registration changes here — which is why this wave lands
  after Wave 2's e2e suite exists to cover it.

### Shared shell changes

- Settings gains the autostart toggle and the health panel entry.
- `api.ts` grows the doctor and scheduler wire types under the existing
  ratcheted oxlint caps; if a cap must rise, its comment names the next
  split (7D ratchet rule).

## Error handling

- `doctor` findings are rows, not errors (polarity doctrine above).
- Scheduler failures (an index batch returning `Err`) surface as notices
  on the tray ("last run failed — open Health") and hold the loop with
  backoff rather than hot-looping; the error is kept for the health panel.
- E2E failures in CI block merge; a flaky test is fixed or quarantined
  with a watchlist entry, never retried-until-green silently.

## Testing

- Policy function: exhaustive unit tests per arm plus a proptest that
  override-wins-over-auto holds for all inputs.
- Power probe: trait-mocked in scheduler tests; the real macOS probe gets
  a cfg-gated smoke test (7C pattern: the gate and the coverage gap
  recorded together).
- Doctor checks: each check gets a test that forces its `Warn`/`Fail`
  (missing binary on PATH via env manipulation, unwritable dir, truncated
  blob) — every guard ships with the test that fails when it's deleted.
- Wire fixtures for every new command outcome (`doctor`, scheduler
  state/override), both sides, `MAJ_UPDATE_FIXTURES=1` regeneration.
- Parity rows: `services_parity.rs` for `maj doctor` (idempotent helper),
  `tauri_parity.rs` for the doctor command payload.
- The e2e suite is itself the new test layer; its assertions target
  user-visible outcomes (rendered rows, navigation), not implementation
  details.
- `cargo-mutants` scoped runs at close over `doctor.rs` and the policy
  module — foreground, one at a time (standing mandate).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **Chunk 1**: `services::doctor` + `maj doctor` + MCP `doctor` tool +
   fixtures + parity rows.
2. **Chunk 2**: e2e harness (plugins, wdio project, fixture-catalog setup)
   + launch smoke + CI job. Manual-smoke rule retired on green.
3. **Chunk 3**: per-surface e2e flows (five).
4. **Chunk 4**: doctor GUI panel (mockup → command → panel).
5. **Chunk 5**: policy + power probe + scheduler loop (headless: services
   + indexer.rs, no tray yet).
6. **Chunk 6**: tray + hide-to-tray + autostart + Settings toggle + tray
   mockups' states wired; e2e extended with a tray-adjacent check where
   drivable.
7. **Closing PR**: watchlist updates, mutants triage, handoff for 7F.

## Deferred (watchlist items with this spec's attribution)

- MCP long-running-tool progress notifications (carried again; the tray
  polls `index status`, so no new seam was needed).
- CLI ingest progress rendering (carried).
- The ingest queue (carried).
- Windows/Linux release artifacts, signing, distribution; localization
  (carried).
- An index-engine cancel/progress seam (`RunControl` for indexing): the
  batched-`limit` approach makes it unnecessary this phase; a future
  phase wanting mid-batch cancel or true progress events adds the seam
  then.
- Battery-threshold policy refinements (e.g. hold below 20%): the policy
  function's shape admits it; not built until asked for.

## As-built (phase 7E)

What shipped, where it differs from the design above. Written as what IS,
not as a change log. Seven chunk PRs squash-merged (or, for #122, pending
merge behind this closing PR) after green CI, plus this closing one.

**PR #109 — spec + plan** (docs). This spec and
`docs/superpowers/plans/2026-08-26-phase7e-alwayson-e2e-doctor.md`,
written from `docs/superpowers/HANDOFF-phase7D.md`.

**PR #110 — `maj doctor`: services verb + CLI + MCP** (chunk 1).
`crates/services/src/doctor.rs`'s seven checks (`ffmpeg`, `imagemagick`,
`models`, `state_dir`, `catalog`, `blob_residue`, `platform`), each a
private `fn check_*`, emitted in one documented, test-pinned order; `maj
doctor [--json]`, the MCP `doctor` read tool, and a `services_parity` row.
Findings are rows — `doctor` returns `Ok` even when every check fails,
per the phase-6 exit-code polarity doctrine.

**PR #111 — Rust 1.98 clippy hotfix**. Unrelated to the phase's own
scope: `chunks_exact` → `as_chunks` and an unused `async_trait` impl, to
keep the workspace's zero-warnings baseline current against a toolchain
bump that landed mid-phase.

**PR #112 — WebDriver e2e harness** (chunk 2). `tauri-plugin-wdio` +
`tauri-plugin-wdio-webdriver` registered behind `#[cfg(debug_assertions)]`
in `apps/desktop/src-tauri/src/lib.rs`, so a release binary carries no
listening test server; the `apps/desktop/e2e/` WebdriverIO project with
the `@wdio/tauri-service` embedded provider; a fixture catalog seeded by
shelling out to the debug `maj` binary; the launch smoke spec; and the
`gui-e2e` CI job, required for merge. Its green run on main retired the
standing manual-GUI-smoke rule from `docs/superpowers/HANDOFF-phase7D.md`
— see "the e2e job is the smoke" below.

**PR #113 — per-surface e2e flows** (chunk 3). Search, Volumes, Browse,
and Organize each got a spec exercising one real flow against the
fixture catalog; the Ingest flow was dropped (native OS dialogs have no
test bridge), a gap declared rather than worked around with an invented
backdoor.

**PR #117 — doctor mutants closed with hermetic seams**. Landed between
chunks 4 and 5 in commit order rather than at phase close — see
"Deviations" below for why the plan's own Task-16-at-close ordering
did not hold in practice.

**PR #121 — doctor GUI panel** (chunk 4). The `health-panel.html` mockup,
user-reviewed before code; the `doctor_report` Tauri command
(`doctor_report_impl` over `majestical_services::doctor::doctor`, working
before any catalog is selected — the one command that does); wire
fixtures and a `tauri_parity` row comparing the whole document against
`maj doctor --json`; and `SettingsView.svelte`'s health panel, one row
per check in the outcome's own order.

**PR #116 — autopilot policy + power probe + scheduler loop** (chunk 5,
headless). `crates/services/src/autopilot.rs`'s pure
`autopilot_decision(power, throttle, pending_items)` — the whole policy
is the function shown in the design above, unchanged; `apps/desktop/
src-tauri/src/power.rs`'s `pmset`-backed probe, parsing both the Intel
`lowpowermode` and Apple Silicon `powermode` keys; and `indexer.rs`'s
loop thread (`TICK` 30s, `BATCH_LIMIT` 25 items per kind, `PACE_LOW` 5s),
plus the `scheduler_state`/`set_throttle` commands. The no-progress hold
rule, the per-batch `update_failure_report` call, and the `catch_unwind`
guard (all in the Task 12 amendment below) shipped in this same PR after
a spec review caught the hot-loop risk before merge.

**PR #122 — tray, hide-to-tray, autostart, Always-on section** (chunk 6;
open, pending merge behind this closing PR). `tray.rs`'s pure `menu_model`
function and its thin `build_menu`/`refresh` shim; the four `TrayLook`
icon states; `on_window_event`'s hide-to-tray with a macOS activation-
policy switch so no zombie Dock icon remains; `tauri-plugin-autostart`
wired to a Settings toggle, default off; and `AlwaysOnSection.svelte` +
`scheduler-status.ts` + `autostart.ts`.

**Closing PR (this one)** — this section, the phase 7E deferrals and
cargo-mutants triage in `docs/superpowers/plans/2026-07-29-phase2-
watchlist.md`, and `docs/superpowers/HANDOFF-phase7F.md`.

### Deviations from the design above

**`blob_residue` scans interrupted-write temp files, not `heal.rs`**
(PR #110, plan's Task 1 AMENDED note). The design's "blob-store
truncated-tail residue check" pointed at `heal.rs`, which turned out to
be a private, MUTATING blob↔`text_fts` healer, not a detector — doctor
must never mutate. The real check walks the blob store root (via
`BlobStore::root()`, never a re-derived path) for `.tmp-{pid}-{seq}`
orphans, plus `*.partial` files under the state dir's runs directory —
both are what a crash strands mid-write.

**The volumes e2e fixture is always offline** (PR #113, plan's Task 5
AMENDED note). `volume_is_online` reads a `--volume`-labeled id as
online only when `/Volumes/<label>` is a real mount, which a scanned
temp directory never is in any environment including CI. The spec
asserts the real offline badge instead of an online one.

**The ingest e2e flow was dropped, not adapted** (PR #113, plan's Task 6
AMENDED note; see the deferrals list). `IngestView`'s pickers go through
native OS dialogs with no text fallback and the suite has no dialog
bridge by design; the gap is declared, and a type-a-path affordance is a
7F candidate rather than a mid-chunk workaround.

**The Settings e2e check is its own spec file** (PR #121, plan's Task 9
AMENDED note). The smoke spec's `describe` callback sat at the e2e
project's 50-line `max-lines-per-function` cap, so Settings coverage
lives in `specs/settings.e2e.ts` — the same per-surface pattern chunk 3
already used — instead of extending `smoke.e2e.ts`'s loop.

**The Low Power Mode probe accepts two keys, not one** (PR #116, plan's
Task 11 AMENDED note). `pmset -g` on Apple Silicon (captured on macOS
26.6.2, M1 Max) has no `lowpowermode` line at all — the equivalent key is
`powermode` (0 automatic, 1 low power, 2 high power). `parse_low_power_mode`
accepts either key with a trailing `1`; the Intel `lowpowermode` literal
is Apple's documented format, not machine-captured (no Intel Mac was
available — see the deferrals list).

**The scheduler holds on no-progress batches, updates the failure report,
and survives panics — three rules the design's "small batched index run"
sentence did not anticipate** (PR #116, plan's Task 12 AMENDED note).
`index::run` returns `Ok` even when every item in a batch fails
per-item — failures ride each kind's own `failed` list, and `plan_work`
(the source of the status poll's `pending` count) never reads the
failure report — so "successful batch → immediate re-tick" would spin
forever on a permanently failing item. `IndexRunOutcome::made_progress()`
gates the pace instead: no progress holds for a full `TICK` and records
the failure count. `update_failure_report` runs after every batch, and
the whole batch runs under `catch_unwind`, so a panic lands in
`last_error` instead of leaving `running` stuck `true` on a dead thread.

**Every tray string is pinned, and `catalog_selected` is a `menu_model`
input, not a wire field** (PR #122, plan's Task 13 AMENDED note, user-
approved as-is). The design's tray copy was illustrative; the mockup
review pinned the exact strings `menu_model`'s tests assert byte-for-byte,
including the pluralization rule ("1 item pending" / "{n} items pending").
`SchedulerStateOutcome` does not grow a field for whether a catalog is
selected — `menu_model(&state, catalog_selected: bool)` takes it as a
second input, read from `AppState` at rebuild time.

**Four platform-forced deviations from the approved tray mockup** (PR
#122, plan's Task 14 AMENDED note): the attention state is a badge dot on
the template icon, not an amber tint (macOS template icons are
alpha-only, so no color survives); only the 22×22 `@1x` icons load at
runtime (`tray_icon` builds the `NSImage` from raw pixels 1:1 in points,
with no HiDPI representation — see the deferrals list for the committed,
unused `@2x` set); Quit is an immediate `app.exit(0)`, not "waits for the
batch" — SQLite keeps an aborted batch atomic, and Cmd+Q from the default
macOS app menu exits the same way; and a left click on the tray icon opens
the menu only, since macOS menu tracking swallows the mouse-up before any
`on_tray_icon_event` handler would see it — "Open Majestical" is the only
click-to-window path.

**`set_throttle` recomputes the decision immediately, and the Always-on
section is a sibling component, not a `SettingsView` addition** (PR #122,
plan's Task 15 AMENDED note). Without an immediate recompute, both the
tray and Settings would keep showing the previous decision for up to a
full `TICK` after a throttle change. `AlwaysOnSection.svelte` +
`scheduler-status.ts` + `autostart.ts` sit alongside `SettingsView.svelte`
rather than inside it, and `scheduler-status.ts`'s `statusLine`
deliberately does not port `tray.rs`'s power-source second line or its
Low-Power-Mode pending-count refinement (see the deferrals list). A
rejected `enable`/`disable` renders as the surface's own error line,
since `Notices` only carries notice arrays, not command errors.

### Review-loop shape

Each task ran implementer → adversarial spec-compliance review (probing
claims empirically, including hand-written mutation probes) → code-quality
review → fix rounds until APPROVED, the process `docs/superpowers/
HANDOFF-phase7D.md` established. Every task's reviewer-found survivors
were closed in the same chunk rather than deferred, with two exceptions
recorded above as accepted equivalents (doctor's `check_models` gate,
Task 16's own triage). Task 9's implementer lost a background build to
the subagent's turn ending twice in a row; the team lead finished Task 9
directly rather than retry a third background run, which is why the
phase's standing mandate ("no `run_in_background` for anything a
controller must wait on; a stalled subagent gets replaced, not
re-nudged indefinitely") reads as strongly as it does in the 7F handoff.
`cargo-mutants`' scoped runs at close (Task 16) ran out of the plan's own
intended order — PR #117 closed doctor's mutants between chunks 4 and 5
rather than waiting for phase close, because a doctor survivor surfaced
during PR #121's own review and was cheaper to close immediately than to
carry to Task 16.

**"The e2e job is the smoke."** `docs/superpowers/HANDOFF-phase7D.md`'s
standing rule — a hand-run GUI smoke recorded on the PR whenever
`lib.rs`'s plugin registration, `tauri.conf.json`, or a surface's mount
path changes — held through PR #112's own plugin-registration change
(its last hand-run smoke, recorded on that PR) and was retired the
moment `gui-e2e` went green on main. Every chunk from #113 onward relied
on the e2e job alone, including #122's tray/hide-to-tray/autostart
changes to `lib.rs`.
