# Phase 7G — Captions Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the GUI a Captions settings section at parity with
`maj describer set|show|test`, move the OpenRouter key into the macOS
Keychain at all three heads, name a rejected key and an empty account
instead of showing the server's text, and stop unit tests leaking state
directories into the user's data dir.

**Architecture:** A new head-side crate, `crates/secrets`, is the only code
that touches the Keychain; the heads resolve `env, then Keychain` through
it and pass the key into `crates/services` through the parameters that
already exist, so services and `crates/describe` stay free of ambient
reads. Services owns the pure decisions (`key_source`, `plan_key_write`,
the credentials reasons); the heads execute them. `PortFailure` gains a
third class so a 401/402 is transient, aborts the pass like an outage, and
carries a named reason. The desktop caches the resolved key in managed
state and exposes four commands the new `CaptionsSection.svelte` calls
directly through `api-captions.ts`.

**Tech Stack:** Rust (workspace + standalone `src-tauri` workspace),
`security-framework` (macOS only), `httpmock`, proptest, Tauri 2, Svelte 5,
vitest, WebdriverIO e2e.

Spec: `docs/superpowers/specs/2026-09-18-phase7g-captions-parity-design.md`.
Mockup: `docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`
— **must be approved by the user before Task 9 starts.**

---

## Standing mandates (from the handoff — every task inherits these)

1. NO Claude-Session trailers in commit messages. Plain git — do NOT use the
   `submitting-changes` skill.
2. Shared checkout: stage ONLY your files, never `git add -A`. Reviewers
   (who mutate files empirically) never run concurrently with implementers;
   nobody switches branches while either is running.
3. Shell variables do NOT persist across Bash invocations. `trash`, never
   the recursive-force remove — a hook blocks any Bash command containing
   that pattern, including heredoc text, so the justfile is edited with the
   Edit tool.
4. Push/pull via
   `git -c credential.helper='!gh auth git-credential' <cmd> https://github.com/statik/majestical.git ...`
5. Zero warnings. Verify the current stable version of every new dependency
   at execution time — never from memory.
6. `cargo-mutants` runs FOREGROUND, one at a time, `--in-place` on a warm
   target, `git status` clean after each. If a run dies, check
   `git status` before anything else: a mutant may be left in the tree.
7. Subagents report through their final message; a build a subagent must
   wait on runs in the foreground with a 600 s timeout.
8. Fetch/pull `main` explicitly before rebasing; baseline diffs on local
   `main` (`origin/main` goes stale in this SSH-less checkout).
9. Services never print and never read the environment or any other
   ambient store; `crates/describe` has the same rule. The heads read and
   pass in.
10. MCP mutating tools default to dry-run; a dry run describes real state
    and never writes. Tauri commands stay one-liners over `*_impl`. Every
    new command outcome gets a wire fixture on BOTH sides
    (`MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` in
    `apps/desktop/src-tauri`, then `pnpm test` in `apps/desktop`).
11. Local GUI verification is `pnpm check && pnpm lint && pnpm test &&
    pnpm build` in `apps/desktop`; `pnpm check && pnpm lint` in
    `apps/desktop/e2e` too when e2e files change.
12. Gate a commit on its verification with `&&`, never a pipe.
13. Changing an existing verb's output needs a TEMPORARY parity normalizer
    in `crates/cli/tests/services_parity.rs` (exact-substring removal on
    both binaries, gated on the clean-fixture value, unit-tested, deletion
    scheduled in Task 12).
14. Never `gh pr merge --auto`. Watch CI to green (including `gui-e2e`,
    by convention), then squash-merge explicitly.
15. **A key never appears in test output, a notice, an error, a fixture or
    a log.** Tests use the literal `sk-test`. Errors on the key's path
    render `{err}` (outermost context), never `{err:#}`.

## Planning-time amendments to the spec

Found while reading the code for this plan. Each is recorded again in the
spec's as-built section at close.

