# Majestical Phase 7F — Hardening: failed-item ledger, ingest path entry, fixture and wire debt

Written 2026-09-14 from a brainstorming session against
`docs/superpowers/HANDOFF-phase7F.md`. Parent spec:
`docs/superpowers/specs/2026-07-28-majestical-design.md`. The deferrals this
phase draws from are the "Phase 7E deferrals" section of
`docs/superpowers/plans/2026-07-29-phase2-watchlist.md`. Mockup (approved
before code, per the standing convention):
`docs/superpowers/specs/mockups/2026-09-14-phase7f/ingest-path-entry.html`.

## Scope decisions (from design session)

- **Shape: a hardening phase**, chosen over a distribution phase
  (Windows/Linux artifacts) and over new product capability. The parent
  spec's build order is fully covered; what remains is the deferrals
  ledger, and the two correctness items on it hurt the always-on story
  phase 7E just shipped.
- **Seven items, in.** (1) A failed-item ledger: the planner skips items
  known to fail, with an explicit retry at all three heads, and status
  reports them as a named count rather than as pending. (2) The scheduler
  describer key: a named "no key configured" failure plus the environment
  override the CLI already honors. (3) A type-a-path affordance on the
  Ingest surface, with a mockup, and the Ingest e2e flow it unblocks.
  (4) HiDPI tray icons. (5) The wire-layer split the `api.ts` cap comment
  promises. (6) Two one-line fixes: the "1 results" pluralization and the
  sync-location skeleton resolving its blobs path through
  `BlobStore::root()`. (7) The whisper conformance fixture: commit it, and
  close the vacuous-gate hole the flake exposed.
- **Retry policy: sticky until explicitly retried.** Chosen over automatic
  backoff (needs per-item timestamps, still retries a broken item forever)
  and over "sticky plus auto-clear on environment change" (every trigger
  is another thing to detect and test). A file re-ingested after being
  fixed gets a new content hash and re-enters on its own.
- **Wire layer: perform the promised split**, not codegen. Codegen
  (`tauri-specta` or similar) is a phase of its own with its own spec; the
  split is mechanical, adds no dependency, and keeps the fixture-pinning
  mechanism that catches Rust-side drift today.
- **Whisper fixture: commit the audio.** Chosen over retry-until-not-silent
  (keeps a known-flaky dependency in the critical path) and over a
  cross-platform synthesizer (changes the reference audio every gate was
  tuned against). CI stops calling `say` entirely.
- **Not in this phase** (stays on the watchlist): the root pre-commit hook
  compiling the desktop workspace (a hook-speed tradeoff deserving its own
  conversation); GUI describer settings (the GUI has none — a parity gap
  noted here, not built); MCP progress notifications, CLI ingest progress
  rendering, the ingest queue, Windows/Linux artifacts, localization (all
  carried again).

## What the investigation found (facts the design rests on)

- **The scheduler key gap is smaller than the watchlist implied.**
  `HttpDescriber::new` calls `DescriberConfig::effective_api_key(env_key)`,
  which falls back to the key stored in the catalog's `describer.toml`. The
  environment variable (`MAJ_OPENROUTER_KEY`) is an override, honored only
  for the OpenRouter backend. So a scheduler batch with `api_key: None`
  already works for a stored key; only the env override is missing under
  the scheduler, and a GUI launched as a login item has no shell
  environment anyway. What is missing is a *named* failure when OpenRouter
  is configured with no key from either source.
- **HiDPI tray icons need no new plumbing.** `tray-icon` 0.24.2's macOS
  path (`platform_impl/macos/mod.rs`, `set_icon`) builds an `NSImage` from
  the PNG bytes and then calls `setSize` to 18 points tall (width scaled
  by aspect) regardless of pixel dimensions. A 44×44 PNG therefore renders
  at 18pt with 2× pixel density on Retina, and downsampled on a 1× display.
  The phase 7E watchlist note ("would render at double the intended size")
  was wrong for this library version.
