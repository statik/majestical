# Phase 7E — Always-on Tray / WebDriver E2E / `maj doctor` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `maj doctor` at all three heads, a WebDriver e2e suite that
retires the manual GUI smoke, and the always-on tray app with power-aware
auto-indexing and start-at-login.

**Architecture:** Doctor and the autopilot policy are compute-only additions
to `crates/services`; the scheduler loop, power probe, and tray live in the
desktop app (`indexer.rs` / `power.rs` / `tray.rs`, following the `ingest.rs`
module-per-long-running-thing pattern). E2E drives the real debug-built app
through WebdriverIO's `@wdio/tauri-service` embedded provider — the
documented macOS path.

**Tech Stack:** Rust (workspace + standalone `src-tauri` workspace), Tauri 2,
Svelte 5, WebdriverIO + `@wdio/tauri-service` + `tauri-plugin-wdio{,-webdriver}`,
`tauri-plugin-autostart`, `pmset` (power probe — no new native deps).

Spec: `docs/superpowers/specs/2026-08-26-phase7e-alwayson-e2e-doctor-design.md`.

---

## Standing mandates (verbatim from the handoff — every task inherits these)

1. NO Claude-Session trailers in commit messages. Plain git — do NOT use the
   `submitting-changes` skill.
2. Shared checkout: stage ONLY your files, never `git add -A`. Parallel work
   needs a git worktree. Reviewers (who mutate files empirically) never run
   concurrently with implementers.
3. Shell variables do NOT persist across Bash invocations. `trash`, never
   `rm -rf`.
4. Push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
5. Zero warnings. Verify current stable versions of every new dep/action at
   execution time — never from memory.
6. `cargo-mutants` runs FOREGROUND, one at a time — no `run_in_background`,
   no monitors, no sleep-polling; each run finishes before the next starts.
7. Subagents report through `SendMessage`, not prose.
8. `git fetch origin` before reviewing or rebasing; baseline diffs on local
   `main` (SSH-less repo — `origin/main` goes stale).
9. Services never print: `print_stderr` denied in `crates/services`;
   diagnostics ride outcome `notices` (failure path too, via
   `attach_on_err` where the sink is local).
10. MCP read tools take no `confirm`. Tauri commands stay one-liners over
    `*_impl`. Every new command outcome gets a wire fixture on BOTH sides.
11. The manual GUI smoke rule is IN FORCE until Chunk 2's e2e job is green
    on main: any change to `lib.rs` plugin registration, `tauri.conf.json`,
    or a surface mount path needs a hand-run smoke, recorded on the PR.

## File structure (created/modified across the phase)

```
crates/services/src/doctor.rs                 NEW  doctor checks (compute-only)
crates/services/src/autopilot.rs              NEW  pure scheduling policy
crates/services/src/lib.rs                    MOD  two `pub mod` lines
crates/cli/src/main.rs                        MOD  Doctor subcommand
crates/cli/src/commands.rs                    MOD  doctor renderer
crates/cli/src/mcp_cmd/read_tools.rs          MOD  `doctor` read tool
crates/cli/tests/services_parity.rs           MOD  doctor parity row
apps/desktop/src-tauri/src/power.rs           NEW  pmset probe + parsers
apps/desktop/src-tauri/src/indexer.rs         NEW  scheduler loop + commands
apps/desktop/src-tauri/src/tray.rs            NEW  tray icon + menu
apps/desktop/src-tauri/src/commands.rs        MOD  doctor_report command; config-dir env seam
apps/desktop/src-tauri/src/lib.rs             MOD  plugins, managed state, handlers
apps/desktop/src-tauri/tests/wire_fixtures.rs MOD  doctor + scheduler fixtures
apps/desktop/src-tauri/tests/tauri_parity.rs  MOD  doctor row
apps/desktop/src/lib/api.ts                   MOD  doctor + scheduler types
apps/desktop/src/lib/SettingsView.svelte      NEW  health panel + always-on section
apps/desktop/src/lib/fixtures/*.json          NEW  doctor_outcome, scheduler_state
apps/desktop/src/App.svelte                   MOD  Settings sidebar entry; tray nav event
apps/desktop/e2e/                             NEW  WebdriverIO project + specs + media fixtures
.github/workflows/ci.yml                      MOD  gui-e2e job
docs/superpowers/specs/mockups/2026-08-26-phase7e/  NEW  tray-menu.html, health-panel.html
```

---

## PR Chunk 1 — `maj doctor`: services verb + CLI + MCP

### Task 1: `services::doctor`

**Files:**
- Create: `crates/services/src/doctor.rs`
- Modify: `crates/services/src/lib.rs` (add `pub mod doctor;`)

Doctor is the one services verb that does NOT take an `App` — it must run
when no catalog exists (that absence is a finding, not a precondition).
It builds its own `Notices` sink and opens the catalog itself when given
one, via `App::open` (`crates/services/src/app.rs:69`).

**Wire shapes (serde snake_case; notices absent-when-empty like every outcome):**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, serde::Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    /// What was observed, concretely ("ffmpeg 7.1 at /opt/homebrew/bin/ffmpeg").
    pub detail: String,
    /// The command or action that fixes it. Absent when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub remedy: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct DoctorOutcome {
    pub checks: Vec<DoctorCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct DoctorRequest {
    /// Catalog to health-check; `None` skips catalog checks with a Warn row.
    pub catalog: Option<std::path::PathBuf>,
}

