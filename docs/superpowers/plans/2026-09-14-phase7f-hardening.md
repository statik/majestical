# Phase 7F — Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the phase 7E deferrals that hurt the always-on story (a
failed-item ledger with retry at all three heads, a named describer-key
failure), add path entry to the Ingest surface and the e2e flow it unblocks,
sharpen the tray icons, split the wire layer, commit the whisper fixture, and
fix two one-liners.

**Architecture:** The ledger is a services-crate concern (pure merge/partition
functions over the existing state-dir failures file, read by `build_plan`),
so every head — CLI, MCP, and the desktop scheduler — skips known failures
without any head-specific logic. The desktop gains one command
(`retry_failed_items`), one scheduler field (`failed_items`), and a
condvar-based nudge so a retry starts on the next tick rather than after up
to 30 s. The Ingest surface's path fields are pure UI over the existing
plan/run commands. The whisper fixture becomes a committed artifact so CI
never synthesizes speech again.

**Tech Stack:** Rust (workspace + standalone `src-tauri` workspace), Tauri 2,
Svelte 5, WebdriverIO e2e, ImageMagick (icons), ffmpeg + macOS `say`
(fixture regeneration only), proptest.

Spec: `docs/superpowers/specs/2026-09-14-phase7f-hardening-design.md`.
Mockup (approved): `docs/superpowers/specs/mockups/2026-09-14-phase7f/ingest-path-entry.html`.

---

## Standing mandates (verbatim from the handoff — every task inherits these)

1. NO Claude-Session trailers in commit messages. Plain git — do NOT use the
   `submitting-changes` skill.
2. Shared checkout: stage ONLY your files, never `git add -A`. Parallel work
   needs a git worktree. Reviewers (who mutate files empirically) never run
   concurrently with implementers.
3. Shell variables do NOT persist across Bash invocations. `trash`, never
   `rm -rf` in commands you run (the justfile's own temp-dir trap is
   existing repo code, not a command you type).
4. Push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
5. Zero warnings. Verify current stable versions of every new dep/action at
   execution time — never from memory.
6. `cargo-mutants` runs FOREGROUND, one at a time, `--in-place` on a warm
   target, `git status` clean after each — no `run_in_background`, no
   monitors, no sleep-polling.
7. Subagents report through `SendMessage`, not prose.
8. `git fetch origin` before reviewing or rebasing; baseline diffs on local
   `main` (SSH-less repo — `origin/main` goes stale).
9. Services never print and never read the environment: `print_stderr` is
   denied in `crates/services`; env values (the describer key) are read by
   the head and passed in. `crates/describe` has the same no-env rule (see
   `client.rs:72`).
10. MCP read tools take no `confirm`. Tauri commands stay one-liners over
    `*_impl`. Every new/changed command outcome gets a wire fixture on BOTH
    sides (`MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` in
    `apps/desktop/src-tauri`, then `pnpm test` in `apps/desktop`).
11. Local GUI verification is the four-command line:
    `pnpm check && pnpm lint && pnpm test && pnpm build` (in `apps/desktop`).
12. A subagent's `run_in_background` build dies with its turn. Run a build
    you must wait on in the foreground with a 600 s timeout.
13. Never `gh pr merge --auto`. Watch CI to green, then merge explicitly.
14. `gui-e2e` is watched to green by convention, not enforced by GitHub.

## File structure (created/modified across the phase)

```
conformance/whisper/fixture.wav                     NEW  committed speech fixture (~300 KB)
justfile                                            MOD  whisper-fixture recipe; whisper-conformance uses the file
crates/index/tests/common/mod.rs                    NEW  shared `is_silent`
crates/index/tests/whisper_conformance.rs           MOD  silence guard
crates/index/tests/whisper_gated.rs                 MOD  uses shared helper; guard test
.github/workflows/ci.yml                            MOD  whisper job comment

crates/index/src/work.rs                            MOD  KindStatus.failed
crates/services/src/index/run.rs                    MOD  ItemFailure {asset,path,error,transient}; no-key failure
crates/services/src/index/mod.rs                    MOD  ledger (read/merge/record/clear/apply_ledger); status `failed`
crates/describe/src/config.rs                       MOD  OPENROUTER_KEY_ENV const
crates/cli/src/main.rs                              MOD  --retry-failed
crates/cli/src/index_cmd.rs                         MOD  flag, status/run rendering
crates/cli/src/describer_cmd.rs                     MOD  env_api_key uses the const
crates/cli/src/mcp_cmd/write_tools.rs               MOD  index_run retry_failed
crates/cli/src/mcp_cmd/read_tools.rs                MOD  index_status doc
crates/cli/tests/index_smoke.rs                     MOD  ledger integration test
crates/cli/tests/mcp_smoke.rs                       MOD  retry_failed dry-run/exec tests

crates/services/src/doctor.rs                       MOD  failed_items + describer checks; DoctorRequest.describer_env_key
crates/cli/src/*.rs (cmd_doctor's module)           MOD  passes env key
apps/desktop/src-tauri/src/indexer.rs               MOD  failed_items, SchedulerWake, retry_failed_items
apps/desktop/src-tauri/src/commands.rs              MOD  doctor_report_impl passes env key; env_api_key helper
apps/desktop/src-tauri/src/lib.rs                   MOD  manage SchedulerWake; register retry_failed_items
apps/desktop/src-tauri/tests/wire_fixtures.rs       MOD  scheduler fixtures gain failed_items
apps/desktop/src/lib/fixtures/scheduler_state*.json MOD  regenerated
apps/desktop/src/lib/api.ts                         MOD  split FIRST (Task 7), then failed_items
apps/desktop/src/lib/scheduler-status.ts            MOD  failedLine
apps/desktop/src/lib/AlwaysOnSection.svelte         MOD  failed line + Retry button
apps/desktop/src/lib/AlwaysOnSection.test.ts        MOD  three tests
apps/desktop/src/lib/fixtures.test.ts               MOD  scheduler/doctor pins move out
apps/desktop/src/lib/api-alwayson.ts                NEW  always-on/health wire subject (Task 7)
apps/desktop/src/lib/fixtures.alwayson.test.ts      NEW  its fixture pins, incl. failed_items
apps/desktop/.oxlintrc.json                         MOD  api.ts cap lowered, next split named

apps/desktop/src/lib/IngestView.svelte              MOD  path fields
apps/desktop/src/lib/IngestView.test.ts             MOD  button names
apps/desktop/src/lib/IngestView.paths.test.ts       NEW  typed-path tests
apps/desktop/src/lib/ingest-test-support.ts         MOD  helpers for typed paths
apps/desktop/e2e/setup/fixture-catalog.ts           MOD  ingest source/dest dirs, PARA node
apps/desktop/e2e/specs/ingest.e2e.ts                NEW  the flow
apps/desktop/e2e/wdio.conf.ts                       MOD  explicit spec order

apps/desktop/src-tauri/src/tray.rs                  MOD  embed @2x PNGs
crates/cli/src/search.rs                            MOD  results_line
crates/services/src/sync.rs                         MOD  BlobStore::root()

docs/superpowers/plans/2026-07-29-phase2-watchlist.md          MOD  7F deferrals + mutants
docs/superpowers/specs/2026-09-14-phase7f-hardening-design.md  MOD  as-built section
docs/superpowers/HANDOFF-phase7G.md                            NEW
```

Chunk order below is the merge order. Chunk 2 (whisper) goes FIRST after
the spec PR because main is red on that job today.

**Two AMENDMENTS to the spec, decided while planning.** First: the wire
split moves from chunk 6 to the front of chunk 4 — `api.ts` sits at 639
lines under a 640-line cap, so the two lines the scheduler field and its
wrapper add would fail `pnpm lint` before the split. Second: the spec put a
`pub fn env_api_key()` in `crates/describe`; that crate documents itself
as never reading the environment (`client.rs:72`), and so does
`crates/services`. So the *variable name* lives in `crates/describe` as
`pub const OPENROUTER_KEY_ENV: &str = "MAJ_OPENROUTER_KEY"`, and each head
(CLI, MCP — both in `crates/cli`, and the desktop) reads the env itself
through that const. Doctor's describer check takes the key as a
`DoctorRequest` field the head fills in — the hermetic seam the spec's
Testing section already asked for.