- **The adapter is a trait, not three free functions.** Head tests must
  drive the write path without a real Keychain, so `crates/secrets` exposes
  `trait KeyStore` with `SystemKeyStore` (real) and `MemoryKeyStore` (the
  test double both heads' tests use).
- **`PortFailure::CredentialsRejected` carries which problem it was**
  (`CredentialsProblem::{KeyRejected, OutOfCredit}`), because services
  picks the named reason from it.
- **`DoctorRequest.describer_env_key: Option<String>` becomes
  `describer_key: KeyPresence`**, not a renamed `Option<String>`: the row
  only ever needed presence, and it now names the source.
- **`describer_config::set` returns `()`**; the head calls `show` for the
  echo, because the view now depends on the head's key presence.
- **The state-dir leak has a second source the `cfg(test)` seam cannot
  reach**: unit tests in `crates/cli/src` and `apps/desktop/src-tauri/src`
  link the non-test build of services. Task 1 measures each suite and adds
  a justfile-level `MAJ_STATE_DIR` for the `test` recipes.
- **`CaptionsSection` imports `captionsApi` directly**, not through the
  `api` object: `api.ts` is at 559/560 and a spread would cost two lines.
- **`App.test.ts` is at a refused cap**, and the new section's mount needs
  a `describer_settings` mock in every Settings-mounting test, so Task 9
  performs the split its cap comment promises (`src/App.settings.test.ts`).
- **The desktop key cache also refills in `adopt_catalog`** (a catalog
  switch changes whether the backend is OpenRouter).
- **Eight PR chunks, not seven**: the Keychain work is two chunks so each
  stays at 1-2 tasks.
- **`KeyCheck` has a fourth state, `missing`** (added in chunk 3's quality
  review): OpenRouter with no effective key. `describer test` used to
  promise caption work in that case and the next `index run` failed every
  item. The wire type in Task 8 and `testLines` in Task 9 carry it; the GUI
  line reuses the approved "No key" status string, so the mockup gains no
  new wording.

## File structure (created/modified across the phase)

```
crates/services/src/state_dir.rs                    MOD  cfg(test) fallback base + guard test
justfile                                            MOD  MAJ_STATE_DIR for `test` / `gui-test`

crates/core/src/ports.rs                            MOD  CredentialsProblem; PortFailure::CredentialsRejected; PortError::credentials
crates/describe/src/client.rs                       MOD  DescribeHttpError::Credentials; 401/402 mapping; check_key + KeyVerdict
crates/describe/src/lib.rs                          MOD  re-export KeyVerdict
crates/services/src/capability.rs                   MOD  two new reasons; missing-key reason names Settings
crates/services/src/index/run.rs                    MOD  caption_failure's third arm; credentials_reason; proptest arm
crates/services/src/describer_config.rs             MOD  KeyCheck on DescriberProbe; KeySource/KeyPresence/key_source;
                                                         FileKey/plan_key_write; clear_file_key; ClearKeyOutcome; wants_keychain
crates/services/src/doctor.rs                       MOD  DoctorRequest.describer_key: KeyPresence; row names the source

crates/secrets/Cargo.toml                           NEW  majestical-secrets
crates/secrets/src/lib.rs                           NEW  SecretError, KeyStore, MemoryKeyStore, resolve, ResolvedKey
crates/secrets/src/system.rs                        NEW  SystemKeyStore (macOS impl + stub), SUPPORTED
Cargo.toml                                          MOD  workspace member

crates/cli/Cargo.toml                               MOD  majestical-secrets
crates/cli/src/describer_key.rs                     NEW  the head's resolve / store / clear over a &dyn KeyStore
crates/cli/src/describer_cmd.rs                     MOD  set/show/test/clear-key rendering
crates/cli/src/main.rs                              MOD  DescriberCmd::ClearKey
crates/cli/src/index_cmd.rs, commands.rs            MOD  resolved key instead of env_api_key
crates/cli/src/mcp_cmd/write_tools.rs               MOD  set_describer {api_key, clear_key}; test_describer key field
crates/cli/src/mcp_cmd/read_tools.rs                MOD  get_describer + doctor use the resolved key
crates/cli/tests/describer_key.rs                   NEW  set keeps a file key; clear-key; show names the source
crates/cli/tests/services_parity.rs                 MOD  TEMPORARY normalizer(s)

apps/desktop/src-tauri/Cargo.toml                   MOD  majestical-secrets
apps/desktop/src-tauri/src/captions.rs              NEW  DescriberKeyCache, four *_impl + four commands
apps/desktop/src-tauri/src/lib.rs                   MOD  manage the cache; register commands; fill at setup
apps/desktop/src-tauri/src/commands.rs              MOD  adopt_catalog refills; doctor takes KeyPresence; env_api_key stays
apps/desktop/src-tauri/src/indexer.rs               MOD  batch key from the cache
apps/desktop/src-tauri/tests/wire_fixtures.rs       MOD  three fixtures
apps/desktop/src-tauri/tests/tauri_parity.rs        MOD  describer_settings row

apps/desktop/src/lib/api-captions.ts                NEW  wire types + captionsApi
apps/desktop/src/lib/captions-status.ts             NEW  pure: keyStatusLine, removeKeyVisible, testLines
apps/desktop/src/lib/CaptionsSection.svelte         NEW
apps/desktop/src/lib/SettingsView.svelte            MOD  mounts the section; onchanged → load()
apps/desktop/src/app.css                            MOD  .captions-*
apps/desktop/src/lib/fixtures/describer_*.json      NEW  three fixtures
apps/desktop/src/lib/fixtures.captions.test.ts      NEW
apps/desktop/src/lib/captions-status.test.ts        NEW
apps/desktop/src/lib/CaptionsSection.test.ts        NEW
apps/desktop/src/App.settings.test.ts               NEW  the promised split out of App.test.ts
apps/desktop/.oxlintrc.json                         MOD  lower App.test.ts caps after the split
apps/desktop/e2e/specs/settings.e2e.ts              MOD  the Ollama save flow
```

---

## PR Chunk 2 — stop the state-dir leak

### Task 1: the `cfg(test)` base, the justfile env, and the measurement

**Files:**
- Modify: `crates/services/src/state_dir.rs` (`state_base`, tests)
- Modify: `justfile` (the `test` recipe at line 5 and the `gui-test`
  recipe around line 30) — with the Edit tool, mandate 3.

- [ ] **Step 1: Measure before.** Record the number for the PR body:

```bash
/bin/ls ~/Library/Application\ Support/majestical/catalogs | wc -l
cargo test -p majestical-services --lib >/dev/null 2>&1
/bin/ls ~/Library/Application\ Support/majestical/catalogs | wc -l
cargo test -p majestical-cli --lib >/dev/null 2>&1
/bin/ls ~/Library/Application\ Support/majestical/catalogs | wc -l
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --lib >/dev/null 2>&1
/bin/ls ~/Library/Application\ Support/majestical/catalogs | wc -l
```

  The three deltas say which suites leak and how much.

- [ ] **Step 2: Failing guard test** in `state_dir.rs`'s `mod tests`:

```rust
#[test]
fn the_test_build_base_is_never_the_platform_data_dir() {
    // Only meaningful when the override is absent, which is how every
    // unit test in this crate runs.
    if std::env::var_os("MAJ_STATE_DIR").is_some() {
        return;
    }
    let base = state_base().expect("base");
    let data = dirs::data_dir().expect("data dir");
    assert!(
        !base.starts_with(&data),
        "test builds must not write under {}",
        data.display()
    );
    assert!(base.starts_with(std::env::temp_dir()));
}
```

- [ ] **Step 3: Run** `cargo test -p majestical-services --lib the_test_build_base` — fails.
- [ ] **Step 4: Implement.** Split the fallback out so the two builds differ
  in one place:

```rust
fn state_base() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MAJ_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    default_base()
}

/// Where state lives when `MAJ_STATE_DIR` is unset: the platform data dir.
#[cfg(not(test))]
fn default_base() -> Result<PathBuf> {
    let data =
        dirs::data_dir().context("no platform data directory; set MAJ_STATE_DIR explicitly")?;
    Ok(data.join("majestical"))
}

/// This crate's own unit tests open catalogs without setting
/// `MAJ_STATE_DIR` (it is process-global, and they run in parallel), so
/// the test build falls back to the system temp dir instead of the user's
/// real data dir — which is where tens of thousands of directories leaked
/// before phase 7G.
#[cfg(test)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature as the non-test build's fallible fallback"
)]
fn default_base() -> Result<PathBuf> {
    Ok(std::env::temp_dir().join("majestical-test-state"))
}
```

  If clippy does not raise `unnecessary_wraps` here, delete the `expect`
  (an unfulfilled `expect` is itself a warning).

- [ ] **Step 5: The justfile.** In the `test` recipe, replace
  `cargo test --workspace` with
  `MAJ_STATE_DIR="$(mktemp -d)" cargo test --workspace`, and do the same to
  the `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml` line in
  `gui-test`. CI runs `just test`, so CI is covered; the GUI CI job calls
  `cargo test --manifest-path …` directly at `.github/workflows/ci.yml:231`
  — give that step `env: MAJ_STATE_DIR: ${{ runner.temp }}/maj-state`.
  Run `actionlint .github/workflows/ && zizmor .github/workflows/`.
  Suites that set the variable themselves (the CLI integration tests per
  child, `tests/commands.rs` under `ENV_LOCK`) keep doing so and win.
- [ ] **Step 6: Measure after**, same commands as Step 1 but through
  `just test` and `just gui-test`. The services delta must be 0. Record
  all numbers in the PR body. A non-zero delta under a bare
  `cargo test -p majestical-cli --lib` is expected and goes on the
  watchlist in Task 12 with its measured size.
- [ ] **Step 7: Run** `cargo test -p majestical-services --lib && just check`.
- [ ] **Step 8: Commit**:
  `git commit -m "fix: unit tests stop leaking state dirs into the user's data dir"`

---

## PR Chunk 3 — named key failures

### Task 2: the credentials class, the client mapping, `check_key`

**Files:**
- Modify: `crates/core/src/ports.rs`
- Modify: `crates/describe/src/client.rs`, `crates/describe/src/lib.rs`

The core additions:

```rust
/// What a port's credentials failure was about. Both are the operator's to
/// fix and say nothing about the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialsProblem {
    /// The port did not accept the caller's key (HTTP 401).
    KeyRejected,
    /// The key is fine and the account behind it cannot pay (HTTP 402).
    OutOfCredit,
}

pub enum PortFailure {
    Unavailable,
    RefusedInput,
    /// The port answered, and the problem is the caller's key or account —
    /// not this input, and not an outage. Transient like `Unavailable`
    /// (the same input succeeds once the operator fixes it), but nameable.
    CredentialsRejected(CredentialsProblem),
}

impl PortError {
    #[must_use]
    pub fn credentials(
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
        problem: CredentialsProblem,
    ) -> Self {
        Self {
            context: context.into(),
            source: Box::new(source),
            failure: PortFailure::CredentialsRejected(problem),
        }
    }
}
```

The client additions:

```rust
#[error("backend did not accept the credentials for {url} (HTTP {status})")]
Credentials { url: String, status: u16, problem: CredentialsProblem },

fn credentials_problem(status: u16) -> Option<CredentialsProblem> {
    match status {
        401 => Some(CredentialsProblem::KeyRejected),
        402 => Some(CredentialsProblem::OutOfCredit),
        _ => None,
    }
}

/// What the backend's key endpoint said about the configured key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyVerdict { Accepted, Rejected }
```

`post_chat`'s `map_err` gains an arm BEFORE the `is_client_rejection` arm:
`ureq::Error::StatusCode(status)` with `credentials_problem(status)` being
`Some(problem)` → `DescribeHttpError::Credentials`. `to_port_error` maps it
to `PortError::credentials(context, error, problem)` (bind `problem` before
moving `error`). `NOT_ABOUT_THE_PAYLOAD` is unchanged — 401/402 are still
not payload rejections — but its doc comment now says 401/402 are taken by
the credentials arm first.

`check_key`, beside `probe`:

```rust
/// Asks the backend's key endpoint whether the configured key is accepted.
/// OpenRouter-specific (`GET {base_url}/v1/key`); callers gate on the
/// backend.
///
/// # Errors
/// Returns `PortError` when the endpoint cannot be reached or answers
/// anything other than success or 401 — the key was not judged.
pub fn check_key(&self) -> Result<KeyVerdict, PortError> {
    let url = format!("{}/v1/key", self.config.base_url.trim_end_matches('/'));
    match self.authorize(self.agent.get(&url)).call() {
        Ok(_) => Ok(KeyVerdict::Accepted),
        Err(ureq::Error::StatusCode(401)) => Ok(KeyVerdict::Rejected),
        Err(other) => Err(PortError::new(
            "key check",
            DescribeHttpError::Request { url, message: other.to_string() },
        )),
    }
}
```

- [ ] **Step 1: Failing tests** in `client.rs`'s `mod tests`, using the
  existing `caption_failure_for_status` helper and `MockServer`:
  - `a_401_is_a_rejected_key_and_a_402_is_an_empty_account` — asserts
    `PortFailure::CredentialsRejected(CredentialsProblem::KeyRejected)` for
    401 and `…(OutOfCredit)` for 402.
  - Edit `credential_account_routing_and_timing_4xx_are_unavailable`
    (line 626): its loop drops 401 and 402 and keeps 404/407/408/429, and
    it is renamed `routing_and_timing_4xx_are_unavailable`.
  - `check_key_accepts_on_200`, `check_key_rejects_on_401`,
    `check_key_is_an_error_on_500_and_on_a_dead_port` — mock
    `GET /v1/key`; the 200 case also asserts the
    `Authorization: Bearer sk-test` header was sent.
  - `a_credentials_error_never_renders_the_key` — 401 with key `sk-test`;
    assert `!error.to_string().contains("sk-test")`.
- [ ] **Step 2: Run** `cargo test -p majestical-describe` — the new tests fail to compile.
- [ ] **Step 3: Implement** core, then the client. Re-export `KeyVerdict`
  from `crates/describe/src/lib.rs` beside `HttpDescriber`. The workspace
  will not build yet: `crates/services/src/index/run.rs:1606` matches
  `PortFailure` exhaustively. Add the arm there as
  `PortFailure::CredentialsRejected(_) => CaptionFailure::Backend(error.to_string())`
  — Task 3 replaces it with the named reason.
- [ ] **Step 4: Run** `cargo test -p majestical-core -p majestical-describe -p majestical-services --lib && just check`.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: a describer's 401/402 is its own port failure class; check_key"`

### Task 3: the named reasons, the caption pass, and Test's key check at CLI and MCP

**Files:**
- Modify: `crates/services/src/capability.rs`
- Modify: `crates/services/src/index/run.rs` (`caption_failure`, its caller, tests, the proptest)
- Modify: `crates/services/src/describer_config.rs` (`DescriberProbe`, `test_impl`)
- Modify: `crates/cli/src/describer_cmd.rs` (`cmd_test`)
- Modify: `crates/cli/src/mcp_cmd/write_tools.rs` (`test_describer_result`)

The reasons (exact text — the GUI and the tray show these):

```rust
pub const OPENROUTER_KEY_MISSING_REASON: &str = "OpenRouter needs an API key — save one in \
    Settings → Captions, or set it with `maj describer set --api-key` or MAJ_OPENROUTER_KEY";

/// Recorded when OpenRouter answers 401. Transient: the operator replaces
/// the key and the same items succeed, so the ledger never remembers it.
pub const OPENROUTER_KEY_REJECTED_REASON: &str = "OpenRouter rejected the API key (HTTP 401) — \
    save a new one in Settings → Captions or with `maj describer set --api-key`";

/// Recorded when OpenRouter answers 402. Transient for the same reason.
pub const OPENROUTER_OUT_OF_CREDIT_REASON: &str = "OpenRouter reports the account is out of \
    credit (HTTP 402) — add credit at openrouter.ai and captions resume on their own";
```

The existing test that the missing-key reason contains `OPENROUTER_KEY_ENV`
still holds.

The caption pass:

```rust
fn caption_failure(error: &PortError, backend: BackendKind) -> CaptionFailure {
    match error.failure {
        PortFailure::Unavailable => CaptionFailure::Backend(error.to_string()),
        PortFailure::RefusedInput => CaptionFailure::Item(error.to_string()),
        PortFailure::CredentialsRejected(problem) => {
            CaptionFailure::Backend(credentials_reason(problem, backend, error))
        }
    }
}

/// The named reason applies to OpenRouter only: a 401 from a local LM
/// Studio is not about an OpenRouter key, so it keeps the server's text.
fn credentials_reason(
    problem: CredentialsProblem,
    backend: BackendKind,
    error: &PortError,
) -> String {
    match (backend, problem) {
        (BackendKind::OpenRouter, CredentialsProblem::KeyRejected) => {
            OPENROUTER_KEY_REJECTED_REASON.to_string()
        }
        (BackendKind::OpenRouter, CredentialsProblem::OutOfCredit) => {
            OPENROUTER_OUT_OF_CREDIT_REASON.to_string()
        }
        (BackendKind::Ollama | BackendKind::LmStudio, _) => error.to_string(),
    }
}
```

`run_caption_items` captures `let backend = config.backend;` before
`HttpDescriber::new(config, …)` consumes the config, and threads it to the
one `caption_failure` call site inside `caption_one_item` (add a field to
whatever that function already takes from the pass — do not add a sixth
positional parameter; if it is at five, put `backend` on `PassEnv`'s
sibling struct or pass `&describer` and add
`HttpDescriber::backend(&self) -> BackendKind`. The accessor is the smaller
change).

The probe:

```rust
/// What `describer test` learned about the key. `NotChecked` covers a
/// backend with no key endpoint, no key to check, and a key endpoint that
/// could not be reached (a notice says which).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCheck { Accepted, Rejected, NotChecked }

pub struct DescriberProbe {
    pub model: String,
    pub model_listed: bool,
    pub vision: Option<bool>,
    pub key: KeyCheck,
}
```

In `test_impl`, after a successful `probe()`: if the backend is OpenRouter
AND `config.effective_api_key(api_key.clone()).is_some()` (compute this
BEFORE `HttpDescriber::new` consumes both), call `check_key()`:
`Ok(Accepted|Rejected)` map across; `Err(e)` → `NotChecked` and
`notices.push(format!("note: the key was not checked ({e})"))`. Otherwise
`NotChecked` with no notice.

CLI `cmd_test` prints one line after the vision line, only when not
`NotChecked`: `key: accepted` or
`key: REJECTED — OpenRouter answered 401; set a new key`. The closing
"caption … work will run" line additionally requires
`probe.key != KeyCheck::Rejected`. MCP's `test_describer` result carries
the field through serde with no extra code — assert it in its test.

- [ ] **Step 1: Failing tests.**
  - `run.rs`: `a_401_from_openrouter_aborts_the_pass_with_the_named_reason`
    — `openrouter_catalog(dir, server.base_url(), &notices, Some("sk-test"))`,
    `MockServer` answers `POST /v1/chat/completions` with 401, two items →
    row 0 `error == OPENROUTER_KEY_REJECTED_REASON`, row 1
    `error == DESCRIBER_SKIPPED_REASON`, both `transient == true`, and the
    mock was hit exactly once. A 402 twin asserts the out-of-credit reason.
  - `run.rs`: `a_401_from_a_local_backend_keeps_the_servers_text` — unit
    test on `credentials_reason` with `BackendKind::LmStudio`.
  - The ledger proptest (the one that asserts no transient failure reaches
    the ledger): extend its failure generator with the credentials arm so
    a generated run outcome can contain credentials rows; the property is
    unchanged.
  - `describer_config.rs`: `test_reports_an_accepted_key`,
    `test_reports_a_rejected_key`,
    `test_does_not_check_a_key_for_ollama` (mock has no `/v1/key` route and
    is asserted never hit), `test_with_an_unreachable_key_endpoint_is_not_checked_with_a_notice`
    (mock 500 on `/v1/key`; assert the notice text and that it does not
    contain `sk-test`). These need a config on disk: use `set` against a
    tempdir root, as `show_of_an_unconfigured_catalog_is_none` does.
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib caption a_401 test_reports` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Parity.** Build the reference binary at the merge-base per
  the header of `services_parity.rs` and run it. The missing-key reason
  changed; if any row differs, add a TEMPORARY normalizer for the old and
  new substrings (mandate 13) with its two unit tests. If no row differs
  (the clean fixture has no describer), add nothing and say so in the PR.
- [ ] **Step 5: Run** `cargo test -p majestical-services -p majestical-cli -p majestical-describe && just check`.
- [ ] **Step 6: Commit**:
  `git commit -m "feat: a rejected OpenRouter key and an empty account fail by name; describer test checks the key"`

---

## PR Chunk 4 — the Keychain adapter and the services decisions

### Task 4: `crates/secrets`

**Files:**
- Create: `crates/secrets/Cargo.toml`, `crates/secrets/src/lib.rs`, `crates/secrets/src/system.rs`
- Modify: `Cargo.toml` (workspace `members`)

`Cargo.toml` — copy `[lints]`/`[package]` inheritance from
`crates/describe/Cargo.toml`; then:

```toml
[dependencies]
thiserror = { workspace = true }

[target.'cfg(target_os = "macos")'.dependencies]
security-framework = "3.7.0"   # verify the current version at execution time
```

`lib.rs`:

```rust
//! The one place a secret store is touched. Head-side only: `services`
//! and `describe` never depend on this crate — the heads resolve a key
//! here and pass it in, exactly as they pass the environment's.
mod system;
pub use system::{SUPPORTED, SystemKeyStore};

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("this platform has no supported secret store")]
    Unsupported,
    /// The store's own message. Never contains the secret.
    #[error("secret store: {0}")]
    Store(String),
}

