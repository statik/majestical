I have a complete picture of the repo. Writing up the findings now.

# hyperdeck-adapter — implementation patterns

Repo: `/Users/emurphy/kindlyops/hyperdeck-adapter`. ~5,300 lines of Rust across 2 library crates + 1 Tauri app, ~83 unit tests, all inline. This is a **Rust port of a prior Go implementation** — nearly every module docstring says "port of `internal/...`", which explains several of the stylistic choices below (they mirror Go idioms rather than idiomatic Rust).

---

## 1. Workspace / crate decomposition

**Two workspaces, deliberately split.** `/Users/emurphy/kindlyops/hyperdeck-adapter/Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/hyperdeck-core", "crates/hyperdeck-os"]
# src-tauri is its own workspace (needs the desktop webview toolchain); keep it
# out of the library workspace so the Linux CI matrix never builds webkit.
exclude = ["src-tauri"]

[workspace.package]
edition = "2021"
license = "Apache-2.0"
repository = "https://github.com/kindlyops/hyperdeck-adapter"

[workspace.dependencies]
regex = "1"
```

`workspace.dependencies` is used for exactly one crate (`regex`); everything else is versioned per-crate. `workspace.package` carries edition/license/repository, inherited via `edition.workspace = true`.

Crates:

- **`hyperdeck-core`** (~2,500 LOC) — pure, zero-OS-dependency. Deps: `quick-xml`, `regex`, `serde`, `serde_json`, `serde_norway` (a maintained `serde_yaml` fork). Submodules: `domain/` (value objects), `port.rs` (all traits, single file), `app/` (services: `Session`, `VirtualDeck`, `LockManager`, `Reconciler`), `protocol/` (parser/responder/TCP server), `config/` (YAML profile store, JSON selection store, first-run seeding), `clipsource/`, `stateprobe/`, `testsupport.rs`.
- **`hyperdeck-os`** (~1,100 LOC) — driven adapters. Depends on `hyperdeck-core` only (one-directional). Platform code gated by `[target.'cfg(windows)'.dependencies]` / `[target.'cfg(target_os = "macos")'.dependencies]`. Hosts the `injcheck` binary in `src/bin/`.
- **`src-tauri`** (~500 LOC) — its own workspace with its own `Cargo.lock`; depends on both library crates by path.

**Hexagonal structure is real and clean.** All ports live in one file, `crates/hyperdeck-core/src/port.rs`, split by comment into driving (inbound) and driven (outbound):

```rust
/// Driven port: load validated profiles.
pub trait ProfileStore { fn load(&self) -> DeckResult<Vec<Profile>>; }

/// The deck's command surface (driving port).
pub trait Transport {
    fn play(&self) -> DeckResult<()>;
    fn stop(&self) -> DeckResult<()>;
    fn record(&self) -> DeckResult<()>;
    fn goto(&self, clip_id: i32) -> DeckResult<()>;
    fn next(&self) -> DeckResult<()>;
    fn prev(&self) -> DeckResult<()>;
    fn rehome(&self) -> DeckResult<()>;
}

/// The deck's read surface (driving port).
pub trait Query {
    fn transport_info(&self) -> TransportInfo;
    fn clips(&self) -> ClipList;
    fn slot_info(&self) -> SlotInfo;
    fn device_info(&self) -> DeviceInfo;
}

/// Driven port: deliver keystrokes to a window.
pub trait KeyInjector {
    fn focus(&self, w: &Window) -> DeckResult<()>;
    fn send_keys(&self, w: &Window, chords: &[Chord]) -> DeckResult<()>;
}

/// Driven port: list currently-open OS windows.
pub trait WindowEnumerator { fn open_windows(&self) -> DeckResult<Vec<Window>>; }

/// Driven port: perform a resolved transport action on the locked player through
/// an out-of-band control channel (e.g. an HTTP API or UI Automation) instead of
/// synthesizing keystrokes. Used by API/UIA control profiles.
pub trait PlayerController {
    fn control(&self, p: &Profile, w: &Window, key: KeyName) -> DeckResult<()>;
}

/// Driven port: produce the active clip list.
pub trait ClipSource { fn list(&self) -> DeckResult<ClipList>; }

/// Driven port: best-effort real-state detection. Returns `None` when the probe
/// cannot determine the state (Go's `(state, detected=false)`).
pub trait StateProbe { fn detect(&self, w: &Window) -> Option<TransportState>; }

/// Driven port: reflect lock status in the UI.
pub trait StatusPresenter { fn present(&self, lock: &LockState); }
```