---

## PR Chunk 2 — the committed whisper fixture (fixes red main)

### Task 1: commit the fixture, regenerate it safely, guard both tests

**Files:**
- Create: `conformance/whisper/fixture.wav`
- Create: `crates/index/tests/common/mod.rs`
- Modify: `crates/index/tests/whisper_gated.rs`, `crates/index/tests/whisper_conformance.rs`
- Modify: `justfile` (the `whisper-conformance` recipe; new `whisper-fixture`)
- Modify: `.github/workflows/ci.yml` (comment on the whisper job's ffmpeg step)

Evidence this task rests on: CI run 34882576400 (main, 2026-09-14) — the
`say` step produced silence, `whisper_rs_matches_faster_whisper_reference`
PASSED on it (shared hallucination), and only `whisper_gated`'s `is_silent`
assert failed. Two defects, both closed here.

- [ ] **Step 1: Produce the fixture from a known-good local file.**
  `target/whisper-fixture.wav` exists on the dev machine (2026-08-01,
  306 766 bytes, the same `say` text the recipe uses). Check it is not
  silent, then copy it:

```bash
ffmpeg -v error -i target/whisper-fixture.wav -af volumedetect -f null - 2>&1 | grep max_volume
# expect something like "max_volume: -3.1 dB" — NOT "-91.0 dB"
cp target/whisper-fixture.wav conformance/whisper/fixture.wav
```

  If `target/whisper-fixture.wav` is missing or silent, run the
  `whisper-fixture` recipe from Step 4 instead (it writes the same path).

- [ ] **Step 2: Write the failing guard tests.** Create
  `crates/index/tests/common/mod.rs` (first check `ls crates/index/tests`
  — if a `common/` already exists, add to it):

```rust
//! Shared helpers for the whisper gates. `mod common;` in each test file.
#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

/// True when every sample is (numerically) zero: the shape a silent `say`
/// fixture decodes to. Both whisper gates refuse such a fixture up front
/// rather than letting two models agree on a hallucination.
pub fn is_silent(pcm: &[f32]) -> bool {
    pcm.iter().all(|sample| sample.abs() < 1e-6)
}

pub const SILENT_FIXTURE_MSG: &str =
    "fixture is silent — regenerate with `just whisper-fixture`";
```

  In `whisper_gated.rs`: delete the local `is_silent`, add `mod common;`
  and `use common::{is_silent, SILENT_FIXTURE_MSG};`, use the constant in
  both asserts, and add (NOT `#[ignore]`d — it runs everywhere):

```rust
#[test]
fn is_silent_discriminates_zero_from_signal() {
    assert!(is_silent(&[0.0, 0.0, 1e-7]));
    assert!(!is_silent(&[0.0, 0.0, 1e-3]));
    assert!(is_silent(&[]), "an empty buffer has no signal");
}
```

  In `whisper_conformance.rs`: `mod common;`, and right after
  `let pcm = video::extract_audio_pcm(...)` add
  `assert!(!common::is_silent(&pcm), "{}", common::SILENT_FIXTURE_MSG);`.

- [ ] **Step 3: Run the non-ignored guard test** —
  `cargo test -p majestical-index --test whisper_gated is_silent` → PASS
  (the helper is trivial; the point is that the test now exists in a
  binary that runs on every CI leg).

- [ ] **Step 4: The recipes.** In `justfile`, replace the `say`/`ffmpeg`
  lines of `whisper-conformance` with the committed path, and add the
  regeneration recipe. The silence check parses ffmpeg's `volumedetect`
  filter (`max_volume: -91.0 dB` is digital silence; real speech peaks
  well above -30 dB). The recipe's temp dir is cleaned by the same
  `trap ... EXIT` idiom the `tray-icons` recipe already uses — copy that
  line verbatim from `tray-icons` rather than typing it:

```make
# Regenerates the committed whisper fixture from macOS `say`. Refuses a
# silent result (a flake `say` produces on headless runners — CI run
# 34882576400 is the recorded instance), so the committed file can never
# be the silent one. CI never runs this; it reads the committed file.
whisper-fixture:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    # <the tray-icons recipe's trap line, verbatim>
    say -o "$tmp/fixture.aiff" "The quick brown fox jumps over the lazy dog. \
        We reviewed the quarterly budget on Tuesday and shipped the release candidate."
    # 2s leading silence — see whisper_conformance.rs's module doc.
    ffmpeg -y -v error -i "$tmp/fixture.aiff" -af "adelay=2000:all=1" -ar 16000 -ac 1 "$tmp/fixture.wav"
    peak=$(ffmpeg -v error -i "$tmp/fixture.wav" -af volumedetect -f null - 2>&1 \
        | sed -n 's/.*max_volume: \(-\{0,1\}[0-9.]*\) dB.*/\1/p')
    if [ -z "$peak" ] || awk -v p="$peak" 'BEGIN { exit !(p < -60) }'; then
        echo "whisper-fixture: synthesized audio is silent (peak ${peak:-unknown} dB) — not written" >&2
        exit 1
    fi
    mv "$tmp/fixture.wav" conformance/whisper/fixture.wav
    echo "wrote conformance/whisper/fixture.wav (peak ${peak} dB)"
```

  and `whisper-conformance` becomes: model fetch (unchanged), `mkdir -p
  target` (the golden still lands there), then
  `uv run conformance/whisper/golden.py --revision {{WHISPER_TORCH_REVISION}} --audio conformance/whisper/fixture.wav --out target/whisper-golden.json`,
  then the `cargo test` line with
  `MAJ_AUDIO="{{justfile_directory()}}/conformance/whisper/fixture.wav"`.
  Delete the `say`/`adelay` lines. Keep `brew install ffmpeg` in `ci.yml`
  (PCM extraction still needs it) and change its comment from "say ships
  with macOS; only ffmpeg needs installing" to "PCM extraction only — the
  fixture is committed, nothing synthesizes speech in CI".

- [ ] **Step 5: Prove the recipe's refusal path.** Run the silence check
  against a generated silent file (in the session scratchpad dir):

```bash
ffmpeg -v error -f lavfi -i anullsrc=r=16000:cl=mono -t 2 "$SCRATCH/silent.wav"
ffmpeg -v error -i "$SCRATCH/silent.wav" -af volumedetect -f null - 2>&1 | grep max_volume
# expect max_volume: -91.0 dB  → the recipe's awk branch would refuse it
```

  Record the observed line in the PR description. Then run the real
  gate locally once: `just whisper-conformance` → both tests pass (the
  model cache is ~2 GB; if absent, the recipe's own `maj model fetch`
  step downloads it).

- [ ] **Step 6: Verify + commit**

```bash
cargo clippy -p majestical-index --all-targets --all-features -- -D warnings
cargo test -p majestical-index --test whisper_gated is_silent
git add conformance/whisper/fixture.wav crates/index/tests/common/mod.rs \
  crates/index/tests/whisper_gated.rs crates/index/tests/whisper_conformance.rs \
  justfile .github/workflows/ci.yml
git commit -m "test: commit the whisper fixture and refuse silence in both gates"
```

  PR description records the CI evidence (run id, the vacuous pass) and
  the observed refusal line. Watch `whisper-conformance` to green.

---

## PR Chunk 3 — failure classes + the ledger (services, CLI, MCP)

### Task 2: `ItemFailure` and the transient class

**Files:**
- Modify: `crates/services/src/index/run.rs` (every `failed: Vec<(PathBuf, String)>`
  and every `.failed.push(...)` site — `rg -n 'failed\.push|failed: Vec' crates/services/src/index/run.rs`
  lists all of them; there are ten outcome structs and nine push sites)
- Modify: `crates/cli/src/index_cmd.rs` (`failed_json`, the `eprintln!` loop)
- Modify: `apps/desktop/src-tauri/src/indexer.rs` tests (constructing failures)

**Wire shape:**

```rust
/// One item that did not get its derivation this pass. `transient` means
/// the item was never really attempted — the describer backend failed or
/// cascaded, or the source vanished mid-batch — and must NOT be remembered
/// by the ledger (`crate::index::record_failures`); everything else is a
/// permanent failure of this item's bytes and is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ItemFailure {
    pub asset: String,
    #[serde(serialize_with = "path_display")]
    pub path: PathBuf,
    pub error: String,
    pub transient: bool,
}

impl ItemFailure {
    /// Classifies a runner error by the one signal every kind shares: if
    /// the source path no longer exists when the failure is recorded, the
    /// volume went away under the batch — transient. The empty-path
    /// sentinel `TranscriptEmbed` items carry (they read a blob, not the
    /// source) is never "vanished".
    #[must_use]
    pub fn classify(item: &work::WorkItem, error: impl std::fmt::Display) -> Self {
        let vanished = !item.abs_path.as_os_str().is_empty() && !item.abs_path.exists();
        Self {
            asset: item.asset.clone(),
            path: item.abs_path.clone(),
            error: error.to_string(),
            transient: vanished,
        }
    }

    /// A failure the caller already knows is not the item's fault.
    #[must_use]
    pub fn transient(item: &work::WorkItem, error: impl Into<String>) -> Self {
        Self {
            asset: item.asset.clone(),
            path: item.abs_path.clone(),
            error: error.into(),
            transient: true,
        }
    }
}
```

`path_display` replaces today's `serialize_failed_items`/`FailedItem`
(delete both):
`fn path_display<S: serde::Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(&p.display().to_string()) }`.
Every `Vec<(PathBuf, String)>` becomes `Vec<ItemFailure>`; every push site
becomes `ItemFailure::classify(item, err)` except the caption pass's
`CaptionFailure::Backend` arm and its `DESCRIBER_SKIPPED_REASON` cascade,
which become `ItemFailure::transient(...)`. `transcript_failures()` returns
`Vec<ItemFailure>`. The CLI deletes its own `failed_json` and uses
`serde_json::to_value(&o.<kind>.failed)` — the rows come out as
`{asset, path, error, transient}` from the struct's own derive; its text loop prints
`failed (transient): {err}` when `transient` else `failed: {err}`.

- [ ] **Step 1: Failing tests** in `run.rs`'s `mod tests` (next to the
  existing wire-shape test near `run.rs:1959`, which you update to expect
  the four fields):
  - `classify_marks_a_vanished_source_transient`: a `WorkItem` whose
    `abs_path` is a tempdir file that you delete before classifying →
    `transient == true`; the same item with the file present → `false`.
  - `classify_never_marks_the_empty_path_sentinel_transient`: `abs_path:
    PathBuf::new()` → `false`.
  - `backend_failure_cascade_is_transient_for_every_skipped_item`: drive
    `run_caption_items` against the existing mock-backend arrangement
    the tests near `run.rs:2244` use, with the backend refusing → every
    `failed` row has `transient == true`.
  - `failed_item_wire_shape_carries_transient`: `serde_json::to_value`
    of one `ItemFailure` == `json!({"asset":..,"path":..,"error":..,"transient":false})`.
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib index::run`
  — compile errors.
- [ ] **Step 3: Implement** as specced. `made_progress` is unchanged (it
  reads written counters, not failures). `total_failures` in
  `indexer.rs` is unchanged (`.len()`). Fix the desktop test that pushes
  a tuple (`indexer.rs`, `batch_outcome_pace_holds_and_names_the_failure_count_when_nothing_progressed`)
  to push an `ItemFailure`.
- [ ] **Step 4: Run**
  `cargo test -p majestical-services -p majestical-cli && cargo clippy --workspace --all-targets --all-features -- -D warnings`
  and, in `apps/desktop/src-tauri`, `cargo test --lib indexer && cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** (`crates/services/src/index/run.rs crates/cli/src/index_cmd.rs apps/desktop/src-tauri/src/indexer.rs`):
  `git commit -m "feat: classify per-item index failures as transient or permanent"`