/// One stored secret: the OpenRouter API key.
pub trait KeyStore {
    fn supported(&self) -> bool;
    /// # Errors
    /// `Unsupported` off macOS; `Store` when the Keychain refuses (locked,
    /// or the user denied the prompt).
    fn read(&self) -> Result<Option<String>, SecretError>;
    /// # Errors
    /// As [`Self::read`].
    fn store(&self, key: &str) -> Result<(), SecretError>;
    /// `Ok(false)` when nothing was stored.
    /// # Errors
    /// As [`Self::read`].
    fn delete(&self) -> Result<bool, SecretError>;
}

/// Where the head found the key it will pass to services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadKeySource { Env, Keychain, Absent }

#[derive(Debug, Clone)]
pub struct ResolvedKey {
    pub key: Option<String>,
    pub source: HeadKeySource,
    /// Set when the store was consulted and failed; the head pushes it as
    /// a notice. Never contains a key.
    pub notice: Option<String>,
}

/// `env`, then the store. The store is read ONLY when `env` is absent and
/// `wants_keychain` (the configured backend is OpenRouter) — so a
/// local-backend user never sees a macOS prompt, and neither does anyone
/// who set the variable.
#[must_use]
pub fn resolve(env: Option<String>, wants_keychain: bool, store: &dyn KeyStore) -> ResolvedKey {
    if let Some(key) = env {
        return ResolvedKey { key: Some(key), source: HeadKeySource::Env, notice: None };
    }
    let absent = |notice| ResolvedKey { key: None, source: HeadKeySource::Absent, notice };
    if !wants_keychain || !store.supported() {
        return absent(None);
    }
    match store.read() {
        Ok(Some(key)) => ResolvedKey {
            key: Some(key),
            source: HeadKeySource::Keychain,
            notice: None,
        },
        Ok(None) | Err(SecretError::Unsupported) => absent(None),
        Err(SecretError::Store(message)) => absent(Some(format!(
            "note: the macOS Keychain could not be read ({message}) — \
             set MAJ_OPENROUTER_KEY to supply the key without it"
        ))),
    }
}

