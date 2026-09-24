# Majestical — Phase 7G mid-phase handoff (resume at chunk 6)

Written 2026-09-22, with chunks 1-5 of 8 merged. This is NOT a phase-close
handoff: phase 7G is half done, its spec and plan are approved and current,
and the work resumes at **Task 7**. Read this, then the plan.

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
- Repo: github.com/statik/majestical · `main` at `a91beb0`

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

## Remaining

- **Chunk 6 — Task 7** (desktop: cached key + four commands) and **Task 8**
  (the wire: fixtures, `api-captions.ts`, `tauri_parity`).
- **Chunk 7 — Task 9** (`CaptionsSection.svelte` + the Settings mount + the
  `App.test.ts` split) and **Task 10** (the e2e flow).
- **Chunk 8 — Task 11** (cargo-mutants) and **Task 12** (watchlist, as-built,
  normalizer deletion, the 7H handoff).

Start Task 7 on a branch off `main`. Nothing is in flight; no branch has
unmerged work.

## The shape the code is in now (read before writing Task 7)

Task 7 is the desktop mirror of Task 6, so read
`crates/cli/src/describer_key.rs` first — the desktop's `captions.rs` is
the same job at a different head.

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

**Task 7 must extend this to the desktop.**
`apps/desktop/src-tauri/tests/tauri_parity.rs` spawns `maj` via `MAJ_BIN`
and does NOT set `MAJ_KEYCHAIN_SERVICE`; the e2e
harness (`apps/desktop/e2e/wdio.conf.ts`) injects env per run and will need
the same. The `keychain_guard` test only scans `crates/cli/tests`.

Rules for anyone running things by hand or briefing a subagent:

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

- **A green parity result means nothing unless `/tmp/maj-ref` exists.** The
  suite SKIPS every diff and still reports "56 passed". Build the reference
  from the merge-base (`git worktree add`, `cargo build -p majestical-cli`,
  copy to `/tmp/maj-ref`), run it, then remove both. I was nearly fooled.
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
