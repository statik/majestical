# Majestical — Phase 7D handoff

Written 2026-08-10 at the close of Phase 7C. Read this first; everything else
is linked from here. Supersedes `HANDOFF-phase7C.md` (kept for history).

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
- Phase 7C implementation plan:
  `docs/superpowers/plans/2026-08-09-phase7c-infra.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #93, this closing PR pending)

**Shipped and working**:

- Phases 1-7B (see `HANDOFF-phase7C.md` for the detail): catalog
  init/scan/tag/meta/volumes/para, verified multi-destination ingest + ASC
  MHL verify, unified search with saved searches, blob store + thumbnails +
  diff-as-queue indexing, SigLIP 2 + MiniLM + whisper behind four
  conformance gates, scene keyframes, describer backends, OCR/PDF, layered
  text search, multi-location sync, inbox contributions; `crates/services`
  + `maj mcp` (phase 7A, #64-#70); the Tauri 2 + Svelte 5 desktop app with
  search/volumes/inspector surfaces, notices rendering, an armed updater
  and the tag-triggered release pipeline (phase 7B, #71-#83, plus #84-#88
  release fixes and dependabot triage).
- Phase 7C, four workstreams:
  - **Notices survive failure** (#91). `ServiceError::WithNotices` carries
    the sink's contents out of a failing call; `Notices::attach_on_err`
    attaches it at the four sync verbs (the only verbs whose local sink
    dropped on `Err`), and each head splits the carrier once — CLI to
    stderr before the error, MCP as leading text content blocks on the
    `isError` result, GUI into `CommandError.notices`.
  - **The TS wire layer is pinned against Rust** (#92). A Rust test
    serializes every outcome struct the GUI consumes into six committed
    JSON fixtures; a TS module assigns each fixture to its `api.ts`
    interface so drift fails a build no matter which side moved.
  - **Target-gated Apple seams + a `{macos, ubuntu}` Rust CI matrix**
    (#93). objc2/Vision/PDFKit, the CoreML EP and whisper's Metal feature
    live under `cfg(target_os = "macos")` target tables; OCR, PDF, the
    `sips` HEIC decode and the CoreML EP have non-macOS siblings returning
    honest `PlatformUnavailable` errors; the planner excludes the absent
    derivations with named counts and `index status` names the gap;
    transcription stays functional (whisper CPU) off-macOS. The ubuntu leg
    runs the full non-Apple suite — currently 209 `majestical-services`
    and 127 `majestical-index` lib tests among it (the services count is
    the same on both legs; the index count is 129 on macOS, where seven
    Apple-only tests compile in place of the five stub-path ones).
  - **CI hardening + phase close** (this closing PR). The zizmor rider
    (signing secrets scoped to a `release` environment, the redundant
    toolchain action replaced with a rustup step), the phase 7C
    cargo-mutants triage (three scoped runs; all 21 sync.rs and 23+6
    work.rs genuine gaps closed with new tests), and these docs.

**The user personally ran the manual `tauri dev` smoke on 2026-08-05** (7B
close) — nothing in 7C touched app startup, but the standing rule holds:
repeat the smoke by hand whenever `lib.rs`'s plugin registration or
`tauri.conf.json` changes; no automated test covers app launch end to end.

## Operator TODO (outstanding — before the next release)

The `release` GitHub Environment exists and `release.yml`'s desktop job
references it, but the two secret VALUES are still repository-scoped. Only
the user can move them:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY --env release
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release
```

Then a release dry run proves the environment-scoped secrets end to end;
only after that should the repository-level secrets be deleted. This is safe
to leave pending — GitHub falls back to the repository secret of the same
name — but the move is not done until the dry run says so. Full
instructions: `docs/RELEASING.md`, "The private key".

## Architecture pointers