/// In-memory [`KeyStore`] for head tests. `fail` makes every call return
/// `SecretError::Store(fail)`.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    pub key: std::sync::Mutex<Option<String>>,
    pub fail: Option<String>,
    pub unsupported: bool,
}
// impl KeyStore for MemoryKeyStore: supported() = !unsupported; each
// method returns Unsupported when unsupported, Store(fail) when fail is
// set, else reads/writes `key` (poisoned lock → into_inner).
```

`system.rs` — `pub const SUPPORTED: bool = cfg!(target_os = "macos");`,
`pub struct SystemKeyStore;`, and two `impl KeyStore` blocks selected by
`#[cfg(target_os = "macos")]` / `#[cfg(not(target_os = "macos"))]` (the
second returns `Unsupported` everywhere, `supported()` false). The macOS
block delegates to three private functions that take the service name:

```rust
const SERVICE: &str = "majestical";
const ACCOUNT: &str = "openrouter-api-key";
/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;

fn read_item(service: &str) -> Result<Option<String>, SecretError> {
    match security_framework::passwords::get_generic_password(service, ACCOUNT) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| SecretError::Store("the stored key is not UTF-8".to_string())),
        Err(err) if err.code() == NOT_FOUND => Ok(None),
        Err(err) => Err(SecretError::Store(err.to_string())),
    }
}
// store_item: set_generic_password(service, ACCOUNT, key.as_bytes())
// delete_item: delete_generic_password → Ok(true); NOT_FOUND → Ok(false)
```

  Confirm the three function names and `Error::code()` against the
  `security-framework` docs for the version actually pinned (context7)
  before writing them.

- [ ] **Step 1: Failing tests.** In `lib.rs`, over `MemoryKeyStore`:
  `env_wins_and_the_store_is_never_read` (store has `fail` set; result has
  no notice — proving it was not consulted),
  `the_store_is_not_read_for_a_local_backend`,
  `a_stored_key_resolves_as_keychain`, `an_empty_store_resolves_as_none`,
  `a_store_failure_is_a_notice_not_an_error_and_names_the_env_var`,
  `an_unsupported_store_is_silent`. In `system.rs`, macOS-only:

```rust
#[cfg(all(test, target_os = "macos"))]
mod tests {
    // A throwaway service per test process so a crashed run cannot collide
    // with the next, and the user's real item is never touched.
    fn service() -> String { format!("majestical-test-{}", std::process::id()) }

    #[test]
    fn store_read_overwrite_delete_round_trip() {
        let service = service();
        assert_eq!(super::read_item(&service).expect("read"), None);
        super::store_item(&service, "sk-test").expect("store");
        assert_eq!(super::read_item(&service).expect("read").as_deref(), Some("sk-test"));
        super::store_item(&service, "sk-test-2").expect("overwrite");
        assert_eq!(super::read_item(&service).expect("read").as_deref(), Some("sk-test-2"));
        assert!(super::delete_item(&service).expect("delete"));
        assert!(!super::delete_item(&service).expect("second delete"));
    }
}
```

  and, non-macOS, `the_stub_is_unsupported_everywhere`.
- [ ] **Step 2: Run** `cargo test -p majestical-secrets` — fails to compile.
- [ ] **Step 3: Implement.** Run the macOS round-trip locally: it must pass
  with no prompt (the test binary creates the item, so it owns it).
- [ ] **Step 4: Prove it on CI before building on it.** Push the branch and
  open the chunk's PR as a draft; watch the `rust` job's macOS leg. If the
  round-trip fails there (a headless runner can answer
  `errSecInteractionNotAllowed`, -25308), gate that one test on
  `std::env::var_os("MAJ_KEYCHAIN_TESTS")` — early `return` when unset —
  add `just keychain-test` that sets it, and record the change for the
  spec's as-built section. Do not skip the test silently.
- [ ] **Step 5: Run** `cargo test -p majestical-secrets && just check`.
- [ ] **Step 6: Commit**:
  `git commit -m "feat: crates/secrets — the OpenRouter key in the macOS Keychain, behind a KeyStore trait"`

### Task 5: services — `key_source`, `FileKey`, `plan_key_write`, `clear_file_key`, the doctor row

**Files:**
- Modify: `crates/services/src/describer_config.rs`
- Modify: `crates/services/src/doctor.rs`
- Modify (mechanical, to keep the workspace building — no Keychain yet):
  `crates/cli/src/main.rs`, `crates/cli/src/describer_cmd.rs`,
  `crates/cli/src/commands.rs:789`, `crates/cli/src/mcp_cmd/read_tools.rs`,
  `crates/cli/src/mcp_cmd/write_tools.rs`,
  `apps/desktop/src-tauri/src/commands.rs:230-238`
- Modify: `crates/cli/tests/services_parity.rs` (TEMPORARY normalizer)

The new services surface:

```rust
/// What the head found outside the config file. Services never looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyPresence { pub env: bool, pub keychain: bool }

/// Where the key a caption run would use comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeySource { Env, Keychain, File, None }

/// The same order the run resolves in: the head's key (env, then
/// Keychain) applies to OpenRouter only — `effective_api_key`'s rule —
/// and the file's key is last.
#[must_use]
pub fn key_source(config: &DescriberConfig, presence: KeyPresence) -> KeySource {
    let head = match config.backend {
        BackendKind::OpenRouter if presence.env => Some(KeySource::Env),
        BackendKind::OpenRouter if presence.keychain => Some(KeySource::Keychain),
        BackendKind::OpenRouter | BackendKind::Ollama | BackendKind::LmStudio => None,
    };
    match (head, &config.api_key) {
        (Some(source), _) => source,
        (None, Some(_)) => KeySource::File,
        (None, None) => KeySource::None,
    }
}

pub struct DescriberConfigView {
    pub backend: String,
    pub base_url: String,
    pub model: String,
    pub key_source: KeySource,
}

/// What `set` does with the key field of `describer.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKey { Keep, Set(String), Clear }

pub struct SetArgs {
    pub backend: BackendKind,
    pub model: String,
    pub base_url: Option<String>,
    pub file_key: FileKey,
}

/// Where a newly supplied key goes. With a supported Keychain the key goes
/// there and the file is written keyless; otherwise the file holds it. No
/// key supplied means nothing about the key changes.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyWrite { pub keychain: Option<String>, pub file: FileKey }

#[must_use]
pub fn plan_key_write(keychain_supported: bool, key: Option<String>) -> KeyWrite {
    match (key, keychain_supported) {
        (None, _) => KeyWrite { keychain: None, file: FileKey::Keep },
        (Some(key), true) => KeyWrite { keychain: Some(key), file: FileKey::Clear },
        (Some(key), false) => KeyWrite { keychain: None, file: FileKey::Set(key) },
    }
}

