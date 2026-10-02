# Majestical Phase 7G — Captions parity: describer settings in the GUI, the key in the Keychain, named key failures

Written 2026-09-18 from a brainstorming session against
`docs/superpowers/HANDOFF-phase7G.md`. Parent spec:
`docs/superpowers/specs/2026-07-28-majestical-design.md`. The deferrals this
phase draws from are the "Phase 7F deferrals" section of
`docs/superpowers/plans/2026-07-29-phase2-watchlist.md`. Mockup (to be
approved before code, per the standing convention):
`docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`.

**Amended at planning (2026-09-18).** Reading the code for the plan changed
eight details — the adapter is a `KeyStore` trait, the credentials class
carries which problem it was, `DoctorRequest` takes key presence, `set`
returns nothing, the state-dir leak has a second source needing a justfile
env, the section imports its wire module directly, `App.test.ts` gets its
promised split, and delivery is eight chunks. They are listed under
"Planning-time amendments to the spec" in
`docs/superpowers/plans/2026-09-18-phase7g-captions-parity.md`; where that
list and this document disagree, the list wins until the as-built section
is written.

## Scope decisions (from design session)

- **Shape: a parity phase**, chosen over platform reach (Windows/Linux
  artifacts), wire-layer codegen, and new product capability. The GUI is
  the only head with no describer configuration: a GUI-only user cannot get
  captions, and since phase 7F the doctor row tells them so without
  offering a fix.
- **Four items, in.** (1) The OpenRouter key moves to the macOS Keychain at
  all three heads. (2) A Captions section in GUI Settings: backend, base
  URL, model, key, Save, Test, Remove key — with a mockup first. (3) A bad
  key is named: a 401 or 402 from a describer becomes its own failure
  class with a named reason, and `describer test` checks the key. (4) The
  unit-test state-dir leak stops at its source.
- **Key storage: the macOS Keychain.** Chosen over storing through the
  existing `describer.toml` path (exact parity, no dependency, but a
  plaintext key on disk) and over env-only with an explanatory caption
  (leaves the GUI-only user in a terminal, which is the gap).
- **Keychain reach: all heads on macOS.** Chosen over "GUI writes, everyone
  reads" (two places a key can live on one machine) and over GUI-only
  (breaks the three-heads-at-parity rule: a terminal `maj index run` would
  report no key beside a GUI that captions).
- **Placement: a small head-side adapter crate** over `security-framework`
  directly. Chosen over the `keyring-core` ecosystem (more dependencies for
  a Linux secret-service path nobody has asked for, and one that fails on
  headless CI) and over placing it in `crates/services` (services would
  start reading an ambient secret store; the heads read ambient state and
  pass it in).
- **`describer set` without a key keeps the stored key.** Today it rewrites
  the config keyless, silently dropping the key. The GUI can never read the
  key back, so an empty field on Save must mean "keep"; one rule at every
  head is chosen over the GUI being the exception. Removal becomes
  explicit.
- **No active migration of file keys.** A key already in a macOS
  `describer.toml` keeps working as the last fallback and moves the next
  time it is set. Chosen over a first-read migration (a head that rewrites
  a config file as a side effect of reading it).
- **Model entry is free text**, as at the CLI; Test reports whether the
  backend lists it. A model picker is deferred.
- **No cleanup of the already-leaked state directories.** Their names are
  one-way hashes of the catalog path, so a leaked test directory cannot be
  told from a real catalog's state by name, and the same folder holds the
  user's real catalog state.
- **Not in this phase** (stays on the watchlist): wire-layer codegen,
  Windows/Linux artifacts, ledger entries keyed finer than `(kind, asset)`,
  MCP progress notifications, CLI ingest progress rendering, the ingest
  queue, localization.

## What the investigation found (facts the design rests on)

- `crates/services` never reads the key. `IndexRunReq.api_key`,
  `describer_config::test(api_key)` and `DoctorRequest.describer_env_key`
  all take it from the head; the CLI (`describer_cmd::env_api_key`) and the
  desktop (`commands::env_api_key`) each read `MAJ_OPENROUTER_KEY` in three
  lines. `DescriberConfig::effective_api_key(env_key)` applies the head's
  key for OpenRouter only and falls back to the file's `api_key`.
- `describer.toml` lives in the per-catalog state dir, is written `0o600`,
  and carries an optional plaintext `api_key`. `describer_config::set`
  replaces the file wholesale, so a `set` without `--api-key` drops a
  stored key.
