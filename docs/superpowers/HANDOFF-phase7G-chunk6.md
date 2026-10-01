# Majestical — Phase 7G mid-phase handoff (resume at Task 8)

Written 2026-09-22; **revised 2026-10-01, Task 7 complete and approved.**
Chunks 1-5 of 8 are merged. This is NOT a phase-close handoff: phase 7G is
half done, its spec and plan are approved and current, and the work resumes
at **Task 8**, on the branch described below. Read this, then the plan.

- Parent spec (approved):
  `docs/superpowers/specs/2026-07-28-majestical-design.md`
- Phase 7G spec:
  `docs/superpowers/specs/2026-09-18-phase7g-captions-parity-design.md`
- Phase 7G plan, the thing to execute:
  `docs/superpowers/plans/2026-09-18-phase7g-captions-parity.md`
- Mockup, **approved by the user before code**, and the contract for Task 9:
  `docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`
- The phase's own entry handoff (what 7G was for):
  `docs/superpowers/HANDOFF-phase7G.md`
- Repo: github.com/statik/majestical · `main` at `2789316` · work in flight
  on `phase7g-desktop-head` (4 commits, unmerged, no PR yet)

## What phase 7G is

The GUI is the only head with no describer configuration: a GUI-only user
cannot get captions, and since 7F the doctor row tells them so without
offering a fix. This phase closes that, and moves the OpenRouter API key out
of a plaintext `api_key` in the per-catalog `describer.toml` into the macOS
Keychain, at every head.

## Done (merged)