pub fn doctor(req: &DoctorRequest) -> Result<DoctorOutcome, ServiceError>
```

**Checks, in emitted order** (each is a private `fn check_*() -> DoctorCheck`;
a helper `fn probe_binary(name: &str, args: &[&str]) -> DoctorCheck` covers
the first two):

| name | Ok when | Fail/Warn detail + remedy |
|---|---|---|
| `ffmpeg` | `ffmpeg -version` runs, exit 0 | Fail; remedy `brew install ffmpeg` |
| `imagemagick` | `magick -version` runs | Fail; remedy `brew install imagemagick` |
| `models` | every model file the index runner resolves exists on disk | Fail per missing model, detail names the file; remedy is the documented fetch command. Reuse the runner's own path resolution — find it with `rg -n "models_dir\|model_path" crates/index/src crates/services/src` and call the same functions; do NOT re-derive paths. |
| `state_dir` | state dir exists and a probe file writes+deletes | Fail; remedy names the dir. Resolution via `state_dir::state_dir_for` needs a catalog — when `req.catalog` is None this check is Warn "no catalog selected". |
| `catalog` | `App::open` succeeds and the SQLite catalog opens/syncs (same open path `volumes_list` uses — `open_catalog`, `crates/services/src/catalog.rs`) | None → Warn "no catalog selected; pass --catalog or run maj init". Open failure → Fail with the error chain as detail. |
| `blob_residue` | the interrupted-write scan finds zero orphans | **AMENDED (Task 1 execution):** the original pointer to `heal.rs` was wrong — that file is a private, MUTATING blob↔`text_fts` healer, not a detector, and doctor must never mutate. The real residue concept is interrupted-write orphans: the blob store writes via `.tmp-{pid}-{seq}` temp names renamed into place (`crates/index/src/blob.rs:281`), and journal migration lands `.partial` files (`crates/services/src/state_dir.rs:132`); a crash strands both. The check walks the blob store root (resolve it with the SAME function the blob-reading code uses — no re-derived paths) counting `.tmp-*` entries, plus `*.partial` under the state dir's runs dir. Zero → Ok; else Warn, detail = count + up to 3 sample paths, remedy = "delete the leftover temp files (safe while nothing is indexing or syncing)". Skipped as Warn when no catalog. |
| `platform` | always Ok on macOS | On non-macOS builds, Warn listing each absent Apple capability by its `AVAILABLE` const name (find: `rg -n "AVAILABLE" crates/ --type rust`), detail "expected on this platform". Never Fail — honest absence is not an error. |

Exit-code polarity (phase-6 doctrine): `doctor` returns `Ok(outcome)` when
the checks RAN, even if every check failed — findings are rows. `Err` is
reserved for "could not check at all" (should be near-unreachable; probe
failures are rows).

- [ ] **Step 1: Write failing tests** in `doctor.rs`'s `#[cfg(test)] mod tests`:
  - `doctor_with_no_catalog_warns_but_runs`: `DoctorRequest::default()` →
    `Ok`; the `catalog`, `state_dir`, `blob_residue` rows are `Warn`; the
    `ffmpeg` row exists (status not asserted — the machine may or may not
    have it).
  - `doctor_with_real_catalog_reports_ok_catalog`: temp catalog via
    `App::init` (copy the arrange from `volumes.rs` tests), request with
    that path → `catalog` row `Ok`.
  - `doctor_with_missing_catalog_path_fails_catalog_row`: request with a
    nonexistent dir → `Ok(outcome)` overall, `catalog` row `Fail`, remedy
    non-empty. (Pins the polarity doctrine: a bad catalog is a row, not an
    `Err`.)
  - `probe_binary_missing_names_remedy`: `probe_binary("definitely-not-a-real-binary-xyz", &["-version"])`
    → `Fail`, remedy present.
  - `check_status_serializes_snake_case`: `serde_json::to_value(CheckStatus::Ok)`
    == `json!("ok")` (wire pin).
- [ ] **Step 2: Run to verify failure** —
  `cargo test -p majestical-services --lib doctor` — compile errors.
- [ ] **Step 3: Implement** as specced above. `probe_binary` uses
  `std::process::Command`, captures the first stdout line as detail on
  success; a spawn error IS the Fail detail. No wildcard matches; every
  `CheckStatus` arm explicit.
- [ ] **Step 4: Run** `cargo test -p majestical-services --lib doctor &&
  cargo clippy -p majestical-services --all-targets --all-features -- -D warnings`
  — PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/services/src/doctor.rs crates/services/src/lib.rs