Three patterns worth stealing:

**(a) Blanket impls for `Arc<T>` on the inbound ports** so a shared deck satisfies the trait directly (`port.rs:38-79`): `impl<T: Transport + ?Sized> Transport for std::sync::Arc<T>`. Lets `Server::new(Arc<VirtualDeck>)` hand cheap clones per connection without a wrapper type.

**(b) Named `Shared*` type aliases** colocated with the consumer, not in `port.rs`:
```rust
pub type SharedInjector    = Arc<dyn KeyInjector + Send + Sync>;      // app/virtualdeck.rs
pub type SharedClipSource  = Arc<dyn ClipSource + Send + Sync>;       // app/session.rs
pub type SharedStateProbe  = Arc<dyn StateProbe + Send + Sync>;       // app/session.rs
pub type SharedEnumerator  = Arc<dyn WindowEnumerator + Send + Sync>; // app/lockmanager.rs
pub type SharedPresenter   = Arc<dyn StatusPresenter + Send + Sync>;
```

**(c) Factory closures injected from the composition root** (`app/lockmanager.rs:10-12`), so the core never names a concrete adapter even when it must build one lazily per profile:
```rust
pub type ClipSourceFactory = Box<dyn Fn(&Profile) -> SharedClipSource + Send + Sync>;
pub type StateProbeFactory = Box<dyn Fn(&Profile) -> SharedStateProbe + Send + Sync>;
```
Wired in `src-tauri/src/backend.rs:120-123` as `Box::new(|p: &Profile| clipsource::new(p))`.

**Optional dependency via builder** (`app/virtualdeck.rs:41-46`): `VirtualDeck::new(session, injector).with_controller(controller)` — the controller is `Option<SharedController>` and its absence produces a runtime error only for profiles that need it.

**Concurrency model**: `Session` is a single `Mutex<Inner>` with narrow accessor methods. One thoughtful detail — `set_state_if_changed` folds read-modify-write into one lock acquisition specifically so concurrent transport commands can't make a wrong play/pause toggle decision (`app/session.rs:94-104`). Everything else is OS threads + `thread::sleep` polling; **no async runtime in the core at all**.

---

## 2. Error handling

**No `thiserror`, no `anyhow`.** A hand-rolled 3-variant enum with a manual `Display` impl is the entire error surface of the core (`crates/hyperdeck-core/src/error.rs`, 30 lines):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckError {
    /// A transport command arrived with no locked player
    NotLocked,
    /// An injector / controller failed to deliver an action.
    Injector(String),
    /// Any other failure carrying a human-readable message.
    Other(String),
}
pub type DeckResult<T> = std::result::Result<T, DeckError>;
```

Consequences, good and bad:

- `Clone + PartialEq` on the error makes assertions trivial: `assert_eq!(d.play(), Err(DeckError::NotLocked));` (`app/virtualdeck.rs:398`).
- Context is added by **`map_err` with a formatted string at every boundary**, always naming the operation and the path/input:
  ```rust
  std::fs::read(&self.path)
      .map_err(|e| DeckError::Other(format!("read config {:?}: {e}", self.path)))?;
  ```
  Error messages are consistently actionable, e.g. `vlchttp.rs:76`: `"vlc control {key:?}: unauthorized — check api.password matches VLC's HTTP password"`.
- The validation layer (`config/store.rs`) returns plain `Result<_, String>` internally and only lifts to `DeckError::Other` at the port boundary. Messages are uniformly prefixed with the profile id: `format!("profile {id:?}: invalid control {other:?} (want keys|api|uia)")` — the "(want X|Y|Z)" suffix pattern is used everywhere and reads well.
- `Err` is deliberately erased at the protocol edge (`protocol/responder.rs:110`): every failure becomes `"{CODE_INVALID_STATE} invalid state"` because the wire protocol has no richer vocabulary. Detail is lost, but that's the protocol's fault.
- **Weakness**: `String` payloads mean callers can't discriminate failures programmatically beyond the three variants, and `src-tauri/src/backend.rs::start` degrades further to `Result<Backend, String>`.

---

## 3. Testing

**There is no `tests/` directory anywhere.** Every test is an inline `#[cfg(test)] mod tests` at the bottom of its module — ~83 tests total, concentrated in `virtualdeck.rs` (16), `config/store.rs` (10), `responder.rs` (6), `vlchttp.rs`/`parser.rs`/`lockmanager.rs` (5 each). No cucumber, no proptest, no assert_cmd, no golden files, no snapshot testing, no mutation testing.

