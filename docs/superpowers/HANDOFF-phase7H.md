# Majestical — Phase 7H handoff

Written at the close of Phase 7G. Read this first; everything else is
linked from here. Supersedes `HANDOFF-phase7G.md` (kept for history) and
the mid-phase `HANDOFF-phase7G-chunk6.md` (deleted by the closing PR; its
still-true content is below and in the watchlist).

## What this project is

A local-first macOS media catalog for hybrid/remote teams: verified ingest
(OffShoot territory), offline search of disconnected drives (NeoFinder),
local AI semantic search (Shade's gap), CRDT catalog sync through dumb file
transports (NAS, Dropbox, shuttle drives), PARA folders on disk + folksonomy
tags in the catalog, and agent-native access (CLI + MCP + GUI, all at
parity). The app ships on macOS; the Rust workspace builds and its tests
run on Linux, with the Apple-only derivations honestly absent there. Since
phase 7E the app is always-on (a menu-bar tray and a power-aware background
indexer); since 7F that indexer remembers permanent failures. Since 7G the
GUI configures captions itself, and the OpenRouter API key lives in the
macOS Keychain at all three heads.

- Parent spec (approved): `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 3-7F specs + as-built deviations: see `docs/superpowers/specs/`
- Phase 7G spec (captions parity) + its as-built section:
  `docs/superpowers/specs/2026-09-18-phase7g-captions-parity-design.md`;
  its mockup is
  `docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`
- Phase 7G implementation plan:
  `docs/superpowers/plans/2026-09-18-phase7g-captions-parity.md`
- Repo: github.com/statik/majestical · Site: https://statik.github.io/majestical/
- License Apache-2.0. Perpetual-vs-subscription positioning matters.

## State at handoff (main at #145 plus this closing PR)

**Shipped and working**:

- Phases 1-7F (see `HANDOFF-phase7G.md` and earlier for the detail).
- Phase 7G, PRs #134-#145 (squash-merged; #140 and #141 are open
  Dependabot PRs and #142 a closed one, none part of the phase):
  - **#134** — the spec, the plan, and the approved mockup.
  - **#135** — the unit-test state-dir leak stopped at its source
    (`cfg(test)` base in `state_dir.rs`, `MAJ_STATE_DIR` in `just test`,
    `just gui-test` and the desktop CI step).
  - **#136** — a rejected key and an empty account fail by name
    (`PortFailure::CredentialsRejected`); `describer test` checks the key
    (`DescriberProbe.key`: `accepted`/`rejected`/`missing`/`not_checked`).
  - **#137** — `crates/secrets`; views that name the key's source
    (`KeySource`); config errors that never quote the file (a key leak
    that predates the phase).
  - **#138** — the Keychain at the CLI and MCP heads; `maj describer
    clear-key` and MCP `clear_describer_key`.
  - **#139** — the mid-phase handoff (deleted by this closing PR).
  - **#143** — the desktop head: four describer commands over a cached
    Keychain key, their wire fixtures, a `tauri_parity` row, and the
    compile-time spawn guard.
  - **#144** (unplanned) — CI passing again under Rust 1.99 and PyAV 19,
    both of which broke main with no commit.
  - **#145** — the Captions section in Settings, end to end, and the e2e
    harness's throwaway Keychain service.
  - **This closing PR** — cargo-mutants over the phase's files with three
    test commits for the real gaps, the phase 7G deferrals and triage in
    `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, the spec's
    `## As-built (phase 7G)` section, and this handoff.
- **Outstanding: Task 10's hand check, the user's.** Save an OpenRouter
  key in Settings; Keychain Access shows `majestical` /
  `openrouter-api-key`; `maj describer show` says `(from keychain)` after
  one macOS allow prompt; Remove key deletes the item.
- Full deviation detail (amendments, per-PR summaries, the review-loop
  outcomes) is in the spec's own as-built section — read it, not just this
  handoff, before touching any phase 7G code.

## Secrets and release state (unchanged since the 7D handoff)

The two signing secrets live ONLY in the `release` GitHub Environment.
Key rotation instructions: `docs/RELEASING.md`, "The private key". Nothing
in 7E, 7F or 7G touched the release pipeline, and the Keychain adds no
release step: the item is created on the user's machine at first save.
The app and `maj` are not code-signed, so macOS asks once per binary for
Keychain access (a shared access group needs both signed by one team —
watchlist).

## Architecture pointers

Everything in the 7G handoff's pointer list still holds (failure classes,
`PortFailure`, the ledger, retry at the heads, the scheduler wake, doctor,
the Ingest surface, the e2e spec order, the tray icons, the whisper
fixture, the parity harness). What is new or changed:

- **`crates/secrets`** (`majestical-secrets`) is the ONLY code that
  touches a secret store. No library crate may depend on it; the three
  heads are its only permitted dependents. Surface: `trait KeyStore`,
  `SystemKeyStore` (macOS Keychain through `security-framework`; a stub
  elsewhere that is always `Unsupported`, selected by `cfg(target_os)`),
  `SUPPORTED`, `SERVICE_ENV` (`MAJ_KEYCHAIN_SERVICE`), `resolve(env,
  wants_keychain, &store) -> ResolvedKey`, and the shared test doubles
  `MemoryKeyStore::{holding, failing, held}` and `PanickingKeyStore`.
  **Use those doubles — do not write another copy.** The crate never reads
  the environment; a head reads `SERVICE_ENV` and passes the name to
  `SystemKeyStore::new`. The real item is service `majestical`, account
  `openrouter-api-key`.
- **`resolve`'s contract**: env (`MAJ_OPENROUTER_KEY`) wins and the store
  is then NEVER read; the store is read only when `wants_keychain` (the
  STORED backend is OpenRouter). A read failure is a notice, never an
  error. `ResolvedKey` has a hand-written redacting `Debug` and must never
  derive `Serialize`.
- **`crates/services` owns the pure decisions** and never reads ambient
  state or prints: `KeyPresence { Env, Keychain, Absent }` (what a head
  reports, `serde(skip)`), `KeySource` (what a view shows; `Absent`
  serializes as `"none"`), `key_source`, `plan_key_write(backend,
  keychain_supported, key)`, `wants_keychain`, `clear_file_key`,
  `ClearKeyOutcome`, and the private `carried_key` inside `set`.
  `describer_config::set` returns `()`; the head calls `show` afterwards
  for its echo. For a `set` that changes the backend, resolve presence
  AFTER `set`.
- **Rules a head must not re-derive.** Only an OpenRouter key goes to the
  Keychain (`plan_key_write` decides). The head writes the Keychain FIRST
  and stops on failure — the file half is a clear, so file-first then a
  refused Keychain write would leave no key anywhere. A stored file key
  never follows a backend switch, and a host move warns (`carried_key`).
- **The CLI/MCP key path** is `crates/cli/src/describer_key.rs`. `index
  run` resolves once above the `--watch` loop and only when the requested
  kinds include caption work.
- **The cached key at the desktop head**
  (`apps/desktop/src-tauri/src/captions.rs`). `DescriberKeyCache
  (RwLock<ResolvedKey>)` is managed state; a poisoned lock is recovered.
  The key is resolved once, never per scheduler tick — a Keychain read is
  a macOS access check that a denied prompt does not remember, so a
  per-tick read could prompt every poll. The cache refills at startup
  (`setup_app` in `lib.rs`), on `adopt_catalog`, and after a save or
  clear. `KeyRefresh { cache, store, env }`, `CaptionDeps { keys, wake }`,
  `refresh_key`; four commands (`describer_settings`, `save_describer`,
  `clear_describer_key`, `test_describer`) as one-liners over `*_impl`s.
  A blank `base_url` from the form is treated as absent.
