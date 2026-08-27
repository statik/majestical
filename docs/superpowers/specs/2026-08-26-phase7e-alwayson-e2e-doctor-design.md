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