git commit -m "feat: services::doctor environment and catalog checks"
```

### Task 2: `maj doctor` + MCP `doctor` tool + parity row

**Files:**
- Modify: `crates/cli/src/main.rs` (new `Doctor { json: bool }` subcommand —
  mirror the flat verbs, not a subcommand group), `crates/cli/src/commands.rs`
  (renderer), `crates/cli/src/mcp_cmd/read_tools.rs` (+ `mod.rs` registration)
- Test: a new `crates/cli/tests/` e2e in the per-verb pattern
  (`rg -l "volumes list" crates/cli/tests/` for the model), plus a
  `services_parity.rs` row

- [ ] **Step 1: failing CLI e2e test**: `maj doctor --json` prints
  `DoctorOutcome` as JSON (assert `checks` is a non-empty array and every
  element has `name`/`status`/`detail`); `maj doctor` (human) prints one
  line per check containing the name and a status word — assert on the
  `catalog` line with no catalog configured (stable: `Warn`); exit code 0
  in both cases even though rows warn (polarity pin).
- [ ] **Step 2: run, verify failure.**
- [ ] **Step 3: implement.** Clap variant `Doctor { catalog: Option<PathBuf>,
  json: bool }` reusing the global `--catalog` resolution the other verbs
  use (find: `rg -n "fn resolve_catalog\|catalog_dir" crates/cli/src/main.rs`)
  but tolerating absence (doctor is the one verb that runs without one).
  Human renderer: `{status:>4}  {name:<14} {detail}` plus an indented
  `remedy: …` line when present; notices to stderr like every read verb.
- [ ] **Step 4: MCP read tool `doctor`** in `read_tools.rs` — no `confirm`
  param, optional `catalog` string param, result = `DoctorOutcome` via the
  established `tool_ok` path. Extend the tool-list pinning test
  (find: `rg -n "browse_tree" crates/cli/src/mcp_cmd/`) with the new name
  and schema.
- [ ] **Step 5: parity row** in `crates/cli/tests/services_parity.rs`:
  doctor is read-only → follow the pattern the 7D browse rows used for a
  verb the reference binary predates (read the harness's helper docs first
  — the file documents how new-verb rows are added; mirror the `browse
  tree` row from PR #98). Constrain the diffed invocation to
  `maj doctor --json` against a fixture catalog so environment-dependent
  rows (`ffmpeg` on PATH) still compare equal between the two binaries
  running on the SAME machine.
- [ ] **Step 6: run all**
  `cargo test -p majestical-cli && cargo clippy -p majestical-cli --all-targets -- -D warnings` — PASS.
- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/src/mcp_cmd/ crates/cli/tests/
git commit -m "feat: maj doctor + MCP doctor read tool"
```

**Open PR (chunk 1), squash-merge when green.** Branch `phase7e-doctor`.

---

## PR Chunk 2 — e2e harness + launch smoke + CI job

### Task 3: wdio plugins in the Tauri app (debug builds only)

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml`,
  `apps/desktop/src-tauri/src/lib.rs`
- Modify: `apps/desktop/src-tauri/src/commands.rs` (config-dir env seam)

- [ ] **Step 1: dependency versions.** Look up CURRENT stable versions of
  `tauri-plugin-wdio` and `tauri-plugin-wdio-webdriver` (crates.io) and of
  `@wdio/cli`, `@wdio/local-runner`, `@wdio/mocha-framework`,
  `@wdio/tauri-service` (npm). Do not trust memory. Record them in the PR
  description.
- [ ] **Step 2: config-dir seam, failing test first.** In `commands.rs`,
  wherever the app-config dir is resolved (find:
  `rg -n "app_config_dir" apps/desktop/src-tauri/src/`), honor
  `MAJ_DESKTOP_CONFIG_DIR` when set — a plain function
  `fn config_dir(app: &AppHandle) -> Result<PathBuf>` with a unit test that
  sets the env var (use `temp-env` or set/remove around the assert — the
  desktop workspace tests are single-threaded per file, but still restore)
  and asserts the override wins. This is the seam e2e uses to point the
  real app at a fixture catalog.
- [ ] **Step 3: register the plugins debug-only** in `lib.rs`:

```rust
let mut builder = tauri::Builder::default();
#[cfg(debug_assertions)]
{
    builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
}
```

  (Adjust init calls to the plugins' current API — their READMEs are the
  authority at execution time.) Cargo deps behind
  `[target.'cfg(debug_assertions)'...]` is not a thing — use plain
  `[dependencies]` plus the `#[cfg(debug_assertions)]` registration; the
  release binary carries no listening server because registration is
  compiled out. State this in a comment at the registration site.
- [ ] **Step 4: run** `cargo check` in `apps/desktop/src-tauri` (debug and
  `--release`) + existing tests — PASS. **Manual smoke required** (lib.rs
  changed; the e2e job does not exist yet — this is the LAST hand-run
  smoke if the chunk lands). Record it on the PR.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.lock apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/commands.rs