### Task 3: the ledger — read, merge, record, clear, apply

**Files:**
- Modify: `crates/index/src/work.rs` (`KindStatus.failed: u64`, doc: "blob
  missing, but a previous run failed on this item and the ledger holds it
  back until retried")
- Modify: `crates/services/src/index/mod.rs`
- (`proptest` is already a dev-dependency of `crates/services` —
  `proptest.workspace = true` — nothing to add)

**Shapes (in `mod.rs`, replacing `read_failure_report`/
`failure_report_json`/`merge_failure_report`/`write_failure_report`/
`update_failure_report` — delete those, no shims):**

```rust
/// One remembered permanent failure. `asset` is the key the planner
/// matches on; `path`/`error` are for display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LedgerRow {
    pub asset: String,
    pub path: String,
    pub error: String,
}

/// `{kind: [row, ..]}`, kind = the `--kinds` name (`workkind_name`).
pub type Ledger = BTreeMap<String, Vec<LedgerRow>>;

/// Reads the ledger; missing → empty, unparsable → empty + notice
/// ("note: ignoring unparsable failure ledger at {path} — treating as empty").
#[must_use]
pub fn read_ledger(state_dir: &Path, notices: &Notices) -> Ledger;
fn write_ledger(state_dir: &Path, ledger: &Ledger) -> Result<()>;

/// This pass's PERMANENT failures as a ledger fragment (transient dropped).
fn permanent_failures(outcome: &IndexRunOutcome) -> Ledger;

/// previous ∪ current, per kind, deduplicated by `asset` — a re-failed
/// item's row is REPLACED (fresh error text), never duplicated. No kind is
/// ever cleared by a run; only `clear_failures` clears.
fn merge_ledger(previous: Ledger, current: Ledger) -> Ledger;

/// After a run: folds its permanent failures into the ledger.
pub fn record_failures(
    catalog_dir: &Path,
    outcome: &IndexRunOutcome,
    notices: &Notices,
) -> Result<(), ServiceError>;

/// Drops every row for `kinds`; returns how many rows were removed.
pub fn clear_failures(
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    notices: &Notices,
) -> Result<u64, ServiceError>;

/// The ledger as read — what the MCP dry-run counts a retry would clear.
pub fn known_failures(catalog_dir: &Path, notices: &Notices) -> Result<Ledger, ServiceError>;

/// Pure: removes items with a ledger row for (kind name, asset), moving
/// each from its kind's `pending` to its `failed` count.
#[must_use]
pub fn apply_ledger(plan: WorkPlan, ledger: &Ledger) -> WorkPlan;
```

`build_plan` gains a `ledger: &Ledger` parameter and ends with
`apply_ledger(plan, ledger)` after the `kinds` retain. Both `status_impl`
and `run_impl` read the ledger from the state dir they already resolve.
`IndexRunReq` gains `pub retry_failed: bool`; `run_impl` calls
`clear_failures(catalog_dir, &req.kinds, notices)?` before `build_plan`
when set, pushing a notice `"cleared {n} known failure(s) for retry"`.
`KindStatusRow` gains `failed`; `IndexStatusOutcome.failed_last_run`
becomes `pub failed: Ledger` (the rows). `record_failures` is what the
CLI's `run_once`, MCP's `index_run_exec`, and the desktop's `run_batch`
call in place of `update_failure_report` (rename at all three sites —
`rg -n update_failure_report` finds them; the `kinds` argument goes away).

- [ ] **Step 1: Failing tests** in `mod.rs`'s `mod tests` (the
  failure-report tests at `mod.rs:708-830` are replaced by these):
  - `merge_ledger_replaces_a_refailed_row_and_keeps_other_kinds`.
  - `permanent_failures_drops_transient_rows`: an outcome with one
    transient and one permanent thumb failure → ledger has one row.
  - `apply_ledger_moves_a_matching_item_from_pending_to_failed`: a
    hand-built `WorkPlan` with two Thumb items and `thumbs.pending == 2`
    → after applying a ledger naming one asset under `"thumbs"`: one item
    left, `pending == 1`, `failed == 1`. A row under a different kind for
    the same asset does NOT remove it.
  - `apply_ledger_maps_both_transcript_work_kinds_to_the_transcripts_key`
    (and likewise both OCR kinds → `"ocr"`): pins `workkind_name`'s
    two-to-one mapping in the ledger key.
  - `clear_failures_removes_only_the_named_kinds_and_reports_the_count`.
  - `read_ledger_treats_missing_and_unparsable_as_empty_with_a_notice`.
  - proptest `no_transient_failure_ever_reaches_the_ledger`: random
    `IndexRunOutcome`s (strategy: per kind 0-4 `ItemFailure`s with random
    `transient`) → `permanent_failures` contains exactly the
    `!transient` ones, each asset at most once per kind. Pattern:
    `crates/core/tests/crdt_properties.rs` (NOT `projection.rs`).
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib index` — fails.
- [ ] **Step 3: Implement.** `apply_ledger` needs a
  `kind_status_mut(plan: &mut WorkPlan, kind: WorkKind) -> &mut KindStatus`
  helper (exhaustive match, no wildcard) and `workkind_name` (already
  there). Keep every function ≤100 lines.
- [ ] **Step 4: Run** the services tests +
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
  Fixing the downstream call sites (CLI, MCP, desktop) is Task 4; if the
  workspace does not compile in between, land Tasks 3 and 4 as ONE commit
  rather than leaving a red commit on the branch.
- [ ] **Step 5: Commit** (`crates/index/src/work.rs crates/services/src/index/mod.rs`):
  `git commit -m "feat: failed-item ledger — skip known permanent failures until retried"`

### Task 4: CLI `--retry-failed`, status rendering, MCP `retry_failed`

**Files:**
- Modify: `crates/cli/src/main.rs` (clap: `IndexRun` gains
  `#[arg(long)] retry_failed: bool` with doc "Clear the failure ledger for
  the selected kinds first, then run — a still-broken item is simply
  re-recorded"), `crates/cli/src/index_cmd.rs`, `crates/cli/src/mcp_cmd/write_tools.rs`,
  `crates/cli/src/mcp_cmd/read_tools.rs` (doc comment of `index_status`:
  "plus the known permanent failures the ledger holds back until retried")
