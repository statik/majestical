# Phase 7B: GUI Slice + Release Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the 28 services-crate stderr diagnostics into outcome-struct `notices` (plus the schemars-enum and dry-run-validation watchlist fixes), then land the first Tauri 2 + Svelte 5 desktop slice (Search + Volumes, three-pane layout C) over `crates/services`, with the full release pipeline (tauri-action draft releases, armed auto-updater, cargo-about, version-sync, 3-OS GUI CI) from the first Tauri commit.

**Architecture:** `crates/services` gains a `Notices` sink (owned by `App`, threaded as `&Notices` where no `App` exists); every outcome-returning service fn drains collected notices into a `notices: Vec<String>` field on its outcome, so all three heads see the same diagnostics. The CLI prints drained notices verbatim to stderr — both CLI streams stay **byte-identical**, proven by the existing CI reference-binary parity harness. The GUI is `apps/desktop`: a separate cargo workspace (`src-tauri`) whose `#[tauri::command]`s are thin wrappers returning services outcome structs as-is; thumbnails ship over a `thumb://` custom protocol, never IPC.

**Tech Stack:** Rust workspace edition 2024, strict clippy. GUI: Tauri 2 + Svelte 5 (runes) + Vite + TypeScript strict, pnpm (exact pins, ignore-scripts, 24h minimum release age), oxlint/oxfmt/vitest. Release: tauri-action, tauri updater + signer, cargo-about. **Every new dependency version below is a placeholder to verify at execution time** — check crates.io / `pnpm view <pkg> version` / the GitHub release page before writing it down.

**Spec:** `docs/superpowers/specs/2026-08-04-phase7b-gui-release-design.md` — read it first, plus the phase 7 spec's GUI + release sections it instantiates (`2026-08-02-phase7-agent-surface-gui-design.md`).

**Process conventions (mandatory):**
- TDD every task: failing test → verify fail → implement → verify pass → commit.
- Stage ONLY your files, never `git add -A`. No Claude-Session trailers. Never push to main. Push via `git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git <branch>`.
- `just check` runs fmt + clippy (also the prek hook — first hook run of a session can take >2 min, that is not a hang). `cargo test -p <crate>` for the crate you touched.
- Long-running verification steps (cargo-mutants, release builds) run FOREGROUND, one at a time — no `run_in_background`, no monitors (phase 6/7A lesson: subagents park on background children that die with their turn).
- PR chunks (squash-merge after green CI): Tasks 1-3 = PR1, Tasks 4-5 = PR2, Task 6 = PR3, Task 7 = PR4, Task 8 = PR5, Task 9 = PR6, Tasks 10-11 = PR7.

## The byte-identical rule (Tasks 1-3)

The CLI's stdout AND stderr for every existing invocation must not change through the notices migration. The proof is the existing harness:

- `crates/cli/tests/services_parity.rs` diffs stdout + stderr + exit code against `/tmp/maj-ref`. Locally: `cargo build -p majestical-cli && cp target/debug/maj /tmp/maj-ref` **from the merge-base commit before your first change**, then run `cargo test -p majestical-cli --test services_parity`. In CI the reference is built automatically from `git merge-base origin/main HEAD` — every PR gets a real run.
- The full smoke suites (`cli_smoke`, `inbox_smoke`, `sync_smoke`, `index`/`search` suites, cucumber) assert stderr content in places; they are the regression net. Run `just test` before each PR. (If a smoke test asserts a notice appears on stderr, the migration must keep that true — the CLI still prints every notice, just from the drained buffer.)

Key facts that make byte-parity tractable (verified against the code, 2026-08-04):

1. The CLI **hand-renders** all of its JSON/text output (e.g. `print_search_results_json` builds its own `json!` payload; `sync_cmd`/`inbox_cmd` render row-by-row). No CLI path serializes an outcome struct directly to stdout, so adding fields to outcome structs cannot leak into CLI stdout.
2. Parity compares stdout and stderr **as separate streams**, so "services printed stderr mid-compute" → "CLI prints the same lines after the service call returns" preserves each stream byte-for-byte as long as the per-stream line order is preserved. Within a verb, all notices go through ONE chronologically-ordered buffer (the app's, or a single local `Notices`), so order is preserved by construction.
3. MCP wire pins survive because `notices` is `#[serde(skip_serializing_if = "Vec::is_empty", default)]` — shapes only change when a notice actually occurs, and no existing `mcp_smoke` pin covers a notice-producing input.

## The notices design (Tasks 1-3) — decision matrix

One rule per situation. Do not invent a fourth mechanism.

| Situation | Mechanism |
| --- | --- |
| Code with `&FsApp`/`&mut FsApp` in reach (incl. deep helpers — pass `app.notices()` down as `&Notices`) | push into `app.notices()` |
| Free function with no `App` anywhere above it (`state_dir::*`, sync config-path helpers, `describer_config::*`, `tags::reject`) | takes a `notices: &Notices` parameter; the compiler finds every caller |
| Service fn returning an outcome struct | drains `app.notices()` (and/or its local `Notices`) into `outcome.notices` immediately before `Ok(...)` |
| Service fn returning `()` (tag_add/tag_rm/confirm, meta_set, searches_rm, location_add/rm, …) | leaves notices in the buffer/param; the **head** drains |
| CLI head | prints `outcome.notices` lines verbatim to stderr before rendering stdout; `main.rs` arms that opened an app also drain leftovers after dispatch (covers ()-verbs and error paths) |
| MCP head | outcome-serializing tools get `notices` for free; `json!`-building write tools fold drained notices in via a `with_notices` helper; app-less tools own a local `Notices` and do the same |
| GUI head (Task 7+) | commands return outcome structs — `notices` is already in the payload |

`Notices` uses a `Mutex` (not `RefCell`) so `App` stays `Sync`; `push` through `&self` is what lets `App::events(&self)` record without a signature change rippling through every verb.

## File structure

**Chunk 0 (Tasks 1-5):**
- `crates/services/src/notices.rs` — create: the `Notices` sink.
- `crates/services/src/{app,state_dir,catalog,search,tags,inbox,sync,ingest,index/{mod,run,heal}}.rs` — modify: sites → sink; outcome structs gain `notices`; delete every `#[expect(clippy::print_stderr)]`.
- `crates/services/src/{meta,scan,para,volumes,describer_config}.rs` — modify: outcome drain / `&Notices` params (no eprintln sites of their own).
- `crates/services/src/{tags,para,ingest,describer_config}.rs` — modify: the four wire enums (`TagOp`, `ParaOp`, `DedupeMode`, `DescriberBackend`).
- `crates/services/Cargo.toml` — modify: add `schemars.workspace = true`.
- `crates/cli/src/main.rs` — modify: post-dispatch leftover drain.
- `crates/cli/src/{search,commands,tags_cmd,inbox_cmd,index_cmd,sync_cmd,describer_cmd}.rs` — modify: print `outcome.notices` / pass `&Notices`.
- `crates/cli/src/mcp_cmd/write_tools.rs` — modify: `with_notices`, enum params, dry-run `ensure_asset_known`.
- `crates/cli/tests/mcp_smoke.rs` — modify: new tests + snapshot update.

**GUI (Tasks 6-9):**
- `apps/desktop/` — create: `package.json`, `.npmrc`, `vite.config.ts`, `tsconfig.json`, `index.html`, `src/` (Svelte app), `src-tauri/` (own cargo workspace: `Cargo.toml`, `tauri.conf.json`, `build.rs`, `src/{main,lib,commands,thumb_protocol}.rs`, `capabilities/default.json`, `icons/`, `tests/`).
- `justfile` — modify: `gui-install`, `gui-check`, `gui-test`, `gui-build`, `version-sync` recipes.
- `.github/workflows/ci.yml` — modify: `gui` matrix job + `version-sync` step.

**Release (Task 10):**
- `.github/workflows/release.yml` — create.
- `about.toml`, `about.hbs` — create (cargo-about).
- `apps/desktop/src-tauri/tauri.conf.json` — modify: updater `pubkey` + endpoint.

---

### Task 1: The `Notices` sink — core plumbing (`notices.rs`, `app.rs`, `state_dir.rs`, CLI leftover drain)

**Files:**
- Create: `crates/services/src/notices.rs`
- Modify: `crates/services/src/lib.rs`, `crates/services/src/app.rs`, `crates/services/src/state_dir.rs`, `crates/services/src/catalog.rs:26-32` (`open_catalog`), every `crate::state_dir::{catalog_paths,state_dir_for}` caller inside `crates/services` (`inbox.rs:96`, `search.rs:421`, `index/mod.rs:273,336`, `index/run.rs:231`, `ingest.rs:271`, `catalog.rs:27`, `sync.rs:86`, `tags.rs:146` — plus the module-test callers the compiler flags), `crates/cli/src/main.rs`

**Important:** In this task the migrated notices land in the app buffer and the CLI drains them; **outcome fields arrive in Tasks 2-3**. To keep MCP from silently losing the four app/state-dir notices between Task 1 and Task 2, Task 1 only migrates the mechanism; the per-verb outcome drains land in the same PR (Tasks 1-3 = PR1, never shipped separately).

- [ ] **Step 1: Build the parity reference (once for PR1)**

```bash
git stash --include-untracked   # if you have anything uncommitted
merge_base=$(git merge-base origin/main HEAD)
git checkout --detach "$merge_base"
cargo build -p majestical-cli && cp target/debug/maj /tmp/maj-ref
git checkout -   # back to your branch
git stash pop 2>/dev/null || true
```

- [ ] **Step 2: Write the failing unit tests**

Create `crates/services/src/notices.rs` with only the test module first:

```rust
//! A shared, order-preserving sink for user-facing diagnostics that used to
//! go straight to stderr. Services push; heads drain — the CLI prints each
//! line verbatim to stderr, MCP/GUI serialize them as `notices` fields on
//! outcome structs. Interior mutability (a `Mutex`, so `App` stays `Sync`)
//! lets `&self` methods like `App::events` record without a signature
//! change rippling through every verb.
use std::sync::Mutex;

#[cfg(test)]
mod tests {
    use super::Notices;

    #[test]
    fn push_then_drain_preserves_order_and_empties() {
        let notices = Notices::default();
        notices.push("first");
        notices.push("second".to_string());
        assert_eq!(notices.drain(), vec!["first", "second"]);
        assert!(notices.drain().is_empty(), "drain must empty the buffer");
    }
}
```

Add `pub mod notices;` to `crates/services/src/lib.rs` (alphabetical position, after `meta`).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p majestical-services notices`
Expected: FAIL — `Notices` not defined.

- [ ] **Step 4: Implement `Notices`**

Above the test module in `notices.rs`:

```rust
#[derive(Default)]
pub struct Notices(Mutex<Vec<String>>);

impl Notices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one diagnostic line, exactly as it used to appear on stderr.
    pub fn push(&self, message: impl Into<String>) {
        // A poisoned lock means another thread panicked mid-push; the
        // buffer itself is still valid Vec data — keep collecting rather
        // than dropping diagnostics on the floor.
        let mut buf = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.push(message.into());
    }

    /// Takes every collected line, in push order, leaving the sink empty.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        let mut buf = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *buf)
    }
}
```

- [ ] **Step 5: Verify pass**

Run: `cargo test -p majestical-services notices`
Expected: PASS.

- [ ] **Step 6: Failing test — `App` records the corrupt-line warning instead of printing it**

In `crates/services/src/app.rs`'s existing `#[cfg(test)]` module (create one if absent — check first):