git commit -m "feat: embedded wdio webdriver plugins (debug builds) + config-dir env seam"
```

### Task 4: e2e project, fixture catalog, launch smoke, CI job

**Files:**
- Create: `apps/desktop/e2e/package.json`, `apps/desktop/e2e/wdio.conf.ts`,
  `apps/desktop/e2e/tsconfig.json`, `apps/desktop/e2e/setup/fixture-catalog.ts`,
  `apps/desktop/e2e/specs/smoke.e2e.ts`,
  `apps/desktop/e2e/fixtures/media/` (tiny committed sample files:
  one small `.jpg`, one one-page `.pdf`, one `.txt` — no video, so the
  suite never needs ffmpeg)
- Modify: `pnpm-workspace.yaml` if present (find:
  `fd pnpm-workspace.yaml`), else the e2e package stands alone;
  `.github/workflows/ci.yml`

**Fixture-catalog setup** (`setup/fixture-catalog.ts`, run by wdio
`onPrepare`): shells out to the debug `maj` binary
(`target/debug/maj`, built by the CI job before wdio runs):

```
maj init <tmp>/catalog
maj scan <tmp>/catalog --volume fixture --root apps/desktop/e2e/fixtures/media
maj tag <tmp>/catalog ... (one tag on the jpg, so Organize has a vocabulary)
```

(Exact scan/tag argument spellings: crib from
`crates/cli/tests/services_parity.rs`'s arrange helpers — they drive the
same binary.) Then write `{"catalog": "<tmp>/catalog"}` to
`<tmp>/config/config.json` and export `MAJ_DESKTOP_CONFIG_DIR=<tmp>/config`
into the app-launch env via the tauri-service capabilities.

- [ ] **Step 1: scaffold the wdio project.** Embedded provider per the
  Tauri v2 WebDriver guide (https://v2.tauri.app/develop/tests/webdriver/).
  Framework mocha, TS via the wdio-recommended transform, oxlint added to
  the existing lint recipe (`just` — find the gui lint recipe with
  `rg -n "oxlint" justfile`). Exact-pin all versions (repo rule: no `^`).
- [ ] **Step 2: write the failing smoke spec** `specs/smoke.e2e.ts`:
  - app window appears (`browser.getTitle()` resolves);
  - for each sidebar surface — Search, Volumes, Browse, Organize, Ingest —
    click its nav entry (add stable `data-e2e="nav-<name>"` attributes to
    `App.svelte`'s sidebar in this task; find the nav markup with
    `rg -n "sidebar\|nav" apps/desktop/src/App.svelte`) and assert one
    surface-specific element renders with fixture data (e.g. Volumes shows
    a row containing `fixture`; Browse tree shows the volume; Organize
    lists the tag).
  - assert zero webview console errors via the tauri-service log capture.
- [ ] **Step 3: run locally** — `pnpm --dir apps/desktop/e2e test` after
  `cargo build` + `cargo tauri build --debug` (or the service's configured
  binary path; wire the path in `wdio.conf.ts`). Iterate until green
  locally.
- [ ] **Step 4: CI job** `gui-e2e` in `ci.yml`: `runs-on: macos-latest`;
  steps: checkout (pinned SHA, `persist-credentials: false` — match the
  file's existing style), Rust toolchain + cache, Node 22 + pnpm 11
  (match the existing gui job's setup — find: `rg -n "pnpm" .github/workflows/ci.yml`),
  `just gui-install`, build `maj` debug + the app debug bundle, run the
  wdio suite. Required-for-merge like the other jobs (it lands in the same
  workflow; branch protection already requires the workflow's jobs).
  Run `actionlint` and `zizmor` on the modified workflow before committing.
- [ ] **Step 5: retire the manual-smoke rule** — in this plan and in the
  handoff conventions: once this job is green on main, edit
  `docs/superpowers/HANDOFF-phase7E.md`'s standing-rule paragraph is
  historical (do not edit history); instead the 7F handoff (Task 17)
  states the new rule: "the e2e job is the smoke." Until merge, rule 11
  above still applies.
- [ ] **Step 6: Commit**

```bash
git add apps/desktop/e2e/ apps/desktop/src/App.svelte .github/workflows/ci.yml pnpm-workspace.yaml
git commit -m "feat: WebDriver e2e harness + launch smoke + gui-e2e CI job"
```

**Open PR (chunk 2), squash-merge when green.** Branch `phase7e-e2e-harness`.

---

## PR Chunk 3 — per-surface e2e flows

### Task 5: Search + Volumes + Browse flows

**Files:**
- Create: `apps/desktop/e2e/specs/search.e2e.ts`,
  `apps/desktop/e2e/specs/volumes.e2e.ts`, `apps/desktop/e2e/specs/browse.e2e.ts`

- [ ] **Step 1 (failing specs):**
  - search: type the fixture jpg's filename into the search input, submit,
    assert a result card with that name renders.
  - volumes: **AMENDED (Task 5 execution):** the fixture volume is
    necessarily OFFLINE — `volume_is_online`
    (`crates/services/src/volumes.rs`) reads a `--volume`-labeled id as
    online only when `/Volumes/<label>` is a real mount, which a scanned
    temp dir never is, in every environment including CI. Assert the
    offline badge (the real behavior), plus the label and an asset count
    of 3 (the three fixture files).
  - browse: click the fixture volume in the tree, assert the grid shows 3
    cards; click a folder if the fixture has one (put the jpg under
    `media/sub/` when Task 4 lands to make this real — if Task 4 shipped
    flat, restructure the fixture dir here and adjust smoke counts).
- [ ] **Step 2:** add whatever `data-e2e` attributes the selectors need to
  the Svelte components — attributes only, no behavior changes.
- [ ] **Step 3:** run locally until green; commit.

```bash
git add apps/desktop/e2e/specs/ apps/desktop/src/lib/
git commit -m "test: e2e flows for search, volumes, browse"
```

### Task 6: Organize + Ingest (dry-run) flows

**Files:**
- Create: `apps/desktop/e2e/specs/organize.e2e.ts`,
  `apps/desktop/e2e/specs/ingest.e2e.ts`

- [ ] **Step 1 (failing specs):**
  - organize: assert the tag vocabulary lists the fixture tag with count 1;
    from Browse, select a card and assign a new tag via `SelectionBar`;
    assert Organize now lists it (a REAL mutation against the throwaway
    fixture catalog — each wdio run builds a fresh one, so mutation is
    safe).
  - ingest: **AMENDED (Task 6 execution) — flow DROPPED, gap declared.**
    IngestView's source and destination pickers go straight through
    `@tauri-apps/plugin-dialog`'s native OS folder dialog with no text
    fallback, and the e2e suite deliberately has no guest bridge to
    inject dialog answers. The three exits were: a test-only backdoor
    (rejected — invents an affordance), the wdio guest bridge (rejected —
    reverses the suite's documented no-bridge design), or declaring the
    gap. Declared: smoke.e2e.ts's setup-board render stays the only
    Ingest e2e coverage; watchlist entry at phase close. A REAL
    type-a-path affordance on IngestView (agent-native: an agent or
    power user typing a path is a legitimate product feature, and it
    would incidentally unblock this flow) is a 7F candidate, to be
    designed with a mockup, not slipped in mid-chunk.
- [ ] **Step 2:** run locally until green; commit.

```bash
git add apps/desktop/e2e/specs/ apps/desktop/e2e/fixtures/
git commit -m "test: e2e flows for organize and ingest dry-run"
```

**Open PR (chunk 3), squash-merge when green.** Branch `phase7e-e2e-flows`.

---

## PR Chunk 4 — doctor GUI panel

### Task 7: health-panel mockup (USER REVIEW GATE)

**Files:**
- Create: `docs/superpowers/specs/mockups/2026-08-26-phase7e/health-panel.html`

- [ ] **Step 1:** standalone HTML mockup of a Settings surface with a
  Health section: one row per check (status pill Ok/Warn/Fail, name,
  detail, remedy line under non-Ok rows), a "Run checks again" button, and
  a placeholder Always-on section marked "arrives with chunk 6" so the
  page shows the surface's final shape. Match the app's existing visual
  language (crib CSS variables from `apps/desktop/src/app.css`).
- [ ] **Step 2:** send to the user for review. Every field the mockup wants
  that `DoctorCheck` does not carry becomes an AMENDED note here (wire-gap
  ledger) — render without it, do NOT invent values.
- [ ] **Step 3:** commit the mockup after review.

```bash
git add docs/superpowers/specs/mockups/2026-08-26-phase7e/health-panel.html
git commit -m "docs: phase 7E health panel mockup"
```

### Task 8: `doctor_report` Tauri command + fixtures + parity

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands.rs` (command), `src/lib.rs`
  (handler list), `tests/wire_fixtures.rs`, `tests/tauri_parity.rs`