- Modify: `crates/cli/tests/index_smoke.rs`, `crates/cli/tests/mcp_smoke.rs`
- Check: `rg -n 'failed_last_run|index' crates/cli/tests/services_parity.rs`
  — if a parity row compares status JSON, update its expected keys.

CLI rendering:
- `print_kind_status`: append `, {} failed` (after `need model`).
- `kind_status_json`: add `"failed"`.
- `cmd_index_status` JSON: key `"failed"` (was `failed_last_run`) with the
  ledger rows; text: `print_known_failures(&outcome.failed)` prints, per
  kind with rows,
  `"{kind}: {n} known failure(s), skipped until `maj index run --retry-failed` ({first error})"`.
- `IndexRunArgs` gains `retry_failed: bool`; the req is built with it.

MCP:
- `IndexRunArgs` (write_tools) gains `#[serde(default)] retry_failed: bool`
  with doc "Clear the failure ledger for `kinds` before running, so items
  that failed permanently are attempted again."
- `index_run_dry` reads `known_failures` and adds
  `"retry_failed": args.retry_failed` and
  `"known_failures": {kind: count}` (only kinds in the request), and when
  `retry_failed` the `would` string becomes
  `"clear {n} known failure(s) for {kinds-with-rows}, then run one derivation pass over {kinds}"`.
  Real state read, never a guess.
- `index_run_exec` passes `retry_failed` on the req and calls
  `record_failures`.

- [ ] **Step 1: Failing integration test** in `index_smoke.rs`:

```rust
/// A permanently undecodable image (`.png` extension, text bytes) fails
/// once, is remembered, is SKIPPED on the next run (no attempt, no new
/// failure), and is attempted again only under `--retry-failed`.
#[test]
fn index_run_remembers_a_permanent_failure_and_skips_it_until_retried() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("broken.png"), b"this is not a png").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");
    let state = catalog.path().join("state");
    maj(&root, &state).args(["catalog", "init"]).assert().success();
    maj(&root, &state).args(["scan"]).arg(media.path()).assert().success();

    maj(&root, &state).args(["index", "run", "--kinds", "thumbs"]).assert().success()
        .stdout(contains("thumbnails: 0 written, 1 failed"));
    maj(&root, &state).args(["index", "status"]).assert().success()
        .stdout(contains("thumbs: 0 done, 0 pending, 0 offline, 0 unsupported, 0 need ffmpeg, 0 need model, 1 failed"))
        .stdout(contains("thumbs: 1 known failure(s), skipped until `maj index run --retry-failed`"));
    // Skipped: no attempt, so no failure this run.
    maj(&root, &state).args(["index", "run", "--kinds", "thumbs"]).assert().success()
        .stdout(contains("thumbnails: 0 written, 0 failed"));
    // Retried: attempted again, fails again, re-recorded (still 1 row, not 2).
    maj(&root, &state).args(["index", "run", "--kinds", "thumbs", "--retry-failed"]).assert().success()
        .stdout(contains("thumbnails: 0 written, 1 failed"));
    let out = maj(&root, &state).args(["index", "status", "--json"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["failed"]["thumbs"].as_array().unwrap().len(), 1);
    assert_eq!(json["thumbs"]["failed"], 1);
}
```

  And in `mcp_smoke.rs` (pattern: `index_status_matches` at `:639`,
  `call_tool`): `index_run_dry_run_reports_the_rows_a_retry_would_clear`
  — seed the same broken-png catalog via the CLI, run once via the CLI,
  then `call_tool("index_run", json!({"kinds": ["thumbs"], "retry_failed": true}))`
  → structured content has `known_failures.thumbs == 1` and `would`
  starts with `"clear 1 known failure(s) for thumbs"`; the ledger file
  (`<state>/index-failures.json`) is byte-identical before/after (dry
  run mutates nothing). Then `confirm: true` → the response's
  `thumbs.failed` array has length 1 (attempted again). With
  `retry_failed: true` against an EMPTY ledger the dry-run `would` reads
  `"clear 0 known failures (none recorded), then run one derivation pass over thumbs"`
  — a no-op named as such, never an error.
- [ ] **Step 2: Run** both test binaries — fail.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run**
  `cargo test -p majestical-cli --test index_smoke --test mcp_smoke --test services_parity && cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: --retry-failed at the CLI and MCP heads; status reports the ledger"`

---

## PR Chunk 4 — the ledger at the GUI head, doctor, and the describer key

### Task 5: the key constant, the head readers, and the named no-key failure

**Files:**
- Modify: `crates/describe/src/config.rs` — add
  `pub const OPENROUTER_KEY_ENV: &str = "MAJ_OPENROUTER_KEY";` next to
  `DescriberConfig`, and reference it from the `api_key` field doc and
  `effective_api_key`'s doc.
- Modify: `crates/cli/src/describer_cmd.rs` — `env_api_key` reads
  `std::env::var(majestical_describe::config::OPENROUTER_KEY_ENV)`.
- Modify: `crates/services/src/index/run.rs` — the no-key failure in
  `run_caption_items`.