- MCP already has `get_describer`, `set_describer { api_key }` and
  `test_describer`. The GUI has no describer code at all; `SettingsView`
  is 80 lines holding Health and `AlwaysOnSection`.
- `HttpDescriber::probe` calls `{base_url}/v1/models`, which OpenRouter
  serves without authentication — a probe cannot detect a bad key.
  `GET https://openrouter.ai/api/v1/key` with a bearer key answers 200 with
  key details or 401; a 402 is returned by a completion request, not by
  the key endpoint (OpenRouter API reference, read 2026-09-18).
- The describe client folds 401 and 402 into `PortFailure::Unavailable`
  through `NOT_ABOUT_THE_PAYLOAD`; the caption pass renders the server's
  text. The ledger's rule — only permanent failures are remembered — is
  pinned by a proptest.
- The desktop scheduler reads the key once per tick
  (`batch_request(decision, env_api_key())`).
- The e2e harness injects `MAJ_STATE_DIR` and `MAJ_DESKTOP_CONFIG_DIR` per
  run, so it can inject a key the same way.
- `state_dir::state_base` falls through to `dirs::data_dir()` when
  `MAJ_STATE_DIR` is unset. About 50 `crates/services` unit tests across
  16 modules call `FsApp::init` without it. The dev machine held 33,193
  leaked directories on 2026-09-15 and 39,990 on 2026-09-18. The CLI
  integration tests already set the variable.
- `apps/desktop/src/lib/api.ts` is at 559 of its 560-line cap.
- `security-framework` is at 3.7.0 (crates.io, 2026-09-18). Verify again
  at execution time.

## Architecture

### Wave 1 — The state-dir seam (`crates/services/src/state_dir.rs`)

Under `cfg(test)`, `state_base`'s fallback is a directory under
`std::env::temp_dir()` (`majestical-test-state`), never
`dirs::data_dir()`. `MAJ_STATE_DIR` still wins in both builds. Env vars are
process-global and the tests run in parallel, so a per-test override is not
practical; a compile-time seam needs no call-site change. A guard test
asserts the test-build base is not under the platform data dir. The PR
records the directory count under the real data dir before and after
`cargo test -p majestical-services`.

### Wave 2 — Named key failures (`crates/core`, `crates/describe`, `crates/services`, CLI, MCP)

- `PortFailure` gains `CredentialsRejected`: the port answered, and the
  problem is the caller's key or account — not the input, not an outage.
  `PortError::credentials(context, source)` constructs it.
- The describe client maps 401 and 402 to it through a new
  `DescribeHttpError::Credentials { url, status, kind }` where `kind` is
  `KeyRejected` (401) or `OutOfCredit` (402). 404, 407, 408 and 429 stay
  `Unavailable`; 403 stays a rejection.
- `caption_failure` maps the new class to the existing `Backend` arm:
  transient, the pass stops, the remaining items cascade as skipped. Only
  the reason changes. A credentials failure is never written to the
  ledger.
- `capability.rs` gains `OPENROUTER_KEY_REJECTED_REASON` and
  `OPENROUTER_OUT_OF_CREDIT_REASON` beside the missing-key reason, and the
  missing-key reason gains the Settings remedy. The named reasons are used
  only when the configured backend is OpenRouter; any other backend's 401
  keeps the server's text.
- `HttpDescriber::check_key` calls `{base_url}/v1/key`.
  `describer_config::test` runs it for OpenRouter when a key resolves, and
  `DescriberProbe` gains `key: KeyCheck` — `Accepted`, `Rejected`, or
  `NotChecked` (no key, or not OpenRouter). A transport failure on the key
  endpoint is `NotChecked` with a notice, not a failed Test. The CLI
  prints a `key:` line; MCP's `test_describer` carries the field.

### Wave 3 — The Keychain adapter and head resolution (`crates/secrets`, `crates/services`, CLI, MCP)

- **`crates/secrets`** (package `majestical-secrets`), a workspace member
  with no dependency on any other workspace crate:
  - `openrouter_key() -> Result<Option<String>, SecretError>`
  - `store_openrouter_key(&str) -> Result<(), SecretError>`
  - `delete_openrouter_key() -> Result<bool, SecretError>` (`false` when
    nothing was stored)
  - `SUPPORTED: bool`, true only on macOS.
  On macOS the three call `security-framework`'s generic-password
  functions for one item: service `majestical`, account
  `openrouter-api-key`. On every other target they return
  `SecretError::Unsupported`. Selection is `cfg(target_os)`, never a cargo
  feature. The item's service name is a private parameter of the inner
  functions, so the crate's tests use a throwaway name and delete it.