/// What a head reports after `clear-key`. Built by the head (it owns the
/// Keychain half); defined here so all three heads share one shape.
#[derive(Debug, serde::Serialize)]
pub struct ClearKeyOutcome {
    pub keychain_cleared: bool,
    pub file_cleared: bool,
    /// `MAJ_OPENROUTER_KEY` is set, so a key is still supplied.
    pub env_still_supplies: bool,
}

/// True when the stored config's backend is OpenRouter — the head's cue to
/// consult the Keychain. Unconfigured or unreadable is `false`: there is
/// nothing a key would be used for.
#[must_use]
pub fn wants_keychain(catalog_root: &Path, notices: &Notices) -> bool

/// Removes `api_key` from an existing `describer.toml`. `Ok(false)` when no
/// config exists or it held no key (and then nothing is written).
pub fn clear_file_key(catalog_root: &Path, notices: &Notices) -> Result<bool, ServiceError>
```

Signature changes: `to_view(config, presence)`, `show(root, presence,
notices)`, `set(root, args, notices) -> Result<(), ServiceError>`.
`set_impl` resolves `FileKey::Keep` by loading the existing config (if any)
and carrying its `api_key` forward; `Set(k)` → `Some(k)`; `Clear` → `None`.
`REDACTED_MARKER` and the view's `api_key` field are deleted.

Doctor: `DoctorRequest.describer_key: KeyPresence` replaces
`describer_env_key`. `describer_config_row(config, presence)` calls
`key_source`: OpenRouter with `Env|Keychain|File` →
`ok("{backend} · {model} · key from {env|keychain|file}")` (render with an
explicit `match`, not `Debug`); `None` → the existing `Fail` row with
detail `"{backend} · {model} · no API key"` and the (updated) missing-key
remedy.

The mechanical head edits in THIS task (no Keychain yet, behavior otherwise
unchanged except the `set` semantic): every head builds
`KeyPresence { env: env_api_key().is_some(), keychain: false }`;
`Set`'s `api_key: Option<String>` maps through
`plan_key_write(false, api_key).file`; `cmd_set` calls `set` then `show`;
`print_view` prints `api-key:  (from env|keychain|file)` or
`api-key:  (none)` via an explicit match.

- [ ] **Step 1: Failing tests** in `describer_config.rs`:
  - `key_source` per arm: OpenRouter × {env, keychain, file, none, env+
    keychain+file → Env}; Ollama with env present and no file key → `None`;
    Ollama with a file key → `File`.
  - `plan_key_write` — all three arms.
  - `set_without_a_key_keeps_the_stored_key` — `set` with `Set("sk-test")`,
    then `set` with `Keep` and a different model; load the config and
    assert the model changed and `api_key == Some("sk-test")`.
  - `set_with_clear_removes_the_stored_key`.
  - `clear_file_key` — `false` on an unconfigured root (and no file is
    created), `true` then `false` on a keyed config, other fields intact.
  - `wants_keychain` — unconfigured, Ollama, OpenRouter.
  - Replace the two `to_view_*` tests with
    `the_view_carries_a_source_and_never_a_key`: serialize the view of a
    keyed config to JSON and assert the string contains neither `sk-test`
    nor an `api_key` member.
  - `doctor.rs`: update the six `describer_env_key` call sites; add a case
    per source asserting the detail text; the no-key case keeps `Fail`.
- [ ] **Step 2: Run** `cargo test -p majestical-services --lib describer_config doctor` — fails.
- [ ] **Step 3: Implement** services, then the mechanical head edits.
- [ ] **Step 4: Parity.** `describer show`'s `api-key:` line changed.
  Rebuild the reference at the merge-base and run `services_parity`. For
  each differing row add a TEMPORARY normalizer (mandate 13): remove the
  exact line `api-key:  (redacted)` / `api-key:  (from file)` on both
  sides, with its two unit tests and a `TEMPORARY — deleted in phase 7G
  Task 12` doc line.
- [ ] **Step 5: Run** `cargo test --workspace && cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml && just check`.
- [ ] **Step 6: Commit**:
  `git commit -m "feat: describer views name the key's source; set keeps a stored key unless told otherwise"`

---

## PR Chunk 5 — the Keychain at the CLI and MCP heads

### Task 6: `describer_key.rs`, `clear-key`, `set_describer { clear_key }`

**Files:**
- Modify: `crates/cli/Cargo.toml` (`majestical-secrets = { path = "../secrets" }`)
- Create: `crates/cli/src/describer_key.rs`
- Modify: `crates/cli/src/main.rs`, `describer_cmd.rs`, `index_cmd.rs:229`,
  `commands.rs:789`, `mcp_cmd/read_tools.rs` (`get_describer`, doctor at
  406), `mcp_cmd/write_tools.rs`
- Create: `crates/cli/tests/describer_key.rs`

The head module — every function takes the store, so tests pass
`MemoryKeyStore` and production passes `&SystemKeyStore`:

```rust
//! The CLI and MCP heads' reading and writing of the OpenRouter key:
//! `MAJ_OPENROUTER_KEY`, then the Keychain, through `majestical_secrets`.
//! Services is handed the result and never looks for itself.

/// Resolves the key for `catalog_root` and pushes the store's failure, if
/// any, as a notice.
pub(crate) fn resolve(catalog_root: &Path, store: &dyn KeyStore, notices: &Notices) -> ResolvedKey {
    let wants = describer_config::wants_keychain(catalog_root, notices);
    let resolved = majestical_secrets::resolve(env_api_key(), wants, store);
    if let Some(notice) = &resolved.notice {
        notices.push(notice.clone());
    }
    resolved
}

pub(crate) fn presence(resolved: &ResolvedKey) -> KeyPresence {
    match resolved.source {
        HeadKeySource::Env => KeyPresence { env: true, keychain: false },
        HeadKeySource::Keychain => KeyPresence { env: false, keychain: true },
        HeadKeySource::Absent => KeyPresence::default(),
    }
}

/// Executes [`describer_config::plan_key_write`]: the Keychain write comes
/// FIRST and its failure stops everything, so the file and the Keychain
/// never disagree. Returns what `set` should do with the file's key.
///
/// # Errors
/// The Keychain refused the write. The message never contains the key.
pub(crate) fn store(key: Option<String>, store: &dyn KeyStore) -> anyhow::Result<FileKey> {
    let plan = describer_config::plan_key_write(store.supported(), key);
    if let Some(key) = &plan.keychain {
        store.store(key).context("storing the key in the macOS Keychain")?;
    }
    Ok(plan.file)
}

/// # Errors
/// The Keychain refused the delete, or `describer.toml` could not be rewritten.
pub(crate) fn clear(
    catalog_root: &Path,
    store: &dyn KeyStore,
    notices: &Notices,
) -> anyhow::Result<ClearKeyOutcome> {
    let keychain_cleared = if store.supported() {
        store.delete().context("removing the key from the macOS Keychain")?
    } else {
        false
    };
    let file_cleared = describer_config::clear_file_key(catalog_root, notices)?;
    Ok(ClearKeyOutcome {
        keychain_cleared,
        file_cleared,
        env_still_supplies: env_api_key().is_some(),
    })
}
```

`env_api_key` moves here from `describer_cmd.rs` (its one definition).

Call sites: `cmd_set` = `store(args.api_key)` → `set` → `resolve` → `show`
→ `print_view`. `cmd_test`, `index_cmd.rs:229`, both doctor call sites and
`get_describer` use `resolve(...)` — `.key` where a key is wanted,
`presence(&resolved)` where a view or the doctor row is. The doctor call
with no catalog passes `KeyPresence { env: env_api_key().is_some(),
keychain: false }` without touching the store.

CLI: `DescriberCmd::ClearKey` with doc
`/// Remove the stored API key (the Keychain item and describer.toml's).`
`cmd_clear_key` prints, by explicit match on the two booleans:
`removed the key from the Keychain and describer.toml` /
`removed the key from the Keychain` / `removed the key from describer.toml`
/ `no stored key to remove`; then, if `env_still_supplies`,
`MAJ_OPENROUTER_KEY is set and still supplies a key`.

MCP `SetDescriberArgs` gains
`/// Remove the stored API key instead of setting one. Cannot be combined with api_key.`
`#[serde(default)] clear_key: bool`. `api_key` + `clear_key` together is a
tool error. Dry run (`confirm: false`) with `clear_key` reads the current
view and answers `"would": "remove the stored key (currently from
<source>)"` — or `"no stored key to remove"` for `none`, or, for `env`,
`"… MAJ_OPENROUTER_KEY would still supply a key"` — and writes nothing.
The `api_key` dry run's `would` sentence gains
`, storing the key in the macOS Keychain` when `store.supported()`.
`WriteTools`' handlers construct `&SystemKeyStore`; the `*_result`
functions take `&dyn KeyStore` so their tests do not.