- **The whisper "flake" is two defects, confirmed from CI run
  34882576400 (main, PR #123, 2026-09-14).** (a) The recipe's `say -o`
  step produced a silent AIFF on the `macos-latest` runner. (b) On that
  silent fixture, `whisper_rs_matches_faster_whisper_reference` PASSED —
  both the pinned faster-whisper reference and our whisper.cpp emitted
  non-empty segments (the well-known whisper silence hallucination) that
  agreed within the WER and boundary bounds — and only
  `whisper_gated`'s `is_silent` assertion caught it. The conformance gate
  cannot tell speech from a shared hallucination. Main is red on that
  job at the time of writing.
- **The ingest e2e flow cannot share catalog state freely.** An ingest run
  appends events (new assets, a new volume for the destination) that are
  immutable, and `volumes.e2e.ts` asserts exactly one volume row with
  three assets. The ingest spec must run last.

## Architecture

### Wave 1 — Failure classes and the ledger (`crates/services`, CLI, MCP)

- **Failure classes.** Every per-item failure carries a class. The
  per-kind `failed: Vec<(PathBuf, String)>` in `IndexRunOutcome`'s kind
  outcomes becomes `failed: Vec<ItemFailure>` with
  `ItemFailure { path: PathBuf, error: String, transient: bool }`.
  `transient == true` means the item was never really attempted and must
  not be remembered: the describer backend failed or cascaded
  (`CaptionFailure::Backend` and the `DESCRIBER_SKIPPED_REASON` rows), or
  the source became unreadable mid-batch (an I/O `NotFound`/
  `PermissionDenied` on the source path, i.e. a volume that went away
  after planning). Everything else — decode errors, a codec ffmpeg
  rejects, a PDF PDFKit cannot open — is permanent. The CLI's `--json`
  and MCP rows gain a `"transient": bool` field; text rendering marks
  transient rows so an operator can tell "skipped, will retry" from
  "failed, remembered".
- **Ledger storage.** The existing per-catalog `index-failures.json` in
  the state dir (`FAILURES_FILE`) stops meaning "last run" and becomes the
  ledger of known permanent failures. Row shape stays `{kind: [row, ..]}`
  and each row becomes `{asset, path, error}` — `asset` (the catalog id)
  is the key the planner matches on, `path` and `error` are for display.
  `update_failure_report` becomes `record_failures`: for each kind the run
  worked, the merged list is the previous rows *plus* this run's permanent
  failures, deduplicated by `asset` (a re-failed item's row is refreshed,
  not duplicated). Transient failures are never written. Rows for a kind
  the run did not work are untouched, as today. Nothing prunes rows for
  assets that later leave the plan (deleted, or their derivation arrived
  by sync): they are not counted (below) and cost nothing; a retry clears
  them with everything else.
- **Planning.** `index::build_plan` reads the ledger (missing/unparsable →
  empty, with the existing notice) and partitions the plan: an item whose
  `(kind, asset)` has a row leaves `items` and is counted in a new
  `failed: u64` bucket on `KindStatus`/`KindStatusRow`. `pending` is
  therefore "will actually be attempted". This is what makes the
  scheduler's `NoPendingWork`, the tray's pending line, and the Always-on
  status line truthful without touching any of them. `plan_work` in
  `crates/index` stays pure and ledger-unaware; the partition is a
  separate pure function in services (`apply_ledger(plan, ledger) ->
  WorkPlan`) so it is unit-testable without a catalog.
- **Status.** `IndexStatusOutcome::failed_last_run` is replaced by
  `failed: BTreeMap<String, Vec<LedgerRow>>` — the ledger rows themselves,
  typed rather than raw JSON ("replace, don't deprecate"). The CLI's
  `index status` prints the per-kind `failed` count in the table and lists
  the rows under a "known failures (skipped until retried)" heading; the
  MCP `index status` read tool serializes the same struct. Wire fixtures on
  both sides where the struct is pinned.
- **Retry.** `IndexRunReq` gains `retry_failed: bool`. When set, `run`
  clears the ledger rows for the request's `kinds` *before* planning, then
  proceeds normally — a still-broken item simply gets a fresh row. CLI:
  `maj index run --retry-failed`. MCP `index_run`: a `retry_failed`
  parameter; the dry-run reports, per kind, how many rows it would clear
  (`"would": "clear 3 known failures for captions, then run one derivation
  pass over …"`) — real state it read, never a guess.

### Wave 2 — The ledger at the GUI head, doctor, and the key (desktop, `doctor.rs`, `describe`)

- **Scheduler state.** `SchedulerStateOutcome` gains `failed_items: u64`,
  summed from the status's per-kind `failed` exactly as `pending_items`
  sums `pending`. Fixture on both sides (`wire_fixtures.rs` +
  `fixtures/scheduler_state.json`), the whole-document `tauri_parity` row
  updated.
- **Retry command.** `retry_failed_items` (a one-liner over
  `retry_failed_items_impl`) clears the ledger for every kind of the
  selected catalog and nudges the scheduler loop so the next tick starts
  now rather than after up to one `TICK`. The nudge is the same
  mechanism `set_throttle_impl` uses to recompute `last_decision`
  immediately; it does not run a batch inline on the command thread.
  Returns the refreshed `SchedulerStateOutcome`.
- **Always-on section** (mockup frame 2). Under the throttle status line,
  shown only when `failed_items > 0`: the pinned strings
  `1 item skipped after failing` / `N items skipped after failing`, and a
  `Retry failed items` button. `scheduler-status.ts` gains a
  `failedLine(n)` sibling to `pendingLine`. The line never repeats the
  remedy text; the Health panel's doctor row (below) carries that.
- **Doctor: two new checks** (`crates/services/src/doctor.rs`, reachable at
  all three heads for free). `failed_items`: `Ok` "no known failures" when
  the ledger is empty; `Warn` with the count and up to three example paths,
  remedy `maj index run --retry-failed` (or the Settings button). Runs only
  when a catalog is given, like the other catalog checks. `describer`: `Ok`
  when no describer is configured ("captions off") or a local backend is
  set; `Fail` when the backend is OpenRouter and `effective_api_key` with
  the env override resolves to `None`, remedy naming both
  `maj describer set --api-key …` and `MAJ_OPENROUTER_KEY`.
- **Shared key helper.** `crates/describe` gains
  `pub fn env_api_key() -> Option<String>` (the one place the variable
  name lives); the CLI's `describer_cmd::env_api_key`, the MCP
  `index_run_exec`, the desktop scheduler's `batch_request`, and the
  doctor check all call it. `batch_request` stays pure by taking the key
  as a parameter; `run_tick` reads it once per tick.
- **Named no-key failure.** In the caption pass, before any request: if
  the backend is OpenRouter and the effective key is `None`, every item in
  the batch fails with one pinned reason (`"OpenRouter needs an API key —
  set it with `maj describer set --api-key` or MAJ_OPENROUTER_KEY"`),
  class transient. Under the scheduler this lands in `last_error` via the
  no-progress hold, so the tray's Attention state names the missing key
  instead of a server rejection.

### Wave 3 — Ingest path entry and its e2e flow (desktop)

- **`IngestView.svelte`** (mockup frame 1). The source panel's
  `Choose source…` button becomes a text input (`aria-label="Source
  path"`) with a `Browse…` button beside it in a `.ctl-actions` row; the
  native dialog moves behind `Browse…` and writes into the same field.
  Committing the field (Enter or blur) sets `source` and calls `editJob()`
  exactly as the dialog path does. The destinations panel gets the same
  field (`aria-label="Destination path"`) with `Add` and `Browse…`; Enter
  adds. A typed path is taken as-is — no client-side existence check; the
  plan command validates and its error renders in the same panel as
  today. `addDest`'s existing duplicate refusal becomes visible: the
  pinned message `<path> is already a destination.` in the panel's
  `.error` slot. No wire change: `ingest_plan`/`ingest_run` already take
  plain path strings. `IngestView.svelte` sits at a 533-line cap whose
  comment names its next split (the run-phase states, progress
  subscription and outcome poll into a component of their own); if the
  path fields push the script past the cap, that split is performed in
  the same chunk — the cap is not raised.
- **Fixture seeding** (`apps/desktop/e2e/setup/fixture-catalog.ts`).
  `onPrepare` additionally creates `ingest-src/` with two small files and
  an empty `ingest-dst/` under the fixture temp dir, and seeds one fileable
  PARA node via `maj para add` (the same verb the CLI's own tests use).
  `FixtureCatalog` gains `ingestSourceDir`, `ingestDestDir`, `paraNodeName`.
- **`ingest.e2e.ts`.** Types both paths, picks the node, plans, asserts
  the plan's file count, starts the verified copy, waits for the done
  state, and asserts on disk that both files exist under
  `ingest-dst/<subdir>/`. Runs **last**: `wdio.conf.ts`'s `specs` becomes
  an explicit ordered list ending with the ingest spec, with a comment
  naming why (immutable events; `volumes.e2e.ts`'s exact-count asserts).
  No catalog cleanup is possible or needed — `onComplete` removes the
  whole fixture dir.

### Wave 4 — HiDPI icons, the wire split, the small fixes

- **Tray icons.** `TrayLook::bytes` embeds the `@2x` (44×44) PNGs; the
  `@1x` set stays generated and committed by `just tray-icons` (nothing
  loads it; the recipe's doc comment says so). Verified by hand on the
  Retina dev machine, recorded on the PR — no automated test sees pixels.
- **Wire split.** `apps/desktop/src/lib/api-alwayson.ts` takes
  `SchedulerStateOutcome`, `SchedulerDecision`, `HoldReason`,
  `ThrottleOverride`, `PowerState`/`PowerSource`, `DoctorReport`/
  `DoctorCheck`, and their wrappers (`schedulerState`, `setThrottle`,
  `retryFailedItems`, `doctorReport`); `fixtures.alwayson.test.ts` takes
  their fixture pins. `api.ts`'s `max-lines` cap in `.oxlintrc.json` drops
  to a number the file then meets, and the cap comment names the next
  split subject (the ingest wire types) truthfully.
- **Pluralization.** `crates/cli/src/search.rs`'s summary line prints
  `1 result` / `N results`, with a test on the exact one-hit string.
- **Sync skeleton.** `location_add`'s skeleton loop resolves the blobs
  directory through `majestical_index::blob::BlobStore::root()`; a test
  asserts the created layout is exactly what `BlobStore` opens.

### Wave 5 — The whisper fixture

- **Committed audio.** `conformance/whisper/fixture.wav` (16 kHz mono,
  ~11.5 s including the 2 s lead-in; ~300 KB) is checked in. The
  `whisper-conformance` recipe uses it directly: no `say`, no `ffmpeg`
  synthesis step. The golden is still produced live from the pinned
  faster-whisper reference on every run, so the gate keeps comparing two
  implementations rather than pinning one's output.
- **Regeneration recipe.** `just whisper-fixture` runs the `say` +
  `ffmpeg adelay` steps into a temp file, checks it is not silent (peak
  amplitude above a floor, via `ffmpeg -af volumedetect` or a ten-line
  `uv run` script), and only then moves it into place — a silent result
  fails with a named message and leaves the committed file untouched.
- **Silence guards in the tests.** `whisper_conformance.rs` asserts the
  decoded fixture is not silent before loading any model (the same
  `is_silent` predicate `whisper_gated.rs` already has, moved to a shared
  test helper). Both tests fail fast with "fixture is silent — regenerate
  with `just whisper-fixture`". The gated test's standalone `say`
  fallback path stays for a bare `cargo test --ignored` run.

## Error handling

- Ledger rows are the result, not errors: `run` returns `Ok` with rows,
  status returns `Ok` with counts. Only an unwritable ledger file is a
  hard error, as `write_failure_report` already is.
- A transient failure is reported in the run outcome and dropped from
  memory on purpose; the scheduler's no-progress hold remains the defense
  against a transient failure repeating every tick.
- `retry_failed` on a kind with no rows is a no-op, not an error; the
  dry-run says "would clear 0 known failures".
- A typed ingest path that does not exist fails at plan time with the
  plan command's existing error, rendered where setup errors already
  render.
- A silent whisper fixture fails at regeneration, and independently at
  test start, each with a message naming the recipe.

## Testing

- **Pure functions, per-branch.** `apply_ledger`, the merge in
  `record_failures`, the failure classification, `failedLine`, and the
  doctor checks' decision logic are pure with unit tests per arm. A
  proptest over random run outcomes asserts no transient failure ever
  reaches the ledger and every permanent one does, deduplicated by asset.
- **Integration, CLI.** `crates/cli/tests`: a fixture catalog with a
  deliberately undecodable file (a `.mov` whose bytes are text) — run
  once, assert `index status` shows the row and `pending` excludes it;
  run again, assert the item was not attempted; `--retry-failed`, assert
  it was attempted and re-recorded. The "1 result" string.
- **MCP.** `index_run` dry-run reports the would-clear count; execute
  with `retry_failed: true` clears; `index status` carries `failed`.
- **Doctor.** Both new checks get hermetic seams in the style of #117:
  the ledger check against a state dir with a written ledger, the
  describer check against a config with and without a key and with the
  env override injected through a parameter, never the process env.
- **Desktop.** `retry_failed_items_impl` under `commands.rs`'s existing
  `*_impl` tests; wire fixtures regenerated (`MAJ_UPDATE_FIXTURES=1`) for
  `scheduler_state`; `tauri_parity` row updated. `IngestView.test.ts`:
  typing a source path commits it; Enter in the destination field adds;
  a duplicate shows the pinned message; `Browse…` still calls the dialog.
  `AlwaysOnSection` tests for the line's presence/absence and the button
  invoking the command. Cap rule: `IngestView.test.ts` shares a 385-line
  cap with `App.test.ts` whose named next split (`App.settings.test.ts`)
  is about the other file, so the new cases go in a sibling
  `IngestView.paths.test.ts` rather than raising the number.
- **E2E.** `ingest.e2e.ts` as above; the suite's spec order pinned in
  `wdio.conf.ts`.
- **Whisper.** The silence guard has a test that fails when deleted: a
  unit test on the shared `is_silent` helper with an all-zero buffer and
  a one-sample-nonzero buffer.
- **`cargo-mutants`** at close over `crates/services/src/index/mod.rs`,
  `doctor.rs`, and `indexer.rs` — foreground, one at a time, `--in-place`
  on a warm target, `git status` clean after each (standing mandate).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **Chunk 1**: this spec + the implementation plan + the mockup.
2. **Chunk 2 (may go first — it fixes red main)**: the committed whisper
   fixture, the regeneration recipe, the silence guards.
3. **Chunk 3**: failure classes + the ledger + `apply_ledger` + status's
   `failed` + CLI `--retry-failed` + MCP `retry_failed` + fixtures.
4. **Chunk 4**: `failed_items` on scheduler state + `retry_failed_items`
   command + Always-on line and button + the two doctor checks + the
   shared key helper + the named no-key failure.
5. **Chunk 5**: Ingest path entry + component tests; then fixture seeding
   + `ingest.e2e.ts` + spec ordering.
6. **Chunk 6**: HiDPI icons + the wire split + the two one-line fixes.
7. **Closing PR**: mutants triage, the spec's as-built section, watchlist
   updates, handoff for 7G.

## Deferred (watchlist items with this spec's attribution)

- GUI describer settings (backend, model, key) — the GUI has no describer
  configuration at all; a GUI-only user cannot get captions without the
  CLI. Named here as a parity gap; a phase of its own with a mockup.
- The root pre-commit hook compiling the desktop workspace (carried, with
  the hook-speed tradeoff still undecided).
- Wire-layer codegen from the Rust outcome structs (the split buys time;
  codegen is the real close).
- Ledger pruning of rows whose asset left the plan (not counted, not
  harmful; a retry clears them — build pruning only if the file ever
  grows enough to matter).
- Ledger entries keyed by something finer than `(kind, asset)` — e.g. a
  describer model tag, so switching describers retries captions
  automatically. `retry_failed` covers it manually for now.
- MCP progress notifications, CLI ingest progress rendering, the ingest
  queue, Windows/Linux artifacts, localization (carried again).