**Shared test doubles live in a non-test module**, `crates/hyperdeck-core/src/testsupport.rs`, gated by `#[cfg(test)] mod testsupport;` in `lib.rs` with a file-level `#![allow(dead_code)]` and a comment explaining why:

```rust
//! Shared in-memory test doubles (port of `injector.Mock`), available only to
//! the crate's own unit tests.
//!
//! Some helpers here are consumed by sibling modules' tests (e.g. the lock
//! manager), so not every item is exercised from within this module.
#![allow(dead_code)]
```

It provides `MockInjector` (`Mutex<MockState>` recording `focus`/`send_keys`, with injectable `focus_err`/`send_err`/`enum_err`), `FakePresenter`, `FakeClipSource`, `NoProbe`, `PlayingProbe`. The key affordance is `sent_keys() -> Vec<String>`, which flattens every chord's base key so assertions read as `assert_eq!(m.sent_keys(), vec!["space", "s"]);`.

**Test data**: `testdata/` (`profiles.yaml`, `sample.m3u`, `sample.xspf`) is referenced via a const built at compile time from the manifest dir — clean and CWD-independent:

```rust
const TESTDATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/profiles.yaml");
```

**`examples/` is not Cargo examples** — it's a single user-facing `examples/profiles.yaml`. Its notable trick (`config/default.rs:11`): it is *embedded into the binary* as the first-run seed, so the documented example and the shipped default cannot drift, and a test asserts the seed parses and contains every expected profile id:

```rust
pub const DEFAULT_PROFILES: &str = include_str!("../../../../examples/profiles.yaml");
```

**Other patterns worth noting:**
- Rejection tests use a one-line helper plus inline byte-string YAML, which keeps 8 negative cases compact: `fn rejects(yaml: &[u8]) { assert!(load_bytes(yaml).is_err()); }`.
- Real-socket integration tests inline in the unit module: `protocol/server.rs:105-190` binds `127.0.0.1:0`, spawns the server, connects a real `TcpStream`, asserts the banner and `200 ok`, then checks the mock injector recorded `["space"]`. `vlchttp.rs` does the same with a hand-rolled one-shot HTTP listener instead of a mocking crate.
- **Weakness**: temp files use `std::env::temp_dir()` + PID-suffixed names with manual `remove_file` cleanup (`config/selection.rs:68-74`, `config/default.rs:57`) rather than `tempfile`. Leaks on panic and is racy across concurrent runs of the same PID namespace.
- **Weakness**: the sleep-based synchronization in the TCP test (`thread::sleep(Duration::from_millis(50))` before asserting) is a flake vector.

---

## 4. Tauri integration

**This is the biggest divergence from what majestical needs: there are zero `#[tauri::command]` handlers and no IPC.** It's a tray-only app. `ui/index.html` is an 11-line placeholder that says "HyperDeck Adapter runs in the system tray." No frontend framework, no bundler for the app UI. `tauri.conf.json` has `"windows": []` and `"withGlobalTauri": false`.