- Modify: `apps/desktop/src-tauri/src/commands.rs` — add
  `pub(crate) fn env_api_key() -> Option<String>` (same three lines as the
  CLI's, same const). `apps/desktop/src-tauri/Cargo.toml` does NOT yet
  depend on `majestical-describe` (verified while planning): add it with
  the same path style the `majestical-services` line there uses.

The named failure, in `run_caption_items` right after `load_config`
succeeds:

```rust
/// Recorded for every caption item in a pass when OpenRouter is configured
/// but no key is available from the config file or the environment —
/// before any request is made. Transient: operator-fixable, not the
/// item's fault, so the ledger never remembers it.
pub const OPENROUTER_KEY_MISSING_REASON: &str =
    "OpenRouter needs an API key — set it with `maj describer set --api-key` or MAJ_OPENROUTER_KEY";

if config.backend == BackendKind::OpenRouter
    && config.effective_api_key(env.api_key.clone()).is_none()
{
    for item in items {
        outcome.failed.push(ItemFailure::transient(item, OPENROUTER_KEY_MISSING_REASON));
    }
    return outcome;
}
```

- [ ] **Step 1: Failing test** in `run.rs`'s `mod tests`:
  `openrouter_without_a_key_fails_every_item_transiently_before_any_request`
  — write a `describer.toml` with `backend = "open-router"`, a
  `base_url` pointing at a port nothing listens on (so a request would
  error differently, proving none was made), `model = "x"`, no key;
  `PassEnv.api_key = None`; two items → both rows `transient == true`
  with `error == OPENROUTER_KEY_MISSING_REASON`. A second case with
  `PassEnv.api_key = Some("sk-test")` must NOT take this branch (it
  proceeds and fails on the dead port with a different reason). Use the
  same temp-catalog + `describer_config::set` arrangement the tests near
  `run.rs:2244` use.
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib openrouter_without` — fails.
- [ ] **Step 3: Implement**; `rg -n MAJ_OPENROUTER_KEY crates apps` must
  afterwards hit only the const definition, doc comments, and the reason
  string.
- [ ] **Step 4: Run** `cargo test -p majestical-services -p majestical-cli -p majestical-describe && cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: one OpenRouter key env name; captions fail by name when no key is configured"`

### Task 6: doctor's `failed_items` and `describer` checks

**Files:**
- Modify: `crates/services/src/doctor.rs`
- Modify: the CLI's `cmd_doctor` (`rg -n 'fn cmd_doctor' crates/cli/src`)
  and the MCP `doctor` read tool (`rg -n 'fn doctor' crates/cli/src/mcp_cmd/read_tools.rs`)
  to fill the new request field from `describer_cmd::env_api_key()`.
- Modify: `apps/desktop/src-tauri/src/commands.rs::doctor_report_impl` to
  fill it from the desktop's `env_api_key()`.

**Shapes:**

```rust
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct DoctorRequest {
    pub catalog: Option<PathBuf>,
    /// The head's reading of `OPENROUTER_KEY_ENV`, passed in because this
    /// crate never reads the environment. `None` = not set.
    #[serde(default)]
    pub describer_env_key: Option<String>,
}
```

Two new checks, emitted after `blob_residue` and before `platform`
(update `doctor_emits_checks_in_documented_order`):

| name | Ok when | otherwise |
|---|---|---|
| `failed_items` | catalog given and `index::read_ledger` (via `state_dir_for`) is empty — detail "no known failures" | Warn; detail `"{n} item(s) skipped after failing permanently: {up to 3 paths}"`; remedy `maj index run --retry-failed` (or "Retry failed items" in Settings → Always-on). No catalog → Warn "no catalog selected", like the other catalog checks. |
| `describer` | no catalog → Warn "no catalog selected"; `load_config` → `None` → Ok "no describer configured — captions off"; local backend (Ollama / LM Studio) → Ok naming backend + model; OpenRouter with `effective_api_key(req.describer_env_key)` `Some` → Ok "open-router, key configured" | OpenRouter and key `None` → Fail; detail "open-router configured but no API key from describer.toml or MAJ_OPENROUTER_KEY"; remedy the same two commands `OPENROUTER_KEY_MISSING_REASON` names. Unreadable config → Warn with the error. |

Both are `fn check_*(catalog: Option<&Path>, env_key: Option<&str>, notices: &Notices) -> DoctorCheck`
(the ledger check ignores `env_key`; keep one shape for the seam).

- [ ] **Step 1: Failing tests** (`doctor.rs` `mod tests`, copying the
  temp-catalog arrange from `doctor_with_real_catalog_reports_ok_catalog`):
  - `failed_items_is_ok_on_an_empty_ledger` / `failed_items_warns_with_count_and_remedy`
    (write a ledger JSON with two rows under `"thumbs"` into the state
    dir first; assert `detail` contains `"2 item(s)"` and both paths).
  - `describer_is_ok_when_unconfigured`, `describer_is_ok_for_a_local_backend`,
    `describer_fails_for_openrouter_without_any_key`,
    `describer_is_ok_for_openrouter_with_only_the_env_key` (config keyless,
    `env_key: Some("sk")` → Ok), `describer_is_ok_for_openrouter_with_only_the_file_key`.
  - `doctor_emits_checks_in_documented_order` updated to the nine names.
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib doctor` — fails.
- [ ] **Step 3: Implement.** Each check ≤100 lines; explicit `BackendKind`
  arms, no wildcard.
- [ ] **Step 4: Heads.** Fill `describer_env_key` at all three call sites.
  Then in `apps/desktop/src-tauri`: `cargo test --test tauri_parity doctor`
  (the GUI-vs-CLI doctor row must still match — both read the same
  process env in the test, so the new rows agree).
- [ ] **Step 5: Run** `cargo test -p majestical-services -p majestical-cli --test doctor_smoke && cargo clippy --workspace --all-targets --all-features -- -D warnings`
  and the desktop `cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 6: Commit**:
  `git commit -m "feat: doctor reports the failure ledger and the describer key"`

### Task 7: split the always-on/health wire subject out of `api.ts`

**Files:**
- Create: `apps/desktop/src/lib/api-alwayson.ts`
- Create: `apps/desktop/src/lib/fixtures.alwayson.test.ts`
- Modify: `apps/desktop/src/lib/api.ts`, `apps/desktop/src/lib/fixtures.test.ts`,
  `apps/desktop/.oxlintrc.json`, and every importer of the moved names
  (`rg -n "ThrottleOverride|PowerSource|PowerState|SchedulerDecision|HoldReason|SchedulerStateOutcome|CheckStatus|DoctorCheck|DoctorOutcome|NAVIGATE_SETTINGS_EVENT" apps/desktop/src --glob '!api.ts'`)

Moves, verbatim with their doc comments: the types `ThrottleOverride`,
`PowerSource`, `PowerState`, `SchedulerDecision`, `HoldReason`,
`SchedulerStateOutcome`, `CheckStatus`, `DoctorCheck`, `DoctorOutcome`;
the constant `NAVIGATE_SETTINGS_EVENT`; and the wrappers `doctorReport`,
`schedulerState`, `setThrottle` into

```ts
// The always-on/health wire subject — `indexer.rs`'s scheduler commands
// and `commands::doctor_report` — split out of `api.ts` when that file
// reached its cap (see .oxlintrc.json). Same rules as api.ts: one
// interface per outcome struct, pinned by `fixtures.alwayson.test.ts`.
import { invoke } from "@tauri-apps/api/core";
/* …types… */
export const alwaysOnApi = {
  doctorReport: () => invoke<DoctorOutcome>("doctor_report"),
  schedulerState: () => invoke<SchedulerStateOutcome>("scheduler_state"),
  setThrottle: (throttle: ThrottleOverride) =>
    invoke<SchedulerStateOutcome>("set_throttle", { throttle }),
};
```

`api.ts` imports it and spreads it: `export const api = { ...alwaysOnApi, appStatus: … }` —
one object, one import site for consumers that already call `api.x()`.
Types are NOT re-exported from `api.ts`: importers switch to
`./api-alwayson` (AlwaysOnSection.svelte, scheduler-status.ts,
SettingsView.svelte, App.svelte for the event name, tray-adjacent code,
the tests). `fixtures.alwayson.test.ts` takes the `scheduler_state`,
`scheduler_state_held` and `doctor_outcome` describes from
`fixtures.test.ts`. `.oxlintrc.json`: set the `api.ts` cap to the file's
new line count rounded up to the next 10, and rewrite the comment's last
sentence: "The next raise is refused: the ingest subject (plan/run/
progress types and their wrappers) moves to `api-ingest.ts` next."

- [ ] **Step 1:** Move; `pnpm check` finds every stale import — fix each.
- [ ] **Step 2:** Run the four-command line; `pnpm lint` must be clean at
  the new cap (`wc -l src/lib/api.ts` to pick the number).
- [ ] **Step 3: Commit** (all touched files by name):
  `git commit -m "refactor: split the always-on/health wire types into api-alwayson.ts"`

