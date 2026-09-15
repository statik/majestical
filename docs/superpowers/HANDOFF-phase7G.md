# Majestical — Phase 7G handoff

Written at the close of Phase 7F. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase7F.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder),
local AI semantic search (Shade's gap), CRDT catalog sync through dumb file
transports (NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy
tags in the catalog, and agent-native access (CLI + MCP + GUI, all at
parity). The app ships on macOS; the Rust workspace builds and its tests
run on Linux, with the Apple-only derivations honestly absent there. Since
phase 7E the app is always-on: a menu-bar tray keeps a power-aware
background indexer running with no window open. Since phase 7F that
indexer remembers permanent failures and skips them until asked to retry.

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-7E specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 7F spec (hardening) + its as-built section:
  `docs/superpowers/specs/2026-09-14-phase7f-hardening-design.md`; its
  mockup is `docs/superpowers/specs/mockups/2026-09-14-phase7f/ingest-path-entry.html`
- Phase 7F implementation plan:
  `docs/superpowers/plans/2026-09-14-phase7f-hardening.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main at #132 plus this closing PR)

**Shipped and working**:

- Phases 1-7E (see `HANDOFF-phase7F.md` for the detail): catalog
  init/scan/tag/meta/volumes/para, verified multi-destination ingest,
  unified search, blob store + thumbnails + diff-as-queue indexing, four
  conformance-gated model backends, scene keyframes + keyframe images,
  describer backends, OCR/PDF, layered text search, multi-location sync,
  inbox contributions; `crates/services` + `maj mcp`; the Tauri 2 +
  Svelte 5 desktop app with Search/Volumes/Browse/Organize/Ingest/Settings
  surfaces; the WebDriver e2e suite; `maj doctor`; the always-on tray with
  the power-aware scheduler.
- Phase 7F, five chunk PRs (#126, #128, #130, #131, #132) plus #125 (spec
  + plan + mockup) and this closing PR:
  - **The committed whisper fixture** (#126). `conformance/whisper/
    fixture.wav` replaces per-run `say` synthesis in CI; both whisper gates
    refuse silence before any model loads. Main had been red on that job
    twice because a silent fixture made the conformance comparison pass on
    a shared hallucination.
  - **Failure classes and the ledger** (#128). `ItemFailure { asset, path,
    error, transient }` on every per-item index failure; `PortFailure {
    Unavailable, RefusedInput }` on the core port error so a describer's
    per-item rejection is permanent and only an unavailable backend is
    transient; the state-dir ledger (`index-failures.json`) of permanent
    failures the planner holds back; `maj index run --retry-failed`, the
    MCP `index_run` `retry_failed` parameter with a truthful dry run, and
    `index status`'s `failed` column and rows.
  - **The ledger at the GUI head** (#130). `api-alwayson.ts` (the wire
    split); one `OPENROUTER_KEY_ENV` name with a named no-key caption
    failure; doctor's `failed_items` and `describer` rows; the scheduler's
    `failed_items`, an interruptible sleep, and the `retry_failed_items`
    command; the Always-on section's "N items skipped after failing" line
    and `Retry failed items` button.
  - **Typed paths on Ingest, and the Ingest e2e flow** (#131). Source and
    destination text fields with `Browse…` beside them; the run phases
    split into `IngestRunPanel.svelte`; `ingest.e2e.ts` runs last by an
    explicit spec list.
  - **HiDPI tray icons and two one-line fixes** (#132).
  - **This closing PR** — the phase 7F deferrals and cargo-mutants triage
    in `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, the spec's
    `## As-built (phase 7F)` section, the deletion of the three temporary
    parity normalizers, and this handoff.
- Full deviation detail (AMENDED notes, per-PR summaries, the review-loop
  shape) is in the spec's own as-built section — read it, not just this
  handoff, before touching any phase 7F code.

## Secrets and release state (unchanged since the 7D handoff)

The two signing secrets live ONLY in the `release` GitHub Environment.
Key rotation instructions: `docs/RELEASING.md`, "The private key". Nothing
in 7E or 7F touched the release pipeline.

## Architecture pointers

Everything in the 7F handoff's pointer list still holds (`maj doctor`'s
shape, the autopilot policy, the power probe, the scheduler loop and its
no-progress hold, the tray's `menu_model`/`build_menu` split, autostart,
the e2e harness, the cap rules). What is new or changed:

- **Failure classes** (`crates/services/src/index/run.rs`). Every kind's
  `failed` list holds `ItemFailure`s. `ItemFailure::classify(item, err)`
  is the general rule: transient iff the source path is non-empty and no
  longer exists at record time (the volume went away). It is deliberately
  imprecise in the cheap direction — a false transient costs one retry, a
  false permanent hides a healthy item until an explicit retry — and
  `Path::exists()` being false for permission-denied is intended.
  `ItemFailure::transient` is for callers that know better: the caption
  pass's backend-unavailable arm and its cascade, and the named no-key
  failure. The transcript-embed row carries the transcript blob path and
  is always permanent.
- **`PortFailure`** (`crates/core/src/ports.rs`). `PortError::new` is
  `Unavailable` (every existing adapter keeps that meaning);
  `PortError::refused` is `RefusedInput`. The describe client
  (`crates/describe/src/client.rs`) maps 4xx to refused except
  `NOT_ABOUT_THE_PAYLOAD` = 401/402/404/407/408/429, and 403 stays a
  rejection because OpenRouter uses it for moderation; `Shape`/`Malformed`
  are rejections. The caption pass's `caption_failure(&PortError)` maps
  refused → `Item` (permanent, pass continues) and unavailable →
  `Backend` (transient, pass aborts). Invalid/expired keys arrive as
  401/402 and stay transient; only an ABSENT key is named
  (`missing_openrouter_key`, `capability::OPENROUTER_KEY_MISSING_REASON`).
- **The ledger** (`crates/services/src/index/mod.rs`). `Ledger =
  BTreeMap<kind-name, Vec<LedgerRow { asset, path, error }>>` in
  `FAILURES_FILE` (`index-failures.json`) under the catalog's state dir;
  `read_ledger` (missing/unparsable → empty + notice), `record_failures`
  (permanent rows only, dedup by asset, fresh error wins, never clears),
  `clear_failures(kinds) -> count`, `known_failures`, and the pure
  `apply_ledger(plan, ledger)` that `build_plan` ends with, moving matched
  items from `pending` to the new `failed` bucket. Keyed by `--kinds` name,
  so a row under `transcripts` or `ocr` holds back both of that kind's
  work kinds. Writes are temp+rename with a per-writer temp name — the
  desktop scheduler thread and a command thread can both write. `run_impl`
  clears BEFORE it plans when `IndexRunReq.retry_failed` is set (the CLI
  integration test pins that order). Status's `failed` count is
  plan-derived; the `failed` rows are the ledger; they can disagree once a
  derivation arrives by sync.
- **Retry at the heads.** CLI `--retry-failed` (first pass only under
  `--watch`, `retry_on_pass`); MCP `index_run { retry_failed }` whose dry
  run reports `known_failures` per requested kind and a `would` sentence
  from the same counts; desktop `retry_failed_items` (all kinds, zero the
  count, nudge). Doctor's `failed_items` row is the third head's view.
- **The scheduler wake** (`apps/desktop/src-tauri/src/indexer.rs`).
  `SchedulerWake { Mutex<bool>, Condvar }` is a SECOND managed state, not
  a field on `SchedulerState` (whose `RwLock` is not reentrant);
  `run_loop` waits on it instead of sleeping; `nudge` is remembered if it
  lands before the wait. `publish_poll(shared, status, power, decision)`
  is the pure seam every tick's stores go through — the one line without
  a test that fails on revert is the `wait` call itself, inside the
  infinite loop.
- **Doctor** (`crates/services/src/doctor.rs`). Nine rows in this order:
  `ffmpeg`, `imagemagick`, `models`, `state_dir`, `catalog`,
  `blob_residue`, `failed_items`, `describer`, `platform`.
  `DoctorRequest.describer_env_key` is the head's reading of
  `OPENROUTER_KEY_ENV`, filled by the CLI, the MCP tool, and
  `doctor_report_impl(cfg, env_key)` — never client-supplied. The
  unreadable-config detail renders `{err}` (outermost context), never
  `{err:#}`: a malformed `api_key` line must not echo a key.
- **The Ingest surface** (`apps/desktop/src/lib/IngestView.svelte`,
  `IngestRunPanel.svelte`). The source commits on every keystroke
  (`oninput` → `setSource`), kept as typed, trimmed only through the one
  `sourceArg` derived value — Plan enables while the operator is still in
  the field, because Chrome/WebKit do not blur a field for a click on a
  disabled button. `<IngestRunPanel>` must stay mounted unconditionally:
  its mount-time `adoptRunningState` is how the surface rejoins a run in
  flight. The surface's script cap is 333 and the panel is under the
  default 300; the completion card is the next named split.
- **The e2e spec order** (`apps/desktop/e2e/wdio.conf.ts`). `SPEC_FILES`
  is a module constant listing all seven files with ingest LAST (an ingest
  run appends immutable events the volumes spec's exact counts would see);
  `onPrepare` refuses a full run when a file on disk is missing from it.
  The launcher rewrites `config.specs` for `--spec` runs before
  `onPrepare`, which is why the constant exists. Against the embedded
  WebKit driver, `selectByVisibleText` does not change a `<select>`;
  `ingest.e2e.ts`'s `selectNode` sets the value and dispatches `change`.
- **The tray icons** (`apps/desktop/src-tauri/src/tray.rs`). The `@2x`
  PNGs are embedded; `tray-icon` 0.24 sizes them to 18 pt itself. Both
  sets are generated by `just tray-icons`.
- **The whisper fixture** (`conformance/whisper/fixture.wav`, `just
  whisper-fixture`). 16 kHz mono, 9.584 s with a 2 s lead-in. The
  regeneration recipe refuses silence (peak below -60 dB) and reports a
  probe failure with ffmpeg's own log; the `volumedetect` probe needs
  `-v info`.
- **Parity harness** (`crates/cli/tests/services_parity.rs`). The three
  temporary normalizers this phase added are deleted in the closing PR;
  the pattern (exact-substring removal on both binaries, gated on the
  clean-fixture value, TEMPORARY doc, unit tests, deletion scheduled in
  the plan) is the way to change an existing verb's output on a branch.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` carries a "Phase 7F
deferrals" section (attributed per PR) and a "cargo-mutants triage (phase
7F)" section. Everything the phase 7E watchlist carried forward — MCP
progress notifications, CLI ingest progress rendering, the ingest queue,
Windows/Linux release artifacts, localization — is carried again.

## Phase 7G recommendation

From the phase 7F deferrals, roughly in order of value:

- **GUI describer settings.** The GUI has no describer configuration at
  all; a GUI-only user cannot get captions without the CLI, and the doctor
  row now tells them so without offering a fix. A Settings section with
  backend, model and key (stored through the same `describer_config::set`
  path, shown through the redacting view), with a mockup first.
- **Stop the state-dir leak.** Tens of thousands of directories under the
  user's data dir from unit tests that open catalogs without
  `MAJ_STATE_DIR`. One justfile env line or one `state_dir` seam on the
  two test helpers.
- **Windows/Linux release artifacts** (carried since phase 7).
- **Wire-layer codegen.** `api.ts` is at 559/560 with the ingest subject
  named as the next split; the fourth split in as many phases is the
  signal to generate the file from the Rust outcome structs instead.
- **Name a bad OpenRouter key.** A 401/402 is transient by design; the
  tray shows only the server's text. Surfacing the status class through
  `PortError` would let the no-key failure's sibling exist.
- **The Ingest run-panel seam.** Adopt the panel-owns-`start()` shape when
  the completion card is split out.

Brainstorm fresh rather than picking from this list as written — it is
input to a 7G design session, not its output. The mockup-review convention
worked for a fourth phase in a row: both 7F frames were approved before
code, and the Always-on strings were pinned byte-for-byte in tests.

Write a phase 7G spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

Carried from `HANDOFF-phase7F.md` (items 1-19 there), lightly trimmed:

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Each task: fresh implementer subagent →
   adversarial spec-compliance reviewer (probes empirically, mutation-tests
   claims) → code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Never `gh pr merge --auto`.
3. **NO Claude-Session trailers in commit messages** (user mandate). The
   harness's own reminder asks for one; the mandate wins — amend it out.
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain git).
5. Shared checkout: implementers stage ONLY their files, never `git add -A`.
   Do not run reviewers (who mutate files empirically) concurrently with
   implementers, and do not switch branches while either is running.
6. Shell: variables do NOT persist across Bash invocations. `trash`, never
   the recursive-force `rm` — a hook blocks any Bash command containing
   that pattern, including heredoc TEXT, so a justfile recipe or a doc
   that carries the words must be written with the Edit/Write tool.
7. Git auth: SSH unavailable; push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings; verify current versions of deps/actions at execution time.
9. Reviewers' findings get fixed in the same chunk when cheap; deferred
   items go on the watchlist with attribution.
10. **`cargo-mutants` runs FOREGROUND, one at a time, `--in-place` on a
    warm target, `git status` clean after each.** The harness moves a
    600-second-plus command to a tracked job; that is not the same as
    starting the next run — wait for the job to finish.
11. **Subagents report through their final message.** A subagent that
    starts a background command inside its own turn loses it when the turn
    ends: nudge once ("run it in the foreground, then report"), and if it
    stalls again, finish the task directly or replace the subagent. It
    happened in four tasks this phase; one nudge recovered each.
12. **`origin/main` goes stale.** `git fetch`/pull `main` explicitly
    before rebasing; a reviewer working from a stale ref reports
    conflicts that do not exist.
13. **Local setup**: `just`, `protoc`, ffmpeg, ImageMagick, ~2GB model
    cache, Node 22+, pnpm 11+ for `apps/desktop`. `just gui-install`
    before any GUI recipe.
14. **Local GUI verification is the four-command line**: `pnpm check &&
    pnpm lint && pnpm test && pnpm build`, and `pnpm check && pnpm lint`
    in `apps/desktop/e2e` too when e2e files change.
15. **The session scratchpad is wiped on date rollover.** Facts a closing
    agent needs belong in the repo or in memory.
16. **`gui-e2e` is watched to green by convention, not enforced.** Main
    carries no branch-protection rulesets.

Added this phase:

17. **Gate a commit on its verification with `&&`, never a pipe.** `pnpm
    test 2>&1 | grep … ; git commit` commits on a red test because the
    pipe's status is `grep`'s. One controller commit shipped red this way
    and was fixed in the next; the reviewer caught it.
18. **Changing an existing verb's output on a branch needs a temporary
    parity normalizer** in `services_parity.rs` (the reference binary is
    built at the merge-base), scheduled for deletion in the plan's close
    task. Three were added and deleted this phase.
19. **A local e2e run needs the debug bundle**: `cargo build -p
    majestical-cli`, then `pnpm tauri build --debug -b app --config
    src-tauri/tauri.e2e.conf.json` in `apps/desktop` (about 25 minutes
    cold, a few warm), then `pnpm test` in `apps/desktop/e2e` (about ten
    seconds plus startup). `pnpm test --spec specs/<one>.e2e.ts` runs one
    file. Rebuild only after a Rust or Svelte change.
20. **A screenshot from the automation session is black** unless the
    terminal has Screen Recording permission; a pixel-level hand check is
    the user's, recorded on the PR.

## Phase-7F lessons worth carrying

- **A review can find a design gap the spec missed, and the right answer
  is a task, not a note.** "Transient = any describer error" would have
  made a 400 on one image transient forever — the exact loop the ledger
  exists to stop. The fix (`PortFailure`) touched three crates and was
  worth its own task inside the chunk.
- **A vacuous gate looks like a passing gate.** The whisper conformance
  test passed twice on silence because both models hallucinated the same
  text. A guard on the INPUT (not silent) is what made the comparison mean
  something again.
- **A pure seam is cheaper than a hermetic tick test.** `publish_poll`
  took twenty-five lines and kills every store mutant; a test that drove a
  real tick would have needed a catalog, the power probe, and the
  environment.
- **The heads read the environment; the library crates never do.** The
  const in `crates/describe`, two three-line readers in the CLI and the
  desktop, and a parameter on `DoctorRequest` — no shared env-reading
  crate — kept the no-environment rule true across the split.
- **A cap is a ratchet, not a target.** Two splits this phase were forced
  by caps (`api.ts`, `IngestView.svelte`) and both lowered the number
  afterwards; neither raised it.
- **Test the consequence, not the precondition.** A test that asserted
  focus was still in the field passed identically with the old blur-commit;
  asserting the button was already enabled is what pinned the amendment.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it never
  prints and never reads the environment, and neither does
  `crates/describe`.
- Warnings ride the outcome, not stderr — on the failure path too.
- Exit-code/error polarity is decided once in the outcome structs:
  per-item failures are recorded rows inside a successful result; only
  operator-fixable or total failures become hard errors. `doctor` returns
  `Ok` even when every check fails.
- **The ledger remembers only permanent failures.** A transient
  `ItemFailure` must never be written to it (the proptest pins this), and
  a per-item rejection from a describer IS permanent. Only an explicit
  retry clears rows; a run never clears a kind.
- **The failed-item count is honest.** `pending` means "will actually be
  attempted"; held-back items are in `failed`, at every head.
- Platform selection is `cfg(target_os)`, never a cargo feature.
- `sample_ops()` in `crates/core/src/projection.rs` is the op-variant
  absence assertion; phase 7F added no `Op` variant.
- MCP mutating tools default to dry-run; a dry run describes REAL state
  it read (the `index_run` dry run counts the ledger it embeds) and never
  writes.
- Tauri commands stay one-liners over `*_impl` (plus the tray-refresh
  line where the tray must follow); every new wire field gets a fixture
  on both sides (`failed_items` did).
- A background thread that owns mutable state must never leave it stuck
  on a panic, and must never be left sleeping past a change it should
  react to — `SchedulerWake` is the second half of that rule.
- Never lie about data safety or completeness; degradation names the
  specific gap and remedy; a doctor row never echoes a secret.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it is deleted.