- **Scope of the item: per machine.** `describer.toml` is per catalog; the
  Keychain item is not. The env var is already per machine, and an
  OpenRouter account belongs to the person.
- **Resolution.** Each head computes `env, then Keychain` and passes the
  result through the parameters that exist today; the file fallback inside
  `effective_api_key` stays last. The Keychain is read only when the
  configured backend is OpenRouter, so a local-backend user never sees a
  macOS prompt. `DoctorRequest.describer_env_key` is renamed
  `describer_key`.
- **`key_source`.** `DescriberConfigView` gains `key_source`: `env`,
  `keychain`, `file`, or `none`, decided by one pure function in
  `describer_config` from the config and two booleans the head supplies
  (env present, Keychain present). `describer show`, the doctor
  `describer` row and the GUI render from it. The view's `api_key` marker
  is removed in its favor.
- **Writing.** Where `SUPPORTED`, `maj describer set --api-key K` and MCP
  `set_describer { api_key }` store K in the Keychain first and then write
  the file keyless. Elsewhere they write the file as today.
- **`set` keeps a stored key.** `SetArgs.api_key: None` means "leave the
  key alone": the Keychain is untouched, and a key in the existing file is
  carried into the rewritten file. Removal is `maj describer clear-key`
  and MCP `set_describer { clear_key: true }` (dry-run by default, naming
  the source it would clear). Both clear the Keychain item and the file
  field. They cannot clear the env var: when it is set, the outcome says
  that `MAJ_OPENROUTER_KEY` still supplies a key.
- **Parity.** `describer show` output changes, so the branch carries a
  temporary normalizer in `services_parity.rs`, deleted in the closing PR.

### Wave 4 — The desktop head (`apps/desktop/src-tauri`)

- **A cached key.** New managed state holds the head's resolved key. It is
  filled at startup and refilled after a save or a clear — never per tick,
  so a denied macOS prompt is not repeated every poll. `batch_request` and
  `doctor_report` take the cached value.
- **Four commands**, each a one-liner over an `*_impl`:
  `describer_settings` (the view, `keychain_supported`, notices),
  `save_describer` (backend, model, optional base URL, optional key),
  `clear_describer_key`, `test_describer`. After a save or a clear the
  command refreshes the cache and nudges `SchedulerWake`.
- **The key's path.** It crosses the IPC once, into `save_describer`, and
  is never returned. Errors on this path render the outermost context
  only.
- **Wire.** A new `api-captions.ts`; every new field gets a fixture on
  both sides; `tauri_parity` gains rows.

### Wave 5 — The Captions section (`apps/desktop/src/lib`)

`CaptionsSection.svelte`, between Health and Always-on:

- Backend select (Ollama, LM Studio, OpenRouter); base URL prefilled from
  the backend default and editable; model as free text.
- A password field for the key, shown only for OpenRouter and emptied
  after Save, with a status line from `key_source`: stored in the
  Keychain; supplied by `MAJ_OPENROUTER_KEY` and overriding the Keychain;
  in `describer.toml`; none.
- Save, Test, and Remove key (when the source is `keychain` or `file`).
  Test renders reachability, whether the model is listed, vision (LM
  Studio), and the key check.
- An `onchanged` callback; `SettingsView` re-runs its doctor load so the
  Health `describer` row follows a save.

The mockup's frames: unconfigured; Ollama configured; OpenRouter with a
Keychain key; env override; Test results (ok, model not listed, key
rejected, unreachable); a save error. Its strings are pinned byte-for-byte
in tests.

## Error handling

- A Keychain read failure is a notice, never a hard error: the head
  continues with env or file, and the notice names the failure and
  `MAJ_OPENROUTER_KEY` as the way around it.
- A Keychain write failure on `set` or Save is a hard error, and
  `describer.toml` is not written — the Keychain write comes first so the
  config and the key never disagree.
- `clear-key` with nothing stored is a no-op that says so.
- A credentials failure is a recorded transient row inside a successful
  run; `doctor` still returns `Ok`.
- A key-endpoint transport failure during Test is `NotChecked` with a
  notice.