### Task 8: `failed_items` on the scheduler, the nudge, `retry_failed_items`

**Files:**
- Modify: `apps/desktop/src-tauri/src/indexer.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (`.manage(SchedulerWake::default())`
  next to `SchedulerState`; register `indexer::retry_failed_items`)
- Modify: `apps/desktop/src-tauri/tests/wire_fixtures.rs` (both scheduler
  fixtures gain `failed_items`: 0 in `scheduler_state`, 3 in
  `scheduler_state_held`), regenerate `apps/desktop/src/lib/fixtures/scheduler_state*.json`
- Modify: `apps/desktop/src/lib/api-alwayson.ts` (`failed_items: number` on
  `SchedulerStateOutcome`; `retryFailedItems: () => invoke<SchedulerStateOutcome>("retry_failed_items")` in `alwaysOnApi`)
- Modify: `apps/desktop/src/lib/fixtures.alwayson.test.ts` (pin `failed_items` on both)

**Shapes (indexer.rs):**

```rust
pub struct SchedulerShared {
    /* existing fields */
    /// The sum of every kind's `failed` row from the last status poll —
    /// items the ledger holds back until retried. See [`failed_items`].
    pub failed_items: u64,
}

pub struct SchedulerStateOutcome {
    /* existing fields, then: */
    pub failed_items: u64,
}

/// Sum of every kind's `failed` row — the mirror of [`pending_items`].
#[must_use]
pub fn failed_items(status: &IndexStatusOutcome) -> u64;

/// The loop's sleep, made interruptible: `retry_failed_items` sets the
/// flag and notifies so the next tick starts now instead of after up to
/// [`TICK`]. Managed separately from `SchedulerState` so the existing
/// `RwLock` users (`tray.rs`, the tests) are untouched.
#[derive(Default)]
pub struct SchedulerWake {
    woken: Mutex<bool>,
    cv: Condvar,
}

impl SchedulerWake {
    /// Sleeps up to `pause`, returning early if nudged; consumes the nudge.
    pub fn wait(&self, pause: Duration);
    pub fn nudge(&self);
}
```

`run_loop` calls `app.state::<SchedulerWake>().wait(pause)` instead of
`std::thread::sleep(pause)`. `poll_and_decide` stores
`shared.failed_items = failed_items(&status)`. `batch_request` gains an
`api_key: Option<String>` parameter (pure; tests pass `None`), and
`poll_and_decide` reads `crate::commands::env_api_key()` once per tick.
`run_batch` calls `index::record_failures(&cfg.catalog, &outcome, fs_app.notices())`.
`IndexRunReq` construction sets `retry_failed: false`.

The command:

```rust
/// Clears the failure ledger for every kind of the selected catalog, zeroes
/// the reported count, and nudges the loop. The next tick re-plans, so the
/// retried items are attempted within seconds rather than a full `TICK`.
pub(crate) fn retry_failed_items_impl(
    cfg: Option<&CatalogCfg>,
    scheduler: &SchedulerState,
    wake: &SchedulerWake,
) -> Result<SchedulerStateOutcome, CommandError> {
    let Some(cfg) = cfg else {
        // `CommandError` has `impl<E: Into<anyhow::Error>> From<E>` (commands.rs:84).
        return Err(anyhow::anyhow!("no catalog selected").into());
    };
    let fs_app = open_app(cfg)?;
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    index::clear_failures(&cfg.catalog, &kinds, fs_app.notices())?;
    scheduler.0.write().unwrap_or_else(PoisonError::into_inner).failed_items = 0;
    wake.nudge();
    Ok(scheduler_state_impl(scheduler))
}

#[tauri::command]
pub fn retry_failed_items(
    app: AppHandle,
    state: State<'_, AppState>,
    scheduler: State<'_, SchedulerState>,
    wake: State<'_, SchedulerWake>,
) -> Result<SchedulerStateOutcome, CommandError> {
    let outcome = retry_failed_items_impl(selected_catalog(&state).as_ref(), &scheduler, &wake);
    crate::tray::refresh(&app);
    outcome
}
```

- [ ] **Step 1: Failing tests** in `indexer.rs`'s `mod tests`:
  - `failed_items_sums_every_kind` (extend `kind_row` to take `failed`).
  - `wake_returns_early_when_nudged_and_consumes_the_nudge`: nudge, then
    `wait(Duration::from_secs(5))` returns in well under a second (assert
    elapsed < 1 s); a second `wait(50ms)` without a nudge takes ≥ 50 ms.
  - `retry_failed_items_without_a_catalog_is_an_error` (`cfg: None`).
  - `retry_failed_items_clears_the_ledger_zeroes_the_count_and_nudges`:
    build a temp catalog (copy the arrange the `*_impl` integration tests
    in `apps/desktop/src-tauri/tests/commands.rs` use), write a ledger with one row into
    its state dir, set `shared.failed_items = 1`, call the impl → the
    ledger file has no rows, outcome `failed_items == 0`, and a
    subsequent `wake.wait(5 s)` returns immediately.
  - Update `wire_shape_run_full_omits_last_error` / `wire_shape_held_…`
    for the new field.
- [ ] **Step 2: Run** `cargo test --lib indexer` (in `apps/desktop/src-tauri`) — fails.
- [ ] **Step 3: Implement**; regenerate fixtures:
  `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures`, then update
  `fixtures.alwayson.test.ts` to assert `failed_items` is `0`/`3` on the
  two scheduler fixtures; `api-alwayson.ts` gets the field and the wrapper.
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings`
  (desktop workspace) and, in `apps/desktop`, the four-command line.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: scheduler reports failed items; retry_failed_items clears the ledger and nudges the loop"`

### Task 9: the Always-on section's failed line and Retry button

**Files:**
- Modify: `apps/desktop/src/lib/scheduler-status.ts` (types now from `./api-alwayson`) — add
  `export function failedLine(n: number): string` returning
  `"1 item skipped after failing"` / `` `${n} items skipped after failing` ``.
- Modify: `apps/desktop/src/lib/AlwaysOnSection.svelte` — per mockup
  frame 2, under the status line:

```svelte
{#if scheduler && scheduler.failed_items > 0}
  <div class="ctl-actions">
    <p class="settings-status settings-failed" role="status">
      {failedLine(scheduler.failed_items)}
    </p>
    <button class="ctl-btn" onclick={() => void retryFailed()}>
      Retry failed items
    </button>
  </div>
{/if}
{#if retryError}
  <p class="error" role="alert">{retryError}</p>
{/if}
```

  with `async function retryFailed() { retryError = null; try { scheduler = await api.retryFailedItems(); } catch (f) { retryError = errorMessage(f); } }`.
  `app.css`: `.settings-failed { color: var(--attention); }` next to the
  other `settings-` rules.
- Modify: `apps/desktop/src/lib/AlwaysOnSection.test.ts`.

- [ ] **Step 1: Failing tests** (pattern: the existing three in that file):
  - `no failed line when failed_items is 0` (the `auto` fixture):
    `screen.queryByText(/skipped after failing/u)` is null and no
    "Retry failed items" button.
  - `the failed line and button appear with the held fixture's count`:
    `findByText("3 items skipped after failing")`, button present.
  - `clicking Retry invokes retry_failed_items and applies the RESPONSE`:
    mock `retry_failed_items` to return the `auto` fixture (0 failed) →
    after the click the line disappears; the mock was called once.
  - `failedLine` unit: `failedLine(1)` and `failedLine(2)` exact strings
    (in `scheduler-status.test.ts` if it exists, else alongside).
- [ ] **Step 2: Run** `pnpm test -- AlwaysOnSection` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the four-command line in `apps/desktop`.
- [ ] **Step 5: Commit** (`AlwaysOnSection.svelte AlwaysOnSection.test.ts scheduler-status.ts app.css`):
  `git commit -m "feat: Always-on shows skipped-after-failing items with a Retry button"`

  Chunk 4 PR description: the mockup frame this implements, the doctor
  rows as rendered by `maj doctor` on the dev machine (paste the two new
  lines), and a hand check that the Retry button's tick fires promptly
  (watch the tray status change within a few seconds).