- Create: `apps/desktop/src/lib/fixtures/doctor_outcome.json` (generated)
- Modify: `apps/desktop/src/lib/api.ts` (types), `fixtures.test.ts` (row)

- [ ] **Step 1: failing wire-fixture test** — a `doctor_outcome` fixture
  generated from a `DoctorOutcome` with one row per status (Ok, Warn with
  remedy, Fail with remedy) plus a notice, so the TS side sees every
  optional field both present and absent.
- [ ] **Step 2:** `doctor_report` command — one-liner over
  `doctor_report_impl`, which builds `DoctorRequest { catalog }` from the
  selected catalog (may be None — doctor is the one command that works
  before catalog selection; do NOT gate it on `AppState` being populated)
  and calls `majestical_services::doctor::doctor`.
- [ ] **Step 3:** `api.ts` interfaces `DoctorCheck`/`DoctorOutcome`
  (string-union `status: "ok" | "warn" | "fail"`), `fixtures.test.ts`
  assignment row. Regenerate via
  `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures`.
- [ ] **Step 4:** `tauri_parity.rs` row: whole-document comparison of the
  command payload against `maj doctor --json` (the 7D read-verb rule: the
  verb prints the outcome struct as-is, so the WHOLE document is the
  contract).
- [ ] **Step 5:** run both workspaces' tests + clippy + `pnpm -C apps/desktop test`
  — PASS. Commit.

```bash
git add apps/desktop/src-tauri/src/ apps/desktop/src-tauri/tests/ apps/desktop/src/lib/
git commit -m "feat: doctor_report command with wire fixtures and parity row"
```

### Task 9: `SettingsView.svelte` — health panel

**Files:**
- Create: `apps/desktop/src/lib/SettingsView.svelte`,
  `apps/desktop/src/lib/SettingsView.test.ts`
- Modify: `apps/desktop/src/App.svelte` (sidebar entry + route),
  `apps/desktop/e2e/specs/smoke.e2e.ts` (sixth surface in the mount loop)

- [ ] **Step 1: failing component tests** (vitest, the existing
  `*.test.ts` pattern — crib the mock-invoke harness from
  `VolumesView.test.ts`): renders one row per check from a mocked
  `doctor_report`; a Fail row shows its remedy; "Run checks again"
  re-invokes; a command error renders through `Notices.svelte`, not a
  blank panel.
- [ ] **Step 2: implement** per the approved mockup. Status pill styling
  follows the mockup; row order is the outcome's order (services owns
  ordering — the GUI must not sort).
- [ ] **Step 3:** sidebar entry `data-e2e="nav-settings"`; extend the e2e
  smoke's surface loop with Settings (assert a check row renders — the
  real app's doctor runs against the fixture catalog).
- [ ] **Step 4:** run `pnpm -C apps/desktop test` + oxlint + e2e locally —
  PASS. Commit.

```bash
git add apps/desktop/src/lib/SettingsView.svelte apps/desktop/src/lib/SettingsView.test.ts apps/desktop/src/App.svelte apps/desktop/e2e/specs/smoke.e2e.ts
git commit -m "feat: Settings surface with doctor health panel"
```

**Open PR (chunk 4), squash-merge when green.** Branch `phase7e-health-panel`.

---

## PR Chunk 5 — autopilot policy + power probe + scheduler loop (headless)

### Task 10: `services::autopilot` — the pure policy

**Files:**
- Create: `crates/services/src/autopilot.rs`
- Modify: `crates/services/src/lib.rs` (add `pub mod autopilot;`)