- **The notices carrier**: `ServiceError::WithNotices { notices, source }`
  (`crates/services/src/error.rs:65`), constructed only by
  `Notices::attach_on_err` (`crates/services/src/notices.rs:55`), applied
  at the four sync verbs (`crates/services/src/sync.rs:379,419,668,820`).
  Where each head splits it, once: the CLI's `surface_err_notices`
  (`crates/cli/src/main.rs:576`) prints notices to stderr and hands back
  the inner error; MCP's `split_notices` + `error_blocks_with_notices` +
  `tool_error_split` (`crates/cli/src/mcp_cmd/mod.rs:87,103,120`) emit one
  text block per notice ahead of the error's Display chain; the GUI's
  `From<E> for CommandError` (`apps/desktop/src-tauri/src/commands.rs:66`)
  downcasts, splits the carrier, and serializes the notices on the error
  object. A new verb whose sink is local to the call must call
  `attach_on_err` on its way out — `sync.rs`'s module doc says so.
- **The wire-fixtures mechanism**:
  `apps/desktop/src-tauri/tests/wire_fixtures.rs` (six tests, one per wire
  shape) serializes fully-populated outcome structs and compares
  byte-for-byte against `apps/desktop/src/lib/fixtures/*.json`;
  `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` regenerates.
  `apps/desktop/src/lib/fixtures.test.ts` assigns each JSON to its
  `api.ts` interface (drift = compile error under `svelte-check`/`tsc`)
  and asserts the load-bearing runtime shapes. A new Tauri command's
  outcome struct gets a fixture in both places or its wire shape is
  unpinned.
- **`apps/desktop/src/lib/api.ts`** is still the hand-written wire layer:
  one wrapper per command, one interface per outcome struct, snake_case
  serde names mirrored field for field, camelCase argument names
  (`#[tauri::command]`'s `rename_all` default). Since #92 the fixtures
  above cross-check it against Rust.
- **The platform-capability consts**: `ocr::AVAILABLE`
  (`crates/index/src/ocr.rs:27`) and `pdf::AVAILABLE`
  (`crates/index/src/pdf.rs:23`), both `cfg!(target_os = "macos")`. The
  planner's exclusion sites in `crates/index/src/work.rs` (the
  `ocr_unavailable` increments at `:506` and `:563`, the `pdf_unavailable`
  increment at `:601`, plus the pass gates around `:191-255`) count what a
  build cannot derive so status can name the gap. The non-macOS siblings
  of `ocr.rs`/`pdf.rs`, the `sips` path in `thumbs.rs:82`, and
  `apply_coreml_ep` in `encoder.rs:203` return
  `IndexError::PlatformUnavailable` (`crates/index/src/error.rs:30`) — an
  ordinary per-item failure, never a panic. The per-target dependency
  tables live in `crates/index/Cargo.toml`; a dep needed everywhere must
  appear in BOTH tables, as its comment warns.
- **`crates/services/src/runtime.rs`** — `run_off_tokio_runtime`, the Lance
  scoped-thread rule, shared by both async heads. Read its doc comment
  before writing any new async command or tool. The rule is review-enforced
  only; 7B's mutants run proved the tooling cannot check it.
- **`apps/desktop/src-tauri/src/commands.rs`** — Tauri commands stay
  one-liners over `*_impl` functions taking a `CatalogCfg`; the impls are
  the only testable seam (`State`/`AppHandle` cannot be constructed without
  a running app). `tests/commands.rs` drives them against real fixture
  catalogs.
- **The updater flow**: startup check in `apps/desktop/src/lib/updater.ts`,
  banner, download-install-relaunch through `tauri-plugin-process`; every
  failure path ends in `console.debug` and nothing else.
- **The release runbook is `docs/RELEASING.md`** — the file to read before
  cutting anything: five version strings, the tag, the draft-race check,
  abandoning a release, and (new in 7C) the environment-scoped signing
  secrets with the outstanding operator move above. The public key stays in
  `tauri.conf.json` under `plugins.updater.pubkey`, key id
  `9D3AD6D3C4DA4E46`; losing the private key strands every installed app.