- [ ] **Step 1: Failing tests.**
  - `describer_key.rs` unit tests over `MemoryKeyStore`:
    `store_puts_the_key_in_a_supported_store_and_clears_the_file_key`,
    `store_puts_the_key_in_the_file_when_unsupported`,
    `store_without_a_key_changes_nothing`,
    `a_refused_keychain_write_is_an_error_without_the_key_in_it`
    (`fail: Some("denied")`; assert `!format!("{err:#}").contains("sk-test")`),
    `clear_removes_both_and_reports_each`, `clear_with_nothing_stored_is_a_no_op`.
  - `write_tools.rs` tests: `set_describer_clear_key_dry_run_names_the_source_and_writes_nothing`
    (assert the store and the file are untouched),
    `set_describer_rejects_api_key_with_clear_key`,
    `set_describer_stores_the_key_in_a_supported_store_and_leaves_the_file_keyless`.
  - `crates/cli/tests/describer_key.rs` (integration, child processes with
    `MAJ_STATE_DIR` per `common/mod.rs`, and `MAJ_OPENROUTER_KEY` unset).
    These run the real binary, so on macOS they would touch the real
    Keychain: they use **Ollama plus `--api-key`** — never OpenRouter —
    and only on non-macOS targets exercise the file path:
    `#[cfg(not(target_os = "macos"))] set_without_a_key_keeps_the_file_key`,
    `#[cfg(not(target_os = "macos"))] clear_key_removes_the_file_key`.
    On every target: `clear_key_with_nothing_stored_says_so` is NOT safe on
    macOS (it would delete the developer's real item) — mark it
    `#[cfg(not(target_os = "macos"))]` too, and cover macOS behavior in
    the unit tests above. `show_names_the_env_source`
    (`MAJ_OPENROUTER_KEY=sk-test`, OpenRouter config written by `set`
    WITHOUT `--api-key`) is safe everywhere: env short-circuits the store.
    Assert stdout has `api-key:  (from env)` and never `sk-test`.
- [ ] **Step 2: Run** `cargo test -p majestical-cli describer_key set_describer` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Hand check on macOS** (record the result in the PR body):
  `maj describer set --backend open-router --model x --api-key sk-test`,
  confirm `security find-generic-password -s majestical -a openrouter-api-key`
  finds the item and the catalog's `describer.toml` has no `api_key`;
  `maj describer show` says `(from keychain)`; `maj describer clear-key`
  removes it. Use a scratch catalog under the session scratchpad.
- [ ] **Step 5: Run** `cargo test --workspace && just check`, then the
  parity suites.
- [ ] **Step 6: Commit**:
  `git commit -m "feat: the CLI and MCP keep the OpenRouter key in the Keychain; describer clear-key"`

---

## PR Chunk 6 — the desktop head

### Task 7: the cached key and the four commands

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml`
  (`majestical-secrets = { path = "../../../crates/secrets" }`, with a
  comment in the style of its neighbors)
- Create: `apps/desktop/src-tauri/src/captions.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `commands.rs`, `indexer.rs`

```rust
//! The Captions settings commands and the head's cached describer key.
//!
//! The key is resolved (`MAJ_OPENROUTER_KEY`, then the Keychain) at
//! startup, when a catalog is adopted, and after a save or a clear — never
//! per scheduler tick, so a denied macOS prompt is asked once, not every
//! poll.

/// Managed state: what the scheduler and doctor use as the head's key.
pub struct DescriberKeyCache(pub RwLock<ResolvedKey>);

impl Default for DescriberKeyCache { /* key None, source None, notice None */ }

/// Re-resolves and replaces the cached key. A poisoned lock is recovered,
/// never propagated: this state must not get stuck.
pub fn refresh_key(
    cache: &DescriberKeyCache,
    cfg: Option<&CatalogCfg>,
    env: Option<String>,
    store: &dyn KeyStore,
)

#[derive(Debug, Serialize)]
pub struct DescriberSettingsOutcome {
    /// `null` when no describer is configured.
    pub describer: Option<DescriberConfigView>,
    pub keychain_supported: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveDescriberReq {
    pub backend: DescriberBackend,
    pub model: String,
    pub base_url: Option<String>,
    /// Absent or blank means "keep the stored key".
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DescriberProbeOutcome {
    #[serde(flatten)]
    pub probe: DescriberProbe,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// Bundles what the mutating impls need so they stay under five parameters.
pub struct CaptionDeps<'a> {
    pub cache: &'a DescriberKeyCache,
    pub wake: &'a SchedulerWake,
    pub store: &'a dyn KeyStore,
    pub env: Option<String>,
}

pub fn describer_settings_impl(cfg: &CatalogCfg, cache: &DescriberKeyCache, store: &dyn KeyStore)
    -> Result<DescriberSettingsOutcome, CommandError>
pub fn save_describer_impl(cfg: &CatalogCfg, req: SaveDescriberReq, deps: &CaptionDeps<'_>)
    -> Result<DescriberSettingsOutcome, CommandError>
pub fn clear_describer_key_impl(cfg: &CatalogCfg, deps: &CaptionDeps<'_>)
    -> Result<DescriberSettingsOutcome, CommandError>
pub fn test_describer_impl(cfg: &CatalogCfg, key: Option<String>)
    -> Result<DescriberProbeOutcome, CommandError>
```

`save_describer_impl`, in order: blank-to-`None` the key
(`req.api_key.filter(|k| !k.trim().is_empty())`, trimmed); reject a blank
`model` with a `CommandError` whose message is `a model name is required`;
`plan_key_write(store.supported(), key)`; the Keychain write first — on
failure return a `CommandError` with EXACTLY
`Could not store the key in the macOS Keychain — nothing was saved.`
(mockup frame 8; the store's own message is dropped, mandate 15);
`describer_config::set`; `refresh_key`; `deps.wake.nudge()`; return
`describer_settings_impl`'s outcome. `clear_describer_key_impl` deletes the
Keychain item (when supported), `clear_file_key`, then the same
refresh → nudge → outcome. Check how `CommandError` is constructed from a
plain message in `commands.rs` and use that constructor.

Commands: `describer_settings`, `save_describer`, `clear_describer_key` are
sync one-liners building `CaptionDeps` from `State`s,
`commands::env_api_key()` and `&SystemKeyStore`; `test_describer` is
`async` over `blocking(move || …)` exactly as `plan_ingest`
(`commands.rs:1045`) — it makes network calls — reading the cached key
first. All four use `require_catalog`.

Wiring: `lib.rs` manages `DescriberKeyCache::default()` and registers the
four commands; its `setup` calls `refresh_key` once the persisted catalog
is loaded. `adopt_catalog` (`commands.rs:553`) refills it — it needs the
cache, so add the `State`/reference at its two callers (`:1002`, `:1025`);
if that pushes `adopt_catalog` past five parameters, bundle the new ones.
`indexer.rs:259` reads the cache's key instead of `env_api_key()`;
`doctor_report` passes `presence` derived from the cache;
`doctor_report_impl(cfg, presence: KeyPresence)`.

- [ ] **Step 1: Failing tests** in `captions.rs`'s `mod tests`, with
  `MemoryKeyStore` and a tempdir catalog under the `ENV_LOCK` +
  `MAJ_STATE_DIR` pattern from `tests/commands.rs:41` (copy the helper;
  it is eleven lines):
  - `settings_of_an_unconfigured_catalog_is_null_describer`
  - `save_stores_the_key_in_the_keychain_and_leaves_the_file_keyless` —
    then the cache holds `sk-test` with source `Keychain`, and the wake was
    nudged (`wake.wait(Duration::ZERO)` returns immediately — use whatever
    `indexer.rs`'s own nudge tests assert).
  - `save_with_a_blank_key_keeps_the_stored_key` (`Some("   ")`).
  - `save_with_a_blank_model_is_refused_and_writes_nothing`
  - `a_refused_keychain_write_saves_nothing_and_says_so_without_the_key` —
    `fail: Some("denied")`; assert the exact message, that no
    `describer.toml` exists, and that the message lacks `sk-test`.
  - `save_for_ollama_never_touches_the_store` (store has `fail` set; save
    succeeds).
  - `clear_empties_the_cache_and_reports_source_none`
  - `env_overrides_and_the_view_says_env` (`deps.env = Some("sk-test")`).
  - `indexer.rs`: update `batch_request`'s existing test (line ~796) only
    if its signature changed; add
    `the_tick_reads_the_cached_key_not_the_environment` on whatever small
    function now extracts the key from the cache.
- [ ] **Step 2: Run** `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml captions` — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml && cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets -- -D warnings`.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: desktop describer commands over a cached Keychain key"`

### Task 8: the wire — fixtures, `api-captions.ts`, parity

**Files:**
- Modify: `apps/desktop/src-tauri/tests/wire_fixtures.rs`, `tests/tauri_parity.rs`
- Create: `apps/desktop/src/lib/api-captions.ts`,
  `src/lib/fixtures.captions.test.ts`,
  `src/lib/fixtures/describer_settings.json`,
  `describer_settings_unconfigured.json`, `describer_probe.json`

```ts
// The captions wire subject — `captions.rs`'s four commands. Its own
// module because `api.ts` is at its cap (see .oxlintrc.json); imported
// directly by `CaptionsSection.svelte`, not spread into `api`.
import { invoke } from "@tauri-apps/api/core";

/** `describer_config::DescriberBackend`, serialized kebab-case. */
export type DescriberBackend = "ollama" | "lm-studio" | "open-router";

/** `describer_config::KeySource`, serialized snake_case. */
export type KeySource = "env" | "keychain" | "file" | "none";

/** `describer_config::DescriberConfigView`. Never carries a key. */
export interface DescriberConfigView {
  backend: DescriberBackend;
  base_url: string;
  model: string;
  key_source: KeySource;
}

/** `captions::DescriberSettingsOutcome`. `describer` is `null`, not absent,
 *  when nothing is configured. */
export interface DescriberSettingsOutcome {
  describer: DescriberConfigView | null;
  keychain_supported: boolean;
  notices?: string[];
}

/** `describer_config::KeyCheck`, serialized snake_case. */
export type KeyCheck = "accepted" | "rejected" | "missing" | "not_checked";

/** `captions::DescriberProbeOutcome` — the probe, flattened, plus notices.
 *  `vision` is `null` for every backend but LM Studio. */
export interface DescriberProbeOutcome {
  model: string;
  model_listed: boolean;
  vision: boolean | null;
  key: KeyCheck;
  notices?: string[];
}

/** `captions::SaveDescriberReq`. An absent or blank `api_key` keeps the
 *  stored key. */
export interface SaveDescriberReq {
  backend: DescriberBackend;
  model: string;
  base_url?: string;
  api_key?: string;
}

export const captionsApi = {
  describerSettings: () => invoke<DescriberSettingsOutcome>("describer_settings"),
  saveDescriber: (req: SaveDescriberReq) =>
    invoke<DescriberSettingsOutcome>("save_describer", { req }),
  clearDescriberKey: () => invoke<DescriberSettingsOutcome>("clear_describer_key"),
  testDescriber: () => invoke<DescriberProbeOutcome>("test_describer"),
};
```

`DescriberConfigView.backend` is a `String` on the Rust side holding
`BackendKind::as_str()` — confirm those strings equal the kebab-case wire
names (`describer_backend_wire_strings_are_pinned` says they do).

- [ ] **Step 1:** Add three fixture tests to `wire_fixtures.rs` in the
  style of `scheduler_state_fixture` (line 634): an OpenRouter view with
  `key_source: Keychain` and one notice; the unconfigured outcome; a probe
  with `key: Accepted`, `vision: None`. Generate with
  `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` and read the
  three JSON files: no key-shaped string may appear.
- [ ] **Step 2:** `fixtures.captions.test.ts`, modeled on
  `fixtures.alwayson.test.ts`: each fixture assigned to its interface
  (a type-level pin) plus a key-set assertion per fixture.
- [ ] **Step 3:** A `tauri_parity.rs` row: `describer_settings` for a
  catalog configured by the CLI binary (`maj describer set --backend ollama
  --model llava`) equals `maj describer show`'s data — same backend, base
  URL, model, and source `none`.
- [ ] **Step 4: Run** the Rust desktop tests, then in `apps/desktop`:
  `pnpm check && pnpm lint && pnpm test && pnpm build`.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: the captions wire subject, pinned by fixtures on both sides"`

---

## PR Chunk 7 — the Captions section

**Gate: the mockup is approved by the user.** Every quoted string below is
the mockup's; if the approved mockup differs, the mockup wins and this
plan's strings are updated in the same PR.

### Task 9: `captions-status.ts`, `CaptionsSection.svelte`, the Settings mount, the test split

**Files:**
- Create: `apps/desktop/src/lib/captions-status.ts`, `captions-status.test.ts`,
  `CaptionsSection.svelte`, `CaptionsSection.test.ts`
- Create: `apps/desktop/src/App.settings.test.ts`
- Modify: `apps/desktop/src/lib/SettingsView.svelte`, `SettingsView.test.ts`,
  `src/App.test.ts`, `src/app.css`, `.oxlintrc.json`

The pure module (all the section's words live here, as `scheduler-status.ts`
does for Always-on):

```ts
import type { DescriberBackend, DescriberProbeOutcome, KeySource } from "./api-captions";

export const BACKENDS: { value: DescriberBackend; label: string; baseUrl: string }[] = [
  { value: "ollama", label: "Ollama", baseUrl: "http://localhost:11434" },
  { value: "lm-studio", label: "LM Studio", baseUrl: "http://localhost:1234" },
  { value: "open-router", label: "OpenRouter", baseUrl: "https://openrouter.ai/api" },
];

export const UNCONFIGURED_LINE = "No describer is configured — captions are off.";
export const SAVED_LINE = "Saved.";

const KEY_STATUS: Record<KeySource, string> = {
  keychain: "A key is stored in the macOS Keychain.",
  env: "MAJ_OPENROUTER_KEY supplies the key and overrides a stored one.",
  file: "A key is stored in describer.toml. Save a key here to move it to the Keychain.",
  none: "No key. Captions cannot run until one is saved.",
};

export function keyStatusLine(source: KeySource): string {
  return KEY_STATUS[source];
}

/** Only a stored key can be removed: the app cannot unset an env var. */
export function removeKeyVisible(source: KeySource): boolean {
  return source === "keychain" || source === "file";
}

export function keyPlaceholder(source: KeySource): string {
  return removeKeyVisible(source) ? "Leave empty to keep the stored key" : "sk-or-…";
}

export interface TestLine { good: boolean; text: string }

export function testLines(probe: DescriberProbeOutcome): TestLine[] {
  const lines: TestLine[] = [{ good: true, text: "Backend reachable." }];
  lines.push(
    probe.model_listed
      ? { good: true, text: `Model ${probe.model} is listed.` }
      : { good: false, text: `Model ${probe.model} is not listed — check the name.` },
  );
  if (probe.vision === true) lines.push({ good: true, text: "Vision: yes" });
  if (probe.vision === false) {
    lines.push({ good: false, text: "Vision: no — captions will not run with this model" });
  }
  if (probe.key === "accepted") lines.push({ good: true, text: "Key accepted." });
  if (probe.key === "rejected") {
    lines.push({
      good: false,
      text: "Key rejected — OpenRouter answered 401. Save a new key.",
    });
  }
  if (probe.key === "missing") lines.push({ good: false, text: keyStatusLine("none") });
  return lines;
}
```

The two base URLs duplicated here must equal
`BackendKind::default_base_url()`: `captions-status.test.ts` pins them
against the Rust-generated `describer_settings.json` fixture for
OpenRouter, and a comment names `crates/describe/src/config.rs:18` for the
other two.

The component — behavior, in the order the tests below pin it:
- On mount, `captionsApi.describerSettings()`; fill the form from
  `describer` or, when `null`, Ollama with its default URL and
  `UNCONFIGURED_LINE`.
- Changing Backend sets Base URL to that backend's default ONLY if the
  field still holds another backend's default (a hand-edited URL is kept).
- The key row renders only when Backend is OpenRouter.
- Save: disabled while the model is blank or a save is in flight; sends
  `api_key` only when the field is non-blank; on success replaces the
  outcome, EMPTIES the key field, shows `SAVED_LINE` until the next edit,
  clears the Test results, and calls `onchanged()`. On failure: the error
  renders in `<p class="error" role="alert">`, and the key field KEEPS its
  content (frame 8).
- Test: disabled when nothing is saved (`describer === null`), when the
  form differs from the saved config (it tests what is STORED — the hint
  for that is the disabled button's `title`: `Save before testing`), or
  while in flight. Results render as `<ul class="captions-results">` with
  `good`/`bad` classes; a command failure renders in the alert.
- Remove key: shown per `removeKeyVisible`; calls `clearDescriberKey`,
  replaces the outcome, calls `onchanged()`.
- `<Notices notices={outcome.notices ?? []} />` above the form.
- `data-e2e` hooks: `captions-backend`, `captions-model`, `captions-save`,
  `captions-saved`.

`SettingsView.svelte`: import and mount
`<CaptionsSection onchanged={() => void load()} />` between the Health
section and `<AlwaysOnSection />`; update the header comment's "NOT
read-only" paragraph to name both sections.

The split: move the two Settings-shell tests named in `.oxlintrc.json`'s
`App.test.ts` override ("the settings surface swaps in with the doctor's
health rows", "the tray's navigate-settings event selects the Settings
surface") into `src/App.settings.test.ts` with the mocks they need, plus
`describer_settings: () => unconfiguredFixture`. Then LOWER
`App.test.ts`'s two overrides to fit the smaller file (`max-lines` to the
new line count rounded up to the next 5; `import/max-dependencies` to the
new count) and rewrite their comments to describe the file as it now is —
a cap is a ratchet. `SettingsView.test.ts` gains the same one mock line.

- [ ] **Step 1: Failing tests.**
  - `captions-status.test.ts`: `keyStatusLine` for all four sources
    (byte-exact); `removeKeyVisible` truth table; `keyPlaceholder` both
    arms; `testLines` for: all-good OpenRouter (three lines, exact), model
    not listed + key rejected, key missing (the line equals
    `keyStatusLine("none")`), LM Studio vision yes / vision no, Ollama
    (`key: "not_checked"`, `vision: null` → exactly two lines).
  - `CaptionsSection.test.ts` with `mockCommands`/`rejectCommand` from
    `test-support`:
    `an unconfigured catalog preselects Ollama and disables Test`,
    `a configured OpenRouter describer shows the keychain status and an empty key field`,
    `the key row is absent for a local backend`,
    `switching backend replaces a default URL but keeps an edited one`,
    `Save sends no api_key when the field is empty` (assert the mock's
    received args have no `api_key` member),
    `Save sends the typed key, then empties the field and says Saved.`
    — this one asserts the CONSEQUENCE (the input's `value === ""` and the
    args carried `sk-test`), and additionally that `sk-test` appears
    nowhere in `document.body.textContent` afterwards,
    `a failed Save keeps the typed key and shows the alert`,
    `Save calls onchanged`,
    `Remove key is shown for keychain and file, hidden for env and none`,
    `Test is disabled until the form matches what is saved`,
    `Test renders the probe's lines`, `a failed Test renders the alert`.
- [ ] **Step 2: Run** `pnpm test captions CaptionsSection` in `apps/desktop` — fails.
- [ ] **Step 3: Implement** the pure module, the component, the CSS
  (`.captions-form`, `.captions-key-status`, `.captions-actions`,
  `.captions-saved`, `.captions-results` — copy the mockup's rules), the
  mount, then the split.
- [ ] **Step 4: Run** `pnpm check && pnpm lint && pnpm test && pnpm build`.
  If `CaptionsSection.svelte` exceeds the default 300-line script cap,
  split the Test results list into `CaptionsTestResults.svelte` rather
  than raising a cap.
- [ ] **Step 5: Commit**:
  `git commit -m "feat: a Captions section in Settings — backend, model, key, Save, Test"`

### Task 10: the e2e flow

**Files:**
- Modify: `apps/desktop/e2e/specs/settings.e2e.ts`

A third `describe` (each stays under the function line cap), AFTER the
throttle one:

```ts
describe("Majestical desktop — Settings — Captions", () => {
  before(async () => {
    await suppressAutoFocusRecovery(browser);
    await $('[data-e2e="nav-search"]').waitForDisplayed({ timeout: 20_000 });
    await openSurface('[data-e2e="nav-settings"]', ".settings-checks");
  });

  it("saving an Ollama describer changes the Health describer row", async () => {
    const describerDetail = async () => {
      const rows = await $$(".settings-check");
      const names = await rows.map((el) => el.$(".settings-check-name").getText());
      const row = rows[names.indexOf("describer")];
      if (row === undefined) throw new Error("no describer row");
      return row.$(".settings-check-detail").getText();
    };
    expect(await describerDetail()).toBe("no describer configured — captions off");

    await $('[data-e2e="captions-model"]').setValue("llava");
    await $('[data-e2e="captions-save"]').click();
    await $('[data-e2e="captions-saved"]').waitForDisplayed();

    await browser.waitUntil(async () => (await describerDetail()) === "ollama · llava");
  });
});
```

Ollama is the preselected backend, so the `<select>` — which the embedded
WebKit driver cannot change through `selectByVisibleText` — is never
touched. No key is stored and no network call is made.

- [ ] **Step 1: Side effects first.** Saving a describer makes caption work
  plannable for the rest of the e2e session, and nothing listens on
  `localhost:11434`, so the scheduler's next batch fails transiently.
  Before writing the test, grep the four specs that run after
  `settings.e2e.ts` in `SPEC_FILES` (`wdio.conf.ts`) for `last_error`,
  `Last batch failed`, `pending_items` and scheduler status-line text. If
  any assertion could see the failure, move this `describe`'s file
  position instead of weakening that assertion: put the Captions flow in
  its own `captions.e2e.ts`, listed in `SPEC_FILES` immediately before
  `ingest.e2e.ts`, and record why in the constant's comment.
- [ ] **Step 2:** Build the debug bundle and run
  `pnpm test --spec specs/settings.e2e.ts` (handoff convention 19), then
  the full suite once.
- [ ] **Step 3: Run** `pnpm check && pnpm lint` in `apps/desktop/e2e`.
- [ ] **Step 4: Commit**:
  `git commit -m "test: the Captions save flow, end to end"`
- [ ] **Step 5: Hand check for the PR body** (the user's, on a signed or
  dev build): save an OpenRouter key in Settings; Keychain Access shows
  `majestical` / `openrouter-api-key`; `maj describer show` in a terminal
  says `(from keychain)` after one macOS allow prompt; Remove key deletes
  the item.

---

## PR Chunk 8 — phase close

### Task 11: cargo-mutants over the files this phase changed

Foreground, one at a time, `--in-place`, warm target, `git status` clean
after each (mandate 6):

```bash
cargo mutants --in-place -p majestical-secrets
cargo mutants --in-place -p majestical-services -f crates/services/src/describer_config.rs
cargo mutants --in-place -p majestical-describe -f crates/describe/src/client.rs
cargo mutants --in-place -p majestical-cli -f crates/cli/src/describer_key.rs
cd apps/desktop/src-tauri && cargo mutants --in-place -f src/captions.rs
```

- [ ] Run each; triage every survivor as killed-by-a-new-test (write it),
  equivalent (say why), or covered-at-another-head (name the test). On
  macOS the `system.rs` mutants are killed only by the round-trip test —
  if Task 4 gated it, run this one with `MAJ_KEYCHAIN_TESTS=1`.

### Task 12: watchlist, as-built, normalizers, handoff

**Files:**
- Modify: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md` — a
  "Phase 7G deferrals" section (the spec's Deferred list, plus everything
  reviews found, attributed per PR, plus Task 1's residual leak numbers)
  and a "cargo-mutants triage (phase 7G)" section.
- Modify: the spec — `## As-built (phase 7G)`, opening with the
  "Planning-time amendments" above and then per-PR deviations.
- Modify: `crates/cli/tests/services_parity.rs` — delete every TEMPORARY
  normalizer this phase added; rebuild the reference at the closing PR's
  merge-base and re-run `services_parity` and `tauri_parity` end to end;
  record the pass counts.
- Create: `docs/superpowers/HANDOFF-phase7H.md` in the 7G handoff's shape.
- Modify: `docs/RELEASING.md` only if the Keychain changed anything a
  release operator does (expected: no).

- [ ] Write, run the parity suites, `just check`, commit:
  `git commit -m "docs: phase 7G close — deferrals, mutants triage, 7H handoff"`

---

## Verification (end-to-end, per chunk)

- Chunk 2: the before/after directory counts; `just test` green.
- Chunks 3-5: `cargo test --workspace && just check`; `services_parity`
  against a merge-base reference; Task 6's macOS hand check.
- Chunk 6: desktop Rust tests + clippy; the four-command GUI line; fixtures
  regenerated and read for key-shaped strings.
- Chunk 7: the four-command GUI line; the e2e suite locally and `gui-e2e`
  on CI; Task 10's hand check recorded on the PR.
- Chunk 8: mutants triaged; parity re-run with the normalizers deleted;
  `rg -n "sk-" --glob '!*.md' .` hits only the literal `sk-test`,
  `sk-or-…` placeholder text and pre-existing test keys.