---

## PR Chunk 5 — Ingest path entry + the Ingest e2e flow

### Task 10: typed source and destination paths on `IngestView`

**Files:**
- Modify: `apps/desktop/src/lib/IngestView.svelte` (script: `pickSource`,
  `addDest`, new `setSource`, `addTypedDest`, `destDraft`, `destError`;
  markup: the Source and Destinations panels per mockup frame 1)
- Modify: `apps/desktop/src/lib/IngestView.test.ts` (button names only)
- Modify: `apps/desktop/src/lib/ingest-test-support.ts` (two helpers)
- Create: `apps/desktop/src/lib/IngestView.paths.test.ts`

**Markup (replaces the `Choose source…` button and the `+ Add destination`
button; everything else in the two panels stays):**

```svelte
<!-- Source panel -->
<div class="ctl-actions">
  <input
    class="ctl-input"
    type="text"
    aria-label="Source path"
    placeholder="/Volumes/CARD_01 or any folder"
    value={source}
    onchange={(event) => setSource(event.currentTarget.value)}
  />
  <button class="ctl-btn" aria-label="Browse for source" onclick={() => void pickSource()}>
    Browse…
  </button>
</div>

<!-- Destinations panel, after the list -->
<div class="ctl-actions">
  <input
    class="ctl-input"
    type="text"
    aria-label="Destination path"
    placeholder="/Volumes/SHUTTLE_A"
    value={destDraft}
    oninput={(event) => (destDraft = event.currentTarget.value)}
    onkeydown={(event) => { if (event.key === "Enter") addTypedDest(); }}
  />
  <button class="ctl-btn" aria-label="Add destination" onclick={addTypedDest}>Add</button>
  <button class="ctl-btn" aria-label="Browse for destination" onclick={() => void addDest()}>
    Browse…
  </button>
</div>
{#if destError !== null}
  <p class="error" role="alert">{destError}</p>
{/if}
```

**Script:**

```ts
let destDraft = $state("");
let destError = $state<string | null>(null);

/** A typed or browsed source, taken as-is: the plan step validates it. */
function setSource(next: string) {
  const typed = next.trim();
  if (typed === source) return;
  source = typed;
  editJob();
}

/** Adds one destination root; a root already in the list is refused with
 *  the message shown, so a paste that changed nothing does not look like
 *  it did. Shared by the typed field and the folder picker. */
function pushDest(root: string): boolean {
  destError = null;
  if (dests.includes(root)) {
    destError = `${root} is already a destination.`;
    return false;
  }
  dests = [...dests, root];
  editJob();
  return true;
}

function addTypedDest() {
  const typed = destDraft.trim();
  if (typed === "") return;
  if (pushDest(typed)) destDraft = "";
}

async function pickSource() {
  const picked = await pickFolder();
  if (picked !== null) setSource(picked);
}

async function addDest() {
  const picked = await pickFolder();
  if (picked !== null) pushDest(picked);
}
```

`removeDest` also clears `destError`. The existing `source` display line
(`<p class="ingest-path">{source}</p>` / "No source chosen yet.") goes
away — the field IS the display now (the mockup shows no second copy).

- [ ] **Step 1: Failing tests** in the new `IngestView.paths.test.ts`
  (imports and `afterEach` exactly as `IngestView.test.ts`; add two
  helpers to `ingest-test-support.ts`: `typeSource(path)` = clear + type
  into `textbox {name: "Source path"}` then `userEvent.tab()` (blur fires
  `change`); `typeDest(path)` = type into `textbox {name: "Destination path"}`
  + `{Enter}`):
  - `a typed source path is the job's source`: `typeSource(SOURCE)`, pick
    node, click Plan → `callsTo(calls, "plan_ingest")[0].source === SOURCE`
    and the picker was never invoked (`callsTo(calls, PICKER)` empty).
  - `Enter in the destination field adds it and clears the field`:
    `typeDest(DEST_A)` → `DEST_A` listed under "Destinations"; the field's
    value is `""`.
  - `the Add button adds the typed destination`: type without Enter, click
    `button {name: "Add destination"}` → listed.
  - `a duplicate destination is refused with the pinned message`:
    `typeDest(DEST_A)` twice → one row, and `role="alert"` reads
    `${DEST_A} is already a destination.`; removing the row clears the
    alert.
  - `Browse for source still goes through the dialog`: mock `PICKER` with
    `picksInTurn([SOURCE])`, click `button {name: "Browse for source"}` →
    the source field's value is `SOURCE`.
  - `whitespace-only input is ignored`: `typeDest("   ")` → no row, no alert.
  - `IngestView.test.ts`: replace `"Choose source…"` with
    `{ name: "Browse for source" }` and `"+ Add destination"` with
    `{ name: "Browse for destination" }`; `screen.findByText(SOURCE)` after
    a browse becomes a `getByRole("textbox", {name: "Source path"})` value
    check. No new cases there (cap).
- [ ] **Step 2: Run** `pnpm test -- IngestView` — fails.
- [ ] **Step 3: Implement.** Then `pnpm lint`: if `IngestView.svelte`'s
  script crosses its 533-line cap, perform the split its cap comment
  names (the run-phase states + progress subscription + outcome poll into
  `IngestRunPanel.svelte`) in this same task — do NOT raise the number.
- [ ] **Step 4: Run** the four-command line.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: type a source or destination path on the Ingest surface"`

### Task 11: the Ingest e2e flow, seeded and ordered last

**Files:**
- Modify: `apps/desktop/e2e/setup/fixture-catalog.ts`
- Create: `apps/desktop/e2e/specs/ingest.e2e.ts`
- Modify: `apps/desktop/e2e/wdio.conf.ts` (`specs`)

Fixture additions (in `setupFixtureCatalog`, after the tag): create
`<base>/ingest-src/clip-a.txt` ("alpha take one") and
`<base>/ingest-src/clip-b.txt` ("bravo take two") — distinct bytes from
anything already scanned, so the plan says "copy", not "duplicate" —
and an empty `<base>/ingest-dst/`; then
`runMaj(majBin, ["para", "add", "project", PARA_NODE], env)` with
`const PARA_NODE = "e2e-ingest"`. `FixtureCatalog` gains
`ingestSourceDir`, `ingestDestDir`, `paraNodeName` (all strings).

The spec (mirrors `organize.e2e.ts`'s shape; `openSurface` from
`../setup/surfaces.ts`; `suppressAutoFocusRecovery` in `before()`):

```ts
describe("Majestical desktop — Ingest flow", () => {
  let fixture: FixtureCatalog;
  before(async () => {
    fixture = readFixtureCatalog();
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
  });

  it("copies a typed source into a typed destination, verified, filed under the PARA node", async () => {
    await openSurface('[data-e2e="nav-ingest"]', ".ingest-surface");
    const sourceField = await $('[aria-label="Source path"]');
    await sourceField.setValue(fixture.ingestSourceDir);
    await browser.keys("Tab"); // blur commits the field (onchange)
    const destField = await $('[aria-label="Destination path"]');
    await destField.setValue(fixture.ingestDestDir);
    await browser.keys("Enter");
    await expect($('[aria-label="Destinations"]')).toHaveText(expect.stringContaining(fixture.ingestDestDir));

    await $('[aria-label="PARA node"]').selectByVisibleText(`project/${fixture.paraNodeName}`);
    await $("button=Plan").click();
    const counts = await $(".ingest-counts");
    await counts.waitForDisplayed({ timeout: 10_000 });
    await expect(counts).toHaveText(expect.stringContaining("2 to copy"));

    await $("button=Start verified copy").click();
    const card = await $('[aria-label="Completed run"]');
    await card.waitForDisplayed({ timeout: 60_000 });
    await expect($('[aria-label="Failed files"]')).not.toBeExisting();

    const placed = await readdir(fixture.ingestDestDir, { recursive: true });
    expect(placed.some((p) => p.endsWith("clip-a.txt"))).toBe(true);
    expect(placed.some((p) => p.endsWith("clip-b.txt"))).toBe(true);
  });
});
```