**Types and the whole policy (this IS the implementation — it must stay
this small):**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    /// Probe unavailable (non-macOS) or unparseable output.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PowerState {
    pub source: PowerSource,
    pub low_power_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThrottleOverride {
    Auto,
    Paused,
    Low,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    Paused,
    LowPowerMode,
    NoPendingWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", content = "hold_reason", rename_all = "snake_case")]
pub enum SchedulerDecision {
    RunFull,
    RunLow,
    Hold(HoldReason),
}

/// One tick's verdict. Manual override always wins over Auto; Auto holds
/// in Low Power Mode, runs low on battery or an unknown source
/// (conservative), full on AC. No work means hold, whatever the power.
#[must_use]
pub fn autopilot_decision(
    power: PowerState,
    throttle: ThrottleOverride,
    pending_items: u64,
) -> SchedulerDecision {
    if pending_items == 0 && throttle != ThrottleOverride::Paused {
        return SchedulerDecision::Hold(HoldReason::NoPendingWork);
    }
    match throttle {
        ThrottleOverride::Paused => SchedulerDecision::Hold(HoldReason::Paused),
        ThrottleOverride::Low => SchedulerDecision::RunLow,
        ThrottleOverride::Full => SchedulerDecision::RunFull,
        ThrottleOverride::Auto => {
            if power.low_power_mode {
                return SchedulerDecision::Hold(HoldReason::LowPowerMode);
            }
            match power.source {
                PowerSource::Ac => SchedulerDecision::RunFull,
                PowerSource::Battery | PowerSource::Unknown => SchedulerDecision::RunLow,
            }
        }
    }
}
```

- [ ] **Step 1: failing tests** — one per arm (paused wins over AC+work;
  low/full override ignore power including low-power-mode; auto: AC→full,
  battery→low, unknown→low, low-power-mode→hold; zero pending → hold
  no-work for every non-paused throttle; zero pending + paused → hold
  PAUSED, pinning that paused reports itself, not no-work). Plus a
  proptest (the workspace already depends on proptest — find the pattern
  with `rg -n "proptest" crates/core/src/projection.rs`):
  `throttle != Auto` implies the decision never mentions a power-derived
  reason (`Hold(LowPowerMode)` unreachable), and `Paused` implies
  `Hold(Paused)` for ALL inputs.
- [ ] **Step 2:** run to verify failure; **Step 3:** implement (the code
  above); **Step 4:** `cargo test -p majestical-services --lib autopilot`
  + clippy — PASS.
- [ ] **Step 5: wire-shape pin**: serde tests —
  `to_value(SchedulerDecision::Hold(HoldReason::Paused))` ==
  `json!({"mode":"hold","hold_reason":"paused"})`;
  `to_value(SchedulerDecision::RunFull)` == `json!({"mode":"run_full"})`.
- [ ] **Step 6: Commit**

```bash
git add crates/services/src/autopilot.rs crates/services/src/lib.rs
git commit -m "feat: autopilot scheduling policy (pure, exhaustive)"
```

### Task 11: `power.rs` — the pmset probe

**Files:**
- Create: `apps/desktop/src-tauri/src/power.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (`pub mod power;`)

No new native dependencies: the probe shells out to `pmset` (present on
every macOS since 10.4) and parses. Parsers are pure functions over
captured strings; only the two-line `read_power_state` is cfg-gated.

```rust
/// True where the platform has a power probe at all.
pub const POWER_PROBE_AVAILABLE: bool = cfg!(target_os = "macos");

/// Parses `pmset -g batt` — the first line names the drawing source:
/// "Now drawing from 'AC Power'" / "'Battery Power'". Anything else is
/// Unknown (a future macOS rewording degrades to conservative, not wrong).
#[must_use]
pub fn parse_power_source(batt_output: &str) -> PowerSource { /* contains("'AC Power'") etc. */ }

/// Parses `pmset -g` custom output for a `lowpowermode  1` line.
#[must_use]
pub fn parse_low_power_mode(pmset_output: &str) -> bool { /* line-wise: trim, starts_with("lowpowermode"), ends_with('1') */ }

#[cfg(target_os = "macos")]
pub fn read_power_state() -> PowerState { /* run both pmset invocations; parse; any spawn error → Unknown/false */ }

#[cfg(not(target_os = "macos"))]
pub fn read_power_state() -> PowerState {
    PowerState { source: PowerSource::Unknown, low_power_mode: false }
}
```

- [ ] **Step 1: failing parser tests** against captured literals: an AC
  `pmset -g batt` output, a battery one, a garbled one → Unknown; a
  `pmset -g` block with `lowpowermode  1`, with `0`, and with the line
  absent → false. (Capture real output from the dev machine for the
  literals; note in a comment which macOS version produced them.)
- [ ] **Step 2:** run, verify failure; **Step 3:** implement; **Step 4:**
  cfg-gated smoke test (macOS only): `read_power_state()` returns
  `source != PowerSource::Unknown` on a real Mac — the 7C rule: the gate
  and the coverage gap recorded together in the test's doc comment.
- [ ] **Step 5:** tests + clippy in the desktop workspace — PASS. Commit.

```bash
git add apps/desktop/src-tauri/src/power.rs apps/desktop/src-tauri/src/lib.rs
git commit -m "feat: pmset power probe with pure parsers"
```

### Task 12: `indexer.rs` — the scheduler loop + commands + fixtures

**Files:**
- Create: `apps/desktop/src-tauri/src/indexer.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (managed state + handlers),
  `tests/wire_fixtures.rs`, `apps/desktop/src/lib/api.ts`,
  `apps/desktop/src/lib/fixtures/scheduler_state.json` (generated),
  `fixtures.test.ts`

The `ingest.rs` pattern exactly: one module owning the state struct, the
loop thread, and the commands. Read `ingest.rs` top-to-bottom before
writing this file.

**Shape:**

```rust
pub struct SchedulerState(pub std::sync::RwLock<SchedulerShared>);

pub struct SchedulerShared {
    pub throttle: ThrottleOverride,          // default Auto
    pub last_decision: Option<SchedulerDecision>,
    pub power: PowerState,
    pub pending_items: u64,                  // from the last status poll
    pub running: bool,                       // a batch is executing right now
    pub last_error: Option<String>,          // last failed batch, kept for Health
}

/// Wire outcome for `scheduler_state` / `set_throttle`.
#[derive(serde::Serialize)]
pub struct SchedulerStateOutcome {
    pub available: bool,                     // POWER_PROBE_AVAILABLE
    pub throttle: ThrottleOverride,
    pub power: PowerState,
    pub decision: Option<SchedulerDecision>,
    pub pending_items: u64,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_error: Option<String>,
}
```

**Loop** (spawned in `setup` next to `restore_persisted_catalog`, and
re-spawned/aimed when the selected catalog changes — same trigger path
`initialize_catalog`/`use_existing_catalog` use): every `TICK` (const,
30 s) — hold ticks with no catalog selected; else poll pending via the
services index-status verb (find its request/outcome:
`rg -n "pub fn index_status\|StatusOutcome" crates/services/src/index/`),
read power, ask `autopilot_decision`, then on `RunFull`/`RunLow` execute
ONE batch: the services index-run verb with `limit: Some(BATCH_LIMIT)`
(const, 25) and `threads: Some(1)` for Low / `None` (default parallelism)
for Full. After a batch, pace: `PACE_LOW` (const, 5 s) before the next
tick's work under Low, immediate re-tick under Full. A batch `Err` lands
in `last_error` and backs off to the idle tick — never a hot loop. A
throttle change to `Paused` takes effect at the next batch boundary
(batches are short by construction; that IS the pause latency, matching
the ingest-cancel "between files" doctrine).

**Commands** (one-liners over `*_impl`):
- `scheduler_state() -> SchedulerStateOutcome`
- `set_throttle(throttle: ThrottleOverride) -> SchedulerStateOutcome`
  (validated by serde; returns the post-change state)

- [ ] **Step 1: failing unit tests** in `indexer.rs` for the decision-to-
  request mapping (`fn batch_request(decision) -> Option<IndexRunRequest>`:
  RunLow → `threads Some(1)`, RunFull → `threads None`, Hold → None —
  pure, no thread) and for `SchedulerStateOutcome` serialization (fixture
  content: one with `last_error` absent, decision `run_full`; one held).
- [ ] **Step 2:** implement module + loop + commands; register managed
  state and handlers in `lib.rs`.
- [ ] **Step 3:** wire fixtures both sides
  (`MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures`), `api.ts`
  types (`SchedulerStateOutcome`, string unions for throttle/decision),
  `fixtures.test.ts` row.
- [ ] **Step 4:** both workspaces' tests + clippy + `pnpm -C apps/desktop
  test` — PASS. E2E smoke must still pass (lib.rs changed — the e2e job
  now IS the smoke).
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/ apps/desktop/src-tauri/tests/ apps/desktop/src/lib/
git commit -m "feat: background index scheduler with power-aware policy"
```

**Open PR (chunk 5), squash-merge when green.** Branch `phase7e-scheduler`.

---

## PR Chunk 6 — tray, hide-to-tray, autostart, Settings completion

### Task 13: tray-menu mockup (USER REVIEW GATE)

**Files:**
- Create: `docs/superpowers/specs/mockups/2026-08-26-phase7e/tray-menu.html`

- [ ] **Step 1:** HTML approximation of the tray menu in its states —
  idle, indexing (with the "Indexing — N items pending" status line),
  paused-by-user, held-by-low-power — plus the override radio group
  (Auto / Paused / Low / Full), "Open Majestical", "Health…", "Quit".
  The point is agreeing on states and wording before code; visual fidelity
  to macOS menus is not required.
- [ ] **Step 2:** user review; wording changes land here as AMENDED notes.
- [ ] **Step 3:** commit.

```bash
git add docs/superpowers/specs/mockups/2026-08-26-phase7e/tray-menu.html
git commit -m "docs: phase 7E tray menu mockup"
```

### Task 14: `tray.rs` — icon, menu, hide-to-tray

**Files:**
- Create: `apps/desktop/src-tauri/src/tray.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`,
  `apps/desktop/src-tauri/Cargo.toml` (`tauri` `tray-icon` +
  `image-png` features), `apps/desktop/src-tauri/tauri.conf.json` if the
  tray needs a config block (check current Tauri 2 docs at execution),
  `apps/desktop/src/App.svelte` (listen for the `navigate-settings` event)

- [ ] **Step 1:** menu-state rendering as a pure function, failing tests
  first: `fn menu_model(state: &SchedulerShared) -> MenuModel` where
  `MenuModel` is a plain struct (status line String, checked override,
  enabled flags) — the tests pin the exact status strings from the
  approved mockup ("Idle", "Indexing — {n} items pending",
  "Paused", "Paused (Low Power Mode)"). Tauri menu objects are built FROM
  the model in a thin untested shim (same philosophy as one-liner
  commands: logic stays testable).
- [ ] **Step 2:** implement tray: `TrayIconBuilder` with the app icon,
  menu from `menu_model`, rebuilt on scheduler-state change (a `notify`
  hook from the loop — simplest correct: rebuild on tray-menu open if the
  Tauri API supports it, else on each loop tick when state changed).
  Menu events: overrides call the same `set_throttle_impl` the command
  uses; "Open Majestical" shows + focuses the main window; "Health…"
  additionally emits `navigate-settings` to the webview (App.svelte
  listens and selects the Settings surface — unit-test the listener with
  the existing mock-event pattern, `rg -n "listen" apps/desktop/src/lib/updater.ts`);
  "Quit" exits via the standard Tauri exit path (the scheduler's batch
  boundary makes this safe by construction — document that at the call
  site).
- [ ] **Step 3:** hide-to-tray: `.on_window_event` — `CloseRequested` →
  `api.prevent_close()` + hide; on macOS also switch activation policy to
  `Accessory` while hidden and back to `Regular` on show, so no zombie
  Dock icon (cfg-gated; find the API: Tauri 2 `set_activation_policy`).
- [ ] **Step 4:** run desktop tests + clippy; e2e smoke green (window
  close behavior changed — add an e2e assertion IF the harness can drive
  window close without killing the session; if it cannot, record that as
  a named coverage gap in the task's commit message and the watchlist).
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/ apps/desktop/src/App.svelte
git commit -m "feat: menu-bar tray with scheduler status and hide-to-tray"
```

### Task 15: autostart + Settings always-on section

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml` + `src/lib.rs`
  (`tauri-plugin-autostart`, current stable version, macOS
  `LaunchAgent` mode), `apps/desktop/package.json`
  (`@tauri-apps/plugin-autostart`, exact-pinned),
  `apps/desktop/src/lib/SettingsView.svelte` + its test,
  `apps/desktop/src/lib/tauri-caps` / capability JSON if the plugin needs
  a permission entry (find: `rg -l "permissions" apps/desktop/src-tauri/`)

- [ ] **Step 1: failing component tests**: the Always-on section renders
  the throttle radio bound to `scheduler_state`/`set_throttle` (mocked)
  and a "Start at login" toggle bound to the autostart plugin's JS API
  (mocked module — the `updater.ts` test pattern); toggle default
  reflects `isEnabled()`, never assumes.
- [ ] **Step 2:** implement; register the plugin in `lib.rs` (default
  DISABLED — enabling is only ever the user's click; no silent
  enable-on-update).
- [ ] **Step 3:** replace the chunk-4 "arrives with chunk 6" placeholder in
  the Settings mockup's Always-on section with the shipped reality if the
  wording drifted (mockup is a record, keep it truthful).
- [ ] **Step 4:** all desktop tests + oxlint + e2e (Settings spec: assert
  the throttle radio renders and switching to Paused round-trips through
  `scheduler_state` — a REAL command in e2e). PASS. Commit.

```bash
git add apps/desktop/ docs/superpowers/specs/mockups/2026-08-26-phase7e/
git commit -m "feat: start-at-login toggle and always-on settings section"
```

**Open PR (chunk 6), squash-merge when green.** Branch `phase7e-tray`.

---

## PR Chunk 7 — phase close

### Task 16: mutants + parity sweep

- [ ] **Step 1:** `cargo mutants -p majestical-services -f src/doctor.rs`
  — FOREGROUND, wait for completion. Triage every survivor: fix the test
  or record the disposition.
- [ ] **Step 2:** `cargo mutants -p majestical-services -f src/autopilot.rs`
  — same, sequentially, only after Step 1 finishes.
- [ ] **Step 3:** a scoped run over `apps/desktop/src-tauri/src/power.rs`
  parsers and `indexer.rs`'s pure functions if the desktop workspace's
  mutants setup permits (it is a separate workspace; if `cargo mutants`
  cannot run there cleanly, record that as the disposition — do not force
  it).
- [ ] **Step 4:** re-run both parity harnesses end to end
  (`cargo test -p majestical-cli --test services_parity`,
  `cargo test --test tauri_parity` in the desktop workspace) against a
  fresh `/tmp/maj-ref`.

### Task 17: watchlist, handoff, closing PR

- [ ] **Step 1:** append a "Phase 7E deferrals" section to
  `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`: every deferral
  from the spec's Deferred list plus anything reviewers deferred during
  the phase, each attributed to its PR; plus a
  "cargo-mutants triage (phase 7E)" section recording Task 16's runs and
  survivor dispositions.
- [ ] **Step 2:** write the spec's `## As-built (phase 7E)` section
  (deviations, AMENDED notes summary, review-loop shape) — the 7D spec's
  as-built section is the template.
- [ ] **Step 3:** write `docs/superpowers/HANDOFF-phase7F.md` in the
  established format: state at handoff, new architecture pointers (doctor,
  autopilot policy + scheduler, tray, the e2e harness and its "the e2e
  job is the smoke" rule replacing the manual-smoke rule), secrets note
  (unchanged), 7F recommendations from the remaining deferred list.
- [ ] **Step 4:** closing PR with all of the above; squash-merge when
  green.

```bash
git add docs/superpowers/
git commit -m "docs: phase 7E close — deferrals, mutants triage, 7F handoff"
```

---

## Verification (end-to-end, per chunk)

- Chunks 1, 5: `cargo test --workspace && cargo clippy --workspace
  --all-targets --all-features -- -D warnings` in the root workspace.
- Chunks 2-6: additionally, in `apps/desktop/src-tauri`: `cargo test &&
  cargo clippy --all-targets -- -D warnings`; in `apps/desktop`:
  `pnpm test` + oxlint recipe; the e2e suite locally before pushing.
- Every chunk: `prek run` before committing; CI green before squash-merge.
- Chunk 2 is the last chunk that needs a hand-run GUI smoke (Task 3
  changes `lib.rs` before the e2e job exists). From chunk 4 on, the
  gui-e2e job is the smoke.