So the useful lessons are about **composition and state**, not about commands.

**Composition root is a separate module from `main.rs`.** `src-tauri/src/backend.rs` builds the entire core over the OS adapters and returns a plain struct of handles — explicitly documented as UI-independent:

```rust
/// Handles the UI needs once the backend is running: `deck` powers Re-home, and
/// `lock_manager` / `selection` / `profile_ids` / `active` drive the tray Profile
/// submenu (pin a profile, persist it, move the checkmark).
pub struct Backend {
    pub deck: Arc<VirtualDeck>,
    pub lock_manager: Arc<LockManager>,
    pub selection: SelectionStore,
    pub profile_ids: Vec<String>,
    pub active: String,
}

pub fn start(presenter: Arc<dyn StatusPresenter + Send + Sync>, bind: &str, poll: Duration)
    -> Result<Backend, String>
```

Because `start` takes the presenter as a trait object, `main.rs` gets two UIs from one backend for free: `TrayPresenter` (updates tray tooltip + menu item text) and `LogPresenter` (headless mode). That's the cleanest idea in the repo — **the UI is a driven adapter behind a port, so `--headless` is not a special case, it's a different presenter.**

**State management is Rust-side `Arc`, not `tauri::State`.** Handles are moved into the `on_menu_event` closure:

```rust
.setup(|app| {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);  // menu-bar agent, no Dock icon

    let status_item: StatusSlot = Arc::new(Mutex::new(None));
    let presenter: Arc<dyn StatusPresenter + Send + Sync> = Arc::new(TrayPresenter {
        app: app.handle().clone(),
        status_item: status_item.clone(),
    });
    let backend = backend::start(presenter, BIND, POLL)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    build_tray(app, backend, status_item)?;
    Ok(())
})
```

Note the `StatusSlot = Arc<Mutex<Option<MenuItem<tauri::Wry>>>>` chicken-and-egg workaround: the presenter must exist before the menu, so it holds an empty slot the tray fills in later.

**Async**: only for the updater. `tauri::async_runtime::spawn(async move { check_for_updates(handle).await })` inside the menu handler; everything else is blocking on OS threads spawned in `backend::start`.

**Plugins**: `tauri-plugin-updater` + `tauri-plugin-process` + `tauri-plugin-dialog`, all driven from Rust, so `capabilities/default.json` grants only `core:default`, `updater:default`, `process:default`, `dialog:default` with a comment explaining exactly that. Good minimal-capability hygiene to copy.

**`site/` is a separate Vite + TypeScript + three.js marketing site** deployed to GitHub Pages (`base: '/hyperdeck-adapter/'`, `npm run build` = `tsc --noEmit && vite build`). Its `tsconfig.json` sets `strict`, `noUnusedLocals`, `noUnusedParameters`, `noFallthroughCasesInSwitch`, `isolatedModules` — but *not* `noUncheckedIndexedAccess` or `exactOptionalPropertyTypes`, and it uses caret ranges (`"three": "^0.184.0"`), both contrary to the global standards in your CLAUDE.md.

---

## 5. CLI / binary patterns

**No clap, no JSON output.** `crates/hyperdeck-os/src/bin/injcheck.rs` (127 lines) hand-matches `args[1]` against a fixed verb list, with `usage()` and `fail()` as `-> !` functions that `exit(2)`/`exit(1)`:

```rust
fn usage() -> ! {
    eprintln!("usage: injcheck trust | list [filter] | focus <pid> | keys <pid> <chord...> | bgkeys <pid> <chord...>");
    exit(2);
}
fn fail(msg: &str) -> ! { eprintln!("injcheck: {msg}"); exit(1); }
```

Output is column-formatted human text (`println!("{:<8}  {:<28}  TITLE", "HANDLE", "PROCESS")`). Errors are handled with `.unwrap_or_else(|e| fail(&format!("focus: {e}")))` throughout — concise, but it means the binary is untestable as a unit and there's no machine-readable mode. **For majestical's CLI, this is the pattern to depart from, not follow** — you'll want clap + a `--json` output convention if the CLI is meant to be agent-drivable.

