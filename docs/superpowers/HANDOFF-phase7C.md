# Majestical — Phase 7C handoff

Written 2026-08-05 at the close of Phase 7B. Read this first; everything else
is linked from here. Supersedes `HANDOFF-phase7B.md` (kept for history).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder), local
AI semantic search (Shade's gap), CRDT catalog sync through dumb file transports
(NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy tags in the
catalog, and agent-native access (CLI + MCP + GUI, all at parity).

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-6 specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 7 spec (services + MCP + GUI):
  `docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md`
- Phase 7B spec (GUI slice + release pipeline) + its as-built section:
  `docs/superpowers/specs/2026-08-04-phase7b-gui-release-design.md`
- Implementation plan: `docs/superpowers/plans/2026-08-04-phase7b-gui-release.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main @ PR #82, this closing PR pending)

**Shipped and working**:

- Phases 1-6 (see `HANDOFF-phase7.md` for detail): catalog init/scan/tag/meta/
  volumes/para, verified multi-destination ingest + ASC MHL verify, unified
  search with saved searches, blob store + thumbnails + diff-as-queue
  indexing, SigLIP 2 + MiniLM + whisper behind four conformance gates, scene
  keyframes, describer backends, OCR/PDF, layered text search, multi-location
  sync (push/pull/status), inbox contributions.
- Phase 7A (#64-#70): `crates/services` (one function per verb) and `maj mcp`
  (stdio JSON-RPC, 10 read tools, 16 confirm-gated mutating tools, 2
  resources). See `HANDOFF-phase7B.md` for the detail.
- Phase 7B (#71, #72, #75, #76, #77, #80, #81, #82, plus this closing PR):
  - **Notices** (#72). The 28 stderr diagnostics in `crates/services` now go
    to a thread-local sink (`crates/services/src/notices.rs`) and ride each
    verb's outcome struct as `notices: Vec<String>`. All three heads read the
    same field. This is what made the GUI possible: a head that cannot print
    to a terminal was previously blind to every warning the services layer
    produced.
  - **MCP schema tightening** (#75): four enum-shaped string parameters now
    take real `schemars` enums, and both `set_metadata`'s and `tag_assets`'
    dry runs validate the asset exists before describing a write.
  - **The desktop app** (#77, #80, #81, #82): `apps/desktop`, Tauri 2 +
    Svelte 5 + Vite, no SvelteKit. First-run welcome flow (initialize or
    adopt a catalog), search surface with saved-search chips and debounced
    queries, volumes table, asset inspector with thumbnail, tags, PARA,
    metadata fields, verification history and keyframe timecodes, notices
    rendered above every surface, and an update banner.
  - **The release pipeline** (#77 for CI, then this branch's three commits):
    tag-triggered `tauri-action` draft release for both macOS
    architectures, an armed updater (signed bundles + `latest.json`), a
    universal `maj` binary with checksums, and a `cargo-about` license
    bundle. `scripts/version-sync.sh` refuses a release whose five version
    strings disagree.
  - **Tests**: 799 passing in the headless workspace; 27 in the GUI Rust
    workspace (2 lib, 23 `commands.rs`, 2 `tauri_parity`); 52 vitest tests
    across 8 files. `just ci`'s four conformance gates are unchanged.

**The user personally ran the manual `tauri dev` smoke on 2026-08-05** — the
app opens, the first-run flow reaches a working catalog, search returns rows,
and the inspector renders. The updater secrets are stored. No automated test
covers app startup end to end; that smoke is the only proof it launches, and
it should be repeated by hand whenever `lib.rs`'s plugin registration or
`tauri.conf.json` changes.

## Architecture pointers

- **`apps/desktop/src-tauri/src/`** — `lib.rs` (plugin registration, command
  roster, `thumb://` protocol registration, the `setup` hook that restores
  the persisted catalog), `commands.rs` (one `#[tauri::command]` per verb,
  each a one-liner over a `*_impl` function that takes a `CatalogCfg` and
  returns the service's outcome struct as-is), `config.rs` (the persisted
  catalog choice), `thumb_protocol.rs`. The `*_impl` split is deliberate and
  load-bearing: `State`/`AppHandle` cannot be constructed without a running
  Tauri app, so the impls are the only testable seam. `tests/commands.rs`
  drives them against real fixture catalogs.
- **`apps/desktop/src/`** — `App.svelte` (shell, sidebar, surface switching),
  `lib/SearchView.svelte`, `lib/VolumesView.svelte`, `lib/Inspector.svelte`,
  `lib/Welcome.svelte`, `lib/Notices.svelte`, `lib/UpdateBanner.svelte`.
- **`apps/desktop/src/lib/api.ts`** is the wire layer: one wrapper per
  command, one interface per outcome struct, mirroring the Rust field for
  field including snake_case (those are serde's names, not ours to prettify).
  Argument names are camelCase because `#[tauri::command]` defaults to
  `rename_all = "camelCase"`. Nothing cross-checks this file against the Rust
  — see the 7C recommendations.
- **`apps/desktop/src/lib/test-support.ts`** — `mockCommands(handlers)`
  (throws by name on any command a test did not plan for),
  `rejectCommand(message)` (the serialized `CommandError` shape a failing
  command actually produces), `stubManifest(status, body)` (the `thumb://`
  fetch). Use these rather than hand-rolling `mockIPC` callbacks.
- **`crates/services/src/index/blobs.rs`** — the one blob lookup and the one
  remedy text behind both `maj mcp`'s `majestical://` resources and the app's
  `thumb://` protocol.
- **`crates/services/src/runtime.rs`** — `run_off_tokio_runtime`, the Lance
  scoped-thread rule, shared by both async heads. Read its doc comment before
  writing any new async command or tool.
- **The updater flow**: the app checks on startup
  (`apps/desktop/src/lib/updater.ts`), the banner offers the newer version,
  applying downloads and installs then relaunches through
  `tauri-plugin-process`. Every failure path ends in a `console.debug` and
  nothing else — an unreachable endpoint must not put an error in front of a
  user who did not ask about updates.
- **The release runbook is `docs/RELEASING.md`**, and it is the file to read
  before cutting anything. It covers the five version strings, the tag, the
  operator check for the `tauri-action` draft race, and how to abandon a
  release. **Secrets, by name only**: `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are repository secrets; the public
  half is in `apps/desktop/src-tauri/tauri.conf.json` under
  `plugins.updater.pubkey`, key id `9D3AD6D3C4DA4E46`. Losing the private key
  means no existing installation can ever be updated again.
- **The reference-binary parity harness** (`crates/cli/tests/
  services_parity.rs`) remains the extraction-refactor tool of record; see
  `HANDOFF-phase7B.md` for its mechanics. `apps/desktop/src-tauri/tests/
  tauri_parity.rs` is its cross-binary cousin for the GUI.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` now carries a "Phase
7B deferrals" section (21 items, each attributed) and a "cargo-mutants triage
(phase 7B)" section recording five scoped runs and every survivor's
disposition. Three phase-7A items are marked closed there with their PR
numbers: the stderr diagnostics (#71, #72), the four schemars-enum params
(#75), and the two under-validating dry runs (#75, with the two carve-outs
that stayed). Keyframe-image extraction is unchanged and still open.

## Phase 7C recommendation

From the deferral list and the phase-7 spec's own deferred items:

- **A notices payload on `ServiceError`.** Today a service call that fails
  drops whatever notices its sink was holding. `sync::pull_impl` is the case
  that would gain most: its sink holds the buffer `apply_pulled_events`
  folded, and that is exactly what is lost at `PullApplyFailure` — the moment
  a user most wants to know what else went wrong.
- **Pin the TypeScript wire layer against Rust.** A renamed serde field
  breaks the GUI at runtime with nothing failing anywhere. The fix shape is a
  Rust-serialized fixture parsed under the TS types inside vitest.
- **Decide the macOS-only question.** `crates/index`'s unconditional
  `objc2`/Vision/PDFKit dependencies make the Rust half of CI macOS-only and
  will block any non-Apple port. Porting means target-gating those
  dependencies and supplying non-Apple OCR and PDF fallbacks — a phase of its
  own, and worth deciding before more surface area accretes on top.
- **The browse, ingest and organize surfaces**, per the phase-7 spec's
  deferred list. The shell, the wire layer, the notices rendering and the
  inspector are all in place; these are new surfaces on an existing frame
  rather than new infrastructure.

Write a phase 7C spec + plan in the established format before any code.

## Process conventions (follow these — they are user-mandated)

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Plans live in
   `docs/superpowers/plans/YYYY-MM-DD-<name>.md` with full TDD steps and
   code. Each task: fresh implementer subagent → adversarial
   spec-compliance reviewer (probes empirically, mutation-tests claims) →
   code-quality reviewer → fix rounds until APPROVED.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   green. Never push to main directly. Phase 7B ran eight chunk PRs plus
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

## Phase-7B lessons worth carrying

- **Registering a config-carrying Tauri plugin without its config crashes
  startup.** `tauri_plugin_updater` has no default for `pubkey`, so
  registering the plugin without a `plugins.updater` block in
  `tauri.conf.json` fails to deserialize its configuration and the app exits
  instead of opening a window. Plugin registration and plugin config are one
  edit. (Arming the updater turned out to be three edits: the registration,
  the config block, and `bundle.createUpdaterArtifacts` — without the third
  the bundler emits no `.sig` and `latest.json` lists no platforms.)
- **A missing GitHub secret arrives as an empty string, not as an absent
  variable.** The two failures look different and read differently in a log
  (`A public key has been found, but no private key` versus `failed to
  decode secret key`), and code that tests for absence will not catch the
  empty case. Prefer failing loudly on both.
- **The signing self-check is the build log, not the artifacts.**
  `tauri-cli` signs with whatever private key it is handed and only *warns*
  when that key does not match the configured `pubkey`. A build with the
  wrong key still produces a `.sig` beside every bundle and a `latest.json`
  listing both platforms — every artifact-level check passes and installed
  apps then reject the update. The check that works is the absence of `does
  not match the public key` in the desktop job's log; the only definitive
  proof is an installed app taking a real update. `docs/RELEASING.md` has
  the grep.
- **A single-word CSS class collision is invisible to role and text
  queries.** `.volumes` styling the search card's badge row once laid the
  volumes table out as a flex container, head and body side by side, with
  the markup untouched and every jsdom query still passing. The guard is a
  test that loads the real stylesheet and asserts a computed style
  (`apps/desktop/src/styles.test.ts`). It has a vacuous-import trap of its
  own: vitest's default hands CSS back as an empty string, under which every
  assertion in that file passes against no stylesheet at all — hence
  `test.css: true` in `vite.config.ts` and the `beforeAll` that fails the
  suite if the sheet is empty.
- **Vacuous-test patterns to watch for, all three found in this phase.**
  (1) `waitFor(() => expect(x).toBeNull())` passes on its first check,
  before the thing it excludes has had any chance to arrive — wait a fixed
  interval, then assert. (2) A test that clears state before asserting a
  failure masks the failure it exists to catch; append rather than clear.
  (3) A warning that re-arises on its own can make a test pass for the wrong
  reason — the sqlite sync offset suppresses a repeated corrupt-log notice
  on the second read, so a test asserting the notice must be sure which read
  it is looking at.
- **The keying rule for notices**: Svelte's `{#each}` over notices is
  deliberately unkeyed, because the same notice can legitimately arrive
  twice in one outcome (a saved-search run drains the same corrupt-log
  warning from both the projection load and the catalog open) and a keyed
  each throws on the repeat instead of rendering it. `Notices.test.ts` is
  the canonical home of that regression.
- **The Lance scoped-thread rule cannot be tested, and cannot be
  mutation-tested either.** Anything that can reach a Lance
  `VectorStore`/`TextVectorStore` must run through
  `run_off_tokio_runtime`; omitting it panics only on a machine with a model
  installed and an index built, which no fixture has. This phase's
  cargo-mutants run against `runtime.rs` produced exactly one mutant and it
  was unviable — the mutation that would matter is not in the tool's
  catalogue. The rule is review-enforced on both async heads and that is the
  whole of its enforcement.
- **macOS Gatekeeper makes a fresh, unsigned test binary look hung, not
  slow.** A freshly built binary can sit at ~0% CPU for 25-30 seconds before
  its first line of output. Rule this out before diagnosing a "hang" in
  anything newly compiled on this machine.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it no longer
  prints at all, and every head (CLI, MCP, GUI) renders the same outcome
  structs rather than re-deriving behavior.
- Warnings ride the outcome, not stderr. A new diagnostic in
  `crates/services` goes to the notices sink and its verb's `notices` field;
  `print_stderr` is denied in that crate and there are no longer any
  per-site exemptions.
- Exit-code/error polarity is decided once in the outcome structs (phase-6
  doctrine, unchanged): per-item failures are recorded rows inside a
  successful result; only operator-fixable or total failures become hard
  errors (CLI nonzero exit, MCP `isError: true`) — always with partial
  progress attached, never discarded.
- `sample_ops()` in `crates/core/src/projection.rs` is the op-variant
  absence assertion. Phase 7B added zero `Op` variants. Any future phase
  that adds one must extend `sample_ops()` and the proptest generator.
- MCP mutating tools all default to dry-run; `confirm: true` executes. Read
  tools take no `confirm` parameter. A dry-run preview must describe REAL
  state it read, never a guess — and must not promise what its own execute
  path would refuse (see the watchlist for the two carve-outs that remain).
- Tauri commands stay one-liners over `*_impl` functions. Logic in a command
  wrapper is logic no test can reach.
- Never lie about data safety or completeness: counts come from real
  files/rows; degradation names the specific gap and remedy; partial
  progress is reported, never silently discarded.
- Tests must discriminate: reviewers mutation-test; every new guard ships
  with the test that fails when it's deleted.