- No error on the key's path renders more than its outermost context; a
  doctor row never echoes a secret.

## Testing

- **Pure functions, per branch.** `key_source`, the client's status
  mapping, `caption_failure`'s new arm, the reason selection by backend,
  and the section's status-line function. The ledger proptest extends to
  the new class: a credentials failure never reaches the ledger.
- **The adapter.** macOS-only tests store, read, overwrite and delete a
  throwaway item; a test on the stub asserts `Unsupported`. The plan's
  first adapter step confirms these run without a prompt on a GitHub
  macOS runner; if they cannot, they run only under an env gate and this
  spec's as-built section says so.
- **Heads.** Each head's resolution takes the env and Keychain readings as
  parameters, so no test touches the process env or a real keychain. CLI
  integration tests: `set` without a key keeps a file key; `clear-key`
  removes it; `describer show` names the source. MCP: the `clear_key` dry
  run names what it would clear and writes nothing.
- **Describe client.** A local HTTP stub answers 401, 402 and 200 on the
  chat and key endpoints.
- **Desktop.** `*_impl` tests for the four commands (the cache refreshes;
  the scheduler is nudged); wire fixtures regenerated; `tauri_parity`
  rows; `CaptionsSection` component tests with the pinned strings, the
  key field emptied after Save, and Remove key's visibility per source.
- **E2E.** One flow in `settings.e2e.ts`: choose Ollama, type a model,
  Save, see the Health `describer` row change. No key is stored; no
  network is needed.
- **State dir.** The guard test, and the before/after directory count on
  the PR.
- **`cargo-mutants`** at close over `crates/secrets`,
  `describer_config.rs`, `crates/describe/src/client.rs` and the new
  desktop module — foreground, one at a time, `--in-place` on a warm
  target, `git status` clean after each (standing mandate).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **Chunk 1**: this spec + the implementation plan + the mockup.
2. **Chunk 2**: the state-dir seam and its guard test.
3. **Chunk 3**: `CredentialsRejected`, the client mapping, the named
   reasons, `check_key` and `DescriberProbe.key` at CLI and MCP.
4. **Chunk 4**: `crates/secrets`, `key_source`, head resolution at CLI and
   MCP, `clear-key`, the `set` semantic, the temporary parity normalizer.
5. **Chunk 5**: the desktop cached key, the four commands,
   `api-captions.ts`, fixtures, `tauri_parity`.
6. **Chunk 6**: `CaptionsSection.svelte`, its tests, the e2e flow.
7. **Closing PR**: mutants triage, this spec's as-built section, watchlist
   updates, the normalizer's deletion, the handoff for 7H.

## Deferred (watchlist items with this spec's attribution)