The Tauri binary does flag parsing the same crude way: `args.iter().any(|a| a == "--headless")`, `args.iter().any(|a| a == "--check-accessibility")`.

`bin/` at the repo root contains **two untracked 5.4 MB / 3.1 MB Mach-O binaries** (`git status` shows `?? bin/`) that `.gitignore` doesn't cover — it ignores `/hyperdeck-adapter` but not `/bin/`. Minor, but one `git add -A` away from an 8.5 MB commit.

---

## 6. Repo conventions

**`justfile`** — thin, no dependency graph between recipes, documents the *why* in comments:

```make
bind := "127.0.0.1:9993"

default:
    @just --list

# check + lint + test the Rust library crates (the OS-independent core + adapters)
test:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

fmt:
    cargo fmt --all
    cd src-tauri && cargo fmt

build:
    cd src-tauri && cargo tauri build --debug
run:
    cd src-tauri && cargo tauri dev
trust:
    cd src-tauri && cargo run -- --check-accessibility
serve:
    cd src-tauri && cargo run -- --headless

injcheck *ARGS:
    cargo run -q -p hyperdeck-os --bin injcheck -- {{ARGS}}

demo:
    python3 scripts/hyperdeck-demo.py {{bind}}
```

Note `just test` deliberately excludes `src-tauri` (separate workspace), so the local command and the CI job diverge — CI runs a separate `tauri` job for fmt+clippy there.

**`docs/`** — two structures side by side: `docs/*.html` (self-contained design memos, matching your `design-memo` skill) and `docs/superpowers/{plans,specs}/YYYY-MM-DD-<slug>.md`. The plans are large (93 KB for the initial build) and contain literal per-task TDD steps with the test code inline, prefixed by a worker directive:

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

They also open with a **File Structure** block enumerating every Create/Modify with a one-line rationale. Worth replicating for majestical's phase plans.

**`scripts/`** — one file, `hyperdeck-demo.py`, a stdlib-only Python client that drives the TCP protocol end-to-end for manual verification. Cheap and effective for a protocol server; the equivalent for majestical would be a script that exercises the CLI against a scratch catalog.

**CI** (`.github/workflows/`, three workflows):
- `ci.yml` — `rustfmt` job (ubuntu only), `rust` job (3-OS matrix, `fail-fast: false`, clippy `-D warnings` + `cargo test --workspace`), `tauri` job (macOS+Windows only, `defaults.run.working-directory: src-tauri`, fmt+clippy but **no `cargo test`** and no `tauri build`).
- `release.yml` — tag-triggered `tauri-action`, top-level `permissions: {}` with per-job escalation, `persist-credentials: false` on checkout, and the only SHA-pinned action (`tauri-apps/tauri-action@84b9d35...  # v0.6.2`). Sophisticated graceful-degradation: signing steps are gated on `secrets.X != ''` and updater artifacts auto-disable when no signing key exists, so releases build unsigned rather than failing.
- `pages.yml` — path-filtered on `site/**`, `concurrency: {group: pages, cancel-in-progress: false}`.