- **Chunk 1** (#134) — the spec, the plan, and the approved mockup.
- **Chunk 2** (#135) — the unit-test state-dir leak stops at its source.
- **Chunk 3** (#136) — a rejected key and an empty account fail by name;
  `describer test` checks the key.
- **Chunk 4** (#137) — `crates/secrets`; views that name the key's source;
  config errors that never quote the file.
- **Chunk 5** (#138) — the Keychain at the CLI and MCP heads, and
  `describer clear-key`.

## In flight — `phase7g-desktop-head` (READ THIS FIRST)

Branch off `main` (`2789316`), **4 commits, not pushed, no PR**. Working
tree clean; `.superpowers/` untracked and must stay so. Desktop suite is
**182 passing**, clippy clean at `-D warnings` on the desktop tree and on
`-p majestical-secrets -p majestical-services -p majestical-cli`. The only
warning anywhere is the pre-existing `ld: __eh_frame section too large`.

| commit | what |
|---|---|
| `07d4af4` | Task 7: `captions.rs`, the cached key, the four commands |
| `bf8574a` | the nine guards that survived mutation review |
| `52ea2de` | guard every `maj` spawn, not one synthetic command |
| `0013163` | make dropping the spawn guard a compile error |

**Task 7's spec review is APPROVED** (adversarial: three rejection rounds,
all closed). Its behaviour was never broken by any probe — every rejection
was a missing or defeatable *guard*.

### What is left before this branch can be a PR

1. **Task 7 code-quality review** — not yet run.
2. **Task 8** — the wire: fixtures, `api-captions.ts`, the `tauri_parity`
   `describer_settings` row. Plan lines 1210-1299.
3. Then the chunk-6 PR: watch CI green (including `gui-e2e`), squash-merge.

### Three held items never delivered

The spec reviewer owed, and never sent, three write-ups it says it holds
verbatim from completed runs. None blocks Task 8; all three are wanted for
Task 12's as-built, and the first two may need re-probing against `0013163`:

1. **PROBE2** — the refused-Keychain-delete message under `{err}`, `{err:#}`
   and `{err:?}`. Its headline verdict was *no key on any rendering*; the
   full output was truncated away.
2. **The `Debug`-on-`DescriberConfigView` verdict** — whether the derive is
   genuinely required and provably leak-free. Headline was leak-free.
3. **Its own watchlist** — folded into the list below as far as it was
   reported.

## Remaining tasks

- **Chunk 6 — Task 8** (the wire: fixtures, `api-captions.ts`,
  `tauri_parity`). Task 7 is done.
- **Chunk 7 — Task 9** (`CaptionsSection.svelte` + the Settings mount + the
  `App.test.ts` split) and **Task 10** (the e2e flow).
- **Chunk 8 — Task 11** (cargo-mutants) and **Task 12** (watchlist, as-built,
  normalizer deletion, the 7H handoff).

## The shape the code is in now (read before writing Task 8)

`crates/cli/src/describer_key.rs` and `apps/desktop/src-tauri/src/captions.rs`
are the same job at two heads; read whichever is nearer your work.

- **`crates/secrets`** (`majestical-secrets`) is the ONLY code that touches a
  secret store. No library crate may depend on it; the three heads are the
  only permitted dependents. Surface: `trait KeyStore`, `SystemKeyStore`
  (macOS Keychain; a stub elsewhere, selected by `cfg(target_os)`),
  `SUPPORTED`, `SERVICE_ENV`, `resolve(env, wants_keychain, &store) ->
  ResolvedKey { key, source, notice }`, plus the shared test doubles
  `MemoryKeyStore::{holding, failing, held}` and `PanickingKeyStore`.
  **Use those doubles — do not write a fourth copy.**
- **`resolve`'s contract**: env wins and the store is then NEVER read; the
  store is read only when `wants_keychain` (the STORED backend is
  OpenRouter). A read failure is a notice, never an error.
- **`crates/services` owns the pure decisions, and never reads ambient state
  or prints**: `KeyPresence { Env, Keychain, Absent }` (what a head reports,
  `serde(skip)`, never on the wire), `KeySource` (what a view shows; `Absent`
  serializes as `"none"`), `key_source`, `plan_key_write(backend,
  keychain_supported, key)`, `wants_keychain`, `clear_file_key`,
  `ClearKeyOutcome`, and the private `carried_key`.
- **`describer_config::set` returns `()`.** The head calls `show` afterwards
  for its echo, because a view names the key's source and that depends on
  what the head found. For a `set` that CHANGES the backend, resolve the
  presence AFTER `set` — `wants_keychain` reads the stored backend.

### Rules a head must not re-derive

- **Only an OpenRouter key goes to the Keychain.** `plan_key_write` decides;
  the head executes and MUST write the Keychain FIRST, stopping on failure
  (the file half is `Clear`, so file-first then a refused Keychain write
  leaves no key anywhere).
- **A stored file key never follows a backend switch**, and a host move
  warns. That is `carried_key`, inside `set`, so Task 7's desktop head gets
  it for free — do not reimplement it.
- **The key is resolved once and cached at the desktop head**, never per
  scheduler tick: a Keychain read is a macOS access check that a denied
  prompt does not remember, so a per-tick read can prompt every poll. The
  cache refills at startup, on `adopt_catalog`, and after a save or clear.

### What Task 7 actually built (the surface Task 8 wires)

`apps/desktop/src-tauri/src/captions.rs`:

- `DescriberKeyCache(RwLock<ResolvedKey>)` — managed state. A poisoned lock
  is recovered, never propagated.
- `KeyRefresh<'a> { cache, store, env }` and
  `CaptionDeps<'a> { keys, wake }`; `refresh_key(keys, cfg)`;
  `KeyRefresh::ambient(cache, store)`.
- `DescriberSettingsOutcome { describer: Option<DescriberConfigView>,
  keychain_supported: bool, notices }` — `describer` is `null`, not absent,
  when nothing is configured.
- `SaveDescriberReq { backend, model, base_url, api_key }`;
  `DescriberProbeOutcome { #[serde(flatten)] probe, notices }`.
- Four `*_impl`s and four commands: `describer_settings`, `save_describer`,
  `clear_describer_key` (sync one-liners), `test_describer` (`async` over
  `blocking`).

**Amendments to record in Task 12's as-built** (all reviewed and approved):

1. `CaptionDeps` is `{ keys: KeyRefresh, wake }`, not the plan's flat
   four-field struct: `adopt_catalog` also refills the cache and the flat
   shape would have pushed it to seven parameters. It is now at five.
2. `crates/services/src/describer_config.rs` — `Debug` added to
   `DescriberConfigView`. Required: `DescriberSettingsOutcome` derives
   `Debug` and holds one, and `Result::expect_err` needs it. The view has no
   key field by construction.
3. `crates/secrets/src/system.rs` — `SystemKeyStore::service_name() -> &str`,
   `#[cfg(target_os = "macos")]`. Exists so a test can prove `system_store()`
   honoured `MAJ_KEYCHAIN_SERVICE` **without performing a store call**. Off
   macOS the accessor and its test vanish together; the stub has no field
   and every call is `Unsupported`, so there is no item to name.
4. **A blank `base_url` is treated as absent**, like a blank key, with its
   own test. Not in the plan. A Settings text input sends `""`, which `set`
   would otherwise store verbatim instead of defaulting to the backend's
   URL. This is a GUI-shaped bug the CLI never had — a CLI user omits a
   flag rather than submitting an empty field.
5. `lib.rs`'s setup closure became a named `setup_app`: inlining the refresh
   pushed `run()` to 103 lines (`too_many_lines` is 100), and the named
   function sits outside `run()`'s blanket `expect_used`/`exit`
   expectations, which is the house rule anyway.
6. A refused Keychain **delete** keeps the store's own message
   (`could not remove the key from the macOS Keychain: {err}`), as the CLI
   does. Only the **write** message is fixed by mockup frame 8. Rendered
   `{err}`, never `{err:#}`.
7. `commands::key_presence()` was **deleted outright**, no shim;
   `DescriberKeyCache::presence()` replaces it.

## The test seam that protects the developer's real Keychain

**This is the most important operational fact in this handoff.**

The real item is service `majestical`, account `openrouter-api-key`. After
7G a `maj` child reads it whenever the backend is OpenRouter and the env key
is unset, and `describer clear-key` DELETES from it.

- `SystemKeyStore` takes a service name; a head reads `MAJ_KEYCHAIN_SERVICE`
  (`majestical_secrets::SERVICE_ENV`) to override it. The crate itself never
  reads the environment.
- Every CLI test child goes through `common::maj_bin()`, which sets a
  throwaway `majestical-test-<pid>-<n>`. `crates/cli/tests/keychain_guard.rs`
  fails any test file that spawns the binary another way.
- Both cleanup guards (`common::KeychainCleanup`, and `Cleanup` in
  `crates/secrets/src/system.rs`) panic on any name that is not a throwaway.

**Task 7 extended this to the desktop, and the desktop's guard is now the
stronger of the two.** `apps/desktop/src-tauri/tests/tauri_parity.rs` has a
private `mod guarded`:

- `struct Maj(PathBuf)` — the path is a field of that module with **no
  accessor**, so a spawn site cannot reach the binary to bypass the guard
  (`E0616`). **Module-level privacy is load-bearing, not decoration**: a bare
  tuple struct in the same file leaves `maj.0` reachable.
- `Maj::run` applies both protections (a throwaway
  `majestical-test-<pid>-<n>` service, `MAJ_OPENROUTER_KEY` removed), then
  `guarded(Command) -> Guarded`, and only a `Guarded` is spawnable — so
  dropping the check is a compile error (`E0061`/`E0308`), not a silent
  weakening. The guard fires **before** `output()`, so a child that fails it
  is never spawned.
- Three `#[should_panic]` tests in `guarded::tests` pin the guard itself;
  both halves were separately proven to discriminate.

**Known limit (watchlist, irreducible).** Deleting the guard call *and*
rewriting the spawn to re-unwrap still compiles: `Command::output` is
inherent and `self.0` is reachable inside the module. Three structural fixes
were tried and all dissolve on inspection — any function that can reach the
path can spawn. **Types and privacy protect the six spawn sites outside the
module, where an unguarded site would realistically appear; the 40-line
module itself is covered by the three `should_panic` tests.** Do not
"fix" this with a source scan: that is `keychain_guard.rs`'s weakness.

**Still open:** the e2e harness (`apps/desktop/e2e/wdio.conf.ts`) injects no
`MAJ_KEYCHAIN_SERVICE`. **This is a live exposure as of `07d4af4`** — the
desktop head now reaches a real store. Nominally Task 10; close it in chunk
7 rather than waiting. `crates/cli/tests/keychain_guard.rs` still only scans
`crates/cli/tests`, so it says nothing about this head.

Rules for anyone running things by hand or briefing a subagent:

- **While any mutant that could point a child at the real service is in the
  tree, run ONLY the guard test — never the full suite.** This was learned
  the hard way in Task 7: probing a guard-removal variant with the full
  suite spawned six real `maj` children unguarded. No Keychain access
  resulted (no parity row configures a describer, so `wants_keychain` is
  false and `majestical_secrets::resolve` returns before touching the store;
  only `doctor` reaches that path and takes the same early return), but the
  margin was luck, not design. `cargo test --no-run` and a filtered
  `--test tauri_parity guarded::` are the safe probes.
- **Do not accept a reasoning-based safety claim about the Keychain.** Probe
  it, or trace the code path. Twice this phase a plausible claim was wrong.
- Never run `security … -s majestical …`. Never `security … -w` on an item
  `maj` created (it raises an ACL prompt).
- Never run `maj` without `MAJ_STATE_DIR` pointed at a `mktemp -d`, and set
  `MAJ_KEYCHAIN_SERVICE` to a `majestical-test-` name for anything that
  could reach the store. `env -u MAJ_OPENROUTER_KEY` so an ambient key
  cannot decide a result.
- Only `sk-test` / `sk-test-2` as key literals, ever.
- **If a macOS dialog appears, STOP and report.** None appeared across the
  whole phase; one appearing means an assumption broke.

## Process (user-mandated; carried from the 7F/7G handoffs)

1. **Workflow**: for each task, a fresh implementer subagent → an adversarial
   spec-compliance reviewer (probes empirically, mutation-tests every claim)
   → a code-quality reviewer → fix rounds until approved. In every chunk
   that touched the key this turned up a real defect — including two
   credential leaks that predate the phase, each found by a reviewer
   reproducing it on the wire rather than reading the code. Do not
   shortcut it.
2. **Merge as you go**: chunk PRs, squash-merge after CI is green. Never push
   to main. Never `gh pr merge --auto`.
3. **NO Claude-Session trailers in commit messages** (user mandate). The
   harness asks for one; the mandate wins.
4. **Do NOT use the `submitting-changes` skill** (user mandate — plain git).
5. Shared checkout: implementers stage ONLY their files, never `git add -A`
   (`.superpowers/` must stay untracked). Never run reviewers concurrently
   with implementers.
6. Shell variables do NOT persist across Bash calls. `trash`, never the
   recursive-force remove — a hook blocks that pattern even inside heredoc
   text.
7. Git auth:
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
8. Zero warnings. Verify current dependency versions at execution time.
9. **`cargo-mutants` runs FOREGROUND, one at a time, `--in-place`, with
   `git status` clean after each.** A run that dies leaves a mutant in the tree.
10. Gate a commit on its verification with `&&`, never a pipe.
11. Changing an existing verb's output needs a TEMPORARY parity normalizer in
    `crates/cli/tests/services_parity.rs`, deleted in Task 12. **None was
    needed this phase** — every parity row uses Ollama with no key.

### Hard-won operational lessons from this half of the phase

- **The reference-binary trap has three forms, and all three appeared in
  chunk 6 alone.** `/tmp/maj-ref` MISSING (the suite skips every diff and
  still reports "56 passed"); `target/debug/maj` STALE (a Sep 19 binary
  predating chunks 4-5, with no Keychain code at all — every row compared
  against the wrong thing and reported green); and `target/debug/maj`
  ABSENT. `maj_or_skip` only checks the file exists, and even that was false
  once. **Check the binary's mtime against the newest commit it must
  reflect, every time.** Task 12 should make this structural rather than
  carrying it as a warning a fourth time.
- **Every rejection in Task 7 was a guard, never the behaviour.** Three
  adversarial rounds: nine missing tests (four of them straight ports the
  desktop twin dropped from `crates/cli/src/describer_key.rs`, all on
  `clear` — the one path that deletes a credential); then a guard asserting
  on a synthetic command while six real children went unchecked; then the
  guard's own call being deletable. Each fix was correct and each had a hole
  the next round found. It converged only because the protection moved down
  a level each time: test → type → compiler. **When a guard protects the
  Keychain seam, ask what enforces the guard, recursively, until the answer
  is the compiler or a documented irreducible limit.**
- **Mirroring behaviour is not mirroring the suite.** `captions.rs` matched
  `describer_key.rs`'s behaviour closely enough to survive adversarial
  probing, then shipped without four of its tests. When writing a head's
  twin, diff the `mod tests` too.
- **Subagent reports truncate repeatedly, and one was dropped entirely.**
  Budget for it: ask for "ONLY the remainder, compact", say where it cut
  off, and tell reporters to lead with the verdict and anything that went
  wrong. A promised follow-up may simply never arrive — chase it.
- **Subagent reports get truncated in delivery**, sometimes repeatedly and
  always mid-sentence. Ask for "ONLY the remainder, compact" and say where
  it cut off.
- **A subagent that starts a background command loses it** when its turn
  ends. One nudge to "run it in the foreground and report" recovers it; if
  it stalls twice, take the verification over yourself. This happened
  repeatedly. Tell implementers to use narrow, crate-scoped commands.
- **Fable credits were exhausted mid-phase** and an implementer died with
  its work uncommitted (a clean red state, which I finished by hand).
  Consider `model: opus` or `sonnet` explicitly when dispatching.
- **Probe, do not reason.** Twice a plausible simplification was wrong in a
  way only a probe showed — once mine, once a reviewer's. Both would have
  made a dry run lie about a deletion.

## Environment

- `just`, `protoc`, ffmpeg, ImageMagick, ~2GB model cache, Node 22+, pnpm 11+.
  `just gui-install` before any GUI recipe.
- Local GUI verification is the four-command line in `apps/desktop`:
  `pnpm check && pnpm lint && pnpm test && pnpm build`, plus
  `pnpm check && pnpm lint` in `apps/desktop/e2e` when e2e files change.
- A local e2e run needs the debug bundle: `cargo build -p majestical-cli`,
  then `pnpm tauri build --debug -b app --config src-tauri/tauri.e2e.conf.json`
  in `apps/desktop`, then `pnpm test` in `apps/desktop/e2e`. **The desktop
  target was cleaned on 2026-09-19, so that bundle no longer exists** — Task
  10 pays for one cold build. Time it; the 25-minute figure in the 7F
  handoff predates the clean.
- **Both `target` dirs were cleaned on 2026-09-19**, reclaiming 574 GiB
  (root: 2,172,728 files / 435 GiB; desktop: 319,777 files / 139 GiB). They
  are 43G and 19G now. This was not housekeeping: `target/debug/deps` held
  1.8M files, and the macOS Security framework scans the calling binary's
  directory, so every Keychain call from a test binary took 9-24 seconds
  (a 13-test suite took 60s; it takes 0.3s now). **If Keychain tests get
  slow again, check the file count before believing anything else.** Worth
  proposing a standing "clean between phases" convention in Task 12.
- A screenshot from an automation session is black unless the terminal has
  Screen Recording permission; pixel-level checks are the user's.

## Watchlist items accumulated this phase (Task 12 records them properly)

Found during 7G, not yet written to
`docs/superpowers/plans/2026-07-29-phase2-watchlist.md`:

- **~42,389 leaked state directories** remain under
  `~/Library/Application Support/majestical/catalogs/` from before chunk 2's
  fix. Their names are one-way hashes, so a leaked directory cannot be told
  from a real catalog's state by name. Cleanup needs a safe discriminator.
- `MAJ_STATE_DIR=""` is accepted as a relative base (pre-existing).
- No services unit test covers `MAJ_STATE_DIR`'s precedence (pre-existing).
- The models dir ignores `MAJ_STATE_DIR` and resolves under the real data dir.
- `RUSTDOCFLAGS='-D warnings' cargo doc -p majestical-services` fails with 19
  pre-existing errors; none from this phase. The doc gate cannot vouch for
  anything until they are fixed.
- `keychain_guard`'s strict `.env(` requirement is structurally unpinned: an
  allow-listed file satisfies it with ONE occurrence, so a second spawn site
  in the same file rides along unchecked.
- The MCP `set_describer` dry run under-promises when `MAJ_OPENROUTER_KEY` is
  set: it says a stored file key is unchanged where the confirmed call drops
  it. Deliberate (it errs away from over-promising a destructive act) and
  documented on `key_effect`.
- `clear_describer_key_result` loads `describer.toml` three times. A proposed
  simplification was DECLINED with evidence: it would have made the dry run
  deny a deletion it was about to perform for a local backend.
- `acceptance.rs` / `inbox_acceptance.rs` use fixed throwaway service names
  shared across runs (harmless while no scenario stores a key).
- `crates/services/src/sync.rs` chains the toml error when parsing
  `sync.toml`; that file holds no secrets, so it was left alone.
- The `ld: __eh_frame section too large` linker warning is pre-existing.
- Everything the 7F watchlist carried forward is still carried.

Added in chunk 6:

- **`tauri_parity.rs`'s `seeded_cfg` is offline by construction.** Its
  hand-emitted `VolumeSeen`/`AssetSeen` use a made-up `vol1`, and
  `gather_sources` (`crates/services/src/index/mod.rs:118`) resolves
  instances against `volume_identity::mounted_volumes()`'s REAL device ids
  — so every seeded item is *offline*, never *pending*. Fine for the search
  rows it was written for; it silently reports zero pending for anything
  status- or plan-shaped. Task 7 lost a cycle to this. **Task 8 writes a
  parity row — do not reuse it.** Scan a real directory with an auto-detected
  identity instead (`scan(&mut app, &media, None)`, the trick
  `index/mod.rs:693` documents).
- **Mutation 34b is an accepted irreducible limit** of the desktop spawn
  guard (above). Recorded so nobody "fixes" it with a source scan.
- **`crates/cli/tests/keychain_guard.rs` is now the weaker of the two
  guards.** The desktop's `get_envs` assertion could be ported to
  `common::maj_bin()`, retiring the existing watchlist entry about the text
  scan's one-`.env(`-per-file weakness rather than carrying it forward
  again. Cheap; propose it in Task 12.
- The reference-binary trap (above) affects `services_parity.rs`
  identically; it is a phase-wide item, not a desktop one.

## Key invariants (do not break)

- Events are immutable; the log is truth; SQLite/Lance are disposable.
  `crates/services` is compute-only: request in, outcome out, never prints,
  never reads the environment — and now, never reaches a secret store.
- Warnings ride the outcome (`Notices`), not stderr — on the failure path too.
- **A key never appears in any output, notice, error, `Debug` rendering, log
  or test output.** Types holding a key have hand-written redacting `Debug`
  (`ResolvedKey`, `FileKey`, `KeyWrite`, `DescriberConfig`); `ResolvedKey`
  must never derive `Serialize`. Errors on the key's path render `{err}`,
  never `{err:#}`. A config parse error names a line and never quotes the
  file.
- **A dry run describes REAL state and writes nothing.** Four separate bugs
  this phase were a dry run promising something the confirmed call would not
  do. When you add one, pin it against what the confirmed call actually does.
- Platform selection is `cfg(target_os)`, never a cargo feature.
- Tauri commands stay one-liners over `*_impl`; every new wire field gets a
  fixture on BOTH sides.
- Tests must discriminate: reviewers mutation-test, and every new guard ships
  with the test that fails when it is deleted.