- Cleanup of the state directories already leaked under the user's data
  dir (needs a safe way to tell a leaked directory from a real catalog's).
- A model picker fed by the backend's model list.
- A shared Keychain access group, so the CLI and the app read one item
  without a per-binary macOS prompt (needs both signed by one team).
- A secret store on Linux and Windows.
- Per-catalog keys (the Keychain item is per machine by decision).
- Wire-layer codegen, Windows/Linux artifacts, ledger keying finer than
  `(kind, asset)`, MCP progress notifications, CLI ingest progress
  rendering, the ingest queue, localization (carried again).

## As-built (phase 7G)

What shipped, where it differs from the design above. Written for the
state once this closing PR merges: main at #145 plus this PR. Six chunk
PRs squash-merged after green CI (#135, #136, #137, #138, #143, #145),
plus #134 (spec + plan + mockup), #139 (a mid-phase handoff), #144 (an
unplanned CI fix) and this closing one. Every task went through the same
loop: a fresh implementer subagent, an adversarial spec-compliance
reviewer that probed empirically and mutation-tested the claims, a
code-quality reviewer, and fix rounds until both approved. The plan has
eight chunks where the Delivery list above has seven: the Keychain work
became two chunks so each stayed at 1-2 tasks.

**Planning-time amendments** (recorded in the plan's header, decided
while reading the code for it):

- The adapter is a trait, not three free functions: `crates/secrets`
  exposes `trait KeyStore` with `SystemKeyStore` (real) and
  `MemoryKeyStore` (the test double both heads' tests use), so head tests
  drive the write path without a real Keychain.
- `PortFailure::CredentialsRejected` carries which problem it was
  (`CredentialsProblem::{KeyRejected, OutOfCredit}`), because services
  picks the named reason from it.
- `DoctorRequest.describer_env_key: Option<String>` became
  `describer_key: KeyPresence`: the row only ever needed presence, and it
  now names the source.
- `describer_config::set` returns `()`; the head calls `show` for the
  echo, because the view depends on the head's key presence.
- The state-dir leak had a second source the `cfg(test)` seam cannot
  reach: unit tests in `crates/cli/src` and `apps/desktop/src-tauri/src`
  link the non-test build of services. Task 1 measured each suite and
  added a justfile-level `MAJ_STATE_DIR` for the `test` recipes.
- `CaptionsSection` imports `captionsApi` directly, not through the `api`
  object (`api.ts` was at 559/560).
- `App.test.ts` was at a refused cap, so Task 9 performed the split its
  cap comment promised (`src/App.settings.test.ts`).
- The desktop key cache also refills in `adopt_catalog` (a catalog switch
  changes whether the backend is OpenRouter).
- Eight PR chunks, not seven (above).
- `KeyPresence` is a three-variant enum (`Env`, `Keychain`, `Absent`),
  not two booleans: "both true" can never be produced because the head's
  resolver stops at env. It is `serde(skip)`, never on the wire.
  `KeySource`'s absent variant is `Absent`, serialized `none`.
- Task 5b closed a key leak that predates the phase (below, #137).
- `KeyCheck` has a fourth state, `missing`: OpenRouter with no effective
  key. The GUI line reuses the approved "No key" string, so the mockup
  gained no new wording.

**PR #134 — spec + plan + mockup** (docs). This spec, the plan
`docs/superpowers/plans/2026-09-18-phase7g-captions-parity.md`, and the
mockup `docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`,
approved by the user before Task 9.

**PR #135 — the state-dir leak stops at its source** (chunk 2, Task 1).
`state_dir::state_base()` falls back to `default_base()`, whose
`cfg(test)` body returns a directory under the system temp dir; a guard
test asserts that fallback is never under the platform data dir. `just
test`, `just gui-test` and the desktop `cargo test` CI step set
`MAJ_STATE_DIR`. Measured: `cargo test -p majestical-services --lib`
went from +90 directories per run to 0; the CLI `--bins` suite, the
desktop `--lib` suite, `just test` and `just gui-test` measured 0. The
directories already leaked (about 42,389) are not removed: their names
are one-way hashes (watchlist).

**PR #136 — named key failures** (chunk 3, Tasks 2 and 3). As designed:
a 401/402 is `CredentialsRejected`, transient, aborts the caption pass,
never reaches the ledger, and carries a named reason for OpenRouter (a
local backend's 401 keeps the server's text). `describer test` checks
the key through `GET {base_url}/v1/key`; `DescriberProbe.key` is
`accepted`, `rejected`, `missing` or `not_checked`. AMENDED: the
`missing` state (above), so `describer test` no longer promises caption
work for OpenRouter with no key. One equivalent mutant is known: the arm
order in `post_chat`, because 401 and 402 deliberately stay in
`NOT_ABOUT_THE_PAYLOAD` so `is_client_rejection` is truthful on its own.

**PR #137 — `crates/secrets`, key-source views, config errors that never
quote the file** (chunk 4, Tasks 4, 5, 5b). No head touched the Keychain
yet. `resolve(env, wants_keychain, &store)` reads the store only when env
is absent and the configured backend is OpenRouter; a read failure is a
notice. `ResolvedKey` has a redacting `Debug` and never derives
`Serialize`; a non-UTF-8 stored value is a fixed error.
`DescriberConfigView` carries `key_source` in place of the old redacted
`api_key` marker. AMENDED behaviour: `describer set` without a key now
keeps the stored key (it used to drop it silently). **Task 5b, a leak
that predates the phase**: a parse error on `describer.toml` quoted the
offending line, which can be the `api_key` line, through `describer
show|test`, the index-status notice and four MCP tools.
`ConfigError::Parse` now carries a line number and a message and no toml
source; a type error is one fixed sentence; a wrong `backend` value gets
fixed text. `DescriberConfig::store` is atomic (0600 temp file, sync,
rename) and replaces a symlinked `describer.toml` with a regular file.

**PR #138 — the Keychain at the CLI and MCP heads; `describer clear-key`**
(chunk 5, Task 6). `crates/cli/src/describer_key.rs` is the heads' one
key path. MCP gained a separate `clear_describer_key { confirm }` tool,
because `set_describer` requires a backend and a model. `index run`
resolves the key once above the `--watch` loop and only when the
requested kinds include caption work. Credential fixes found in review,
each proven on the wire: only an OpenRouter key goes to the Keychain (a
local backend's token had been moved where nothing read it); a stored key
never follows a backend switch (`carried_key`, dropped with a notice
naming both backends); a host move warns, including for a Keychain key
with a keyless file; the dry run names the destination read off
`plan_key_write`; notices raised in services name no head. The test seam:
`SystemKeyStore` takes a service name, `MAJ_KEYCHAIN_SERVICE` overrides
it at a head, every CLI test child gets a throwaway service through
`common::maj_bin()`, `keychain_guard.rs` fails a test file that spawns
the binary another way, and both cleanup guards refuse a non-throwaway
name. No parity normalizer was needed: every parity row uses Ollama with
no key.

**PR #139 — mid-phase handoff** (docs). Resumed the phase at chunk 6;
superseded and deleted by this closing PR, its still-true content carried
into `docs/superpowers/HANDOFF-phase7H.md` and the watchlist.

**PR #143 — the desktop head and its wire** (chunk 6, Tasks 7 and 8).
`apps/desktop/src-tauri/src/captions.rs`: `DescriberKeyCache` (managed
state; a poisoned lock is recovered), four one-line commands over
`*_impl`s, the key resolved once and refilled at startup, on
`adopt_catalog`, and after a save or clear; `save_describer` writes the
Keychain first and stops if refused. Task 7 amendments, all reviewed and
approved:

1. `CaptionDeps` is `{ keys: KeyRefresh, wake }`, not the plan's flat
   four-field struct: `adopt_catalog` also refills the cache, and the
   flat shape would have taken it to seven parameters. It is at five.
2. `DescriberConfigView` (services) derives `Debug`, required because
   `DescriberSettingsOutcome` derives it and `expect_err` needs it. The
   view has no key field by construction.
3. `SystemKeyStore::service_name() -> &str`, macOS-only, so a test proves
   `system_store()` honoured `MAJ_KEYCHAIN_SERVICE` without a store call.
4. A blank `base_url` is treated as absent: a Settings text input sends
   `""`, which `set` would otherwise store verbatim.
5. `lib.rs`'s setup closure became a named `setup_app` (`run()` reached
   103 lines).
6. A refused Keychain delete keeps the store's own message (`{err}`), as
   the CLI does; only the write message is fixed by mockup frame 8.
7. `commands::key_presence()` was deleted outright;
   `DescriberKeyCache::presence()` replaces it.

The Task 7 code-quality review's fixes (`821497f`): fmt (CI would have
failed), a non-discriminating indexer test deleted, the cache's field
made private, `env_api_key` moved private into `captions.rs`, stale docs,
rationale deduplicated. Three adversarial spec-review rounds rejected
missing or defeatable guards, never the behaviour; the result is the
private `mod guarded` in `tauri_parity.rs`, which makes an unguarded
`maj` spawn a compile error (mutation 34b, deleting the guard inside the
module, is a documented irreducible limit). Three reviewer write-ups
were never delivered (watchlist). Task 8 deviations: `maj describer
show` has no `--json`, so the `describer_settings_matches_cli_show` row
parses its text lines (`cli_stdout` split from `cli_json`);
`wire_fixtures.rs` gained `check_or_update_with_nulls(name, value,
&[json pointers])`, where each named pointer must be present and null and
every other field populated; the parity row uses `PanickingKeyStore` and
a fresh catalog (not `seeded_cfg`), proving an Ollama catalog never reads
the store; the fixture's backend string is derived from
`BackendKind::as_str`. Task 8's spec review ran 25 mutants, 22 killed,
the survivors two equivalents and the TS-only-field case (watchlist).

**PR #144 — CI under Rust 1.99 and PyAV 19** (unplanned, between chunks
6 and 7). Main broke with no commit: CI installs current stable Rust and
resolves faster-whisper's `av` unbounded. Rust 1.99's clippy
`assert_is_empty` failed `-D warnings` on 64 asserts across both
workspaces (rewritten to print the value or name what was empty), and
two `#[expect]`s no longer fulfilled were removed (`float_cmp` in
`crates/describe/src/client.rs`; `exit` on `tauri::generate_context!` in
the desktop `lib.rs`). PyAV 19.0.0 removed the `metadata_errors` keyword
faster-whisper 1.2.1 passes; `conformance/whisper/golden.py` pins
`av==18.1.0`. No test logic changed.

**PR #145 — the Captions section, end to end** (chunk 7, Tasks 9 and
10). `CaptionsSection.svelte` and `captions-status.ts`, mounted between
Health and Always-on, strings and layout per the approved mockup. Task 9
deviations: strings beyond the plan taken from the mockup (the subtitle
as a `SECTION_SUBTITLE` constant, the Model placeholder, "API key", the
button labels); the plan's `config.rs:18` reference corrected to `:37`;
a `modelLine()` helper (oxlint `unicorn/no-immediate-mutation`). Caps:
`App.test.ts` `max-lines` 385 → 320 (split out, while `IngestView.test.ts`
stays at 385), its `import/max-dependencies` override removed, the
12-import override moved to `App.settings.test.ts`; no cap raised. Tests
split into `CaptionsSection.test.ts`, `CaptionsSection.actions.test.ts`
and `captions-test-support.ts`. Spec review round 1 was REJECTED (42
mutants, 21 survived) for one behaviour bug — a key typed under
OpenRouter, then the backend switched away, kept Test disabled and
reappeared on switching back; the typed key is now cleared when the form
leaves OpenRouter — and a set of test gaps; round 2 approved with 15 of
the 21 killed and the rest equivalent or nits. The code-quality review
found the UI promising to keep a file key stored for a local backend
when the form switched to OpenRouter, which Save drops (`carried_key`):
`formKeySource` is the saved `key_source` only when the saved backend
equals the form's, else `none`. Remove key clears stale Test results and
"Saved.". `keychain_supported` is unused by the UI; its Rust doc no
longer promises UI behaviour. Task 10 deviations: the Captions
`describe` stays in `settings.e2e.ts` (only `ingest.e2e.ts` runs after
it, and it asserts nothing scheduler-related; the plan's "four specs that
run after" was stale). The e2e Keychain exposure is closed:
`wdio.conf.ts`'s `isolateKeychain()` sets `MAJ_KEYCHAIN_SERVICE` to
`majestical-test-e2e-<pid>-<uuid>` and deletes `MAJ_OPENROUTER_KEY` from
`process.env` before anything spawns (`@wdio/tauri-service` 1.3.0 spawns
the app with `{...process.env, ...options.env}`). Review found that
WDIO's `runLauncherHook` (`@wdio/cli` 9.31.3) aborts only on
`SevereServiceError` — a plain `Error` from the config's `onPrepare` is
logged and the app launches anyway — so neither the new guard nor the
pre-existing spec-list check could stop a launch. `onPrepare` now wraps
its work and rethrows as `SevereServiceError` (cause kept), isolation
runs first, and a backstop `assertKeychainIsolated()` runs on
`process.env` before the fixture `maj` children and on the merged
capability env right before the service spawns the app. A `waitUntil`
`.catch` rethrows with the last-seen detail, because WebdriverIO reads
`timeoutMsg` at wait start. Locally the Settings spec passed 5/5 and the
full suite 7/7 spec files; the debug bundle plus `cargo build -p
majestical-cli` took 505 s on a warm desktop target. **Task 10's hand
check is the user's and is outstanding**: save an OpenRouter key in
Settings; Keychain Access shows `majestical` / `openrouter-api-key`;
`maj describer show` says `(from keychain)` after one macOS allow prompt;
Remove key deletes the item.

**This closing PR** (chunk 8, Tasks 11 and 12). cargo-mutants over the
files this phase changed, with `system_store` excluded from the
automated runs and hand-checked, because `SystemKeyStore::default()`
names the real Keychain item; three test commits close the survivors
that were real gaps (`PanickingKeyStore`'s own behaviour, the CLI
`system_store` override, the desktop `env_api_key` and
`SaveDescriberReq`'s `Debug`). The triage is in the watchlist's
"cargo-mutants triage (phase 7G)". No temporary parity normalizer was
added this phase, so none was deleted; `services_parity` (56) and
`tauri_parity` (11) pass with no SKIP against a reference rebuilt at
`064c40f`. Also: the phase 7G deferrals, this section,
`docs/superpowers/HANDOFF-phase7H.md`, and the deletion of the mid-phase
handoff. `docs/RELEASING.md` is unchanged: the Keychain adds no release
step.