**CI weaknesses**: `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `actions/setup-node@v4` are tag-pinned, not SHA-pinned (contrary to your global standard, and `zizmor` would flag it); `ci.yml` has no top-level `permissions:` block and no `persist-credentials: false`; there is no cargo cache anywhere; no `cargo deny`, no `cargo audit`, no Dependabot config.

**Cargo lint configuration: none.** There is no `[lints.clippy]` or `[lints.rust]` section in any of the four manifests. Lint enforcement is entirely `-D warnings` on default clippy in CI — no `pedantic`, no `unwrap_used`/`expect_used`/`panic` denials. The code correspondingly uses `.unwrap()` on every mutex lock (`self.inner.lock().unwrap()`, dozens of sites) and `.expect("bundle icon")` in the tray builder. **This is the single biggest gap versus your global standards and the easiest thing to fix at the start of majestical rather than retrofitting.**

Edition is 2021 across all crates (2024 is available and would be the current default).

---

## 7. Notably good / notably problematic

**Good, worth copying directly:**

1. **UI as a driven port.** `StatusPresenter` + `backend::start(presenter, ...)` means headless and tray are two adapters over one composition root (`src-tauri/src/backend.rs:79`, `src-tauri/src/main.rs:41-56`). For majestical this maps to: Tauri UI and CLI both being adapters over the same core service struct, with no logic in either.
2. **Separate workspace for the Tauri app** so the library CI matrix (including Linux) never installs the webview toolchain. Both `Cargo.toml:5` and `src-tauri/Cargo.toml:8` carry the comment explaining it. Costs you a second `Cargo.lock` and a divergent `just test`.
3. **`include_str!` the example config as the first-run seed** (`config/default.rs:11`) with a test asserting the seed parses — kills doc/default drift permanently.
4. **Split the user's hand-edited config from machine-written state.** `profiles.yaml` (user-owned, commented, never rewritten) vs `selection.json` (app-owned), with the rationale in the docstring (`config/selection.rs:9-11`). Directly relevant to majestical: user config file vs SQLite catalog vs app state.
5. **Validate-and-convert at the config boundary.** `config/store.rs` deserializes into private `*Schema` structs of `String`s, then `convert()` produces the domain `Profile` with real enums — the domain type is unconstructable in an invalid state, and every rejection has a targeted message. 10 tests, 8 of them negative.
6. **Factory closures (`Box<dyn Fn(&Profile) -> Shared*>`) injected from the composition root** so the core builds per-entity adapters without naming them (`app/lockmanager.rs:10-12`).
7. **Graceful CI degradation on missing secrets** (`release.yml:44-52, 96-113`) — the release pipeline works end-to-end from day one without any signing credentials.

**Problematic, worth avoiding:**

1. **No `[lints]` configuration anywhere.** Combined with ubiquitous `.unwrap()` on mutex locks, a poisoned mutex panics the whole app. Set `[lints.clippy] pedantic/unwrap_used/expect_used` in `[workspace.lints]` on day one of majestical and inherit it in every crate.
2. **Stringly-typed config in the domain.** `ClipSourceConfig.kind: String` and `StateConfig.kind: String` survive validation as raw strings and are re-matched at runtime in `clipsource::new` / `stateprobe::new`, where the fallback arm silently accepts unknown values: `// "positional" and unknown -> positional`. A typo in `clip_source.type` is not an error, it's a silent behavior change. These should be enums parsed at load time like `ControlMode` already is.
3. **Go-isms carried into Rust.** `DeckError::Other(String)` instead of typed variants; `active: String` with `""` meaning "Auto" instead of `Option<String>` (`app/lockmanager.rs:28`, and `checked_profile`/`validate_active` in `main.rs`/`backend.rs` exist purely to defend that sentinel); `KeyName::parse -> Option` silently dropping unmodeled actions. Every module docstring reads "port of `internal/...`" — useful provenance during the port, but it's now permanent noise pointing at a repo that isn't here.
4. **Tauri app has no `cargo test` in CI** (`ci.yml:31-49` runs only fmt + clippy), so the three `checked_profile` tests in `src-tauri/src/main.rs` never run in CI. Since majestical's Tauri layer will hold real command handlers, this gap matters much more there.
5. **Manual temp-file handling in tests** instead of `tempfile` (`config/selection.rs:68`, `config/default.rs:57`) — leaks on failure, PID-collision-prone.
6. **`bin/` holds 8.5 MB of untracked build artifacts** not covered by `.gitignore`.
7. **Polling everywhere.** Two `loop { work(); thread::sleep(POLL) }` threads at 1 Hz (`backend.rs:139-152`). Fine for window enumeration; a bad template for a file catalog, where you'd want filesystem watching.