```rust
#[test]
fn corrupt_log_line_becomes_a_notice_not_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let mut app = FsApp::init(&root, "m1", "m1").expect("init");
    app.emit(vec![majestical_core::event::Op::SavedSearchSet {
        name: "n".into(),
        query: "q".into(),
    }])
    .expect("emit");
    // Corrupt the log: append a non-JSON line to the machine's events file.
    let events_dir = root.join("events");
    let log_file = std::fs::read_dir(&events_dir)
        .expect("events dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one events jsonl");
    let mut bytes = std::fs::read(&log_file).expect("read log");
    bytes.extend_from_slice(b"this is not json\n");
    std::fs::write(&log_file, bytes).expect("re-write log");

    let events = app.events().expect("events reads through corruption");
    assert_eq!(events.len(), 1);
    let notices = app.notices().drain();
    assert_eq!(notices.len(), 1, "exactly one corrupt-line notice: {notices:?}");
    assert!(
        notices[0].contains("skipped 1 corrupt event log line(s)"),
        "verbatim message preserved: {}",
        notices[0]
    );
}
```

(If the events-file layout differs from `events/*.jsonl`, look at `FileEventLog` in `crates/sync` and adjust the corruption step — the test's point is a real corrupt line, not a particular path.)

Run: `cargo test -p majestical-services corrupt_log_line` → FAIL (`notices` method missing).

- [ ] **Step 7: Migrate `app.rs`'s two sites**

- Add to `App<L>`: field `notices: crate::notices::Notices`, initialized `Notices::new()` in both `FsApp::open` and `FsApp::init`; accessor:

```rust
/// The diagnostics sink every head drains — see `crate::notices`.
pub fn notices(&self) -> &crate::notices::Notices {
    &self.notices
}
```

- Replace `warn_skipped_corrupt_lines` (delete the whole `#[expect]` block) with:

```rust
/// Records a notice when reading an event log skipped corrupt lines.
/// Shared by `App::events` and `catalog::open_catalog` so the message
/// can't drift between the two call sites.
pub fn note_skipped_corrupt_lines(skipped: usize, catalog_root: &Path, notices: &crate::notices::Notices) {
    if skipped > 0 {
        notices.push(format!(
            "warning: skipped {skipped} corrupt event log line(s) in {}/events — damaged transport; affected metadata may be missing",
            catalog_root.display()
        ));
    }
}
```

  The message string must stay **byte-identical** — copy it, don't retype it.
- `App::events`: `note_skipped_corrupt_lines(skipped, &self.catalog_root, &self.notices);`
- `App::emit`'s clamp warning: replace the `#[expect]`/`eprintln!` block with `self.notices.push(format!("warning: {clamped} event(s) carry timestamps more than 24h in the future (worst: ~{days_ahead}d ahead) — a peer's clock may be wrong; ordering was clamped locally"));`
- `crates/services/src/catalog.rs:32`: `warn_skipped_corrupt_lines(skipped, catalog_dir)` → `note_skipped_corrupt_lines(skipped, catalog_dir, app.notices())` (and fix the `use` on line 9).

- [ ] **Step 8: Migrate `state_dir.rs`'s two sites**

Change signatures (the compiler then walks you through every caller):

```rust
pub fn catalog_paths(catalog_root: &Path, notices: &crate::notices::Notices) -> Result<CatalogPaths>
pub fn state_dir_for(catalog_root: &Path, notices: &crate::notices::Notices) -> Result<PathBuf>
fn migrate_legacy(catalog_root: &Path, state_runs: &Path, notices: &crate::notices::Notices) -> Result<()>
fn migrate_legacy_journals(legacy_runs: &Path, state_runs: &Path, notices: &crate::notices::Notices) -> Result<()>
```

Both `eprintln!` blocks become `notices.push("note: removed legacy catalog.db from the sync root (rebuilt locally)")` / `notices.push("note: moved legacy run journals into the local state dir")` (delete the `#[expect]`s). Add a unit test in `state_dir.rs`'s test module driving `migrate_legacy` directly:

```rust
#[test]
fn legacy_catalog_db_removal_is_a_notice() {
    let root = tempfile::tempdir().expect("tempdir");
    let state_runs = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join("catalog.db"), b"legacy").expect("plant legacy db");
    let notices = crate::notices::Notices::new();
    migrate_legacy(root.path(), state_runs.path(), &notices).expect("migrate");
    let drained = notices.drain();
    assert_eq!(
        drained,
        vec!["note: removed legacy catalog.db from the sync root (rebuilt locally)".to_string()]
    );
    assert!(!root.path().join("catalog.db").exists());
}
```

Caller updates inside `crates/services` (all have an app or an obvious local sink at this point in the chain — where the enclosing fn has neither, add a `notices: &Notices` parameter and keep propagating up; Tasks 2-3 connect the tops of those chains to outcomes/heads):

| Caller | Pass |
| --- | --- |
| `catalog.rs:27` (`open_catalog`) | `app.notices()` |
| `search.rs:421` | `app.notices()` (fn has `args.catalog_dir`; check the enclosing fn — it is on the search path, which has the app; thread the parameter if not) |
| `index/mod.rs:273,336`, `index/run.rs:231` | `app.notices()` (both `status` and `run` take `&FsApp`) |
| `ingest.rs:271` | `app.notices()` |
| `inbox.rs:96` (`markers_path`) | add `notices: &Notices` param to `markers_path`/`load_markers`/`store_markers`, pass `app.notices()` from `process` |
| `sync.rs:86` (`config_path`) | add `notices: &Notices` param to `config_path` and thread up through `locations_list`/`location_add`/`location_rm`/`status`/`push`/`pull` as a new leading-edge parameter (heads own the sink — see Task 3 Step 6) |
| `tags.rs:146` (`rejections_path`) | add `notices: &Notices` param; `suggestions`/`confirm` pass `app.notices()`; `reject(catalog, …)` gains a `notices: &Notices` parameter |
| test-module callers | a local `Notices::new()` |

`describer_config.rs` also resolves the state dir (check for `state_dir_for` there; the grep may have missed a wrapped call) — same treatment: `notices: &Notices` parameter on `show`/`set`/`test` if so, otherwise leave untouched.

- [ ] **Step 9: CLI — print the drained buffer**

In `crates/cli/src/main.rs`, add:

```rust
/// Prints every service-collected diagnostic to stderr, verbatim and in
/// order — the CLI head's half of the `crates/services` notices contract.
/// Called after each dispatch that opened an app, including on the error
/// path, so a warning collected before a failure still reaches the user.
fn print_notices(app: &FsApp) {
    for line in app.notices().drain() {
        eprintln!("{line}");
    }
}
```

Then in **every** `main()` match arm that constructs an `FsApp` (lines ~557-658: Scan, Tag, Tags, Search, Searches, Index, Inbox, Volumes, Meta, Para, Ingest), capture the dispatch result, drain, then propagate:

```rust
Cmd::Tag { cmd } => {
    let mut app = FsApp::open(&cli.catalog, &cli.machine_id, &author)?;
    let result = commands::cmd_tag(&mut app, cmd);
    print_notices(&app);
    result?;
}
```

Apply the same shape to each arm. For `Cmd::Sync` and `Cmd::Describer` (no app), Task 3 Step 6 gives the CLI-owned `Notices` treatment.

- [ ] **Step 10: Make it compile, run the tests**

Run: `cargo test -p majestical-services && cargo test -p majestical-cli --test services_parity`
Expected: services tests PASS (including both new ones); parity tests PASS against `/tmp/maj-ref` (not SKIP — if they print SKIP, redo Step 1).

- [ ] **Step 11: Run the wider net**

Run: `just check && cargo test -p majestical-cli`
Expected: zero warnings, all green. Any smoke test that asserted one of these four messages on stderr must still pass unmodified — if one fails, the CLI is printing in the wrong place (or not at all), not the test.

- [ ] **Step 12: Commit**

```bash
git add crates/services/src/notices.rs crates/services/src/lib.rs crates/services/src/app.rs \
  crates/services/src/state_dir.rs crates/services/src/catalog.rs crates/services/src/inbox.rs \
  crates/services/src/sync.rs crates/services/src/tags.rs crates/services/src/search.rs \
  crates/services/src/index crates/services/src/ingest.rs crates/cli/src/main.rs \
  crates/cli/src/sync_cmd.rs crates/cli/src/tags_cmd.rs crates/cli/src/describer_cmd.rs
git commit -m "feat: services Notices sink replaces app/state_dir stderr prints"
```

(Adjust the file list to what you actually touched; never `git add -A`.)

---

### Task 2: `notices` outcome fields — search, tags, catalog, volumes (+ MCP for free)

**Files:**
- Modify: `crates/services/src/search.rs` (6 sites + `SearchOutcome`), `crates/services/src/tags.rs` (1 site + `SuggestionsOutcome`), `crates/services/src/catalog.rs` (`AssetDetail`), `crates/services/src/volumes.rs` (`VolumesOutcome`), `crates/services/src/meta.rs`/`scan.rs`/`para.rs` (drain-only), `crates/cli/src/search.rs`, `crates/cli/src/commands.rs`, `crates/cli/src/tags_cmd.rs`
- Test: `crates/cli/tests/mcp_smoke.rs`

The uniform drain rule: **every service fn that returns an outcome struct and has an app drains `app.notices()` into `outcome.notices` immediately before returning Ok.** The field, on every outcome it is added to, is exactly:

```rust
/// Diagnostics collected during this operation, verbatim — the lines the
/// CLI prints to stderr. Absent from the wire when empty.
#[serde(skip_serializing_if = "Vec::is_empty", default)]
pub notices: Vec<String>,
```

(`default` matters: `PullOutcome`-style structs get deserialized in tests.)

- [ ] **Step 1: Failing test — SearchOutcome carries the corrupt-line notice**

In `crates/services/src/search.rs`'s test module (reuse the corruption helper shape from Task 1 Step 6 — if both files need it, move the "append a garbage line" block into a small `#[cfg(test)]` helper where each test module can reach it, or duplicate the 6 lines; do NOT export a public helper for it):

```rust
#[test]
fn search_outcome_carries_collected_notices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let mut app = FsApp::init(&root, "m1", "m1").expect("init");
    app.emit(vec![Op::AssetSeen {
        asset: AssetId("xxh3:0123456789abcdef0123456789abcdef".into()),
        volume: "vol1".into(),
        path: "clip.txt".into(),
        size: 5,
        mtime_ms: 1000,
    }])
    .expect("emit");
    corrupt_event_log(&root); // the Task-1 helper shape
    let state = tempfile::tempdir().expect("state");
    // SAFETY expectation: search resolves the state dir via env; the test
    // harness elsewhere sets MAJ_STATE_DIR per child process. Here we pass
    // through the catalog_dir-based API directly — follow the existing
    // search test setup in this module for the state-dir arrangement.
    let outcome = search(
        &mut app,
        &root,
        &SearchRequest { query: Some("tag:missing".into()), limit: 10, saved: None, save: None },
    );
    let outcome = outcome.expect("search");
    assert!(
        outcome.notices.iter().any(|n| n.contains("corrupt event log line")),
        "notices must surface the corrupt-line warning: {:?}",
        outcome.notices
    );
}
```

(Adapt setup to this module's existing tests — they already construct catalogs and state dirs; mirror them. A `tag:` filter query avoids the semantic layer entirely, keeping the test model-cache-independent.)

Run: `cargo test -p majestical-services search_outcome_carries` → FAIL (no `notices` field).

- [ ] **Step 2: Implement in `search.rs`**

- Add the `notices` field to `SearchOutcome` (after `text_coverage`).
- The six `#[expect]`/`eprintln!` sites (`:879`, `:890`, `:909`, `:1042`, `:1053`, `:1067`) sit in `semantic_candidates`/`text_semantic_candidates`, which don't see the app. Add a `notices: &crate::notices::Notices` parameter to both (and to `run_search`, which calls them), passed from `search_impl` as `app.notices()`. Each site becomes e.g. `notices.push(miss.note());` — delete the `#[expect]` blocks.
- In `search_impl`, before `Ok(outcome)` (note: AFTER the `--save` emit, so a clamp warning from that emit is captured too):

```rust
let mut outcome = outcome;
outcome.notices = app.notices().drain();
Ok(outcome)
```

- `searches_list` returns `Vec<SavedSearch>` (no struct) and only ever hits app-level warnings — leave it; heads drain (CLI already does via Task 1 Step 9; the GUI command wraps it in Task 7).

- [ ] **Step 3: Implement in `tags.rs`, `catalog.rs`, `volumes.rs`, drain-only modules**

- `tags.rs:238` site: `pending_suggestions` gains `notices: &Notices` (from `suggestions`'s `app.notices()`); message preserved verbatim. `SuggestionsOutcome` gains `notices`; `suggestions` drains before Ok.
- `catalog.rs`: `AssetDetail` gains `notices`; `get_asset` drains into it before returning `Ok(Some(detail))` (the `Ok(None)` arm has nowhere to put notices — leave them for the head's leftover drain).
- `volumes.rs`: `VolumesOutcome` gains `notices`; `volumes_list` drains.
- `meta.rs` (`MetaOutcome`), `scan.rs` (`ScanOutcome`), `para.rs` (`ParaOutcome`, `ArchiveOutcome`): same field + drain — no sites of their own, but app warnings must reach MCP/GUI through these outcomes too.

- [ ] **Step 4: CLI renderers print outcome notices**

- `crates/cli/src/search.rs::cmd_search`: after the service call, before `print_search_results`: `for line in &outcome.notices { eprintln!("{line}"); }`
- `crates/cli/src/tags_cmd.rs::cmd_suggestions`, `crates/cli/src/commands.rs` (`cmd_volumes_list`, `cmd_meta`'s get path, `cmd_scan`, `cmd_para`, asset-detail printing if any): same loop before stdout rendering.
- `main.rs`'s `print_notices` still runs after dispatch — it now prints only what no outcome drained (e.g. a verb that failed midway). Nothing prints twice because `drain` empties the buffer.

- [ ] **Step 5: Verify parity + services tests**

Run: `cargo test -p majestical-services && cargo test -p majestical-cli --test services_parity && cargo test -p majestical-cli --test cli_smoke`
Expected: all PASS. The parity suite is the byte-identical proof for this task.

- [ ] **Step 6: MCP smoke test — notices reach the wire**

In `crates/cli/tests/mcp_smoke.rs` (fixture helpers live in `tests/common/mod.rs`):

```rust
/// The notices contract end-to-end: a diagnostic that used to be stderr
/// (invisible to MCP) now rides the outcome struct. Deterministic trigger:
/// a corrupt event-log line.
#[test]
fn search_assets_surfaces_notices_in_structured_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, state) = common::fixture_catalog(dir.path()); // reuse the existing helper
    // Corrupt the log the same way the services unit tests do.
    let log_file = std::fs::read_dir(catalog.join("events"))
        .expect("events dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("events jsonl");
    let mut bytes = std::fs::read(&log_file).expect("read");
    bytes.extend_from_slice(b"not json\n");
    std::fs::write(&log_file, bytes).expect("write");

    let mut mcp = Mcp::spawn(&catalog, &state);
    let resp = mcp.call_tool("search_assets", &serde_json::json!({"query": "tag:none"}));
    let notices = &resp["result"]["structuredContent"]["notices"];
    assert!(
        notices.as_array().is_some_and(|n| n
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.contains("corrupt event log line")))),
        "structuredContent must carry the notice: {resp}"
    );
}
```

(Adapt the `fixture_catalog` call to `common`'s real signature.) Run → PASS (the field arrived in Step 2; this pins the wire). Also assert one existing pinned tool response still has NO `notices` key (extend an existing wire-shape pin with `assert!(sc.get("notices").is_none())`) — the skip-if-empty contract.

- [ ] **Step 7: Commit**

```bash
git add crates/services/src/search.rs crates/services/src/tags.rs crates/services/src/catalog.rs \
  crates/services/src/volumes.rs crates/services/src/meta.rs crates/services/src/scan.rs \
  crates/services/src/para.rs crates/cli/src/search.rs crates/cli/src/commands.rs \
  crates/cli/src/tags_cmd.rs crates/cli/tests/mcp_smoke.rs
git commit -m "feat: notices ride SearchOutcome and read-verb outcomes"
```

---

### Task 3: `notices` outcome fields — inbox, index, sync, ingest (+ MCP `with_notices`)

**Files:**
- Modify: `crates/services/src/inbox.rs` (4 sites + `InboxOutcome`), `crates/services/src/index/mod.rs` (2 sites + `IndexStatusOutcome`), `crates/services/src/index/run.rs` (5 sites + `IndexRunOutcome`), `crates/services/src/index/heal.rs` (6 sites), `crates/services/src/sync.rs` (outcomes + threading), `crates/services/src/ingest.rs` (`IngestPlanOutcome`/`IngestRun` drain), `crates/cli/src/{inbox_cmd,index_cmd,sync_cmd,commands}.rs`, `crates/cli/src/mcp_cmd/write_tools.rs`
- Test: `crates/services/src/index/mod.rs` tests, `crates/cli/tests/mcp_smoke.rs`

- [ ] **Step 1: Failing test — unparsable failure report becomes a notice**

`read_failure_report` (`index/mod.rs:166`) is `pub` and takes `state_dir` — the deterministic trigger:

```rust
#[test]
fn unparsable_failure_report_is_a_notice() {
    let state = tempfile::tempdir().expect("tempdir");
    std::fs::write(state.path().join(FAILURES_FILE), b"{ not json").expect("plant");
    let notices = crate::notices::Notices::new();
    let report = read_failure_report(state.path(), &notices);
    assert!(report.is_empty());
    let drained = notices.drain();
    assert_eq!(drained.len(), 1);
    assert!(drained[0].contains("ignoring unparsable failure report"), "{drained:?}");
}
```

Run: `cargo test -p majestical-services unparsable_failure_report` → FAIL.

- [ ] **Step 2: Migrate the 13 index sites**

Mechanical, same shape every time — add `notices: &Notices` to the enclosing fn, push the verbatim string, delete the `#[expect]` block, pass `app.notices()` (or the already-threaded param) at the call site:

| Site | Enclosing fn → gains `notices: &Notices` | Callers pass |
| --- | --- | --- |
| `index/mod.rs:79` | `describer_model_tag` → also `capabilities` | `status`/`run`: `app.notices()` |
| `index/mod.rs:179` | `read_failure_report` | `status`: `app.notices()`; CLI callers (if any — grep) pass a local sink and print |
| `index/run.rs:412` (`open_or_rebuild`) | thread from `run` | `app.notices()` |
| `index/run.rs:1149` (text-store rebuild skip count) | same threading | 〃 |
| `index/run.rs:1422` (describer config unreadable) | the caption executor's env — thread via the executor's context struct if one exists, else a param | 〃 |
| `index/run.rs:1551` (unreadable captions blob) | 〃 | 〃 |
| `index/run.rs:1713` (`open_or_rebuild_text`) | thread from `run` | 〃 |
| `index/heal.rs:58,76,108,149,199,223` | each `heal_*_rows` fn + `heal_text_fts` | `run`: `app.notices()` |

Then `IndexRunOutcome` and `IndexStatusOutcome` gain the `notices` field; `index::run` and `index::status` drain before Ok.

- [ ] **Step 3: Migrate the 4 inbox sites**

- `:122` (`load_markers`) — param already threaded in Task 1.
- `:304` (`quiescence_ms`) — gains `notices: &Notices`; `process` threads it.
- `:547` (unlisted-file note in `process_contribution`) — push to the threaded sink.
- `:854`/`:857` (`report_failure_detail`) — gains `notices: &Notices`; both loops push instead of print.

`InboxOutcome` gains `notices`; `process` drains before Ok. `crates/cli/src/inbox_cmd.rs`: print `outcome.notices` to stderr before the row rendering (streams are compared independently; within-stderr order is the push order, which matches today's print order).

- [ ] **Step 4: Sync + ingest**

- `sync.rs`: `SyncStatusOutcome`, `LocationsOutcome`, `PushOutcome`, `PullOutcome` gain `notices`. `status`/`push`/`pull`/`locations_list` create a local `let notices = Notices::new();` at the top (they own no app at the boundary; `pull` ALSO drains its internal app's buffer into the same sink before building the outcome), thread it down (`config_path` from Task 1), and drain into the outcome. `location_add`/`location_rm` gain a `notices: &Notices` **parameter** (()-returning; the head owns the sink).
- `crates/cli/src/sync_cmd.rs`: print `outcome.notices` per verb; for `location add`/`location rm` construct a local `Notices`, pass it, print drained lines after the call (Ok or Err).
- `ingest.rs`: `IngestPlanOutcome` and `IngestRun` gain `notices` + drain (`run_ingest` drains at its end; site `:271` already pushes via Task 1). `commands.rs::cmd_ingest` prints them.
- `describer_config.rs`: if Task 1 gave `show`/`set`/`test` a `notices` param, `describer_cmd.rs` and `write_tools.rs` callers own local sinks (print / attach respectively).

- [ ] **Step 5: MCP `with_notices` for `json!`-building write tools**

In `crates/cli/src/mcp_cmd/write_tools.rs`:

```rust
/// Folds any service-collected diagnostics into a hand-built tool response —
/// the write-tool analogue of an outcome struct's own `notices` field. Absent
/// when empty, same contract.
fn with_notices(mut value: serde_json::Value, notices: Vec<String>) -> serde_json::Value {
    if !notices.is_empty() {
        value["notices"] = serde_json::json!(notices);
    }
    value
}
```

Apply at the end of each result builder that holds an app or a local sink and hand-builds `json!` (not the ones serializing an outcome struct — those carry the field already): `tag_assets_result` (both arms: `Ok(with_notices(json, app.notices().drain()))`), `set_metadata_result`, `scan_volume_result`'s exec arm, `move_para_add`/`move_para_rename`, `rm_saved_search_result`, `add_sync_location_result`/`rm_sync_location_result` (local sink passed to the service), `catalog_init_result` (no app — skip), `test_describer_result`/`set_describer_result` (local sink if Task 1 added the param).

- [ ] **Step 6: Zero `print_stderr` expects remain**

Run: `rg -n "print_stderr" crates/services/src`
Expected: only the (unchanged) mention in any comments if you left one — the goal is **zero `#[expect(clippy::print_stderr)]` attributes and zero `eprintln!` calls** in `crates/services/src` outside `#[cfg(test)]`. `rg -n "eprintln" crates/services/src` → only test code (the `inbox_manifest.rs:629` skip-message is inside a test — leave it).

- [ ] **Step 7: Full verification for PR1**

Run: `just check && just test`
Expected: zero warnings, everything green — including every parity test running for real against `/tmp/maj-ref`.

- [ ] **Step 8: Commit, open PR1**

```bash
git add crates/services/src crates/cli/src crates/cli/tests/mcp_smoke.rs
git status   # review: ONLY files this chunk touched
git commit -m "feat: notices ride inbox/index/sync/ingest outcomes; services stderr-free"
git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git HEAD
gh pr create --title "feat: migrate services stderr diagnostics to outcome notices" --body "..."
```

PR body: what the notices contract is now, the parity proof, the timing-change note (long verbs emit at completion). Squash-merge after green CI.

---

### Task 4: Real enums for the four enum-shaped string parameters

**Files:**
- Modify: `crates/services/Cargo.toml` (add `schemars.workspace = true`), `crates/services/src/tags.rs` (`TagOp`), `crates/services/src/para.rs` (`ParaOp`), `crates/services/src/ingest.rs` (`DedupeMode`), `crates/services/src/describer_config.rs` (`DescriberBackend`), `crates/cli/src/mcp_cmd/write_tools.rs`
- Test: `crates/services` unit tests, `crates/cli/tests/mcp_smoke.rs` (snapshot + rejection test)

The enums live in `crates/services` (canonical — MCP schemas and future GUI dropdowns both consume them), with serde renames pinning today's exact wire strings. `crates/cli`'s clap-side enums (`DedupeArg`, describer's) stay as they are — clap owns the CLI surface, services owns the wire surface, both convert to the underlying domain types.

- [ ] **Step 1: Failing wire-string tests**

In `crates/services/src/tags.rs` tests (and siblings per enum):

```rust
#[test]
fn tag_op_wire_strings_are_pinned() {
    for (op, wire) in [
        (TagOp::Add, "add"),
        (TagOp::Rm, "rm"),
        (TagOp::ConfirmSuggestion, "confirm_suggestion"),
        (TagOp::RejectSuggestion, "reject_suggestion"),
    ] {
        assert_eq!(serde_json::to_value(op).expect("ser"), serde_json::json!(wire));
        assert_eq!(
            serde_json::from_value::<TagOp>(serde_json::json!(wire)).expect("de"),
            op
        );
    }
    assert!(serde_json::from_value::<TagOp>(serde_json::json!("bogus")).is_err());
}
```

Same shape in `para.rs` (`ParaOp`: `add`/`rename`/`archive`), `ingest.rs` (`DedupeMode`: `skip`/`copy`), `describer_config.rs` (`DescriberBackend`: `ollama`/`lm-studio`/`open-router` — note **kebab**-case). Run each → FAIL (types missing).

- [ ] **Step 2: Define the enums**

All four follow this template (in their domain module, near the top):

```rust
/// `tag_assets`'s wire-level op — a real enum so the MCP JSON schema (and a
/// future GUI dropdown) carries the closed value set instead of a free
/// string validated only at call time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TagOp {
    Add,
    Rm,
    ConfirmSuggestion,
    RejectSuggestion,
}
```

```rust
#[serde(rename_all = "snake_case")]
pub enum ParaOp { Add, Rename, Archive }
```

```rust
/// The ingest dedupe surface MCP/GUI expose (`skip`/`copy` only — `Link`
/// still needs the per-destination lookup `maj ingest`'s own clap arg also
/// excludes).
#[serde(rename_all = "snake_case")]
pub enum DedupeMode { Skip, Copy }

impl From<DedupeMode> for majestical_ingest::plan::DedupeMode {
    fn from(v: DedupeMode) -> Self {
        match v {
            DedupeMode::Skip => Self::Skip,
            DedupeMode::Copy => Self::CopyAnyway,
        }
    }
}
```

```rust
#[serde(rename_all = "kebab-case")]
pub enum DescriberBackend { Ollama, LmStudio, OpenRouter }

impl From<DescriberBackend> for majestical_describe::BackendKind {
    fn from(v: DescriberBackend) -> Self {
        match v {
            DescriberBackend::Ollama => Self::Ollama,
            DescriberBackend::LmStudio => Self::LmStudio,
            DescriberBackend::OpenRouter => Self::OpenRouter,
        }
    }
}
```

Add `schemars.workspace = true` to `crates/services/Cargo.toml`. Run Step 1's tests → PASS.

- [ ] **Step 3: Rethread `write_tools.rs`**

- `TagAssetsArgs::op: majestical_services::tags::TagOp` (doc comment updated; schemars now emits the enum). Rename the module's own internal `TagOp<'a>` (the validated op+payload pairing) to `ValidatedTagOp<'a>` first — two `TagOp`s in one file is exactly the confusion the watchlist's `IngestRun` note warns about. `parse_tag_op` matches on the enum:

```rust
fn parse_tag_op(args: &TagAssetsArgs) -> anyhow::Result<ValidatedTagOp<'_>> {
    use majestical_services::tags::TagOp;
    match args.op {
        TagOp::Add => Ok(ValidatedTagOp::Add(
            args.tag.as_deref().context("op 'add' requires 'tag'")?,
        )),
        TagOp::Rm => Ok(ValidatedTagOp::Rm(
            args.tag.as_deref().context("op 'rm' requires 'tag'")?,
        )),
        TagOp::ConfirmSuggestion => Ok(ValidatedTagOp::ConfirmSuggestion(non_empty_tags(
            args.tags.as_ref(),
        )?)),
        TagOp::RejectSuggestion => {
            Ok(ValidatedTagOp::RejectSuggestion(non_empty_tags(args.tags.as_ref())?))
        }
    }
}
```

  The dry-run `json!` keeps `"op": args.op` — the enum serializes to the identical wire string.
- `MoveParaArgs::op: majestical_services::para::ParaOp`; the `move_para` tool's dispatch becomes a `match args.op { ParaOp::Add => …, ParaOp::Rename => …, ParaOp::Archive => … }` — the `other =>` arm disappears (deserialization now rejects unknowns), which is the point.
- `IngestSourceArgs::dedupe: majestical_services::ingest::DedupeMode` with `fn default_dedupe() -> majestical_services::ingest::DedupeMode { majestical_services::ingest::DedupeMode::Skip }`; `ingest_source_result` uses `args.dedupe.into()`; **delete `parse_dedupe`**.
- `SetDescriberArgs::backend: majestical_services::describer_config::DescriberBackend`; `set_describer_result` uses `args.backend.into()`; **delete `parse_backend`**. The dry-run `json!`'s `"backend": args.backend` serializes to the same kebab string.
- Check `describer_config::SetArgs.backend`'s type — if it takes `BackendKind`, `.into()` at the call site is enough; if services should own the enum end-to-end, switch `SetArgs.backend` to `DescriberBackend` and convert inside `set` (either is fine; prefer whichever touches fewer lines).

- [ ] **Step 4: mcp_smoke — snapshot + rejection**

- The tool-list snapshot test now fails (schemas gained `"enum": [...]`). Update the pinned snapshot **after inspecting the diff** — the only changes must be the four params' schemas.
- New test:

```rust
/// A typo'd enum value dies at the parameter-schema layer, before any tool
/// logic runs — the schema-level validation the watchlist item asked for.
#[test]
fn tag_assets_rejects_unknown_op_at_the_parameter_layer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&catalog, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({"asset": "xxh3:0", "op": "bogus", "confirm": false}),
    );
    // Pin the ACTUAL failure shape empirically (rmcp turns a serde failure
    // into either a protocol error or an isError result — run the test,
    // read the response, assert on what really comes back; it must clearly
    // name the bad value or field, and it must NOT be a structured dry-run
    // success).
    assert!(
        resp.get("error").is_some() || resp["result"]["isError"] == serde_json::json!(true),
        "unknown op must fail: {resp}"
    );
    assert!(resp["result"]["structuredContent"].get("would").is_none(), "{resp}");
}
```

- [ ] **Step 5: Verify + commit**

Run: `just check && cargo test -p majestical-services && cargo test -p majestical-cli --test mcp_smoke`
Expected: green.

```bash
git add crates/services/Cargo.toml crates/services/src/tags.rs crates/services/src/para.rs \
  crates/services/src/ingest.rs crates/services/src/describer_config.rs \
  crates/cli/src/mcp_cmd/write_tools.rs crates/cli/tests/mcp_smoke.rs
git commit -m "feat: schemars enums for tag/para/dedupe/backend MCP params"
```

---

### Task 5: Dry-run previews validate the asset exists

**Files:**
- Modify: `crates/cli/src/mcp_cmd/write_tools.rs` (`set_metadata_result`, `tag_assets_result`)
- Test: `crates/cli/tests/mcp_smoke.rs`

CLI behavior (`maj meta get` on an unknown id prints nothing/`null`) is deliberately unchanged — the fix is at the MCP preview layer only, using the already-`pub` `majestical_services::catalog::ensure_asset_known`.

- [ ] **Step 1: Failing tests**

```rust
/// The watchlist's "dry run over-promises" fix: a preview must fail on an
/// unknown asset id exactly like `confirm: true` would, never describe the
/// write as achievable.
#[test]
fn set_metadata_dry_run_fails_on_unknown_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&catalog, &state);
    let resp = mcp.call_tool(
        "set_metadata",
        &serde_json::json!({
            "asset": "xxh3:ffffffffffffffffffffffffffffffff",
            "field": "rating", "value": "5", "confirm": false
        }),
    );
    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("unknown asset"), "remedy must name the problem: {text}");
}

#[test]
fn tag_assets_dry_run_fails_on_unknown_asset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, state) = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&catalog, &state);
    let resp = mcp.call_tool(
        "tag_assets",
        &serde_json::json!({
            "asset": "xxh3:ffffffffffffffffffffffffffffffff",
            "op": "add", "tag": "kf", "confirm": false
        }),
    );
    assert_eq!(resp["result"]["isError"], serde_json::json!(true), "{resp}");
}
```

Run: `cargo test -p majestical-cli --test mcp_smoke dry_run_fails` → FAIL (both currently return dry-run successes).

- [ ] **Step 2: Implement**

In `tag_assets_result`'s dry-run branch, the projection is already loaded — add the guard before building `current_tags`:

```rust
if !args.confirm {
    let projection = app.projection()?;
    let asset_id = AssetId(args.asset.clone());
    majestical_services::catalog::ensure_asset_known(&projection, &asset_id)?;
    let current_tags: Vec<String> = projection.tags(&asset_id).into_iter().collect();
    // ... unchanged ...
```

In `set_metadata_result`, add the same two lines (projection + `ensure_asset_known`) at the top of the fn, before the `meta_get` call — validating for the exec path costs nothing (it re-validates inside `meta_set`) and keeps the dry/exec answers consistent by construction.

- [ ] **Step 3: Verify — including that the existing known-asset dry-run tests still pass**

Run: `cargo test -p majestical-cli --test mcp_smoke`
Expected: green — the pre-existing `set_metadata`/`tag_assets` dry-run tests (known assets) unchanged.

- [ ] **Step 4: Sabotage check (do it, don't skip)**

Delete the `ensure_asset_known` line in `tag_assets_result`, run Step 1's test, confirm it FAILS, restore. Same for `set_metadata_result`. This is the guard-ships-with-its-test rule.

- [ ] **Step 5: Commit, open PR2**

```bash
git add crates/cli/src/mcp_cmd/write_tools.rs crates/cli/tests/mcp_smoke.rs
git commit -m "fix: MCP dry-run previews validate asset existence"
git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git HEAD
gh pr create --title "feat: schemars enum params + honest dry-run previews" --body "..."
```

---

### Task 6: Tauri scaffold — `apps/desktop`, GUI workspace split, version-sync, GUI CI

**Files:**
- Create: `apps/desktop/package.json`, `apps/desktop/.npmrc`, `apps/desktop/vite.config.ts`, `apps/desktop/tsconfig.json`, `apps/desktop/index.html`, `apps/desktop/src/main.ts`, `apps/desktop/src/App.svelte`, `apps/desktop/src/app.css`, `apps/desktop/src/App.test.ts`, `apps/desktop/src-tauri/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,src/main.rs,src/lib.rs,icons/*}`, `scripts/version-sync.sh`
- Modify: `justfile`, `.github/workflows/ci.yml`, root `.gitignore` (add `apps/desktop/node_modules`, `apps/desktop/dist`, `apps/desktop/src-tauri/target`)

- [ ] **Step 1: Verify current stable versions — write them down before writing any file**

```bash
for p in @tauri-apps/api @tauri-apps/cli @tauri-apps/plugin-updater @tauri-apps/plugin-dialog \
         svelte @sveltejs/vite-plugin-svelte vite vitest jsdom typescript oxlint \
         @testing-library/svelte; do echo "$p $(pnpm view "$p" version)"; done
curl -s https://crates.io/api/v1/crates/tauri | jq -r .crate.max_stable_version
curl -s https://crates.io/api/v1/crates/tauri-build | jq -r .crate.max_stable_version
curl -s https://crates.io/api/v1/crates/tauri-plugin-updater | jq -r .crate.max_stable_version
curl -s https://crates.io/api/v1/crates/tauri-plugin-dialog | jq -r .crate.max_stable_version
```

Every `<VERIFIED>` placeholder below means the exact version from this step (pnpm side: exact pins, no `^`/`~`).

- [ ] **Step 2: Frontend scaffold**

`apps/desktop/.npmrc`:

```ini
save-exact=true
ignore-scripts=true
minimum-release-age=1440
```

`apps/desktop/package.json` (versions from Step 1; if `pnpm install` later reports blocked build scripts a dependency genuinely needs — e.g. esbuild on some versions — allow ONLY that package via `"pnpm": {"onlyBuiltDependencies": ["esbuild"]}` and note it in the commit message):

```json
{
  "name": "majestical-desktop",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "check": "tsc --noEmit",
    "lint": "oxlint src",
    "test": "vitest run",
    "tauri": "tauri"
  },
  "dependencies": {
    "@tauri-apps/api": "<VERIFIED>",
    "@tauri-apps/plugin-dialog": "<VERIFIED>",
    "@tauri-apps/plugin-updater": "<VERIFIED>",
    "svelte": "<VERIFIED>"
  },
  "devDependencies": {
    "@sveltejs/vite-plugin-svelte": "<VERIFIED>",
    "@tauri-apps/cli": "<VERIFIED>",
    "@testing-library/svelte": "<VERIFIED>",
    "jsdom": "<VERIFIED>",
    "oxlint": "<VERIFIED>",
    "typescript": "<VERIFIED>",
    "vite": "<VERIFIED>",
    "vitest": "<VERIFIED>"
  }
}
```

`apps/desktop/tsconfig.json` (the house strict set, in full):

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "types": ["svelte", "vite/client"],
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noImplicitOverride": true,
    "noPropertyAccessFromIndexSignature": true,
    "verbatimModuleSyntax": true,
    "isolatedModules": true,
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src"]
}
```

`apps/desktop/vite.config.ts`:

```ts
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  test: { environment: "jsdom" },
});
```

(If `test` in `vite.config.ts` trips the type checker, use the `vitest/config` `defineConfig` import instead — vitest's documented pattern for the installed version wins.)

`apps/desktop/index.html`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <title>Majestical</title>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
```

`apps/desktop/src/main.ts`:

```ts
import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

export default mount(App, { target: document.getElementById("app")! });
```

`apps/desktop/src/App.svelte` (placeholder shell this task; Task 8 replaces the body):

```svelte
<main>
  <h1>Majestical</h1>
</main>
```

`apps/desktop/src/app.css`: minimal reset (box-sizing, system font stack, margin 0). `apps/desktop/src/App.test.ts`:

```ts
import { render, screen } from "@testing-library/svelte";
import { expect, test } from "vitest";
import App from "./App.svelte";

test("shell renders", () => {
  render(App);
  expect(screen.getByRole("main")).toBeTruthy();
});
```

- [ ] **Step 3: `src-tauri` — its own cargo workspace**

`apps/desktop/src-tauri/Cargo.toml` (the empty `[workspace]` table is the split — cargo stops walking up, headless CI never sees this tree; versions are literals because a standalone workspace can't inherit the root's):

```toml
[package]
name = "majestical-desktop"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

# Standalone GUI workspace (phase 7 spec §7): the headless workspace's CI
# never compiles the Tauri tree, and this tree never compiles headless-only
# test suites.
[workspace]

[lib]
name = "majestical_desktop"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "<VERIFIED>", features = [] }

[dependencies]
tauri = { version = "<VERIFIED>", features = [] }
tauri-plugin-dialog = "<VERIFIED>"
tauri-plugin-updater = "<VERIFIED>"
majestical-services = { path = "../../../crates/services" }
majestical-index = { path = "../../../crates/index" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
hostname = "0.4"

[dev-dependencies]
tempfile = "3"

[lints.clippy]
# Copy of the root workspace's lint table (a standalone workspace cannot
# inherit it) — keep in lockstep manually; version-sync does not check this.
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "warn"
panic = "deny"
panic_in_result_fn = "deny"
unimplemented = "deny"
allow_attributes = "deny"
dbg_macro = "deny"
todo = "deny"
print_stdout = "deny"
print_stderr = "deny"
await_holding_lock = "deny"
large_futures = "deny"
exit = "deny"
mem_forget = "deny"
module_name_repetitions = "allow"
similar_names = "allow"
```

`apps/desktop/src-tauri/build.rs`:

```rust
fn main() {
    tauri_build::build();
}
```

`apps/desktop/src-tauri/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    majestical_desktop::run();
}
```

`apps/desktop/src-tauri/src/lib.rs` (this task: empty builder; Tasks 7-9 add commands/protocol):

```rust
//! The desktop head: a Tauri shell over `majestical_services`. Commands are
//! thin wrappers returning services outcome structs as-is (parity by
//! construction, same rule as `maj mcp`).

/// Builds and runs the Tauri app.
///
/// # Panics
/// Panics only if the Tauri runtime itself fails to start — there is no
/// meaningful recovery for a desktop app that cannot open a window.
pub fn run() {
    #[expect(clippy::expect_used, reason = "no recovery exists if the shell cannot start")]
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running majestical desktop");
}
```

`apps/desktop/src-tauri/tauri.conf.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Majestical",
  "version": "0.1.0",
  "identifier": "com.kindlyops.majestical",
  "build": {
    "beforeDevCommand": "pnpm dev",
    "devUrl": "http://localhost:1420",
    "beforeBuildCommand": "pnpm build",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [{ "title": "Majestical", "width": 1280, "height": 820 }],
    "security": {
      "csp": "default-src 'self'; img-src 'self' thumb: asset: data:; style-src 'self' 'unsafe-inline'"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["app", "dmg"],
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.ico"]
  }
}
```

`apps/desktop/src-tauri/capabilities/default.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["core:default", "dialog:default", "updater:default"]
}
```

Icons: generate a placeholder set (ImageMagick is a phase-6 local dep):

```bash
cd apps/desktop
magick -size 1024x1024 xc:'#3b4252' -fill '#e5e9f0' -gravity center \
  -pointsize 520 -annotate 0 'M' icon-source.png
pnpm tauri icon icon-source.png   # writes src-tauri/icons/*
rm icon-source.png
```

- [ ] **Step 4: Install + first green run**

```bash
cd apps/desktop && pnpm install
pnpm check && pnpm lint && pnpm test
cargo build --manifest-path src-tauri/Cargo.toml
```

Expected: all pass; the cargo build produces the GUI workspace's own `src-tauri/target` and `Cargo.lock` (commit the lockfile). First build is slow (it compiles the whole services graph a second time — expected cost of the workspace split).

- [ ] **Step 5: Version-sync check (with the sabotage proof)**

`scripts/version-sync.sh`:

```bash
#!/usr/bin/env bash
# One version, four files: the root workspace, the GUI npm package, the GUI
# cargo package, and tauri.conf.json (what the updater/bundler stamp).
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
npm_pkg=$(jq -r .version apps/desktop/package.json)
conf=$(jq -r .version apps/desktop/src-tauri/tauri.conf.json)
gui=$(grep -m1 '^version' apps/desktop/src-tauri/Cargo.toml | cut -d'"' -f2)
if [[ "$root" != "$npm_pkg" || "$root" != "$conf" || "$root" != "$gui" ]]; then
  echo "version mismatch: workspace=$root package.json=$npm_pkg tauri.conf.json=$conf src-tauri=$gui" >&2
  echo "fix: set all four to the same version before tagging a release" >&2
  exit 1
fi
echo "version-sync ok: $root"
```

`chmod +x scripts/version-sync.sh && shellcheck scripts/version-sync.sh && shfmt -d scripts/version-sync.sh`.

justfile additions:

```make
gui-install:
    cd apps/desktop && pnpm install --frozen-lockfile

gui-check:
    cd apps/desktop && pnpm check && pnpm lint && pnpm test

gui-build:
    cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml

version-sync:
    ./scripts/version-sync.sh
```

Sabotage proof (run it now, once): set `package.json` version to `0.1.1`, run `just version-sync`, confirm exit 1 naming the mismatch, revert, confirm exit 0.

- [ ] **Step 6: CI wiring**

Add to `.github/workflows/ci.yml` (SHA-pin every new action at its current release — look each SHA up at execution time; keep `persist-credentials: false` everywhere):

```yaml
  version-sync:
    name: Version sync
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<existing-pinned-sha>  # v<existing>
        with:
          persist-credentials: false
      - run: ./scripts/version-sync.sh
  gui:
    name: GUI build (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [macos-latest, ubuntu-latest, windows-latest]
    steps:
      - uses: actions/checkout@<existing-pinned-sha>  # v<existing>
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<existing-pinned-sha>  # stable
      - uses: Swatinem/rust-cache@<existing-pinned-sha>  # v<existing>
        with:
          workspaces: apps/desktop/src-tauri
      - name: Install webkit2gtk deps (Linux)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev librsvg2-dev patchelf
      - name: Install protoc (lance build dependency, via services)
        shell: bash
        run: |
          if [ "$RUNNER_OS" = "macOS" ]; then brew install protobuf
          elif [ "$RUNNER_OS" = "Linux" ]; then sudo apt-get install -y protobuf-compiler
          else choco install protoc -y; fi
      - uses: pnpm/action-setup@<verify-sha>  # v<verify>
        with:
          version: <VERIFIED pnpm major>
      - uses: actions/setup-node@<verify-sha>  # v<verify>
        with:
          node-version: 22
          cache: pnpm
          cache-dependency-path: apps/desktop/pnpm-lock.yaml
      - run: pnpm install --frozen-lockfile
        working-directory: apps/desktop
      - run: pnpm check && pnpm lint && pnpm test
        working-directory: apps/desktop
      - run: cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml
```

Run `actionlint .github/workflows/ci.yml` and `uvx zizmor==<current> .github/workflows/` locally — both clean. The four conformance jobs and the `rust` job are untouched.

Also extend `.github/dependabot.yml` with an `npm` ecosystem entry for `/apps/desktop` and a `cargo` entry for `/apps/desktop/src-tauri`, mirroring the existing 7-day-cooldown grouped config.

- [ ] **Step 7: Commit, open PR3**

```bash
git add apps/desktop scripts/version-sync.sh justfile .github/workflows/ci.yml .github/dependabot.yml .gitignore
git commit -m "feat: Tauri desktop scaffold, GUI workspace split, version-sync, GUI CI matrix"
```

Note for the PR body: the GUI workspace builds on all three OSes as feedback; release artifacts remain macOS-only (Task 10).

---

### Task 7: The Tauri commands — services wrappers, `thumb://` protocol, command tests, `tauri_parity`

**Files:**
- Create: `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/config.rs`, `apps/desktop/src-tauri/src/thumb_protocol.rs`, `apps/desktop/src-tauri/tests/commands.rs`, `apps/desktop/src-tauri/tests/tauri_parity.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `crates/services/src/runtime.rs` (create — `run_off_tokio_runtime` moves here), `crates/services/src/lib.rs`, `crates/cli/src/mcp_cmd/mod.rs` + `read_tools.rs`/`write_tools.rs` (use the moved helper), `justfile` (`gui-test`)

**Design constraints carried from 7A (do not relax):**
- The Lance scoped-thread rule: `search_assets`/`run_saved_search` MUST run their service call inside `run_off_tokio_runtime` — Tauri commands execute on a tokio runtime, and Lance's store-open enters its own. Compose it with `tauri::async_runtime::spawn_blocking` so a slow search never stalls the async workers: `spawn_blocking(|| run_off_tokio_runtime(|| …))` — the scoped std thread inside has no runtime context, which is what Lance needs.
- Commands open a fresh `FsApp` per call (same reasoning as `maj mcp`: a long-lived GUI must see changes other processes make).
- Commands return outcome structs **as-is**; errors map to a serializable `CommandError { message }` carrying the full `{err:#}` chain (the remedy text lives there, same as MCP).

**Spec deviation to record later (as-built):** the spec's six commands presume a known catalog path; a real first run needs the app to discover/persist one. This task adds `app_status` / `initialize_catalog` / `use_existing_catalog` (the last two are the spec's `catalog_init` verb plus config persistence). The catalog path persists in a JSON config file in Tauri's app-config dir.

- [ ] **Step 1: Move `run_off_tokio_runtime` into services**

Create `crates/services/src/runtime.rs` with the function moved VERBATIM (doc comment included, updated to name both heads) from `crates/cli/src/mcp_cmd/mod.rs:92-99`; add `pub mod runtime;` to services `lib.rs`; `mcp_cmd` call sites switch to `majestical_services::runtime::run_off_tokio_runtime`. Run `cargo test -p majestical-cli --test mcp_smoke` — the existing search/index tests are the regression net for the move. Commit separately: `refactor: run_off_tokio_runtime moves to services for the Tauri head`.

- [ ] **Step 2: Failing command tests**

`apps/desktop/src-tauri/tests/commands.rs` — tests drive the **impl fns** (plain functions; the `#[tauri::command]` wrappers stay one-liners). `MAJ_STATE_DIR` is process-global, so every test serializes on one mutex and points the var at its own tempdir:

```rust
//! Direct tests over the command impls with a real (fixture) catalog.
//! `MAJ_STATE_DIR` is process-global env, so tests take `ENV_LOCK` and set
//! it per-test — the same reason the CLI's own suites set it per child
//! process; here the "process" is this test binary.
use majestical_desktop::commands::{
    app_status_impl, get_asset_impl, initialize_catalog_impl, list_saved_searches_impl,
    list_volumes_impl, run_saved_search_impl, search_assets_impl, CatalogCfg,
};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = tempfile::tempdir().expect("state dir");
    // SAFETY: serialized by ENV_LOCK; no other thread reads env mid-test.
    unsafe { std::env::set_var("MAJ_STATE_DIR", state.path()) };
    let out = f();
    drop(state);
    out
}

fn seeded_cfg(dir: &std::path::Path) -> CatalogCfg {
    let catalog = dir.join("cat");
    let cfg = CatalogCfg {
        catalog: catalog.clone(),
        machine_id: "gui-test".into(),
        author: "gui-test".into(),
    };
    initialize_catalog_impl(&cfg).expect("init");
    let mut app = majestical_services::app::FsApp::open(&catalog, "gui-test", "gui-test")
        .expect("open");
    app.emit(vec![majestical_core_op_asset_seen()]) // build the same AssetSeen op the services meta tests use
        .expect("emit");
    cfg
}

#[test]
fn search_finds_the_seeded_asset_and_carries_outcome_shape() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let outcome = search_assets_impl(&cfg, Some("clip".into()), None, 10).expect("search");
        assert_eq!(outcome.count, 1);
        assert!(outcome.results[0].name.contains("clip"));
    });
}

#[test]
fn get_asset_returns_detail_for_known_and_none_for_unknown() { /* same shape */ }

#[test]
fn app_status_reports_missing_then_ready() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = CatalogCfg {
            catalog: dir.path().join("cat"),
            machine_id: "gui-test".into(),
            author: "gui-test".into(),
        };
        assert!(!app_status_impl(&cfg).catalog_ready);
        initialize_catalog_impl(&cfg).expect("init");
        assert!(app_status_impl(&cfg).catalog_ready);
    });
}

#[test]
fn initialize_refuses_an_existing_catalog() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let err = initialize_catalog_impl(&cfg).expect_err("must refuse");
        assert!(err.message.contains("already exists"), "{}", err.message);
    });
}
```

(`majestical_core_op_asset_seen()` is shorthand for constructing the same `Op::AssetSeen` literal `crates/services/src/meta.rs`'s tests use — depend on `majestical-core` as a dev-dependency of the GUI workspace, or emit through a small helper; copy the 8-line op literal, don't invent a shared crate for it.)

Run: `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml` → FAIL (module missing).

- [ ] **Step 3: Implement `config.rs` and `commands.rs`**

`apps/desktop/src-tauri/src/config.rs`:

```rust
//! Persisted GUI settings: today, only the catalog path. A JSON file in
//! Tauri's app-config dir — file-based on purpose (agent-inspectable,
//! trivially portable), matching the project's file-first bias.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
pub struct GuiConfig {
    pub catalog: Option<PathBuf>,
}

pub fn load(config_dir: &Path) -> GuiConfig {
    let Ok(bytes) = std::fs::read(config_dir.join("config.json")) else {
        return GuiConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// # Errors
/// Returns an error if the config dir can't be created or the file written.
pub fn store(config_dir: &Path, config: &GuiConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(config_dir.join("config.json"), text)?;
    Ok(())
}
```

`apps/desktop/src-tauri/src/commands.rs` — impl fns first, `#[tauri::command]` wrappers after. The shape (write ALL of these out; `search_assets_impl` shown in full, the rest follow it):

```rust
//! One command per verb the slice uses, each a thin wrapper over
//! `majestical_services` returning the outcome struct as-is — parity by
//! construction, the same rule `maj mcp` follows. Commands open a fresh
//! `FsApp` per call (a long-lived GUI must see other processes' changes).
use majestical_services::app::FsApp;
use serde::Serialize;
use std::path::PathBuf;

/// This app's catalog wiring — managed Tauri state, rebuilt when the user
/// picks or initializes a catalog.
pub struct CatalogCfg {
    pub catalog: PathBuf,
    pub machine_id: String,
    pub author: String,
}

/// The one error shape every command returns: the full anyhow/ServiceError
/// Display chain, where the remedy text already lives (same rule as
/// `maj mcp`'s tool_error).
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl<E: Into<anyhow::Error>> From<E> for CommandError {
    fn from(err: E) -> Self {
        let err: anyhow::Error = err.into();
        Self { message: format!("{err:#}") }
    }
}

#[derive(Serialize)]
pub struct AppStatus {
    pub catalog_path: String,
    pub catalog_ready: bool,
}

pub fn app_status_impl(cfg: &CatalogCfg) -> AppStatus {
    AppStatus {
        catalog_path: cfg.catalog.display().to_string(),
        catalog_ready: majestical_services::catalog::ensure_catalog(&cfg.catalog).is_ok(),
    }
}

pub fn search_assets_impl(
    cfg: &CatalogCfg,
    query: Option<String>,
    saved: Option<String>,
    limit: usize,
) -> Result<majestical_services::search::SearchOutcome, CommandError> {
    let req = majestical_services::search::SearchRequest { query, limit, saved, save: None };
    // The Lance scoped-thread rule (see services::runtime): the semantic
    // layer opens a vector store that builds its own tokio runtime.
    let outcome = majestical_services::runtime::run_off_tokio_runtime(|| {
        let mut app = FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)?;
        Ok(majestical_services::search::search(&mut app, &cfg.catalog, &req)?)
    })?;
    Ok(outcome)
}

// get_asset_impl      -> Result<Option<majestical_services::catalog::AssetDetail>, CommandError>
// list_volumes_impl   -> Result<majestical_services::volumes::VolumesOutcome, CommandError>
// list_saved_searches_impl -> Result<SavedSearches, CommandError>
//   where SavedSearches is a local { saved: Vec<SavedSearch>, notices: Vec<String> } struct:
//   searches_list returns a bare Vec, and the GUI still must see any app
//   notices — drain app.notices() into the wrapper (the searches_list
//   analogue of an outcome drain; mirror read_tools::SavedSearchesResult's
//   field name `saved` so the two heads' shapes agree).
// run_saved_search_impl -> same as search_assets_impl with saved: Some(name)
// initialize_catalog_impl(cfg) -> Result<(), CommandError>:
//   refuse if ensure_catalog(cfg.catalog).is_ok() (message must contain
//   "already exists", mirroring catalog_init_result's wording), else
//   majestical_services::catalog::init(&cfg.catalog, &cfg.machine_id, &cfg.author)?
```

Then the `#[tauri::command]` wrappers + state plumbing (in `commands.rs` below the impls):

```rust
use std::sync::RwLock;
use tauri::State;

/// Managed state: `None` until a catalog is chosen/initialized.
pub struct AppState(pub RwLock<Option<CatalogCfg>>);

fn cfg_of(state: &State<'_, AppState>) -> Result<CatalogCfg, CommandError> {
    let guard = state.0.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()
        .map(|cfg| CatalogCfg {
            catalog: cfg.catalog.clone(),
            machine_id: cfg.machine_id.clone(),
            author: cfg.author.clone(),
        })
        .ok_or_else(|| CommandError { message: "no catalog selected yet — initialize or choose one first".into() })
}

#[tauri::command]
pub async fn search_assets(
    state: State<'_, AppState>,
    query: Option<String>,
    saved: Option<String>,
    limit: Option<usize>,
) -> Result<majestical_services::search::SearchOutcome, CommandError> {
    let cfg = cfg_of(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        search_assets_impl(&cfg, query, saved, limit.unwrap_or(50))
    })
    .await
    .map_err(|err| CommandError { message: format!("search task failed: {err}") })?
}
```

Same wrapper shape for the rest. `initialize_catalog`/`use_existing_catalog` take a `path: String` argument, validate (`use_existing_catalog` runs `ensure_catalog`), update the managed state AND persist via `config::store` (`app.path().app_config_dir()`), and return the new `AppStatus`. `app_status` reads the persisted config on startup (wired in `lib.rs::run` via `.setup()`: load config, populate `AppState`, machine_id/author = `hostname::get()` lossy string).

Register everything in `lib.rs`:

```rust
.manage(commands::AppState(std::sync::RwLock::new(None)))
.invoke_handler(tauri::generate_handler![
    commands::app_status,
    commands::search_assets,
    commands::get_asset,
    commands::list_volumes,
    commands::list_saved_searches,
    commands::run_saved_search,
    commands::initialize_catalog,
    commands::use_existing_catalog,
])
```

Run Step 2's tests → PASS.

- [ ] **Step 4: `thumb://` protocol**

Read `crates/cli/src/mcp_cmd/resources.rs` first — the blob lookup for a thumbnail and the keyframe manifest already exist there; mirror that lookup (via `majestical_index::blob::BlobStore` / `crates/services/src/index/blob_read.rs`), do not reinvent it. If the lookup is more than ~15 lines, extract it into a `pub` services fn both heads call, with the mcp_smoke resource tests as the regression net.

`apps/desktop/src-tauri/src/thumb_protocol.rs`:

```rust
//! `thumb://` — thumbnails and keyframe manifests straight from the blob
//! store to the webview, no image bytes over IPC (phase 7 spec). URLs are
//! built frontend-side with convertFileSrc(id, "thumb"):
//!   thumb://localhost/thumb/<asset_id>      -> image/webp thumbnail
//!   thumb://localhost/keyframes/<asset_id>  -> application/json manifest
use tauri::http::{Response, StatusCode};

pub fn handle(cfg: Option<&crate::commands::CatalogCfg>, uri: &str) -> Response<Vec<u8>> {
    let Some(cfg) = cfg else {
        return status(StatusCode::SERVICE_UNAVAILABLE, "no catalog selected yet");
    };
    // Path parsing + blob lookup; every failure is a plain HTTP status with
    // the reason as the body — the webview shows a broken image, the
    // inspector shows nothing, and the reason is one devtools click away.
    …
}
```

Wire in `lib.rs`: `.register_uri_scheme_protocol("thumb", move |ctx, request| { … })` reading the managed state. Unit-test `handle` directly in the tests file (fixture catalog + a planted thumbnail blob — copy the fixture-planting pattern from `mcp_smoke.rs`'s resource tests).

- [ ] **Step 5: `tauri_parity` — the third head's parity proof**

`apps/desktop/src-tauri/tests/tauri_parity.rs`:

```rust
//! The GUI analogue of services_parity.rs: a command's serialized outcome
//! must be byte-identical JSON to the services outcome the CLI renders
//! from. The comparison here is command-output vs a fresh services call in
//! the same process (both heads wrap the SAME functions — what this pins is
//! that the command layer adds/renames/loses nothing), plus one
//! cross-binary check against `maj search --json`'s row content.
```

Two tests:

1. `command_serializes_outcome_verbatim`: run `search_assets_impl`, serialize with `serde_json::to_value`; call `majestical_services::search::search` directly with the same request against the same catalog; assert the two `Value`s are equal except the (order-preserved) `notices` drained by whichever ran second — easiest exact form: run the command against catalog A and the direct call against an identically-seeded catalog B and compare `Value`s.
2. `search_rows_match_cli_json`: spawn the CLI (`MAJ_BIN` env, default `../../../target/debug/maj` relative to the GUI workspace root — document that `just gui-test` builds it) with `--json`, parse stdout, and assert every `results[i]` field the CLI prints (`asset`, `score`, `name`, `tags`, `para`) equals the command outcome's corresponding row fields. (The CLI hand-renders its JSON, so full-payload equality is not the contract — row-content equality is.)

justfile:

```make
gui-test:
    cargo build -p majestical-cli
    MAJ_BIN="{{justfile_directory()}}/target/debug/maj" \
        cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
```

Add `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml` to the CI `gui` job (macOS leg only for the cross-binary test — gate the `MAJ_BIN` test with a skip-if-absent message like services_parity's, so Linux/Windows legs still run the pure-Rust tests).

- [ ] **Step 6: Verify + commit, open PR4**

```bash
just gui-check && just gui-test && just check && cargo test -p majestical-cli --test mcp_smoke
git add apps/desktop/src-tauri crates/services/src/runtime.rs crates/services/src/lib.rs \
  crates/cli/src/mcp_cmd justfile .github/workflows/ci.yml
git commit -m "feat: Tauri commands over services, thumb:// protocol, tauri_parity harness"
```

---

### Task 8: Search surface + first-run flow (Svelte)

**Files:**
- Create: `apps/desktop/src/lib/api.ts`, `apps/desktop/src/lib/thumb.ts`, `apps/desktop/src/lib/Welcome.svelte`, `apps/desktop/src/lib/SearchView.svelte`, `apps/desktop/src/lib/Inspector.svelte`, `apps/desktop/src/lib/SearchView.test.ts`, `apps/desktop/src/lib/Welcome.test.ts`
- Modify: `apps/desktop/src/App.svelte`, `apps/desktop/src/app.css`

Layout C: left sidebar (Search / Volumes — ONLY those two; no dead buttons), center surface, right inspector that collapses when nothing is selected. Degradation notices render verbatim, never hidden.

- [ ] **Step 1: The typed API layer**

`apps/desktop/src/lib/api.ts` — one `invoke` wrapper per command, with TS types mirroring the outcome structs **field-for-field** (they are the wire contract; `notices` optional everywhere):

```ts
import { invoke } from "@tauri-apps/api/core";

export interface VolumeRef { id: string; label: string; online: boolean }
export interface SearchHit {
  asset: string; score: number; name: string; known: boolean;
  volumes: VolumeRef[]; tags: string[]; para: string | null;
  timestamp_ms?: number; source?: string; locator?: number; snippet?: string;
}
export interface SearchOutcome {
  count: number; results: SearchHit[];
  semantic_coverage?: { embedded: number; eligible: number };
  text_coverage?: { label: string; covered: number; eligible: number; remedy: string; noun: string; source: string }[];
  notices?: string[];
}
export interface AppStatus { catalog_path: string; catalog_ready: boolean }
// …VolumesOutcome, AssetDetail, SavedSearches — mirror the Rust structs;
// when unsure of a field, read the struct, don't guess.

export const api = {
  appStatus: () => invoke<AppStatus>("app_status"),
  searchAssets: (query: string, limit = 50) =>
    invoke<SearchOutcome>("search_assets", { query, limit }),
  runSavedSearch: (name: string, limit = 50) =>
    invoke<SearchOutcome>("run_saved_search", { name, limit }),
  getAsset: (assetId: string) => invoke<AssetDetail | null>("get_asset", { assetId }),
  listVolumes: () => invoke<VolumesOutcome>("list_volumes"),
  listSavedSearches: () => invoke<SavedSearches>("list_saved_searches"),
  initializeCatalog: (path: string) => invoke<AppStatus>("initialize_catalog", { path }),
  useExistingCatalog: (path: string) => invoke<AppStatus>("use_existing_catalog", { path }),
};
```

`apps/desktop/src/lib/thumb.ts`:

```ts
import { convertFileSrc } from "@tauri-apps/api/core";

export const thumbUrl = (assetId: string) => convertFileSrc(`thumb/${assetId}`, "thumb");
export const keyframesUrl = (assetId: string) => convertFileSrc(`keyframes/${assetId}`, "thumb");
```

- [ ] **Step 2: Failing component tests (mockIPC)**

`apps/desktop/src/lib/SearchView.test.ts` — the three behaviors the spec pins: debounce, stale-query cancellation, notices shown verbatim:

```ts
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event"; // add as devDependency (verified version)
import { mockIPC, clearMocks } from "@tauri-apps/api/mocks";
import { afterEach, expect, test, vi } from "vitest";
import SearchView from "./SearchView.svelte";

afterEach(() => { clearMocks(); vi.useRealTimers(); });

const emptyOutcome = { count: 0, results: [] };

test("typing debounces to one search call", async () => {
  const calls: string[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "search_assets") { calls.push((args as { query: string }).query); return emptyOutcome; }
    if (cmd === "list_saved_searches") return { saved: [] };
    throw new Error(`unexpected ${cmd}`);
  });
  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "sunset");
  await waitFor(() => expect(calls).toEqual(["sunset"]));
});

test("a stale response never overwrites a newer query's results", async () => {
  let resolveFirst!: (v: unknown) => void;
  let call = 0;
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    call += 1;
    if (call === 1) return new Promise((resolve) => { resolveFirst = resolve; });
    return { count: 1, results: [hit("second")] };
  });
  render(SearchView, { onselect: () => {} });
  const box = screen.getByRole("searchbox");
  await userEvent.type(box, "a");
  await waitFor(() => expect(call).toBe(1));
  await userEvent.clear(box);
  await userEvent.type(box, "b");
  await waitFor(() => expect(call).toBe(2));
  await waitFor(() => screen.getByText(/second/));
  resolveFirst({ count: 1, results: [hit("stale")] });   // late arrival
  await waitFor(() => expect(screen.queryByText(/stale/)).toBeNull());
});

test("notices and coverage render verbatim and are not dismissable", async () => {
  mockIPC((cmd) => {
    if (cmd === "list_saved_searches") return { saved: [] };
    return {
      count: 0, results: [],
      notices: ["warning: skipped 1 corrupt event log line(s) in /x/events — damaged transport; affected metadata may be missing"],
    };
  });
  render(SearchView, { onselect: () => {} });
  await userEvent.type(screen.getByRole("searchbox"), "q");
  await waitFor(() => screen.getByText(/skipped 1 corrupt event log line/));
});
```

(`hit(name)` is a 6-line fixture helper returning a full `SearchHit`.) `Welcome.test.ts`: renders when `catalog_ready` is false; clicking "Initialize catalog" invokes `initialize_catalog` with the picked path (mock `plugin:dialog|open` through `mockIPC` too) and fires the `oninitialized` callback.

Run: `pnpm test` → FAIL (components missing).

- [ ] **Step 3: Implement the components (Svelte 5 runes)**

`SearchView.svelte` core (write the full component; the load-bearing logic):

```svelte
<script lang="ts">
  import { api, type SearchOutcome } from "./api";
  import { thumbUrl } from "./thumb";

  let { onselect }: { onselect: (assetId: string | null) => void } = $props();

  let query = $state("");
  let outcome = $state<SearchOutcome | null>(null);
  let saved = $state<{ name: string; query: string }[]>([]);
  let error = $state<string | null>(null);
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let requestSeq = 0;

  $effect(() => { api.listSavedSearches().then((s) => (saved = s.saved)).catch(() => {}); });

  function queueSearch(q: string) {
    clearTimeout(debounceTimer);
    if (!q.trim()) { outcome = null; return; }
    debounceTimer = setTimeout(() => void runSearch(() => api.searchAssets(q)), 200);
  }

  async function runSearch(call: () => Promise<SearchOutcome>) {
    const seq = ++requestSeq;
    error = null;
    try {
      const result = await call();
      if (seq !== requestSeq) return; // stale — a newer query owns the surface
      outcome = result;
    } catch (e) {
      if (seq !== requestSeq) return;
      error = String(e instanceof Object && "message" in e ? (e as { message: string }).message : e);
    }
  }
</script>

<div class="search-surface">
  <input
    type="search" role="searchbox" placeholder="Search — terms and key:value filters"
    bind:value={query} oninput={() => queueSearch(query)}
  />
  <div class="chips">
    {#each saved as s (s.name)}
      <button class="chip" onclick={() => void runSearch(() => api.runSavedSearch(s.name))}>{s.name}</button>
    {/each}
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if outcome}
    <p class="count">{outcome.count} results</p>
    {#each outcome.notices ?? [] as notice}<p class="notice">{notice}</p>{/each}
    {#if outcome.semantic_coverage && outcome.semantic_coverage.embedded < outcome.semantic_coverage.eligible}
      <p class="notice">semantic index: {outcome.semantic_coverage.embedded} of {outcome.semantic_coverage.eligible} eligible assets</p>
    {/if}
    {#each outcome.text_coverage ?? [] as tc}
      <p class="notice">{tc.label}: {tc.covered} of {tc.eligible} {tc.noun} — {tc.remedy}</p>
    {/each}
    <ul class="grid">
      {#each outcome.results as hit (hit.asset)}
        <li>
          <button class="card" onclick={() => onselect(hit.asset)}>
            <img src={thumbUrl(hit.asset)} alt="" loading="lazy" />
            <span class="name">{hit.name}</span>
            {#if hit.timestamp_ms !== undefined}<span class="ts">@{Math.floor(hit.timestamp_ms / 60000)}m{String(Math.floor((hit.timestamp_ms % 60000) / 1000)).padStart(2, "0")}s</span>{/if}
            {#if hit.snippet}<span class="snippet">"{hit.snippet}"</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>
```

`Welcome.svelte`: headline, one-paragraph explanation, "Initialize catalog…" button (opens `open({ directory: true })` from `@tauri-apps/plugin-dialog`, then `api.initializeCatalog(path)`) and "Use existing catalog…" (same dialog → `api.useExistingCatalog`); errors shown inline with the full message (the remedy text is in it). `App.svelte`: on mount `api.appStatus()`; not ready → `<Welcome oninitialized={refresh} />`; ready → sidebar (Search | Volumes) + active surface + `<Inspector assetId={selected} />`. `Inspector.svelte` this task: `get_asset` on selection, thumbnail, name/size/dates, tags, PARA, volume badges (Task 9 completes verify state + keyframes). CSS grid: `grid-template-columns: 220px 1fr 320px`; inspector column collapses to 0 when `selected === null`.

- [ ] **Step 4: Verify + run the real app once**

`pnpm test && pnpm check && pnpm lint` → green. Then a live smoke: `pnpm tauri dev` against a real catalog (initialize one in a temp dir through the welcome flow; `maj scan` a small folder into it from the CLI; search finds it, thumbnails load after `maj index run`). Note what you saw in the commit message — this is the one manual check in the phase.

- [ ] **Step 5: Commit, open PR5**

```bash
git add apps/desktop/src apps/desktop/package.json apps/desktop/pnpm-lock.yaml
git commit -m "feat: search surface, first-run welcome flow, inspector skeleton"
```

---

### Task 9: Volumes surface + inspector completion + notices polish

**Files:**
- Create: `apps/desktop/src/lib/VolumesView.svelte`, `apps/desktop/src/lib/VolumesView.test.ts`, `apps/desktop/src/lib/Inspector.test.ts`
- Modify: `apps/desktop/src/lib/Inspector.svelte`, `apps/desktop/src/App.svelte`

- [ ] **Step 1: Failing tests**

- `VolumesView.test.ts`: mockIPC `list_volumes` → table renders one row per volume with label, online/offline badge, asset count, last-verified date; `notices` render above the table; read-only (no buttons).
- `Inspector.test.ts`: mockIPC `get_asset` with a full `AssetDetail` fixture (fields map, tags, para, volumes, verifications) → every section renders; verify state shows the latest verification's date + result; keyframe strip lists the manifest timestamps (mock `fetch` of `keyframesUrl` — jsdom: stub `globalThis.fetch`); `assetId = null` → inspector renders nothing (collapsed).

- [ ] **Step 2: Implement**

`VolumesView.svelte`: `$effect` load, table with `●`/`○` badges (same glyphs the CLI prints), `notices` paragraphs above. `Inspector.svelte` completion: metadata fields table, verification history (latest first, full history available under a details element), keyframe timestamp strip fetching `keyframesUrl(assetId)` JSON and rendering `@MmSSs` chips (no images — the manifest is what exists; images are watchlisted). App shell: surface switcher state, volumes surface wired.

- [ ] **Step 3: Verify + commit, open PR6**

`pnpm test && pnpm check && pnpm lint && just gui-build` → green.

```bash
git add apps/desktop/src
git commit -m "feat: volumes surface and completed inspector"
```

---

### Task 10: Release pipeline — tauri-action, armed updater, cargo-about

**Files:**
- Create: `.github/workflows/release.yml`, `about.toml`, `about.hbs`
- Modify: `apps/desktop/src-tauri/tauri.conf.json` (updater config), `apps/desktop/src-tauri/src/lib.rs` + `apps/desktop/src/App.svelte` (update check), `apps/desktop/package.json` (plugin dep if not present)

- [ ] **Step 1: Updater keypair — STOP, this step needs the user**

Generate: `cd apps/desktop && pnpm tauri signer generate -w /tmp/majestical-updater.key` (prompts for a password). Then **ask the user** to store two GitHub repo secrets (they must run this themselves or hand you a session where `gh` has admin on the repo):

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < /tmp/majestical-updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD   # the password chosen above
trash /tmp/majestical-updater.key                   # never commit or leave it around
```

The PUBLIC key (printed by the generate command, also in `/tmp/majestical-updater.key.pub`) goes in `tauri.conf.json`. **Do not proceed past this step until the secrets exist** — the release workflow degrades gracefully without them (unsigned artifacts), but "armed from day one" is the spec's commitment.

- [ ] **Step 2: Updater config + in-app check**

`tauri.conf.json` gains:

```json
"plugins": {
  "updater": {
    "pubkey": "<the generated public key>",
    "endpoints": ["https://github.com/statik/majestical/releases/latest/download/latest.json"]
  }
}
```

`lib.rs`: `.plugin(tauri_plugin_updater::Builder::new().build())`. `App.svelte` (or a tiny `lib/updater.ts`): on startup, `check()` from `@tauri-apps/plugin-updater`; if an update exists, a non-blocking banner ("Update to vX available — restart to apply") that runs `downloadAndInstall()` + `relaunch()` (`@tauri-apps/plugin-process`) on click. Errors (offline, no release yet) are silently logged to console — an update check must never block first paint. Capability file gains `process:default` if the plugin needs it (check the plugin's docs for the installed version).

- [ ] **Step 3: `release.yml`**

```yaml
name: Release
on:
  push:
    tags: ["v*"]
permissions:
  contents: write
jobs:
  version-sync:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<pinned>  # v<x>
        with: { persist-credentials: false }
      - run: ./scripts/version-sync.sh
  build:
    needs: version-sync
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: aarch64-apple-darwin
          - target: x86_64-apple-darwin
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@<pinned>  # v<x>
        with: { persist-credentials: false }
      - uses: dtolnay/rust-toolchain@<pinned>  # stable
        with: { targets: "${{ matrix.target }}" }
      - name: Install protoc
        run: brew install protobuf
      - uses: pnpm/action-setup@<pinned>  # v<x>
        with: { version: <VERIFIED> }
      - uses: actions/setup-node@<pinned>  # v<x>
        with: { node-version: 22, cache: pnpm, cache-dependency-path: apps/desktop/pnpm-lock.yaml }
      - run: pnpm install --frozen-lockfile
        working-directory: apps/desktop
      - name: License bundle (cargo-about)
        run: |
          cargo install cargo-about --locked --version <VERIFIED>
          cargo about generate --manifest-path apps/desktop/src-tauri/Cargo.toml \
            -o apps/desktop/dist-licenses.html about.hbs
      - uses: tauri-apps/tauri-action@<pinned-sha>  # v<VERIFIED — look it up>
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          projectPath: apps/desktop
          tagName: ${{ github.ref_name }}
          releaseName: "Majestical ${{ github.ref_name }}"
          releaseDraft: true
          includeUpdaterJson: true
          args: --target ${{ matrix.target }}
```

Notes to honor while writing it for real: no build caches in release jobs (no-cache rule); every action SHA-pinned with a version comment; `zizmor` + `actionlint` clean; the license bundle uploads with the release (add an `gh release upload`/`actions/upload-artifact` step wiring it to the draft — check what tauri-action exposes for extra assets in the verified version). `about.toml`: `accepted = ["Apache-2.0", "MIT", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0", "Zlib", "OpenSSL", "MPL-2.0"]` — run locally first and extend only with licenses actually present, each with a comment naming the crate that needs it.

- [ ] **Step 4: Exercise the pipeline with a real prerelease tag**

```bash
git tag v0.1.0-rc1 && git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git v0.1.0-rc1
gh run watch   # foreground; a tauri release build takes a while — let it finish
```

Verify: a DRAFT release exists with `.dmg`/`.app.tar.gz` for both targets, `latest.json`, `*.sig` signature files (proof the signing secrets work), and the license bundle. Delete the draft + tag afterwards (`gh release delete v0.1.0-rc1 --yes && git push --delete` via the credential-helper form). Fix-forward anything that failed — this step ends only when a tag produces a complete draft release.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml about.toml about.hbs apps/desktop
git commit -m "feat: tag-triggered draft releases with armed updater and license bundle"
```

---

### Task 11: Closing — mutation sweep, watchlist reconciliation, handoff 7C

- [ ] **Step 1: Scoped cargo-mutants runs — FOREGROUND, one at a time**

(The 7A lesson, verbatim requirement: no `run_in_background`, no monitors, no sleep-polling. Each run finishes before the next starts.)

```bash
cargo mutants --package majestical-services --timeout 300 --file crates/services/src/notices.rs
cargo mutants --package majestical-services --timeout 300 --file crates/services/src/state_dir.rs
cargo mutants --manifest-path apps/desktop/src-tauri/Cargo.toml --timeout 300 --file src/commands.rs
```

Triage every survivor: caught-by-a-sibling-suite, add-a-test, or accepted-with-reason — written into the watchlist's new "cargo-mutants triage (phase 7B)" section, same format as 7A's. Confirm at least one representative survivor by hand (mutate, run, watch it fail, revert).

- [ ] **Step 2: Watchlist reconciliation**

In `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`:
- Mark CLOSED (with PR numbers): the stderr-diagnostics item, the four schemars-enum params, the two under-validating dry-runs.
- Add a "Phase 7B deferrals" section: anything found during execution, plus carried items this phase consciously skipped (scan_volume walk errors, `"ascmhl"` const, `IngestRun` rename, sync-location divergence, keyframe images, WebDriver e2e, Windows/Linux artifacts + signing/notarization, notices-at-completion timing for long verbs, GUI catalog-switching UX if it came up).

- [ ] **Step 3: Spec as-built section**

Append an "As-built (phase 7B)" section to `docs/superpowers/specs/2026-08-04-phase7b-gui-release-design.md`: the `app_status`/`use_existing_catalog` command additions and the persisted GUI config file, the `spawn_blocking` + `run_off_tokio_runtime` composition, where notices ride params instead of outcomes (sync location add/rm, describer config), and anything else that diverged.

- [ ] **Step 4: `HANDOFF-phase7C.md`**

Write `docs/superpowers/HANDOFF-phase7C.md` in the established shape (what shipped, architecture pointers into `apps/desktop`, the release-pipeline runbook — how to cut a release, where the updater keys live — process conventions carried forward, lessons learned). Update `HANDOFF-phase7B.md`'s header to note it is superseded.

- [ ] **Step 5: Final verification + PR7**

```bash
just ci && just gui-check && just gui-test && just version-sync
git add docs/superpowers apps/desktop crates
git commit -m "docs: phase 7B closing - mutants triage, watchlist, handoff 7C"
```

Squash-merge after green CI. Phase 7B is done when this PR merges and a prerelease tag has produced a complete draft release (Task 10 Step 4).

---

## Self-review notes (kept for the reviewer)

- **Spec coverage:** chunk 0 → Tasks 1-5; GUI scaffold/workspace/CI → Task 6; commands + protocol + tauri_parity → Task 7; search surface + first-run → Task 8; volumes + inspector → Task 9; release pipeline → Task 10; closing → Task 11. The spec's "notices render in the GUI" lands in Tasks 8-9 (SearchView notices block, VolumesView notices block).
- **Known plan-time uncertainties, flagged where they occur rather than guessed:** exact `rmcp` rejection shape for a bad enum value (Task 4 Step 4 pins it empirically); `describer_config`'s state-dir usage (Task 1 Step 8 says check); `common::fixture_catalog`'s exact signature (Task 2 Step 6); resources.rs blob-lookup reuse (Task 7 Step 4 says read it first); every dependency version and action SHA (verified at execution, by mandate).
- **Line numbers** in this plan are as of `main` at 73f167a — re-locate by content (the `#[expect(clippy::print_stderr)]` blocks are unambiguous anchors) if drift has occurred by execution time.