- **The parity harnesses**: `crates/cli/tests/services_parity.rs` (CLI vs
  pre-extraction reference; its CI build step is macOS-only because the
  merge-base can predate the Linux port — see ci.yml's comment) and
  `apps/desktop/src-tauri/tests/tauri_parity.rs` (GUI vs `maj`, loud-skips
  without `MAJ_BIN`).

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` now carries a "Phase
7C deferrals" section (six items, each attributed: non-Apple OCR/PDF
implementations, Windows in the Rust matrix, `api.ts` codegen, the GUI
`get_asset` `Ok(None)` notices drop, the four macOS-gated smoke tests'
ubuntu coverage gap, and the 9 remaining zizmor auditor lows) and a
"cargo-mutants triage (phase 7C)" section recording three scoped runs and
every survivor's disposition. Four earlier items are marked closed there
with their PR numbers: the failing-call notices drop (#91), the unpinned TS
wire layer (#92), the macOS-only services graph (#93, reworded — 2-OS is
real, Windows still out), and the two zizmor auditor informationals (this
closing PR). Keyframe-image extraction is unchanged and still open.

## Phase 7D recommendation

**The Browse / Ingest / Organize surfaces**, per the parent spec's §6 (Agent
surface & GUI, `docs/superpowers/specs/2026-07-28-majestical-design.md`) and
the phase-7 spec's deferred list — to be brainstormed fresh in a 7D design
session. The shell, wire layer (now pinned), notices rendering (now
covering failures too) and inspector are all in place; these are new
surfaces on an existing frame. **The user explicitly requested visual
mockup review as part of the 7D design session** — plan for mockups the
user can look at before code, not just prose.

Write a phase 7D spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 7C ran three chunk PRs plus
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

## Phase-7C lessons worth carrying

- **A clippy lint can fire only on the leg where a cfg branch compiles.**
  The non-macOS `apply_coreml_ep` stub keeps `Result` to share the macOS
  signature, so `clippy::unnecessary_wraps` fires on the ubuntu leg and
  never on a developer Mac — the workspace was "zero warnings" locally and
  red in CI. Under a cfg-gated seam, zero warnings means clippy on both
  targets (or a deliberate `#[expect]` with the reason, which is what
  landed: `crates/index/src/encoder.rs:203`).
- **Mutants scoped to `--lib <name>` can overstate misses — triage against
  that caveat before chasing.** The sync.rs run scoped its test command to
  lib tests with "sync" in the path, so a survivor there might have been
  killed by tests outside the scope. Here the caveat turned out idle: all
  21 survivors were genuine gaps in the sync module's own tests. The
  discipline is checking, not assuming, in either direction.
- **Counter mutants at 0 or 1 items are indistinguishable from correct.**
  `+=` mutated to `*=` survives any test that drives the site zero times
  (never executes) or once with an assertion looser than an exact count
  (`0 *= 1 == 0`). All 29 work.rs survivors were this one defect family.
  The rule that kills them: two assets per bucket and an exact
  `assert_eq!` per counter — the two-asset rule, now written into the
  triage section and the tests themselves.
- **`environment:` on a job is safe before the environment's secrets
  exist.** GitHub resolves `secrets.X` from the environment first and falls
  back to the repository secret of the same name, so the workflow change
  and the operator's secret move can land in either order. What is NOT safe
  is deleting the repository secrets before a dry run proves the
  environment-scoped ones — an empty secret arrives as an empty string, not
  a failure (the 7B lesson, still true).
- **cfg-gating a test costs coverage somewhere else — write down where.**
  Gating the four Vision/PDFKit smoke tests to macOS was correct, but two
  of them were the only CLI-level exercise of the failure-record machinery,
  so the ubuntu leg now has none. The gate and the coverage gap belong in
  the same commit's story (here: the watchlist deferral naming the
  cross-platform fixture that would restore it).

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
  absence assertion. Phases 7B and 7C added zero `Op` variants. Any future
  phase that adds one must extend `sample_ops()` and the proptest
  generator.
- MCP mutating tools all default to dry-run; `confirm: true` executes. Read
  tools take no `confirm` parameter. A dry-run preview must describe REAL
  state it read, never a guess — and must not promise what its own execute
  path would refuse (see the watchlist for the two carve-outs that remain).
- Tauri commands stay one-liners over `*_impl` functions. Logic in a command
  wrapper is logic no test can reach. A new command's wire shape gets a
  fixture on both sides or it is unpinned.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