- **The Captions section** (`apps/desktop/src/lib/CaptionsSection.svelte`,
  `captions-status.ts`, `api-captions.ts`). Imports `captionsApi`
  directly. The key-status line describes the key for the backend chosen
  in the form (`formKeySource`: the saved `key_source` only when the saved
  backend equals the form's, else `none`). A typed key is dropped when the
  form leaves OpenRouter. Save, Test and Remove key share one in-flight
  guard; Test is enabled only when the form matches the saved config.
- **The Keychain test seam, at all three heads.** This is the most
  important operational fact in this handoff.
  - **CLI**: every test child goes through `common::maj_bin()`
    (`crates/cli/tests/common/mod.rs`), which sets a throwaway
    `majestical-test-<pid>-<n>` service; `crates/cli/tests/keychain_guard.rs`
    fails any test file that spawns the binary another way (a text scan —
    the weaker guard; watchlist). The CLI's `system_store` has a unit test,
    `the_system_store_honors_the_keychain_service_override`.
  - **Desktop**: `apps/desktop/src-tauri/tests/tauri_parity.rs` has a
    private `mod guarded`. `struct Maj(PathBuf)` has no accessor outside
    the module (module-level privacy is load-bearing: a bare tuple struct
    in the same file leaves `maj.0` reachable); `Maj::run` applies a
    throwaway service and removes `MAJ_OPENROUTER_KEY`, then `guarded(..)
    -> Guarded`, and only a `Guarded` is spawnable — dropping the check is
    a compile error. The guard fires before `output()`. Three
    `should_panic` tests pin the guard itself. Known irreducible limit
    (mutation 34b): deleting the guard inside the module still compiles.
    Do not "fix" it with a source scan.
  - **e2e**: `apps/desktop/e2e/wdio.conf.ts`'s `isolateKeychain()` sets
    `MAJ_KEYCHAIN_SERVICE=majestical-test-e2e-<pid>-<uuid>` and deletes
    `MAJ_OPENROUTER_KEY` from `process.env` before anything spawns;
    `assertKeychainIsolated()` checks `process.env` before the fixture
    `maj` children and the merged capability env right before the service
    spawns the app. Every refusal in `onPrepare` is rethrown as
    `SevereServiceError` — WDIO only logs a plain `Error` from a config
    hook and launches the app anyway.
  - Both cleanup guards (`common::KeychainCleanup`, and `Cleanup` in
    `crates/secrets/src/system.rs`) panic on any name that is not a
    throwaway.
- **Wire fixtures with nulls**: `wire_fixtures.rs`'s
  `check_or_update_with_nulls(name, value, &[json pointers])` — each named
  pointer must be present and null, every other field populated.

## Backlog pointer

`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` carries a "Phase 7G
deferrals" section (attributed per PR) and a "cargo-mutants triage (phase
7G)" section. Everything the phase 7F watchlist carried forward — wire-layer
codegen, Windows/Linux release artifacts, ledger keying finer than `(kind,
asset)`, MCP progress notifications, CLI ingest progress rendering, the
ingest queue, localization — is carried again.

## Phase 7H recommendation

This is a recommendation for the user to confirm, not a decision. Derived
from the phase 7G deferrals and the carried backlog, roughly in order of
value:

- **Make the test and CI guards structural.** Three items from this phase
  are warnings carried by hand that a small change would retire: the
  reference-binary trap (parity suites skip or compare against a stale
  `maj` and report green — fail instead, and check freshness); porting the
  desktop's `get_envs` assertion into `common::maj_bin()` to retire
  `keychain_guard.rs`'s text-scan weakness; and pinning the Rust toolchain
  and the conformance scripts' Python dependencies, since two upstream
  releases broke main with no commit (#144). The 19 pre-existing
  `cargo doc -D warnings` errors in `majestical-services` belong here too.
- **Cleanup of the ~42,389 leaked state directories**, once a safe
  discriminator between a leaked directory and a real catalog's state is
  designed.
- **Code signing**, which a shared Keychain access group needs (one item,
  no per-binary prompt) and which Windows/Linux artifacts and
  notarization would build on. Larger; it touches `docs/RELEASING.md`.
- **Wire-layer codegen** (carried since 7E): `api.ts` is split three ways
  now (`api.ts`, `api-alwayson.ts`, `api-captions.ts`), and the TS fixture
  tests' `as` casts let a TS-only field pass — generated types would close
  both.
- **A model picker** fed by the backend's model list.
- Smaller: `MemoryKeyStore::unsupported()` (17 call sites); the off-macOS
  "file" key status line (needs a string the mockup lacks).

Brainstorm fresh rather than picking from this list as written — it is
input to a 7H design session, not its output. Write a phase 7H spec + plan
in the established format before any code; a GUI change gets a mockup
approved before code (that convention held for a fifth phase).

## Process conventions (follow these — they are user-mandated)

Carried from the 7G handoffs:

1. **Workflow**: superpowers brainstorming → writing-plans →
   subagent-driven development. Each task: fresh implementer subagent →
   adversarial spec-compliance reviewer (probes empirically,
   mutation-tests every claim) → code-quality reviewer → fix rounds until
   APPROVED. In every 7G chunk that touched the key this found a real
   defect — including two credential leaks that predate the phase, each
   found by a reviewer reproducing it on the wire rather than reading the
   code. Do not shortcut it.
2. **Merge as you go**: chunk PRs (1-2 tasks each), squash-merge after CI
   is green (including `gui-e2e`, by convention), without asking per PR
   (the user's standing instruction). Never push to main
   directly. **Never `gh pr merge --auto`** — on this repo it merges
   immediately; watch the checks, then merge explicitly.
3. **NO Claude-Session trailers or session links in commit messages**
   (user mandate). The harness's own reminder asks for one; the mandate
   wins — amend it out.
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain
   git).
5. Shared checkout: implementers stage ONLY their files by explicit path,
   never `git add -A` (`.superpowers/` must stay untracked). Do not run
   reviewers (who mutate files empirically) concurrently with
   implementers, and do not switch branches while either is running.
6. Shell: variables do NOT persist across Bash invocations. `trash`, never
   the recursive-force `rm` — a hook blocks any Bash command containing
   that pattern, including heredoc TEXT, so a justfile recipe or a doc
   that carries the words must be written with the Edit/Write tool. A dcg
   hook also blocks `git checkout -- <path>`, `git branch -D`, and
   redirects to variable paths: use `git stash` and literal paths; branch
   deletion is the user's.
7. Git auth: SSH unavailable; push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings; verify current versions of deps/actions at execution
   time, never from memory.
9. Reviewers' findings get fixed in the same chunk when cheap; deferred
   items go on the watchlist with attribution.
10. **`cargo-mutants` runs FOREGROUND, one at a time, `--in-place` on a
    warm target, `git status` clean after each.** A run that dies leaves
    a mutant in the tree — check `git status` before anything else. The
    harness moves a 600-second-plus command to a tracked job; wait for it
    to finish before starting the next run. Long CLI suites need
    `--timeout 900` (the default auto-timeouts gave false TIMEOUTs).
11. **cargo-mutants and the Keychain**: on any crate whose head builds a
    `SystemKeyStore`, exclude `system_store` (`--exclude-re
    system_store`) from the automated run and check that mutant by hand,
    alone, against only a test that makes no store call.
    `SystemKeyStore::default()` names the REAL `majestical` item; under
    that mutant the CLI's test children would write to and delete it.
12. **Subagents report through their final message.** A subagent that
    starts a background command loses it when its turn ends: nudge once
    ("run it in the foreground, then report"); if it stalls again, take
    the verification over. Reports truncate, sometimes repeatedly and
    mid-sentence, and one was dropped entirely: ask for "ONLY the
    remainder, compact", say where it cut off, and tell reporters to lead
    with the verdict and anything that went wrong. A promised follow-up
    may never arrive — chase it. Consider `model: opus` or `sonnet`
    explicitly when dispatching (credits for one model ran out mid-phase
    and an implementer died with its work uncommitted). Tell implementers
    to use narrow, crate-scoped commands.
13. **Give each agent uniquely named scratch files.** Two agents sharing
    one scratchpad collided once (a reviewer's script was overwritten).
14. **`origin/main` goes stale** in this SSH-less checkout. Fetch/pull
    `main` explicitly before rebasing; baseline diffs on local `main`.
15. **Gate a commit on its verification with `&&`, never a pipe.** A
    subagent piped a commit gate through `grep` and committed a failing
    test.
16. **Changing an existing verb's output on a branch needs a temporary
    parity normalizer** in `crates/cli/tests/services_parity.rs`
    (exact-substring removal on both binaries, gated on the clean-fixture
    value, unit-tested), scheduled for deletion in the plan's close task.
    None was needed in 7G.
17. **Check the parity reference binary before trusting a green parity
    run**: `/tmp/maj-ref` must exist and be built at the merge-base, and
    `target/debug/maj` must be newer than the newest commit it must
    reflect. A missing reference skips every diff and still reports
    passed (until the watchlist item makes this structural).
18. **The session scratchpad is wiped on date rollover.** Facts a closing
    agent needs belong in the repo or in memory.
19. **A screenshot from the automation session is black** unless the
    terminal has Screen Recording permission; a pixel-level hand check is
    the user's, recorded on the PR.

**Keychain safety rules** (for anyone running things by hand or briefing
a subagent):

- **While any mutant that could point a child at the real service is in
  the tree, run ONLY the guard test — never the full suite.** In Task 7 a
  guard-removal variant probed with the full suite spawned six real `maj`
  children unguarded; no Keychain access resulted only because no parity
  row configured a describer. `cargo test --no-run` and a filtered
  `--test tauri_parity guarded::` are the safe probes.
- **Do not accept a reasoning-based safety claim about the Keychain.**
  Probe it, or trace the code path. Twice in 7G a plausible claim was
  wrong.
- Never run `security … -s majestical …`. Never `security … -w` on an item
  `maj` created (it raises an ACL prompt).
- Never run `maj` without `MAJ_STATE_DIR` pointed at a `mktemp -d`, and
  set `MAJ_KEYCHAIN_SERVICE` to a `majestical-test-` name for anything
  that could reach the store. `env -u MAJ_OPENROUTER_KEY` so an ambient
  key cannot decide a result.
- Only `sk-test` / `sk-test-2` as key literals, ever.
- **If a macOS dialog appears, STOP and report.** None appeared across
  phase 7G; one appearing means an assumption broke.

## Phase-7G lessons worth carrying

- **Upstream drift can break main between commits.** Rust 1.99 and PyAV
  19 each turned main red with no commit (#144). Check main's CI before
  blaming a PR.
- **When a guard protects the Keychain seam, ask what enforces the guard,
  recursively,** until the answer is the compiler or a documented
  irreducible limit. Every rejection in Task 7 was a guard, never the
  behaviour; it converged only as the protection moved test → type →
  compiler.
- **A hook that only logs is not a guard.** WDIO aborts a launch only on
  `SevereServiceError`; a plain `Error` from `onPrepare` is logged and the
  app launches anyway, so two checks in the e2e config — one pre-existing
  — could never stop a run until #145.
- **Mirroring behaviour is not mirroring the suite.** `captions.rs` matched
  `describer_key.rs`'s behaviour and shipped without four of its tests
  (all on `clear`, the one path that deletes a credential). When writing a
  head's twin, diff the `mod tests` too.
- **A dry run that promises what the confirmed call will not do is the
  phase's commonest bug** — four separate instances. Pin every dry run
  against what the confirmed call actually does.
- **`Default` on a type that names a real resource is a mutant waiting to
  happen.** cargo-mutants will substitute it; know which `Default`s point
  at production state before a run.
- **Slow Keychain tests can mean a huge `target` dir.** The Security
  framework scans the calling binary's directory; with 1.8M files in
  `target/debug/deps`, every Keychain call took 9-24 seconds. Check the
  file count before believing anything else (proposal: clean both
  `target` dirs between phases — watchlist).
- **Probe, do not reason.** Twice a plausible simplification was wrong in
  a way only a probe showed; both would have made a dry run lie about a
  deletion.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  Blobs are the exchange format. `crates/services` is compute-only:
  request struct in, outcome struct out, `ServiceError` out — it never
  prints, never reads the environment, and never reaches a secret store;
  neither does `crates/describe`. Only the heads depend on
  `crates/secrets`.
- Warnings ride the outcome (`Notices`), not stderr — on the failure path
  too.
- **A key never appears in any output, notice, error, `Debug` rendering,
  fixture, log or test output.** Types holding a key have hand-written
  redacting `Debug` (`ResolvedKey`, `FileKey`, `KeyWrite`,
  `DescriberConfig`, `SaveDescriberReq`); `ResolvedKey` never derives
  `Serialize`. Errors on the key's path render `{err}`, never `{err:#}`.
  A config parse error names a line and never quotes the file.
- **The Keychain is written before the file is cleared**, and only an
  OpenRouter key goes there. A stored key never follows a backend switch.
- **The desktop resolves the key once and caches it**; it never reads the
  Keychain per scheduler tick.
- Exit-code/error polarity is decided once in the outcome structs;
  `doctor` returns `Ok` even when every check fails.
- The ledger remembers only permanent failures; a credentials failure is
  transient and never reaches it.
- **A dry run describes REAL state and writes nothing.** MCP mutating
  tools default to dry-run.
- Platform selection is `cfg(target_os)`, never a cargo feature.
- Tauri commands stay one-liners over `*_impl`; every new wire field gets
  a fixture on BOTH sides.
- Never lie about data safety or completeness; degradation names the
  specific gap and remedy; a doctor row never echoes a secret.
- Tests must discriminate: reviewers mutation-test, and every new guard
  ships with the test that fails when it is deleted.

## Environment

- `just`, `protoc`, ffmpeg, ImageMagick, ~2GB model cache, Node 22+,
  pnpm 11+. `just gui-install` before any GUI recipe. CI installs the
  current stable Rust (1.99 at handoff) and cargo-mutants 27.1.0 was used
  for the 7G triage.
- Local GUI verification is the four-command line in `apps/desktop`:
  `pnpm check && pnpm lint && pnpm test && pnpm build`, plus
  `pnpm check && pnpm lint` in `apps/desktop/e2e` when e2e files change.
- A local e2e run needs the debug bundle: `cargo build -p majestical-cli`,
  then `pnpm tauri build --debug -b app --config
  src-tauri/tauri.e2e.conf.json` in `apps/desktop`, then `pnpm test` in
  `apps/desktop/e2e` (`pnpm test --spec specs/<one>.e2e.ts` runs one
  file). In 7G the bundle plus the CLI build took 505 s on a warm desktop
  target; a cold build was not timed since the 2026-09-19 clean (the 7F
  handoff's 25-minute figure predates it).
- **Both `target` dirs were cleaned on 2026-09-19**, reclaiming 574 GiB
  (root: 2,172,728 files / 435 GiB; desktop: 319,777 files / 139 GiB).
  If Keychain tests get slow again, check the file count first.
- Parity reference: rebuild `/tmp/maj-ref` at the PR's merge-base before
  trusting `services_parity` or `tauri_parity` (convention 17).
- A screenshot from an automation session is black unless the terminal
  has Screen Recording permission; pixel-level checks are the user's.