(`readdir` from `node:fs/promises`; the walk is recursive because the
subfolder under the destination is `Projects/e2e-ingest/<date>/<label>`
and the date is the test's own day.) `wdio.conf.ts`'s `specs` becomes the
explicit list `smoke, search, volumes, browse, organize, settings,
ingest` (each as `./specs/<name>.e2e.ts`) with this comment: "Explicit
order, ingest LAST: an ingest run appends immutable events (two new
assets, a new volume for the destination) that `volumes.e2e.ts`'s
exact-count asserts would see; nothing can undo them, so nothing runs
after it." `onComplete`'s `rm` of the fixture base already removes the
source and destination dirs.

- [ ] **Step 1: Write the fixture additions and the spec** (above).
- [ ] **Step 2: Build once, run the suite.** From `apps/desktop`:
  `cargo build -p majestical-cli` (repo root, for the debug `maj`),
  then `pnpm tauri build --debug -b app --config src-tauri/tauri.e2e.conf.json`
  (foreground, 600 s timeout — tens of minutes cold), then in
  `apps/desktop/e2e`: `pnpm test`. Expected: seven spec files pass, ingest
  last. Editing the spec afterwards needs no rebuild.
- [ ] **Step 3: Prove the ordering matters.** Temporarily move ingest to
  the front of `specs`, run `pnpm test`, observe `volumes.e2e.ts` fail on
  its exact-count assert, restore. Note the observed failure line in the
  PR description — this is the evidence for the comment.
- [ ] **Step 4: Commit**:
  `git commit -m "test: Ingest e2e flow through typed paths, run last"`

  Watch `gui-e2e` to green before merging chunk 5.

---

## PR Chunk 6 — HiDPI tray icons + the two one-line fixes

### Task 12: embed the `@2x` tray icons

**Files:**
- Modify: `apps/desktop/src-tauri/src/tray.rs` (the four `include_bytes!`
  and the doc comment above them)
- Modify: `justfile` (`tray-icons` doc comment: the `@1x` set is still
  generated for completeness; the app loads `@2x`)

The doc comment replaces the "would render twice the intended size"
paragraph with the fact: `tray-icon` 0.24's macOS `set_icon`
(`platform_impl/macos/mod.rs`) sizes the `NSImage` to 18 points tall
regardless of pixel dimensions, so the 44×44 file renders at 18 pt with
2× density on Retina and downsampled on a 1× display. Change the four
paths to `idle@2x.png`, `indexing@2x.png`, `paused@2x.png`,
`attention@2x.png`.

- [ ] **Step 1:** Make the change; `cargo test --lib tray && cargo clippy --all-targets -- -D warnings`
  (desktop) — `menu_model`'s tests are unaffected; the `TrayLook::bytes`
  exhaustiveness still compiles.
- [ ] **Step 2: Hand check on the Retina dev machine.** `pnpm tauri dev`,
  compare the tray glyph edges against the previous build (or a screenshot
  at 2× via `screencapture -R` of the menu bar region). Record "sharp at
  2×, no size change" and the screenshot filename in the PR description.
  Also confirm on a 1× display if one is attached; otherwise say so.
- [ ] **Step 3: Commit**:
  `git commit -m "feat: HiDPI tray icons — embed the @2x set"`

### Task 13: the two one-line fixes

**Files:**
- Modify: `crates/cli/src/search.rs`
- Modify: `crates/services/src/sync.rs` (`location_add`'s skeleton loop, `:182`)

- [ ] **Step 1: Failing tests.**
  In `search.rs`'s `mod tests`: `results_line_pluralizes`:
  `results_line(0) == "0 results"`, `results_line(1) == "1 result"`,
  `results_line(2) == "2 results"`.
  In `sync.rs`'s tests (find the existing `location_add` test with
  `rg -n 'fn location_add' crates/services/src/sync.rs`; add beside it):
  `location_add_creates_the_blob_root_the_store_opens`: after adding a
  location at temp `loc`, `majestical_index::blob::BlobStore::new(&loc).root().is_dir()`
  and `loc.join("events").is_dir()`.
- [ ] **Step 2: Implement.** `search.rs`: `fn results_line(count: u64) -> String`
  (`if count == 1 { "1 result".into() } else { format!("{count} results") }`)
  and `println!("{}", results_line(outcome.count));`. `sync.rs`: replace
  the `for sub in ["events", "blobs"]` loop with

```rust
let events = canonical.join("events");
let blobs = majestical_index::blob::BlobStore::new(&canonical).root().to_path_buf();
for dir in [events, blobs] {
    std::fs::create_dir_all(&dir).with_context(|| format!("initializing {}", dir.display()))?;
}
```

- [ ] **Step 3: Run** `cargo test -p majestical-cli --lib search -p majestical-services --lib sync && cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] **Step 4: Commit**:
  `git commit -m "fix: pluralize the search summary; sync skeleton uses BlobStore::root"`

---

## PR Chunk 7 — phase close

### Task 14: cargo-mutants over the files this phase changed

- [ ] **Step 1:** One at a time, foreground, `--in-place`, `git status`
  clean after each:
  - `cargo mutants --in-place -p majestical-services -f crates/services/src/index/mod.rs`
  - `cargo mutants --in-place -p majestical-services -f crates/services/src/doctor.rs`
  - (desktop workspace) `cargo mutants --in-place -f src/indexer.rs`
- [ ] **Step 2:** For every survivor: close it with a test in the same PR
  when cheap; otherwise record it on the watchlist under
  "cargo-mutants triage (phase 7F)" with its disposition (the phase 7E
  section is the format).
- [ ] **Step 3: Commit** the closing tests:
  `git commit -m "test: close phase 7F mutants"`

### Task 15: watchlist, as-built, handoff

- [ ] **Step 1:** `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`:
  a "Phase 7F deferrals" section (the spec's Deferred list plus anything
  found during execution, each attributed to its PR) and the mutants
  triage section.
- [ ] **Step 2:** The spec gains `## As-built (phase 7F)`: per-chunk PR
  numbers, every AMENDED note (the env-const amendment above is the
  first), and the review-loop shape.
- [ ] **Step 3:** `docs/superpowers/HANDOFF-phase7G.md` in the format of
  `HANDOFF-phase7F.md`: state, architecture pointers (the ledger, the
  wake, the fixture), process conventions carried, lessons, invariants
  (add: "the ledger remembers only permanent failures; a transient
  failure must never be written to it").
- [ ] **Step 4:** Update the memory index entry for the project state.
- [ ] **Step 4b (added in execution, chunks 3 and 4):** delete the TEMPORARY
  parity normalizers once chunks 3 and 4 are on main — in
  `crates/cli/tests/services_parity.rs`: `without_ledger`,
  `strip_ledger_member` (chunk 3, `index_status_output_is_byte_identical`)
  and `without_new_doctor_rows` with its `NEW_DOCTOR_ROWS` const (chunk 4,
  `doctor_output_is_byte_identical`), `without_result_pluralization`
  (chunk 6, `search_output_is_byte_identical`), their unit-test modules,
  and then `diff_against_ref_normalized` (shared by all three callers —
  last to go); point the three rows back at `diff_against_ref`. Same
  cleanup 7D's Task 21 did for `without_keyframe_images`.
- [ ] **Step 5: Commit + PR**:
  `git commit -m "docs: phase 7F close — deferrals, mutants triage, 7G handoff"`

## Verification (end-to-end, per chunk)

- Chunk 2: `whisper-conformance` green on the PR and, after merge, on main.
- Chunk 3: `cargo test --workspace`; the ledger integration test; MCP
  smoke; `maj index status --json` shows `failed` on a real catalog.
- Chunk 4: desktop `cargo test`; `MAJ_UPDATE_FIXTURES` regenerated and
  `pnpm test` green; `maj doctor` prints the two new rows; Retry button
  hand-checked once.
- Chunk 5: `gui-e2e` green with seven spec files, ingest last.
- Chunk 6: tray hand check recorded; `cargo test --workspace`.
- Chunk 7: mutants dispositions recorded; handoff reviewed by the user.
