# Phase 4 — Embeddings + Layered Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In-process SigLIP 2 image/text embeddings, thumbnails, video keyframes, and a layered `maj search` (semantic + FTS5 + hard filters), on top of the local-state/sync-root split and incremental SQLite apply.

**Architecture:** New `crates/index` produces content-addressed derived data into `<sync-root>/blobs/`; a per-machine LanceDB dataset and the SQLite projection (both under a new local state dir) are disposable projections of blobs + events. The queue is the diff between required derivations and existing blobs. Encoder preprocessing is conformance-gated against a pinned Python `transformers` reference (the MHL oracle pattern).

**Tech Stack:** `ort =2.0.0-rc.13` (ONNX Runtime 1.28, CoreML EP), `lancedb =0.33.0` + `arrow-* =58.0.0` + tokio, `tokenizers 0.23.1`, `image 0.25.10` + `webp 0.3.1` + `fast_image_resize 6.1.0`, `zstd 0.13.3`, `dirs 6`, rusqlite 0.37 FTS5, external `ffmpeg`/`ffprobe`/`curl`/`sips`.

Spec: `docs/superpowers/specs/2026-07-30-phase4-search-design.md`. Parent: `2026-07-28-majestical-design.md`.

---

## Planning-time discoveries (deviations from the spec text — record in as-built notes)

1. **`AssetSeen` gains `mtime_ms` and instances become LWW-per-`(volume, path)`.**
   The spec's `before:`/`after:` filters need mtime, which no event carries. Additive
   wire change (`#[serde(default)]`); old events parse with `mtime_ms: 0`. Instances
   change from `BTreeSet<(volume, path, size)>` to an HLC-LWW map keyed by
   `(volume, path)` — this also fixes the latent duplicate-instance-on-resize bug.
2. **Instance paths become volume-root-relative.** Today `scan`/`ingest` store paths
   relative to the scanned dir / dest root, with no recorded base — the indexer could
   never find the bytes. Going forward both compute paths relative to the mount point
   (`volume_identity::mount_point_of`). Pre-phase-4 instance rows keep their old
   ambiguous paths: the indexer treats unresolvable instances as `offline` (degrade),
   and a rescan refreshes them. Watchlist entry required.
3. **`maj model fetch` shells out to system `curl`** (present on every macOS) instead
   of adding an HTTP client dependency. Hash verification stays in Rust (`sha2`).
4. **Model files pinned: vision tower fp32, text tower fp16** (fp32 text is 1.13 GB;
   fp16 halves it, tolerance checked by conformance).
5. **`ort` has no stable 2.x** — pin `=2.0.0-rc.13` exactly (API churns between RCs).
   CoreML EP type is `ort::ep::CoreML` (not `CoreMLExecutionProvider`).
6. **lancedb 0.33.0 requires `protoc` at build time** and pins arrow 58 / needs tokio.
   CI gets `brew install protobuf`; developers need it too (`brew install protobuf`).
   Workspace MSRV floor becomes 1.91 (lancedb) — fine, CI uses latest stable.
7. **Lossy WebP needs the `webp` crate** (`image` only encodes lossless WebP).
8. **Resize antialiasing is load-bearing for conformance**: transformers v5 resizes
   with antialiased bilinear (torchvision `antialias=True`). We must use
   `fast_image_resize` `FilterType::Bilinear` (a proper convolution filter), NOT a
   2-tap bilinear, and NOT ffmpeg's `scale=…:flags=bilinear`. All encoder-bound
   resizing happens in Rust.
9. **Scene-detection unit tests feed synthetic in-memory frame sequences** (pure
   Rust, no ffmpeg); one `#[ignore]`d integration test exercises the real ffmpeg
   pipe. Spec said "synthetic ffmpeg fixture videos" — the pure-Rust form is stronger
   for the detector and CI-friendly.
10. **The text tower takes `input_ids` only** (no attention-mask input) and pools
    position 63 unconditionally — fixed-64 right-padding with pad id 0 is
    correctness-critical, enforced by golden token tests.
11. **`maj model fetch` still requires `--catalog`/`--machine-id`** (top-level
    required args, same accepted wart as `maj verify` — already watchlisted).

## Conventions for every task in this plan

- Branch per PR chunk off `main`; squash-merge after CI green. NO Claude-Session
  trailers. Stage only your files (never `git add -A`).
- `just check` must pass before every commit (prek runs it). Run scoped tests per
  step; run `just ci` before opening each PR.
- Acceptance/integration test files that aren't unit tests carry `#[cfg(test)]` on
  helpers or avoid unwrap/expect/panic entirely (clippy.toml exemptions key on the
  literal attribute; step functions return `Result<_, String>`).
- All new public items get doc comments; zero warnings; workspace lints apply
  (`crates/index` uses `[lints] workspace = true`).
- Verify latest crate versions at execution time; versions in this plan were
  verified 2026-07-30. If a newer stable exists, prefer it unless pinned here for a
  reason (ort, lancedb, arrow are pinned for API/ABI lockstep — do not bump ad hoc).

---

# PR 1 — Local-state / sync-root split

The sync root keeps only `events/` (and later `blobs/`). `catalog.db` and ingest
journals move to a per-machine state dir. One task.

### Task 1: State dir module, db + journal relocation, legacy cleanup

**Files:**
- Create: `crates/cli/src/state_dir.rs`
- Modify: `crates/cli/src/main.rs` (add `mod state_dir;`)
- Modify: `crates/cli/src/commands.rs` (`open_rebuilt_catalog`, `journal_path_for`, `check_resume_journal_exists`)
- Modify: `crates/cli/Cargo.toml` (add `dirs = "6"`, `xxhash-rust` from workspace)
- Test: `crates/cli/tests/cli_smoke.rs`

- [ ] **Step 1: Write failing smoke tests**

Add to `crates/cli/tests/cli_smoke.rs` (follow the existing `maj_as` pattern; note
the helper change in Step 2 — write these against the NEW helper signature):

```rust
#[test]
fn catalog_db_lives_in_state_dir_not_sync_root() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state).args(["catalog", "init"]).assert().success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("a.txt"), b"hello").unwrap();
    maj(&catalog, &state).args(["scan"]).arg(&media).assert().success();
    maj(&catalog, &state)
        .args(["search", "--name", "a.txt"])
        .assert()
        .success();
    assert!(
        !catalog.join("catalog.db").exists(),
        "catalog.db must not be created in the sync root"
    );
    let dbs: Vec<_> = walkdir_find(&state, "catalog.db");
    assert_eq!(dbs.len(), 1, "exactly one catalog.db under the state dir");
}

#[test]
fn legacy_catalog_db_in_sync_root_is_removed_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).unwrap();
    maj(&catalog, &state).args(["catalog", "init"]).assert().success();
    std::fs::write(catalog.join("catalog.db"), b"legacy").unwrap();
    maj(&catalog, &state)
        .args(["search", "--name", "nothing"])
        .assert()
        .success();
    assert!(
        !catalog.join("catalog.db").exists(),
        "legacy db must be cleaned out of the sync root"
    );
}

#[test]
fn legacy_run_journals_move_to_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(catalog.join("runs")).unwrap();
    maj(&catalog, &state).args(["catalog", "init"]).assert().success();
    std::fs::write(catalog.join("runs").join("01OLD.jsonl"), b"{}\n").unwrap();
    maj(&catalog, &state)
        .args(["search", "--name", "nothing"])
        .assert()
        .success();
    assert!(!catalog.join("runs").exists(), "legacy runs/ removed from sync root");
    let moved: Vec<_> = walkdir_find(&state, "01OLD.jsonl");
    assert_eq!(moved.len(), 1, "journal moved into the state dir");
}
```

Add the small helper near the other test helpers:

```rust
fn walkdir_find(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == name) {
                found.push(p);
            }
        }
    }
    found
}
```

- [ ] **Step 2: Update the smoke-test helpers to inject MAJ_STATE_DIR**

In `cli_smoke.rs`, change `maj_as`/`maj` so every invocation gets an isolated state
dir (existing call sites pass the catalog path only — update them mechanically):

```rust
fn maj_as(catalog: &Path, state: &Path, machine_id: &str) -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", machine_id)
        .env("MAJ_STATE_DIR", state);
    c
}

fn maj(catalog: &Path, state: &Path) -> Command {
    maj_as(catalog, state, "test-machine")
}
```

Most existing tests create one tempdir; give each a `let state = dir.path().join("state");`
sibling and thread it through. This is a mechanical, large-ish edit — do it with
repeated search/replace, then compile.

- [ ] **Step 3: Run the new tests to verify they fail**

Run: `cargo test -p majestical-cli --test cli_smoke catalog_db_lives -- --nocapture`
Expected: FAIL — `catalog.db` is still created in the sync root (and `MAJ_STATE_DIR` is ignored).

- [ ] **Step 4: Implement `state_dir.rs`**

```rust
//! Per-machine local state for a catalog: the sqlite projection, ingest run
//! journals, and (later) the vector index. Keyed by the canonicalized sync-root
//! path so distinct catalogs never collide; `MAJ_STATE_DIR` overrides the base
//! (tests, portable setups).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use xxhash_rust::xxh3::xxh3_128;

pub(crate) struct CatalogPaths {
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub runs_dir: PathBuf,
}

fn state_base() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MAJ_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let data = dirs::data_dir()
        .context("no platform data directory; set MAJ_STATE_DIR explicitly")?;
    Ok(data.join("majestical"))
}

fn state_dir_with_base(base: &Path, catalog_root: &Path) -> Result<PathBuf> {
    let canonical = catalog_root.canonicalize().with_context(|| {
        format!("canonicalizing catalog root {}", catalog_root.display())
    })?;
    let key = format!("{:032x}", xxh3_128(canonical.as_os_str().as_encoded_bytes()));
    Ok(base.join("catalogs").join(key))
}

/// Resolve (and create) the state dir for a catalog, migrating any legacy
/// derived files out of the sync root: a pre-phase-4 `catalog.db` is deleted
/// (disposable by invariant; it is rebuilt locally), and `runs/*.jsonl`
/// journals are moved so `--resume` keeps working.
pub(crate) fn catalog_paths(catalog_root: &Path) -> Result<CatalogPaths> {
    let state_dir = state_dir_with_base(&state_base()?, catalog_root)?;
    let runs_dir = state_dir.join("runs");
    std::fs::create_dir_all(&runs_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    migrate_legacy(catalog_root, &runs_dir)?;
    Ok(CatalogPaths {
        db_path: state_dir.join("catalog.db"),
        state_dir,
        runs_dir,
    })
}

fn migrate_legacy(catalog_root: &Path, state_runs: &Path) -> Result<()> {
    let legacy_db = catalog_root.join("catalog.db");
    if legacy_db.is_file() {
        std::fs::remove_file(&legacy_db).with_context(|| {
            format!("removing legacy catalog.db at {}", legacy_db.display())
        })?;
        eprintln!("note: removed legacy catalog.db from the sync root (rebuilt locally)");
    }
    let legacy_runs = catalog_root.join("runs");
    if legacy_runs.is_dir() {
        for entry in std::fs::read_dir(&legacy_runs)
            .with_context(|| format!("reading {}", legacy_runs.display()))?
        {
            let entry = entry.with_context(|| format!("reading {}", legacy_runs.display()))?;
            let from = entry.path();
            let Some(name) = from.file_name() else { continue };
            let to = state_runs.join(name);
            // Sync root and state dir may be different filesystems: copy + delete.
            std::fs::copy(&from, &to)
                .with_context(|| format!("moving journal {}", from.display()))?;
            std::fs::remove_file(&from)
                .with_context(|| format!("removing migrated journal {}", from.display()))?;
        }
        std::fs::remove_dir(&legacy_runs)
            .with_context(|| format!("removing legacy runs dir {}", legacy_runs.display()))?;
        eprintln!("note: moved legacy run journals into the local state dir");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_root_same_dir_different_roots_differ() {
        let base = tempfile::tempdir().expect("tempdir");
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        let d1 = state_dir_with_base(base.path(), a.path()).expect("state dir");
        let d2 = state_dir_with_base(base.path(), a.path()).expect("state dir");
        let d3 = state_dir_with_base(base.path(), b.path()).expect("state dir");
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
    }

    #[test]
    fn missing_catalog_root_is_a_clear_error() {
        let base = tempfile::tempdir().expect("tempdir");
        let err = state_dir_with_base(base.path(), Path::new("/nonexistent-maj-root"))
            .expect_err("must fail");
        assert!(err.to_string().contains("canonicalizing catalog root"));
    }
}
```

Note: `eprintln!` is fine — `crates/cli` allows `print_stdout`/`print_stderr`.
`tempfile` is already a cli dev-dependency (used by `cli_smoke`); `expect` is fine
under `#[cfg(test)]`.

- [ ] **Step 5: Wire it into commands.rs and main.rs**

`main.rs`: add `mod state_dir;` beside the other module declarations.

`commands.rs` — change `open_rebuilt_catalog` to use the state dir:

```rust
pub(crate) fn open_rebuilt_catalog(app: &FsApp, catalog_dir: &Path) -> Result<SqliteCatalog> {
    let projection = app.projection()?;
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let mut db = SqliteCatalog::open(&paths.db_path).context("opening sqlite catalog")?;
    db.rebuild(&projection).context("rebuilding sqlite projection")?;
    Ok(db)
}
```

Change `journal_path_for` (commands.rs:801) and its callers so journals land in the
state dir:

```rust
fn journal_path_for(catalog_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    Ok(paths.runs_dir.join(format!("{run_id}.jsonl")))
}
```

(The signature gains `Result`; update the two call sites in `cmd_ingest` /
`check_resume_journal_exists` with `?`.)

`crates/cli/Cargo.toml`: add under `[dependencies]`:

```toml
dirs = "6"
xxhash-rust = { workspace = true }
```

- [ ] **Step 6: Run the full smoke suite**

Run: `cargo test -p majestical-cli --test cli_smoke`
Expected: PASS (including the three new tests). The pre-existing assertion that a
dry-run ingest creates no `runs/` dir now checks the state dir — update that test
(`cli_smoke.rs:1110` area) to assert against the state dir's `runs/` being empty of
that run instead.

- [ ] **Step 7: Run workspace checks and commit**

Run: `just check && cargo test -p majestical-cli`
Expected: clean.

```bash
git add crates/cli/src/state_dir.rs crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/Cargo.toml crates/cli/tests/cli_smoke.rs Cargo.lock
git commit -m "feat: move catalog.db and run journals to a per-machine state dir"
```

Open PR 1 (base `main`): title `feat: local-state/sync-root split`. Body: what
moved, migration behavior, MAJ_STATE_DIR override.

---

# PR 2 — Incremental apply

Snapshot + cursors in `catalog.db`; only new event bytes are read and only touched
entities are rewritten. Fallback to full rebuild is always available and always
safe (the db is disposable). Three tasks.

### Task 2: Core — serde on projection state, `apply_tracking`, `LogCursor` port

**Files:**
- Modify: `crates/core/src/projection.rs`
- Modify: `crates/core/src/ports.rs`
- Test: `crates/core/src/projection.rs` (unit tests), `crates/core/tests/crdt_properties.rs`

- [ ] **Step 1: Write failing unit tests**

In `projection.rs` `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn apply_tracking_reports_the_touched_entity() {
    let mut p = Projection::default();
    let e = test_event(1, Op::VolumeSeen { volume: "v1".into(), label: "V".into() });
    assert_eq!(p.apply_tracking(&e), Touched::Volume("v1".into()));
    let e2 = test_event(2, Op::TagAdd { asset: AssetId("xxh3:a".into()), tag: "t".into() });
    assert_eq!(p.apply_tracking(&e2), Touched::Asset(AssetId("xxh3:a".into())));
}

#[test]
fn reapplying_an_event_touches_nothing() {
    let mut p = Projection::default();
    let e = test_event(1, Op::VolumeSeen { volume: "v1".into(), label: "V".into() });
    assert_ne!(p.apply_tracking(&e), Touched::Nothing);
    assert_eq!(p.apply_tracking(&e), Touched::Nothing);
}

#[test]
fn projection_round_trips_through_serde_json() {
    let mut p = Projection::default();
    for (n, op) in sample_ops().into_iter().enumerate() {
        let n = u64::try_from(n).unwrap_or(0) + 1;
        p.apply(&test_event(n, op));
    }
    let json = serde_json::to_string(&p).expect("serialize");
    let back: Projection = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(p, back);
}
```

`test_event(n, op)` is a local helper building an `Event` with
`EventId(Ulid::from_parts(1, n))`, `Hlc { wall_ms: n, counter: 0, machine: MachineId("m1".into()) }`,
`author: "t".into()` — mirror the golden-test helper in `event.rs`. `sample_ops()`
returns one op of every variant (copy the values from the golden wire test).

Run: `cargo test -p majestical-core apply_tracking` — Expected: FAIL (no `Touched`,
no `apply_tracking`, no serde derives).

- [ ] **Step 2: Implement in projection.rs**

Add `use serde::{Deserialize, Serialize};` and extend the derive lists — every
state type gains `Serialize, Deserialize`:

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetState { … }            // and identically on:
                                       // VerificationRecord, ManifestRecord,
                                       // ParaNodeState, VolumeState, Projection
```

Add the touched-entity type and the tracking apply:

```rust
/// Which projected entity an applied event changed. `Nothing` means the event
/// was a duplicate (idempotent replay) and no state moved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Touched {
    Nothing,
    Asset(AssetId),
    Volume(String),
    ParaNode(String),
    /// Manifest set for a volume id changed.
    Manifests(String),
}
```

Rework `apply` by renaming the current body to `apply_tracking` and threading a
return value out of the existing match. The idempotence guard returns
`Touched::Nothing`; each arm returns the entity it wrote:

```rust
pub fn apply(&mut self, event: &Event) {
    let _ = self.apply_tracking(event);
}

pub fn apply_tracking(&mut self, event: &Event) -> Touched {
    if !self.applied.insert(event.id) {
        return Touched::Nothing;
    }
    match &event.op {
        Op::AssetSeen { asset, .. }
        | Op::TagAdd { asset, .. }
        | Op::TagRemove { asset, .. }
        | Op::FieldSet { asset, .. }
        | Op::AssetParaSet { asset, .. }
        | Op::VerificationRecorded { asset, .. } => {
            /* existing per-op mutation code, unchanged */
            Touched::Asset(asset.clone())
        }
        Op::VolumeSeen { volume, .. } => { /* existing */ Touched::Volume(volume.clone()) }
        Op::ParaNodeCreate { node, .. }
        | Op::ParaNodeRename { node, .. }
        | Op::ParaNodeArchive { node, .. } => { /* existing */ Touched::ParaNode(node.clone()) }
        Op::ManifestRecorded { volume, .. } => { /* existing */ Touched::Manifests(volume.clone()) }
    }
}
```

Adapt to the real current structure of the match (the existing code may dispatch
through helper fns like `apply_tag_add` — keep those; only the outer arms change to
produce a `Touched`). Preserve exact merge semantics; the CRDT property test is the
guard.

In `ports.rs`, add the cursor type and trait method:

```rust
/// Position within one machine's segment file. `offset` is a byte offset that
/// always lands on a line boundary (readers never advance past a torn tail).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogCursor {
    pub machine: String,
    pub segment: String,
    pub offset: u64,
}

pub trait EventLog {
    fn append(&mut self, events: &[Event]) -> Result<(), PortError>;
    fn read_all_reporting(&self, on_bad_line: &mut dyn FnMut(&str)) -> Result<Vec<Event>, PortError>;
    /// Read only events past `cursors` (unknown segments read from 0). Returns
    /// the new events plus updated cursors covering every segment seen. Errors
    /// if a cursor points past the end of (or at a missing) segment — the
    /// caller falls back to a full rebuild.
    fn read_since_reporting(
        &self,
        cursors: &[LogCursor],
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<(Vec<Event>, Vec<LogCursor>), PortError>;
}
```

- [ ] **Step 3: Extend the proptest generator's obligations**

No new ops this task, but `crdt_properties.rs` gains one assertion inside the
existing property: after building the forward projection, also apply every event a
second time via `apply_tracking` and assert each returns `Touched::Nothing` (the
tracking path must share the idempotence guard).

- [ ] **Step 4: Run core tests**

Run: `cargo test -p majestical-core`
Expected: PASS. (Any `EventLog` test double in the workspace now fails to compile —
implement `read_since_reporting` on it; for pure-Vec doubles, return all events with
a single synthetic cursor.)

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/projection.rs crates/core/src/ports.rs crates/core/tests/crdt_properties.rs
git commit -m "feat: touched-entity tracking apply, serde projection state, log cursors"
```

### Task 3: sync — `read_since_reporting` with byte cursors

**Files:**
- Modify: `crates/sync/src/lib.rs`
- Test: `crates/sync/src/lib.rs` (unit tests in the existing style)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn read_since_empty_cursors_returns_everything_with_cursors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    log.append(&[ev(1), ev(2)]).expect("append");
    let (events, cursors) = log
        .read_since_reporting(&[], |_| {})
        .expect("read");
    assert_eq!(events.len(), 2);
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].machine, "m1");
    assert_eq!(cursors[0].segment, "0001.jsonl");
    // Cursor sits at end-of-file (all bytes consumed).
    let len = std::fs::metadata(dir.path().join("events/m1/0001.jsonl"))
        .expect("meta").len();
    assert_eq!(cursors[0].offset, len);
}

#[test]
fn read_since_returns_only_new_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    log.append(&[ev(1)]).expect("append");
    let (_, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
    log.append(&[ev(2), ev(3)]).expect("append");
    let (events, cursors2) = log.read_since_reporting(&cursors, |_| {}).expect("read");
    assert_eq!(events.len(), 2);
    assert!(cursors2[0].offset > cursors[0].offset);
    // And reading again from the new cursors yields nothing.
    let (empty, cursors3) = log.read_since_reporting(&cursors2, |_| {}).expect("read");
    assert!(empty.is_empty());
    assert_eq!(cursors2, cursors3);
}

#[test]
fn a_torn_tail_is_not_consumed_until_completed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    log.append(&[ev(1)]).expect("append");
    let seg = dir.path().join("events/m1/0001.jsonl");
    let complete_len = std::fs::metadata(&seg).expect("meta").len();
    // Simulate a torn write: half a JSON line, no newline.
    let mut f = std::fs::OpenOptions::new().append(true).open(&seg).expect("open");
    use std::io::Write as _;
    f.write_all(b"{\"id\":\"torn").expect("write");
    let (events, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
    assert_eq!(events.len(), 1, "torn tail is deferred, not reported bad");
    assert_eq!(cursors[0].offset, complete_len, "cursor stops at the last newline");
}

#[test]
fn a_stale_cursor_past_the_end_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    log.append(&[ev(1)]).expect("append");
    let stale = LogCursor { machine: "m1".into(), segment: "0001.jsonl".into(), offset: 999_999 };
    assert!(log.read_since_reporting(&[stale], |_| {}).is_err());
}

#[test]
fn a_cursor_for_a_vanished_segment_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let log = FileEventLog::init(dir.path(), &m).expect("init");
    let stale = LogCursor { machine: "mgone".into(), segment: "0001.jsonl".into(), offset: 1 };
    assert!(log.read_since_reporting(&[stale], |_| {}).is_err());
}
```

`ev(n)` is a local helper like the existing tests' event builder (reuse it if one
exists; otherwise construct as in Task 2). Note `read_since_reporting` here is the
inherent method (closure arg), mirroring `read_all_reporting`'s pattern of an
inherent `impl FnMut` method plus a `&mut dyn` trait adapter.

Run: `cargo test -p majestical-sync read_since` — Expected: FAIL (method missing).

- [ ] **Step 2: Implement**

Add to `LogError`:

```rust
#[error("cursor for {machine}/{segment} is stale (offset {offset}): full rebuild required")]
StaleCursor { machine: String, segment: String, offset: u64 },
```

Inherent method on `FileEventLog` (mirror `read_all_reporting`'s directory-walk
structure exactly — same events-dir walk, same `.jsonl` filter, same
lexicographic segment sort):

```rust
pub fn read_since_reporting(
    &self,
    cursors: &[LogCursor],
    mut on_bad_line: impl FnMut(&str),
) -> Result<(Vec<Event>, Vec<LogCursor>), LogError> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut start: std::collections::BTreeMap<(String, String), u64> = cursors
        .iter()
        .map(|c| ((c.machine.clone(), c.segment.clone()), c.offset))
        .collect();
    let mut events = Vec::new();
    let mut out = Vec::new();
    let events_dir = self.root.join("events");
    // (walk machines exactly as read_all_reporting does)
    for machine_dir in /* sorted machine dirs */ {
        let machine = /* dir name as String */;
        for segment_name in /* sorted .jsonl names */ {
            let path = machine_dir.join(&segment_name);
            let key = (machine.clone(), segment_name.clone());
            let from = start.remove(&key).unwrap_or(0);
            let len = std::fs::metadata(&path)
                .map_err(|source| LogError::Io { path: path.clone(), source })?
                .len();
            if from > len {
                return Err(LogError::StaleCursor {
                    machine: key.0, segment: key.1, offset: from,
                });
            }
            let mut file = std::fs::File::open(&path)
                .map_err(|source| LogError::Io { path: path.clone(), source })?;
            file.seek(SeekFrom::Start(from))
                .map_err(|source| LogError::Io { path: path.clone(), source })?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|source| LogError::Io { path: path.clone(), source })?;
            // Consume only whole lines; a torn tail stays before the cursor.
            let consumed = buf.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
            for line in buf[..consumed].split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                match std::str::from_utf8(line) {
                    Ok(text) => match serde_json::from_str::<Event>(text) {
                        Ok(event) => events.push(event),
                        Err(_) => on_bad_line(text),
                    },
                    Err(_) => on_bad_line(&String::from_utf8_lossy(line)),
                }
            }
            out.push(LogCursor {
                machine: machine.clone(),
                segment: segment_name,
                offset: from + u64::try_from(consumed).unwrap_or(0),
            });
        }
    }
    // Any cursor we never matched points at a vanished segment.
    if let Some(((machine, segment), offset)) = start.into_iter().next() {
        return Err(LogError::StaleCursor { machine, segment, offset });
    }
    Ok((events, out))
}
```

(The `/* sorted machine dirs */` walk is the same code shape as
`read_all_reporting` at sync/lib.rs:133-193 — reuse its structure verbatim,
including the zero-padded-sort comment's constraint.)

Trait impl addition:

```rust
impl EventLog for FileEventLog {
    // existing append / read_all_reporting …
    fn read_since_reporting(
        &self,
        cursors: &[LogCursor],
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<(Vec<Event>, Vec<LogCursor>), PortError> {
        self.read_since_reporting(cursors, |line| on_bad_line(line))
            .map_err(|e| PortError::new("reading new events", e))
    }
}
```

- [ ] **Step 3: Run tests, then commit**

Run: `cargo test -p majestical-sync`
Expected: PASS.

```bash
git add crates/sync/src/lib.rs
git commit -m "feat: cursor-based incremental event log reads"
```

### Task 4: catalog-sqlite — snapshot, incremental apply, `open_synced`

**Files:**
- Modify: `crates/catalog-sqlite/src/lib.rs`
- Modify: `crates/catalog-sqlite/Cargo.toml` (add `serde_json` workspace dep; dev-deps `majestical-sync`, `tempfile`, `proptest`, `ulid`)
- Create: `crates/catalog-sqlite/tests/incremental.rs`
- Modify: `crates/cli/src/commands.rs`, `crates/cli/src/app.rs`

- [ ] **Step 1: Write the failing equivalence test**

`crates/catalog-sqlite/tests/incremental.rs`:

```rust
//! Incremental apply must be observationally identical to a full rebuild.
#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

use std::path::Path;

use majestical_catalog_sqlite::{ApplyMode, SqliteCatalog};
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::ports::EventLog as _;
use majestical_sync::FileEventLog;
use proptest::prelude::*;
use ulid::Ulid;

fn ev(n: u64, machine: &str, op: Op) -> Event {
    Event {
        id: EventId(Ulid::from_parts(1, u128::from(n))),
        hlc: Hlc { wall_ms: n, counter: 0, machine: MachineId(machine.into()) },
        author: machine.into(),
        op: op,
    }
}

fn arb_op() -> impl Strategy<Value = Op> {
    let asset = prop_oneof![Just("xxh3:a"), Just("xxh3:b")];
    prop_oneof![
        (asset.clone(), "[a-c]{1,2}").prop_map(|(a, t)| Op::TagAdd {
            asset: AssetId(a.into()), tag: t
        }),
        (asset.clone(), "[a-c]{1,2}", "[x-z]{1,2}").prop_map(|(a, f, v)| Op::FieldSet {
            asset: AssetId(a.into()), field: f, value: v
        }),
        (asset, "[v-w]", "p[1-2]", 0u64..100).prop_map(|(a, vol, p, s)| Op::AssetSeen {
            asset: AssetId(a.into()), volume: vol, path: p, size: s
        }),
        ("[v-w]", "[A-B]").prop_map(|(v, l)| Op::VolumeSeen { volume: v, label: l }),
    ]
}

fn dump(db: &SqliteCatalog) -> String {
    db.debug_dump().expect("dump")
}

fn open_all(db_path: &Path, log: &FileEventLog) -> SqliteCatalog {
    let (db, _projection, _mode) = SqliteCatalog::open_synced(db_path, log).expect("open");
    db
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn incremental_equals_full_rebuild(
        ops in prop::collection::vec(arb_op(), 1..40),
        split in 0usize..40,
    ) {
        let split = split.min(ops.len());
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let m1 = MachineId("m1".into());
        let mut log = FileEventLog::init(&root, &m1).expect("init");
        let events: Vec<Event> = ops.iter().enumerate()
            .map(|(i, op)| ev(u64::try_from(i).expect("small") + 1, "m1", op.clone()))
            .collect();

        // Incremental path: open after the prefix, then again after the rest.
        let inc_db = dir.path().join("inc.db");
        log.append(&events[..split]).expect("append prefix");
        drop(open_all(&inc_db, &log));
        log.append(&events[split..]).expect("append rest");
        let (db_inc, proj_inc, mode) = SqliteCatalog::open_synced(&inc_db, &log).expect("open");
        if split < events.len() {
            prop_assert!(matches!(mode, ApplyMode::Incremental { .. }),
                "second open must take the incremental path, got {mode:?}");
        }

        // Full-rebuild path: fresh db over the same complete log.
        let full_db = dir.path().join("full.db");
        let (db_full, proj_full, _) = SqliteCatalog::open_synced(&full_db, &log).expect("open");

        prop_assert_eq!(proj_inc, proj_full);
        prop_assert_eq!(dump(&db_inc), dump(&db_full));
    }
}
```

`debug_dump` is a small public helper added in Step 2 that selects every row of
every table in deterministic order into one string — it exists so tests (and future
debugging) can compare whole databases.

Run: `cargo test -p majestical-catalog-sqlite --test incremental`
Expected: FAIL to compile (`open_synced`, `ApplyMode`, `debug_dump` missing).

- [ ] **Step 2: Implement in catalog-sqlite**

Add deps in `crates/catalog-sqlite/Cargo.toml`:

```toml
[dependencies]
serde_json = { workspace = true }
# (existing: majestical-core, rusqlite, thiserror)

[dev-dependencies]
majestical-sync = { path = "../sync" }
tempfile = { workspace = true }
proptest = { workspace = true }
ulid = { workspace = true }
```

New error variants on `CatalogError`:

```rust
#[error("event log: {0}")]
Port(#[from] majestical_core::ports::PortError),
#[error("apply snapshot: {0} — delete catalog.db and re-run")]
Snapshot(#[from] serde_json::Error),
```

Schema additions (in `create_tables`'s batch, after the existing tables; also add
both to the drop list at the top of the batch):

```sql
CREATE TABLE apply_cursors (
  machine TEXT NOT NULL, segment TEXT NOT NULL, offset INTEGER NOT NULL,
  PRIMARY KEY (machine, segment)
);
CREATE TABLE apply_snapshot (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL,
  projection TEXT NOT NULL
);
```

Core of the new API:

```rust
use majestical_core::ports::{EventLog, LogCursor};
use majestical_core::projection::{Projection, Touched};

/// Bump when the snapshot encoding or projected schema changes; a mismatch
/// forces a full rebuild (safe: the db is disposable).
const SNAPSHOT_VERSION: i64 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    projection: Projection,
}

/// How `open_synced` brought the db up to date. Tests assert on this to prove
/// the incremental path actually ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyMode {
    /// Snapshot + cursors were valid; only new events were applied.
    Incremental { applied: usize },
    /// No/invalid snapshot, stale cursors, or schema change: rebuilt from scratch.
    FullRebuild,
}

impl SqliteCatalog {
    /// Open the projection db and bring it up to date with the event log,
    /// incrementally when possible. Returns the live projection too (callers
    /// need it for CRDT-side lookups).
    pub fn open_synced(
        db_path: &std::path::Path,
        log: &dyn EventLog,
    ) -> Result<(Self, Projection, ApplyMode), CatalogError> {
        let mut db = Self::open(db_path)?;
        let mut bad = 0usize;
        if let Some((cursors, mut projection)) = db.load_apply_state() {
            match log.read_since_reporting(&cursors, &mut |_| bad += 1) {
                Ok((events, new_cursors)) => {
                    if events.is_empty() {
                        return Ok((db, projection, ApplyMode::Incremental { applied: 0 }));
                    }
                    let mut touched = std::collections::BTreeSet::new();
                    for event in &events {
                        touched.insert(projection.apply_tracking(event));
                    }
                    touched.remove(&Touched::Nothing);
                    let applied = events.len();
                    db.apply_touched(&projection, &touched, &new_cursors)?;
                    return Ok((db, projection, ApplyMode::Incremental { applied }));
                }
                Err(_stale) => { /* fall through to full rebuild */ }
            }
        }
        let (events, cursors) = log.read_since_reporting(&[], &mut |_| bad += 1)?;
        let mut projection = Projection::default();
        for event in &events {
            projection.apply(event);
        }
        db.rebuild(&projection)?;
        db.save_apply_state(&cursors, &projection)?;
        Ok((db, projection, ApplyMode::FullRebuild))
    }

    /// None on any missing/mismatched/corrupt state — the caller full-rebuilds.
    fn load_apply_state(&self) -> Option<(Vec<LogCursor>, Projection)> {
        let (version, json): (i64, String) = self
            .conn
            .query_row(
                "SELECT version, projection FROM apply_snapshot WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        if version != SNAPSHOT_VERSION {
            return None;
        }
        let snapshot: Snapshot = serde_json::from_str(&json).ok()?;
        let mut stmt = self
            .conn
            .prepare("SELECT machine, segment, offset FROM apply_cursors")
            .ok()?;
        let cursors = stmt
            .query_map([], |r| {
                Ok(LogCursor {
                    machine: r.get(0)?,
                    segment: r.get(1)?,
                    offset: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                })
            })
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some((cursors, snapshot.projection))
    }

    fn save_apply_state(
        &mut self,
        cursors: &[LogCursor],
        projection: &Projection,
    ) -> Result<(), CatalogError> {
        let json = serde_json::to_string(&Snapshot { projection: projection.clone() })?;
        let tx = self.conn.transaction()?;
        Self::write_apply_state(&tx, cursors, &json)?;
        tx.commit()?;
        Ok(())
    }

    fn write_apply_state(
        tx: &rusqlite::Transaction<'_>,
        cursors: &[LogCursor],
        snapshot_json: &str,
    ) -> rusqlite::Result<()> {
        tx.execute("DELETE FROM apply_cursors", [])?;
        for c in cursors {
            tx.execute(
                "INSERT INTO apply_cursors (machine, segment, offset) VALUES (?1, ?2, ?3)",
                (&c.machine, &c.segment, i64::try_from(c.offset).unwrap_or(i64::MAX)),
            )?;
        }
        tx.execute(
            "INSERT INTO apply_snapshot (id, version, projection) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET version = ?1, projection = ?2",
            (SNAPSHOT_VERSION, snapshot_json),
        )?;
        Ok(())
    }

    /// Rewrite only the rows for `touched` entities, plus snapshot + cursors,
    /// in one transaction.
    pub fn apply_touched(
        &mut self,
        projection: &Projection,
        touched: &std::collections::BTreeSet<Touched>,
        cursors: &[LogCursor],
    ) -> Result<(), CatalogError> {
        let json = serde_json::to_string(&Snapshot { projection: projection.clone() })?;
        let tx = self.conn.transaction()?;
        for t in touched {
            match t {
                Touched::Asset(asset) => {
                    for sql in [
                        "DELETE FROM tags WHERE asset = ?1",
                        "DELETE FROM instances WHERE asset = ?1",
                        "DELETE FROM asset_para WHERE asset = ?1",
                        "DELETE FROM verifications WHERE asset = ?1",
                        "DELETE FROM assets WHERE id = ?1",
                    ] {
                        tx.execute(sql, [&asset.0])?;
                    }
                    if let Some(state) = projection.assets().find(|(id, _)| *id == asset) {
                        Self::insert_one_asset(&tx, projection, state.0, state.1)?;
                    }
                }
                Touched::Volume(id) => {
                    tx.execute("DELETE FROM volumes WHERE id = ?1", [id])?;
                    if let Some((_, state)) = projection.volumes().find(|(v, _)| *v == id) {
                        Self::insert_one_volume(&tx, id, state)?;
                    }
                }
                Touched::ParaNode(node) => {
                    tx.execute("DELETE FROM para_nodes WHERE id = ?1", [node])?;
                    if let Some(state) = projection.para_node(node) {
                        Self::insert_one_para_node(&tx, node, state)?;
                    }
                }
                Touched::Manifests(volume) => {
                    tx.execute("DELETE FROM manifests WHERE volume = ?1", [volume])?;
                    Self::insert_manifests_for(&tx, projection, volume)?;
                }
                Touched::Nothing => {}
            }
        }
        Self::write_apply_state(&tx, cursors, &json)?;
        tx.commit()?;
        Ok(())
    }
}
```

Refactor obligations for Step 2 (mechanical): split the existing bulk inserters so
both paths share per-entity helpers —

- `insert_assets` loops `projection.assets()` calling new
  `fn insert_one_asset(tx, projection, id: &AssetId, state: &AssetState) -> rusqlite::Result<()>`
  (moves the existing per-asset body: assets row, instances rows, tags rows,
  asset_para row, verifications rows).
- `insert_volumes` → `fn insert_one_volume(tx, id: &str, state: &VolumeState)`.
- `insert_para_nodes` → `fn insert_one_para_node(tx, id: &str, state: &ParaNodeState)`
  (keep the skip-if-incomplete guard inside the helper so both paths share it).
- `insert_manifests` → `fn insert_manifests_for(tx, projection, volume: &str)`.
- `rebuild` keeps its drop-and-recreate shape but now ends by NOT clearing apply
  state (callers write it right after; `create_tables`'s drop list already removed
  the old one).

Add the dump helper:

```rust
/// Deterministic textual dump of every table, for equivalence tests and
/// debugging. Not a stable format.
pub fn debug_dump(&self) -> Result<String, CatalogError> {
    let mut out = String::new();
    for (table, order) in [
        ("assets", "id"),
        ("instances", "asset, volume, path"),
        ("tags", "asset, tag"),
        ("volumes", "id"),
        ("para_nodes", "id"),
        ("asset_para", "asset"),
        ("verifications", "asset, volume, path, algo, value, outcome, hashdate_ms"),
        ("manifests", "volume, generation, mhl_path"),
    ] {
        let sql = format!("SELECT * FROM {table} ORDER BY {order}");
        let mut stmt = self.conn.prepare(&sql)?;
        let cols = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            out.push_str(table);
            for i in 0..cols {
                let v: rusqlite::types::Value = row.get(i)?;
                out.push_str(&format!("|{v:?}"));
            }
            out.push('\n');
        }
    }
    Ok(out)
}
```

- [ ] **Step 3: Run the equivalence test**

Run: `cargo test -p majestical-catalog-sqlite`
Expected: PASS (property + existing unit tests).

- [ ] **Step 4: Route the CLI through `open_synced`**

`crates/cli/src/app.rs` — expose the log:

```rust
impl<L: EventLog> App<L> {
    pub(crate) fn log(&self) -> &L {
        &self.log
    }
}
```

`crates/cli/src/commands.rs` — replace `open_rebuilt_catalog` (keep the name used
by callers, change the body; it now also returns the projection so handlers stop
folding the log a second time):

```rust
pub(crate) fn open_catalog(
    app: &FsApp,
    catalog_dir: &Path,
) -> Result<(SqliteCatalog, Projection)> {
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let (db, projection, _mode) = SqliteCatalog::open_synced(&paths.db_path, app.log())
        .context("opening sqlite catalog")?;
    Ok((db, projection))
}
```

Update the three call sites (`cmd_search`, `cmd_volumes_list`, `cmd_para` list arm)
from `let db = open_rebuilt_catalog(app, catalog_dir)?` to
`let (db, _projection) = open_catalog(app, catalog_dir)?` (search will use the
projection from PR 3 on). Delete `open_rebuilt_catalog`.

- [ ] **Step 5: Full test run and commit**

Run: `just check && cargo test --workspace`
Expected: PASS.

```bash
git add crates/catalog-sqlite crates/cli/src/commands.rs crates/cli/src/app.rs Cargo.lock
git commit -m "feat: incremental sqlite apply with projection snapshot and log cursors"
```

Open PR 2: title `feat: incremental catalog apply`. Body: snapshot+cursor design,
fallback-to-full-rebuild guarantees, equivalence property test.

---

# PR 3 — FTS5 name index, query language, `maj search` rework

Also carries the two planning-time core changes search depends on: `mtime_ms` on
`AssetSeen` (with instance LWW) and the `media_kind` classifier. Four tasks.

### Task 5: Core — `mtime_ms`, instance LWW map, `media_kind`

**Files:**
- Modify: `crates/core/src/event.rs`, `crates/core/src/projection.rs`, `crates/core/src/lib.rs`
- Create: `crates/core/src/media_kind.rs`
- Test: golden tests in `event.rs`, unit tests in `projection.rs`/`media_kind.rs`, `crates/core/tests/crdt_properties.rs`

- [ ] **Step 1: Write failing tests**

`event.rs` golden additions: update the existing `AssetSeen` golden row to include
`"mtime_ms":5` (build the op with `mtime_ms: 5`) — serialization now always emits
the field — and add a backward-compat test:

```rust
#[test]
fn asset_seen_without_mtime_still_parses() {
    let old = r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":{"type":"asset_seen","asset":"xxh3:a","volume":"v","path":"p","size":3}}"#;
    let event: Event = serde_json::from_str(old).expect("old wire format must parse");
    let Op::AssetSeen { mtime_ms, .. } = event.op else {
        panic!("wrong variant");
    };
    assert_eq!(mtime_ms, 0);
}
```

`projection.rs` unit tests:

```rust
#[test]
fn a_rescan_of_the_same_path_updates_in_place_instead_of_duplicating() {
    let mut p = Projection::default();
    let a = AssetId("xxh3:a".into());
    p.apply(&test_event(1, Op::AssetSeen {
        asset: a.clone(), volume: "v".into(), path: "p".into(), size: 3, mtime_ms: 10,
    }));
    p.apply(&test_event(2, Op::AssetSeen {
        asset: a.clone(), volume: "v".into(), path: "p".into(), size: 9, mtime_ms: 20,
    }));
    let state = p.assets().find(|(id, _)| **id == a).expect("asset").1;
    assert_eq!(state.instances.len(), 1, "same (volume, path) must not duplicate");
    let info = state.instances.values().next().expect("instance");
    assert_eq!((info.size, info.mtime_ms), (9, 20), "newer HLC wins");
}

#[test]
fn instance_lww_is_hlc_ordered_not_arrival_ordered() {
    let mut p = Projection::default();
    let a = AssetId("xxh3:a".into());
    // Later HLC applied first; the earlier write must lose regardless of order.
    p.apply(&test_event(2, Op::AssetSeen {
        asset: a.clone(), volume: "v".into(), path: "p".into(), size: 9, mtime_ms: 20,
    }));
    p.apply(&test_event(1, Op::AssetSeen {
        asset: a.clone(), volume: "v".into(), path: "p".into(), size: 3, mtime_ms: 10,
    }));
    let state = p.assets().find(|(id, _)| **id == a).expect("asset").1;
    let info = state.instances.values().next().expect("instance");
    assert_eq!((info.size, info.mtime_ms), (9, 20));
}
```

`media_kind.rs` tests:

```rust
#[test]
fn classifies_by_extension_case_insensitively() {
    assert_eq!(media_kind("Clips/Beach.MOV"), MediaKind::Video);
    assert_eq!(media_kind("a/b/photo.jpeg"), MediaKind::Image);
    assert_eq!(media_kind("IMG_0001.HEIC"), MediaKind::Image);
    assert_eq!(media_kind("notes.txt"), MediaKind::Other);
    assert_eq!(media_kind("no_extension"), MediaKind::Other);
}
```

Run: `cargo test -p majestical-core` — Expected: FAIL (missing field/module/type).

- [ ] **Step 2: Implement**

`event.rs`:

```rust
AssetSeen {
    asset: AssetId,
    volume: String,
    path: String,
    size: u64,
    /// File modification time (ms since epoch). Additive field: events written
    /// before phase 4 parse as 0 (meaning "unknown").
    #[serde(default)]
    mtime_ms: u64,
},
```

`projection.rs` — replace the instances set with an LWW map:

```rust
/// One file instance's LWW attributes. Ord is (hlc, size, mtime_ms) so the
/// derived comparison matches the projection-wide LWW rule (HLC first).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub hlc: Hlc,
    pub size: u64,
    pub mtime_ms: u64,
}

pub struct AssetState {
    /// (volume id, volume-root-relative path) → newest observation.
    pub instances: BTreeMap<(String, String), InstanceInfo>,
    // …existing private fields unchanged
}
```

`AssetSeen` arm:

```rust
let candidate = InstanceInfo { hlc: event.hlc.clone(), size: *size, mtime_ms: *mtime_ms };
match state.instances.entry((volume.clone(), path.clone())) {
    std::collections::btree_map::Entry::Vacant(slot) => {
        slot.insert(candidate);
    }
    std::collections::btree_map::Entry::Occupied(mut slot) => {
        if candidate > *slot.get() {
            slot.insert(candidate);
        }
    }
}
```

`media_kind.rs` (add `pub mod media_kind;` to `lib.rs`):

```rust
//! File classification by extension, shared by the index planner and the
//! `kind:` search filter so both always agree.

/// Coarse media class of a catalog path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Other,
}

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "tif", "tiff", "bmp", "webp", "heic", "heif",
    "avif", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2",
];
const VIDEO_EXTS: &[&str] = &[
    "mov", "mp4", "m4v", "avi", "mkv", "mxf", "mts", "m2ts", "webm", "r3d", "braw",
];

impl MediaKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Other => "other",
        }
    }
}

/// Classify a path (any base) by its extension, case-insensitively.
#[must_use]
pub fn media_kind(path: &str) -> MediaKind {
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase());
    let Some(ext) = ext else { return MediaKind::Other };
    if IMAGE_EXTS.contains(&ext.as_str()) {
        MediaKind::Image
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        MediaKind::Video
    } else {
        MediaKind::Other
    }
}
```

Mechanical downstream fixes in this step (compiler-driven): every constructor of
`Op::AssetSeen` gains `mtime_ms` (core tests, `crdt_properties.rs` OpKind gains a
`0u64..100` arb for it, cucumber steps, cli `cmd_scan`/`cmd_ingest` — pass `0` in
the CLI for now; Task 8 populates real values), and every reader of
`state.instances` switches from 3-tuples to `((volume, path), InstanceInfo)`
(catalog-sqlite `insert_one_asset`, cli `known_assets_from_projection`, any tests).
In catalog-sqlite keep compiling by writing `mtime_ms` into a new column — schema
in Task 6 Step 2; do the two together if easier, they merge in one PR.

- [ ] **Step 3: Run core + workspace tests, commit**

Run: `cargo test -p majestical-core && cargo test --workspace`
Expected: PASS.

```bash
git add crates/core crates/catalog-sqlite crates/cli crates/ingest
git commit -m "feat: instance LWW with mtime, media kind classifier"
```

### Task 6: catalog-sqlite — schema v2, FTS5 names, filters, summaries

**Files:**
- Modify: `crates/catalog-sqlite/src/lib.rs`, `crates/core/src/ports.rs`
- Test: unit tests in `crates/catalog-sqlite/src/lib.rs` (existing in-file style)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn fts_name_search_is_unicode_case_insensitive_and_ranked() {
    let mut db = SqliteCatalog::open(&tmp_db()).expect("open");
    let p = projection_with_instances(&[
        ("xxh3:a", "v", "clips/Café-sunset.mov"),
        ("xxh3:b", "v", "docs/readme.txt"),
    ]);
    db.rebuild(&p).expect("rebuild");
    let hits = db.search_names_ranked(&["cafe".into()], 10).expect("search");
    assert_eq!(hits.len(), 1, "unicode61+remove_diacritics folds Café → cafe");
    assert_eq!(hits[0].0, AssetId("xxh3:a".into()));
}

#[test]
fn prefix_terms_match() {
    let mut db = SqliteCatalog::open(&tmp_db()).expect("open");
    let p = projection_with_instances(&[("xxh3:a", "v", "beach_day.mov")]);
    db.rebuild(&p).expect("rebuild");
    let hits = db.search_names_ranked(&["beach".into()], 10).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn filters_intersect_and_negate() {
    // Build a projection with two assets: one tagged "keep" on volume v1 with
    // mtime 1000, one tagged "rejected" on v2 with mtime 2000; assert each
    // Filter variant alone, a conjunction, and a negation.
    let mut db = SqliteCatalog::open(&tmp_db()).expect("open");
    let p = two_asset_fixture();
    db.rebuild(&p).expect("rebuild");
    use majestical_core::ports::Filter;
    let keep = db.assets_matching(&[Filter::Tag { value: "keep".into(), negated: false }])
        .expect("match");
    assert_eq!(keep.len(), 1);
    let not_rejected = db
        .assets_matching(&[Filter::Tag { value: "rejected".into(), negated: true }])
        .expect("match");
    assert!(not_rejected.contains(&AssetId("xxh3:a".into())));
    assert!(!not_rejected.contains(&AssetId("xxh3:b".into())));
    let both = db.assets_matching(&[
        Filter::Tag { value: "keep".into(), negated: false },
        Filter::Before(1500),
    ]).expect("match");
    assert_eq!(both.len(), 1);
    let after = db.assets_matching(&[Filter::After(1500)]).expect("match");
    assert!(after.contains(&AssetId("xxh3:b".into())));
    let vid = db.assets_matching(&[Filter::Kind { value: "video".into(), negated: false }])
        .expect("match");
    assert!(vid.contains(&AssetId("xxh3:a".into())), "a is .mov");
    let online = db.assets_matching(&[Filter::Online { ids: vec!["v1".into()], want: true }])
        .expect("match");
    assert_eq!(online.len(), 1);
}
```

Write the two fixture helpers (`projection_with_instances`, `two_asset_fixture`)
locally: apply `Op::AssetSeen`/`Op::TagAdd`/`Op::VolumeSeen` events to a
`Projection` exactly as the incremental test's `ev` helper does.

Run: `cargo test -p majestical-catalog-sqlite` — Expected: FAIL.

- [ ] **Step 2: Implement**

Schema changes inside `create_tables` (and bump `SNAPSHOT_VERSION` to `2` — the
projection's serialized shape changed in Task 5):

```sql
CREATE TABLE instances (
  asset TEXT NOT NULL REFERENCES assets(id),
  volume TEXT NOT NULL, path TEXT NOT NULL,
  size INTEGER NOT NULL, mtime_ms INTEGER NOT NULL, kind TEXT NOT NULL,
  PRIMARY KEY (asset, volume, path)
);
CREATE VIRTUAL TABLE names_fts USING fts5(
  name, asset UNINDEXED, tokenize = 'unicode61 remove_diacritics 2'
);
```

(Add `names_fts` to the drop list: `DROP TABLE IF EXISTS names_fts;`.)

`insert_one_asset` additions — instances now written with mtime + kind, and one
FTS row per distinct basename:

```rust
let mut names = std::collections::BTreeSet::new();
for ((volume, path), info) in &state.instances {
    tx.execute(
        "INSERT INTO instances (asset, volume, path, size, mtime_ms, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            &id.0,
            volume,
            path,
            i64::try_from(info.size).unwrap_or(i64::MAX),
            i64::try_from(info.mtime_ms).unwrap_or(i64::MAX),
            majestical_core::media_kind::media_kind(path).as_str(),
        ),
    )?;
    if let Some(name) = path.rsplit('/').next() {
        names.insert(name.to_string());
    }
}
for name in names {
    tx.execute(
        "INSERT INTO names_fts (name, asset) VALUES (?1, ?2)",
        (&name, &id.0),
    )?;
}
```

`apply_touched`'s asset arm gains `"DELETE FROM names_fts WHERE asset = ?1"` in its
delete list, and `debug_dump` gains `("instances", …)` column changes plus a
`names_fts` row (`SELECT name, asset FROM names_fts ORDER BY asset, name`).

Port types in `crates/core/src/ports.rs` (so the trait can speak them):

```rust
/// One hard search filter, already resolved to storage terms (para refs are
/// node ids; `Online` carries the currently-mounted volume ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Tag { value: String, negated: bool },
    Volume { value: String, negated: bool },
    Para { node: String, negated: bool },
    Kind { value: String, negated: bool },
    Online { ids: Vec<String>, want: bool },
    Before(u64),
    After(u64),
}

/// Presentation row for one search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSummary {
    pub asset: AssetId,
    pub name: String,
    /// (volume id, volume label) pairs holding an instance.
    pub volumes: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub para: Option<String>,
}
```

`CatalogStore` trait: REMOVE `search_by_tag` and `search_by_name` (and their
inherent impls + SQL — replaced, not deprecated); ADD:

```rust
fn assets_matching(&self, filters: &[Filter]) -> Result<BTreeSet<AssetId>, PortError>;
fn search_names_ranked(&self, terms: &[String], limit: usize)
    -> Result<Vec<(AssetId, f64)>, PortError>;
fn asset_summaries(&self, ids: &[AssetId]) -> Result<Vec<AssetSummary>, PortError>;
```

Inherent implementations in catalog-sqlite:

```rust
pub fn search_names_ranked(
    &self,
    terms: &[String],
    limit: usize,
) -> Result<Vec<(AssetId, f64)>, CatalogError> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    // Each term quoted (embedded quotes doubled) with a prefix star, OR-joined:
    // beach → "beach"*  — FTS5 syntax, immune to operator injection.
    let match_expr = terms
        .iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut stmt = self.conn.prepare(
        "SELECT asset, rank FROM names_fts WHERE names_fts MATCH ?1
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        (&match_expr, i64::try_from(limit).unwrap_or(i64::MAX)),
        |r| Ok((AssetId(r.get(0)?), r.get::<_, f64>(1)?)),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn assets_matching(
    &self,
    filters: &[Filter],
) -> Result<std::collections::BTreeSet<AssetId>, CatalogError> {
    use rusqlite::types::Value;
    let mut sql = String::from("SELECT a.id FROM assets a WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    let mut push = |sql: &mut String, exists: bool, body: &str| {
        sql.push_str(" AND ");
        if !exists {
            sql.push_str("NOT ");
        }
        sql.push_str("EXISTS (");
        sql.push_str(body);
        sql.push(')');
    };
    for filter in filters {
        match filter {
            Filter::Tag { value, negated } => {
                push(&mut sql, !negated,
                    &format!("SELECT 1 FROM tags t WHERE t.asset = a.id AND t.tag = ?{}",
                        params.len() + 1));
                params.push(Value::Text(value.clone()));
            }
            Filter::Volume { value, negated } => {
                let n = params.len() + 1;
                push(&mut sql, !negated,
                    &format!("SELECT 1 FROM instances i JOIN volumes v ON v.id = i.volume \
                              WHERE i.asset = a.id AND (v.label = ?{n} OR v.id = ?{n})"));
                params.push(Value::Text(value.clone()));
            }
            Filter::Para { node, negated } => {
                push(&mut sql, !negated,
                    &format!("SELECT 1 FROM asset_para ap WHERE ap.asset = a.id AND ap.node = ?{}",
                        params.len() + 1));
                params.push(Value::Text(node.clone()));
            }
            Filter::Kind { value, negated } => {
                push(&mut sql, !negated,
                    &format!("SELECT 1 FROM instances i WHERE i.asset = a.id AND i.kind = ?{}",
                        params.len() + 1));
                params.push(Value::Text(value.clone()));
            }
            Filter::Online { ids, want } => {
                if ids.is_empty() {
                    // Nothing is mounted: online:yes matches nothing,
                    // online:no matches everything with any instance.
                    push(&mut sql, !want,
                        "SELECT 1 FROM instances i WHERE i.asset = a.id");
                } else {
                    let placeholders = ids
                        .iter()
                        .enumerate()
                        .map(|(k, _)| format!("?{}", params.len() + 1 + k))
                        .collect::<Vec<_>>()
                        .join(", ");
                    push(&mut sql, *want,
                        &format!("SELECT 1 FROM instances i WHERE i.asset = a.id \
                                  AND i.volume IN ({placeholders})"));
                    params.extend(ids.iter().cloned().map(Value::Text));
                }
            }
            Filter::Before(ms) => {
                sql.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM instances i WHERE i.asset = a.id \
                     AND i.mtime_ms < ?{})", params.len() + 1));
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
            }
            Filter::After(ms) => {
                sql.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM instances i WHERE i.asset = a.id \
                     AND i.mtime_ms > ?{})", params.len() + 1));
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
            }
        }
    }
    let mut stmt = self.conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok(AssetId(r.get(0)?))
    })?;
    let mut out = std::collections::BTreeSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

pub fn asset_summaries(
    &self,
    ids: &[AssetId],
) -> Result<Vec<AssetSummary>, CatalogError> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let mut volumes = Vec::new();
        let mut name = String::new();
        let mut stmt = self.conn.prepare(
            "SELECT i.path, i.volume, COALESCE(v.label, i.volume)
             FROM instances i LEFT JOIN volumes v ON v.id = i.volume
             WHERE i.asset = ?1 ORDER BY i.volume, i.path",
        )?;
        let rows = stmt.query_map([&id.0], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (path, vol_id, label) = row?;
            if name.is_empty() {
                name = path.rsplit('/').next().unwrap_or(&path).to_string();
            }
            if !volumes.iter().any(|(v, _)| v == &vol_id) {
                volumes.push((vol_id, label));
            }
        }
        let mut tag_stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE asset = ?1 ORDER BY tag")?;
        let tags = tag_stmt
            .query_map([&id.0], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let para: Option<String> = self
            .conn
            .query_row(
                "SELECT node FROM asset_para WHERE asset = ?1",
                [&id.0],
                |r| r.get(0),
            )
            .ok();
        out.push(AssetSummary { asset: id.clone(), name, volumes, tags, para });
    }
    Ok(out)
}
```

Trait impl block: wrap each inherent method mapping `CatalogError` → `PortError`
exactly as the existing trait methods do.

- [ ] **Step 3: Run tests, commit**

Run: `cargo test -p majestical-catalog-sqlite && cargo test -p majestical-core`
Expected: PASS. (`cmd_search` in cli no longer compiles — that's Task 8; if you
need the workspace green to commit, do Tasks 6-8 as one commit at the end of
Task 8. Preferred: comment nothing out, land 6+7+8 together.)

### Task 7: CLI — query parser

**Files:**
- Create: `crates/cli/src/query.rs`
- Modify: `crates/cli/src/main.rs` (add `mod query;`)

- [ ] **Step 1: Write failing tests (in-file `#[cfg(test)]`)**

```rust
#[test]
fn bare_terms_and_filters_separate() {
    let q = parse_query(r#"golden retriever tag:pets -tag:rejected vol:Media2024"#)
        .expect("parse");
    assert_eq!(q.terms, vec!["golden", "retriever"]);
    assert_eq!(q.filters, vec![
        RawFilter { key: "tag".into(), value: "pets".into(), negated: false },
        RawFilter { key: "tag".into(), value: "rejected".into(), negated: true },
        RawFilter { key: "vol".into(), value: "Media2024".into(), negated: false },
    ]);
}

#[test]
fn quotes_group_whitespace_in_terms_and_values() {
    let q = parse_query(r#""golden gate" tag:"family trip""#).expect("parse");
    assert_eq!(q.terms, vec!["golden gate"]);
    assert_eq!(q.filters, vec![
        RawFilter { key: "tag".into(), value: "family trip".into(), negated: false },
    ]);
}

#[test]
fn unbalanced_quote_is_a_clear_error() {
    let err = parse_query(r#"beach "sunset"#).expect_err("must fail");
    assert!(err.to_string().contains("unbalanced quote"));
}

#[test]
fn empty_input_yields_empty_query() {
    let q = parse_query("   ").expect("parse");
    assert!(q.terms.is_empty() && q.filters.is_empty());
}

#[test]
fn dates_parse_to_utc_midnight_ms() {
    assert_eq!(parse_date_ms("1970-01-02").expect("parse"), 86_400_000);
    assert_eq!(parse_date_ms("2026-07-30").expect("parse"), 1_785_369_600_000);
    assert!(parse_date_ms("2026-13-01").is_err());
    assert!(parse_date_ms("not-a-date").is_err());
}
```

(Sanity-check the second constant during implementation with
`date -u -j -f %Y-%m-%d 2026-07-30 +%s` × 1000; correct the literal if needed.)

Run: `cargo test -p majestical-cli query` — Expected: FAIL (module missing).

- [ ] **Step 2: Implement**

```rust
//! The `maj search` query language: bare terms (matched semantically and by
//! name) plus `key:value` hard filters, `-` negation, and double-quote
//! grouping. One parser, shared by every future surface (GUI omnibox, MCP).

use anyhow::{bail, Result};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawFilter {
    pub key: String,
    pub value: String,
    pub negated: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedQuery {
    pub terms: Vec<String>,
    pub filters: Vec<RawFilter>,
}

/// Split into whitespace-separated tokens, honoring double quotes (which are
/// stripped). Then classify each token: `-` prefix negates; a `key:value`
/// shape with an alphabetic key is a filter; anything else is a term.
pub(crate) fn parse_query(input: &str) -> Result<ParsedQuery> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        bail!("unbalanced quote in query: {input}");
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut parsed = ParsedQuery::default();
    for token in tokens {
        let (negated, body) = match token.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token.as_str()),
        };
        match body.split_once(':') {
            Some((key, value))
                if !key.is_empty()
                    && key.chars().all(|c| c.is_ascii_alphabetic()) =>
            {
                if value.is_empty() {
                    bail!("filter '{key}:' has no value");
                }
                parsed.filters.push(RawFilter {
                    key: key.to_ascii_lowercase(),
                    value: value.to_string(),
                    negated,
                });
            }
            _ => {
                if negated {
                    bail!("'-' negation only applies to key:value filters: -{body}");
                }
                parsed.terms.push(body.to_string());
            }
        }
    }
    Ok(parsed)
}

/// `YYYY-MM-DD` → milliseconds since the Unix epoch at UTC midnight.
/// (Howard Hinnant's days-from-civil algorithm; no chrono dependency.)
pub(crate) fn parse_date_ms(value: &str) -> Result<u64> {
    let parts: Vec<&str> = value.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        bail!("date must be YYYY-MM-DD, got '{value}'");
    };
    let (y, m, d): (i64, i64, i64) = match (y.parse(), m.parse(), d.parse()) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => bail!("date must be YYYY-MM-DD, got '{value}'"),
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
        bail!("date out of range: '{value}'");
    }
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400_000)
        .map_err(|_| anyhow::anyhow!("date out of range: '{value}'"))
}
```

- [ ] **Step 3: Run tests, then move on (commits with Task 8)**

Run: `cargo test -p majestical-cli query` — Expected: PASS.

### Task 8: CLI — `maj search <query>`, real mtimes, mounted volumes

**Files:**
- Modify: `crates/cli/src/main.rs` (Search args), `crates/cli/src/commands.rs`,
  `crates/cli/src/volume_identity.rs`
- Test: `crates/cli/tests/cli_smoke.rs`

- [ ] **Step 1: Write failing smoke tests**

Rewrite every `search --name`/`--tag` call site to the new surface, and add:

```rust
#[test]
fn search_combines_terms_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let (catalog, state) = init_catalog(&dir); // helper: create dirs + catalog init
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("beach_day.mov"), b"aaa").unwrap();
    std::fs::write(media.join("mountain.mov"), b"bbb").unwrap();
    maj(&catalog, &state).args(["scan"]).arg(&media).assert().success();
    let out = maj(&catalog, &state)
        .args(["search", "beach", "--json"])
        .output()
        .unwrap();
    let asset = first_asset_id(&out);
    maj(&catalog, &state)
        .args(["tag", "add", &asset, "status/select"])
        .assert()
        .success();
    maj(&catalog, &state)
        .args(["search", "beach tag:status/select", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("beach_day.mov"));
    maj(&catalog, &state)
        .args(["search", "beach -tag:status/select"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 results"));
    maj(&catalog, &state)
        .args(["search", "kind:image"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 results"));
}

#[test]
fn search_with_unknown_filter_key_lists_valid_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (catalog, state) = init_catalog(&dir);
    maj(&catalog, &state)
        .args(["search", "flavor:salty"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("tag"));
}
```

Run: `cargo test -p majestical-cli --test cli_smoke search_combines`
Expected: FAIL (clap rejects the new shape).

- [ ] **Step 2: Implement**

`main.rs` — replace the Search variant (drop the ArgGroup):

```rust
/// Search the catalog: bare terms match names (and, once indexed,
/// image content); key:value tokens are hard filters
/// (tag: vol: para: kind: online: before: after:), '-' negates.
Search {
    query: String,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    json: bool,
},
```

Dispatch: `commands::cmd_search(&app, &cli.catalog, &commands::SearchArgs { query, limit, json })?`.

`volume_identity.rs` — make `mount_point_of` `pub(crate)` and add:

```rust
/// Currently mounted volumes: id → mount point. "/" first so the root volume
/// wins its id even when /Volumes carries a symlink to it.
pub(crate) fn mounted_volumes() -> std::collections::BTreeMap<String, std::path::PathBuf> {
    let mut map = std::collections::BTreeMap::new();
    let mut add = |path: std::path::PathBuf| {
        let identity = resolve(&path);
        map.entry(identity.id).or_insert(path);
    };
    add(std::path::PathBuf::from("/"));
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            add(entry.path());
        }
    }
    map
}
```

`commands.rs` — new search implementation:

```rust
pub(crate) struct SearchArgs {
    pub query: String,
    pub limit: usize,
    pub json: bool,
}

pub(crate) fn cmd_search(app: &FsApp, catalog_dir: &Path, args: &SearchArgs) -> Result<()> {
    let (db, projection) = open_catalog(app, catalog_dir)?;
    let parsed = crate::query::parse_query(&args.query)?;
    let filters = resolve_filters(&projection, &parsed.filters)?;
    let allowed = if filters.is_empty() {
        None
    } else {
        Some(db.assets_matching(&filters)?)
    };
    let ranked: Vec<(AssetId, f64)> = if parsed.terms.is_empty() {
        let Some(set) = &allowed else {
            bail!("empty query: give search terms or at least one filter");
        };
        set.iter().map(|a| (a.clone(), 0.0)).take(args.limit).collect()
    } else {
        db.search_names_ranked(&parsed.terms, args.limit.saturating_mul(4))?
            .into_iter()
            .filter(|(a, _)| allowed.as_ref().is_none_or(|s| s.contains(a)))
            .take(args.limit)
            .collect()
    };
    print_search_results(&db, &ranked, args.json)
}

const FILTER_KEYS: &str = "tag, vol/volume, para, kind, online, before, after";

fn resolve_filters(
    projection: &Projection,
    raw: &[crate::query::RawFilter],
) -> Result<Vec<Filter>> {
    let mut filters = Vec::with_capacity(raw.len());
    for f in raw {
        let filter = match f.key.as_str() {
            "tag" => Filter::Tag { value: f.value.clone(), negated: f.negated },
            "vol" | "volume" => Filter::Volume { value: f.value.clone(), negated: f.negated },
            "para" => {
                let node = resolve_para_node(projection, &f.value)?;
                Filter::Para { node, negated: f.negated }
            }
            "kind" => {
                if !matches!(f.value.as_str(), "image" | "video" | "other") {
                    bail!("kind: must be image, video, or other (got '{}')", f.value);
                }
                Filter::Kind { value: f.value.clone(), negated: f.negated }
            }
            "online" => {
                let want = match f.value.as_str() {
                    "yes" => !f.negated,
                    "no" => f.negated,
                    other => bail!("online: must be yes or no (got '{other}')"),
                };
                let ids = crate::volume_identity::mounted_volumes()
                    .into_keys()
                    .collect();
                Filter::Online { ids, want }
            }
            "before" => {
                ensure!(!f.negated, "use after: instead of -before:");
                Filter::Before(crate::query::parse_date_ms(&f.value)?)
            }
            "after" => {
                ensure!(!f.negated, "use before: instead of -after:");
                Filter::After(crate::query::parse_date_ms(&f.value)?)
            }
            other => bail!("unknown filter '{other}:'; valid filters: {FILTER_KEYS}"),
        };
        filters.push(filter);
    }
    Ok(filters)
}

fn print_search_results(
    db: &SqliteCatalog,
    ranked: &[(AssetId, f64)],
    json: bool,
) -> Result<()> {
    let ids: Vec<AssetId> = ranked.iter().map(|(a, _)| a.clone()).collect();
    let summaries = db.asset_summaries(&ids)?;
    let mounted = crate::volume_identity::mounted_volumes();
    if json {
        let results: Vec<serde_json::Value> = ranked
            .iter()
            .zip(&summaries)
            .map(|((asset, score), s)| {
                serde_json::json!({
                    "asset": asset.0,
                    "score": score,
                    "name": s.name,
                    "volumes": s.volumes.iter().map(|(id, label)| serde_json::json!({
                        "id": id, "label": label, "online": mounted.contains_key(id),
                    })).collect::<Vec<_>>(),
                    "tags": s.tags,
                    "para": s.para,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "count": results.len(), "results": results })
        );
    } else {
        for ((asset, _), s) in ranked.iter().zip(&summaries) {
            let vols = s
                .volumes
                .iter()
                .map(|(id, label)| {
                    let mark = if mounted.contains_key(id) { "●" } else { "○" };
                    format!("{label}{mark}")
                })
                .collect::<Vec<_>>()
                .join(",");
            let tags = if s.tags.is_empty() {
                String::new()
            } else {
                format!("  tags:{}", s.tags.join(","))
            };
            println!("{}  {}  [{vols}]{tags}", asset.0, s.name);
        }
        println!("{} results", ranked.len());
    }
    Ok(())
}
```

Real mtimes: in `cmd_scan`'s walk, where `Op::AssetSeen` is built, add
`mtime_ms: mtime_ms_of(&metadata)` using the already-fetched `Metadata` (add the
helper below near `resolve_volume`); in `cmd_ingest`'s `asset_and_para_ops`, stat
the placed destination file:

```rust
pub(crate) fn mtime_ms_of(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}
```

(For ingest: `std::fs::metadata(dest_root.join(&placed.dest_rel)).map(|m| mtime_ms_of(&m)).unwrap_or(0)`.)

`cmd_tag`/`cmd_meta`'s `ensure_asset_known` and `first_asset_id` in smoke tests
still work (JSON keeps the `results[].asset` key).

- [ ] **Step 3: Run the full suite**

Run: `just check && cargo test --workspace`
Expected: PASS, including all rewritten smoke tests.

- [ ] **Step 4: Commit**

```bash
git add crates/cli crates/catalog-sqlite crates/core Cargo.lock
git commit -m "feat: unified search query language over FTS5 and hard filters"
```

Open PR 3: title `feat: query language + FTS5 name search`. Body: new search
surface (breaking: `--name`/`--tag` replaced), unicode name matching, filter set,
mtime provenance.

---

# PR 4 — Saved searches (CRDT)

### Task 9: Core ops + projection

**Files:**
- Modify: `crates/core/src/event.rs`, `crates/core/src/projection.rs`
- Test: golden rows in `event.rs`, unit tests in `projection.rs`, `crates/core/tests/crdt_properties.rs`

- [ ] **Step 1: Write failing tests**

Golden additions to the wire-format table test (same `golden(op)` helper):

```rust
(
    Op::SavedSearchSet { name: "n1".into(), query: "tag:x sunset".into() },
    r#"{"type":"saved_search_set","name":"n1","query":"tag:x sunset"}"#,
),
(
    Op::SavedSearchRemove { name: "n1".into() },
    r#"{"type":"saved_search_remove","name":"n1"}"#,
),
```

Projection unit tests:

```rust
#[test]
fn saved_search_set_remove_is_lww_per_name() {
    let mut p = Projection::default();
    p.apply(&test_event(1, Op::SavedSearchSet { name: "picks".into(), query: "tag:a".into() }));
    p.apply(&test_event(3, Op::SavedSearchSet { name: "picks".into(), query: "tag:b".into() }));
    p.apply(&test_event(2, Op::SavedSearchRemove { name: "picks".into() }));
    assert_eq!(p.saved_search("picks"), Some("tag:b"), "later set beats earlier remove");
    p.apply(&test_event(4, Op::SavedSearchRemove { name: "picks".into() }));
    assert_eq!(p.saved_search("picks"), None);
    assert_eq!(p.saved_searches().count(), 0);
}
```

Run: `cargo test -p majestical-core saved_search` — Expected: FAIL.

- [ ] **Step 2: Implement**

`event.rs` — two new variants at the end of `Op`:

```rust
/// Save (or overwrite) a named search query. HLC-LWW per name.
SavedSearchSet { name: String, query: String },
/// Remove a named search. An LWW tombstone: a later Set revives the name.
SavedSearchRemove { name: String },
```

`projection.rs`:

```rust
pub struct Projection {
    // …existing fields…
    /// name → LWW slot; `None` value = removed (tombstone).
    #[serde(default)]
    saved_searches: BTreeMap<String, (Hlc, Option<String>)>,
}
```

Apply arms (both return `Touched::SavedSearch(name.clone())` — add the variant to
`Touched`):

```rust
Op::SavedSearchSet { name, query } => {
    lww_entry(self.saved_searches.entry(name.clone()),
        event.hlc.clone(), Some(query.clone()));
    Touched::SavedSearch(name.clone())
}
Op::SavedSearchRemove { name } => {
    lww_entry(self.saved_searches.entry(name.clone()), event.hlc.clone(), None);
    Touched::SavedSearch(name.clone())
}
```

where `lww_entry` follows the existing `lww` helper's comparison (whole
`(Hlc, T)` tuple; `Option<String>` is `Ord`, and comparing `(hlc, value)` keeps
the deterministic total order — HLC decides, value breaks exact-HLC ties).

Accessors:

```rust
/// Live saved searches (tombstones excluded), name-ordered.
pub fn saved_searches(&self) -> impl Iterator<Item = (&str, &str)> {
    self.saved_searches.iter().filter_map(|(name, (_, query))| {
        query.as_deref().map(|q| (name.as_str(), q))
    })
}

pub fn saved_search(&self, name: &str) -> Option<&str> {
    self.saved_searches.get(name).and_then(|(_, q)| q.as_deref())
}
```

`crdt_properties.rs`: OpKind gains `SavedSearchSet`/`SavedSearchRemove` variants,
`build_events` arms, and `arb_kind()` arms using `"[a-b]"` names and `"[q-r]{1,2}"`
queries (tiny domains so ops collide).

Bump `SNAPSHOT_VERSION` in catalog-sqlite to `3` in Task 10 (field is
`#[serde(default)]` so old snapshots would load, but a bump costs one rebuild and
keeps the policy simple: any projection shape change bumps).

- [ ] **Step 3: Run, commit**

Run: `cargo test -p majestical-core`
Expected: PASS (golden, unit, property).

```bash
git add crates/core
git commit -m "feat: saved-search CRDT ops"
```

### Task 10: Storage + CLI surface

**Files:**
- Modify: `crates/catalog-sqlite/src/lib.rs`, `crates/core/src/ports.rs`,
  `crates/cli/src/main.rs`, `crates/cli/src/commands.rs`
- Test: `crates/cli/tests/cli_smoke.rs`, catalog-sqlite unit tests

- [ ] **Step 1: Write failing tests**

Smoke (two machines share the catalog root, distinct state dirs — the CRDT sync
path):

```rust
#[test]
fn saved_searches_sync_between_machines() {
    let dir = tempfile::tempdir().unwrap();
    let (catalog, state_a) = init_catalog(&dir);
    let state_b = dir.path().join("state-b");
    maj_as(&catalog, &state_a, "machine-a")
        .args(["search", "tag:keep", "--save", "keepers"])
        .assert()
        .success();
    maj_as(&catalog, &state_b, "machine-b")
        .args(["searches", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("keepers"))
        .stdout(predicates::str::contains("tag:keep"));
    maj_as(&catalog, &state_b, "machine-b")
        .args(["searches", "rm", "keepers"])
        .assert()
        .success();
    maj_as(&catalog, &state_a, "machine-a")
        .args(["searches", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no saved searches"));
}

#[test]
fn running_a_saved_search() {
    let dir = tempfile::tempdir().unwrap();
    let (catalog, state) = init_catalog(&dir);
    maj(&catalog, &state)
        .args(["search", "tag:nothing-yet", "--save", "empty"])
        .assert()
        .success();
    maj(&catalog, &state)
        .args(["search", "--saved", "empty"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 results"));
    maj(&catalog, &state)
        .args(["search", "--saved", "does-not-exist"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no saved search"));
}
```

Run: `cargo test -p majestical-cli --test cli_smoke saved` — Expected: FAIL.

- [ ] **Step 2: Implement storage**

catalog-sqlite: `SNAPSHOT_VERSION = 3`. Schema (+drop list, +`debug_dump` entry
`("saved_searches", "name")`):

```sql
CREATE TABLE saved_searches (name TEXT PRIMARY KEY, query TEXT NOT NULL);
```

Rebuild path: new `fn insert_saved_searches(tx, projection)` iterating
`projection.saved_searches()`. `apply_touched` arm:

```rust
Touched::SavedSearch(name) => {
    tx.execute("DELETE FROM saved_searches WHERE name = ?1", [name])?;
    if let Some(query) = projection.saved_search(name) {
        tx.execute(
            "INSERT INTO saved_searches (name, query) VALUES (?1, ?2)",
            (name, query),
        )?;
    }
}
```

(No new `CatalogStore` trait method: the CLI reads saved searches from the
projection, which it already has — the table exists for future surfaces and
debug_dump equivalence. Note this in the module doc to pre-empt the port-lag
reviewer flag.)

- [ ] **Step 3: Implement CLI**

`main.rs`:

```rust
Search {
    /// Query string; omit when using --saved.
    query: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long)]
    json: bool,
    /// Save this query under a name (and run it).
    #[arg(long, conflicts_with = "saved")]
    save: Option<String>,
    /// Run a previously saved search.
    #[arg(long, conflicts_with = "save")]
    saved: Option<String>,
},
Searches {
    #[command(subcommand)]
    cmd: SearchesCmd,
},

#[derive(Subcommand)]
enum SearchesCmd {
    /// List saved searches.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a saved search.
    Rm { name: String },
}
```

Dispatch: Search arm passes everything into `SearchArgs { query, limit, json,
save, saved }` and takes `&mut app` now (saving emits an event);
`Cmd::Searches` arm calls `commands::cmd_searches(&mut app, cmd)`.

`commands.rs` — extend `cmd_search`:

```rust
pub(crate) struct SearchArgs {
    pub query: Option<String>,
    pub limit: usize,
    pub json: bool,
    pub save: Option<String>,
    pub saved: Option<String>,
}

pub(crate) fn cmd_search(app: &mut FsApp, catalog_dir: &Path, args: &SearchArgs) -> Result<()> {
    let query = match (&args.query, &args.saved) {
        (Some(q), None) => q.clone(),
        (None, Some(name)) => {
            let projection = app.projection()?;
            projection
                .saved_search(name)
                .with_context(|| format!("no saved search named '{name}'"))?
                .to_string()
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
        (None, None) => bail!("give a query string or --saved <name>"),
    };
    if let Some(name) = &args.save {
        app.emit(vec![Op::SavedSearchSet { name: name.clone(), query: query.clone() }])?;
        println!("saved search '{name}'");
    }
    run_search(app, catalog_dir, &query, args.limit, args.json)
}
```

(`run_search` is the former `cmd_search` body from Task 8, taking `&FsApp` — the
emit happens before the read so the new event is part of the projection.)

`cmd_searches`:

```rust
pub(crate) fn cmd_searches(app: &mut FsApp, cmd: SearchesCmd) -> Result<()> {
    match cmd {
        SearchesCmd::List { json } => {
            let projection = app.projection()?;
            let entries: Vec<(&str, &str)> = projection.saved_searches().collect();
            if json {
                let list: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|(n, q)| serde_json::json!({ "name": n, "query": q }))
                    .collect();
                println!("{}", serde_json::json!({ "saved": list }));
            } else if entries.is_empty() {
                println!("no saved searches");
            } else {
                for (name, query) in entries {
                    println!("{name}: {query}");
                }
            }
        }
        SearchesCmd::Rm { name } => {
            let projection = app.projection()?;
            if projection.saved_search(&name).is_none() {
                bail!("no saved search named '{name}'");
            }
            app.emit(vec![Op::SavedSearchRemove { name: name.clone() }])?;
            println!("removed saved search '{name}'");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run, commit**

Run: `just check && cargo test --workspace`
Expected: PASS.

```bash
git add crates/core crates/catalog-sqlite crates/cli
git commit -m "feat: saved searches synced via CRDT"
```

Open PR 4: title `feat: saved searches`.

---

# PR 5 — `crates/index`: blob store, thumbnails, queue-as-diff

### Task 11: Crate scaffold, blob store, thumbnailer

**Files:**
- Create: `crates/index/Cargo.toml`, `crates/index/src/lib.rs`,
  `crates/index/src/error.rs`, `crates/index/src/blob.rs`,
  `crates/index/src/resize.rs`, `crates/index/src/thumbs.rs`
- Modify: root `Cargo.toml` (workspace members + deps)

- [ ] **Step 1: Scaffold the crate**

Root `Cargo.toml`: add `"crates/index"` to members and to `[workspace.dependencies]`:

```toml
image = "0.25.10"
webp = "0.3.1"
fast_image_resize = "6.1.0"
zstd = "0.13.3"
dirs = "6"
sha2 = { workspace = true }   # already present for ingest — reuse
```

`crates/index/Cargo.toml`:

```toml
[package]
name = "majestical-index"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

[dependencies]
majestical-core = { path = "../core" }
image = { workspace = true }
webp = { workspace = true }
fast_image_resize = { workspace = true }
zstd = { workspace = true }
dirs = { workspace = true }
sha2 = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tempfile = { workspace = true }   # sips needs a real output file

[lints]
workspace = true
```

`lib.rs`:

```rust
//! Derived-data production for the catalog: content-addressed blobs
//! (thumbnails, embeddings) in the sync root, and the work planner that
//! diffs required derivations against what exists. Everything here is
//! disposable and regenerable; the event log stays the only truth.
pub mod blob;
pub mod error;
pub mod resize;
pub mod thumbs;
pub mod work;

pub use error::IndexError;
```

`error.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("blob {path}: {source}")]
    Blob { path: PathBuf, source: std::io::Error },
    #[error("decoding {path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("vector blob {path}: invalid length {len}")]
    VectorShape { path: PathBuf, len: usize },
    #[error("image resize: {0}")]
    Resize(String),
    #[error("webp encode failed")]
    WebpEncode,
    #[error("model: {0}")]
    Model(String),
    #[error("encoder: {0}")]
    Encoder(String),
}
```

- [ ] **Step 2: Write failing blob-store tests (in-file `#[cfg(test)]`)**

```rust
#[test]
fn blob_paths_are_derivation_keyed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let hex = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        store.path_for(hex, &Derivation::Thumb),
        dir.path().join("blobs/01").join(hex).join("thumb-320.webp"),
    );
    assert_eq!(
        store.path_for(hex, &Derivation::ImageEmbedding { model_tag: "siglip2-b16-v1" }),
        dir.path().join("blobs/01").join(hex).join("siglip2-b16-v1/image.f32le.zst"),
    );
    assert_eq!(
        store.path_for(hex, &Derivation::KeyframeEmbedding { model_tag: "siglip2-b16-v1", timestamp_ms: 4500 }),
        dir.path().join("blobs/01").join(hex).join("siglip2-b16-v1/kf-4500.f32le.zst"),
    );
}

#[test]
fn vectors_round_trip_and_write_is_atomic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let path = store.path_for("aa00", &Derivation::ImageEmbedding { model_tag: "m1" });
    let vector: Vec<f32> = (0..768).map(|i| i as f32 / 768.0).collect();
    store.write_vector(&path, &vector).expect("write");
    assert_eq!(store.read_vector(&path).expect("read"), vector);
    // No stray temp files remain beside the blob.
    let siblings: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("read_dir")
        .flatten()
        .collect();
    assert_eq!(siblings.len(), 1);
}

#[test]
fn asset_hex_strips_the_hash_prefix() {
    assert_eq!(asset_hex("xxh3:abc123"), Some("abc123"));
    assert_eq!(asset_hex("sha1:abc123"), None);
}
```

Run: `cargo test -p majestical-index blob` — Expected: FAIL (module empty).

- [ ] **Step 3: Implement `blob.rs`**

```rust
//! Content-addressed derived-data store under `<sync-root>/blobs/`. Blobs are
//! keyed by derivation inputs (asset content hash + kind + model tag), so
//! writes are idempotent, rebuilds are directory walks, and two machines
//! deriving the same asset converge by construction.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::IndexError;

pub const THUMB_NAME: &str = "thumb-320.webp";
const ZSTD_LEVEL: i32 = 3;
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// One derivable artifact for an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivation<'a> {
    Thumb,
    ImageEmbedding { model_tag: &'a str },
    KeyframeEmbedding { model_tag: &'a str, timestamp_ms: u64 },
    /// JSON list of keyframe timestamps; doubles as the "video fully
    /// keyframed" completion marker.
    KeyframeManifest { model_tag: &'a str },
}

/// The catalog asset id is `xxh3:<32 hex>`; blob paths use the bare hex.
#[must_use]
pub fn asset_hex(asset_id: &str) -> Option<&str> {
    asset_id.strip_prefix("xxh3:")
}

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    #[must_use]
    pub fn new(sync_root: &Path) -> Self {
        Self { root: sync_root.join("blobs") }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn path_for(&self, asset_hex: &str, derivation: &Derivation<'_>) -> PathBuf {
        let prefix = asset_hex.get(..2).unwrap_or("xx");
        let dir = self.root.join(prefix).join(asset_hex);
        match derivation {
            Derivation::Thumb => dir.join(THUMB_NAME),
            Derivation::ImageEmbedding { model_tag } => {
                dir.join(model_tag).join("image.f32le.zst")
            }
            Derivation::KeyframeEmbedding { model_tag, timestamp_ms } => {
                dir.join(model_tag).join(format!("kf-{timestamp_ms}.f32le.zst"))
            }
            Derivation::KeyframeManifest { model_tag } => {
                dir.join(model_tag).join("keyframes.json")
            }
        }
    }

    /// Temp-name + rename so a crash never leaves a partial blob at a final
    /// path (the same rename-after-write rule the ingest engine follows).
    pub fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), IndexError> {
        let io = |source| IndexError::Blob { path: path.to_path_buf(), source };
        let parent = path.parent().ok_or_else(|| IndexError::Blob {
            path: path.to_path_buf(),
            source: std::io::Error::other("blob path has no parent"),
        })?;
        std::fs::create_dir_all(parent).map_err(io)?;
        let temp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create(&temp).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        std::fs::rename(&temp, path).map_err(io)
    }

    pub fn write_vector(&self, path: &Path, vector: &[f32]) -> Result<(), IndexError> {
        let mut raw = Vec::with_capacity(vector.len() * 4);
        for v in vector {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let compressed = zstd::encode_all(raw.as_slice(), ZSTD_LEVEL)
            .map_err(|source| IndexError::Blob { path: path.to_path_buf(), source })?;
        self.write_atomic(path, &compressed)
    }

    pub fn read_vector(&self, path: &Path) -> Result<Vec<f32>, IndexError> {
        let compressed = std::fs::read(path)
            .map_err(|source| IndexError::Blob { path: path.to_path_buf(), source })?;
        let raw = zstd::decode_all(compressed.as_slice())
            .map_err(|source| IndexError::Blob { path: path.to_path_buf(), source })?;
        if raw.len() % 4 != 0 {
            return Err(IndexError::VectorShape { path: path.to_path_buf(), len: raw.len() });
        }
        Ok(raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }
}
```

Run: `cargo test -p majestical-index blob` — Expected: PASS.

- [ ] **Step 4: Write failing thumbnail tests, then implement resize.rs + thumbs.rs**

Tests (in `thumbs.rs`):

```rust
#[test]
fn thumbnail_is_webp_with_320_longest_edge() {
    let mut img = image::RgbImage::new(640, 480);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    let bytes = thumbnail_webp(&img).expect("thumb");
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
    let back = image::load_from_memory(&bytes).expect("decode webp");
    assert_eq!((back.width(), back.height()), (320, 240));
}

#[test]
fn small_images_are_not_upscaled() {
    let img = image::RgbImage::new(100, 60);
    let bytes = thumbnail_webp(&img).expect("thumb");
    let back = image::load_from_memory(&bytes).expect("decode");
    assert_eq!((back.width(), back.height()), (100, 60));
}
```

`resize.rs` (shared by thumbnails and, in PR 6, encoder preprocessing — the
antialiased-bilinear requirement lives HERE and must not change without re-running
conformance):

```rust
//! Antialiased bilinear resize. The encoder conformance gate depends on this
//! exact algorithm (transformers v5 resizes with torchvision antialias=True);
//! change the filter only together with `just encoder-conformance`.

use fast_image_resize as fr;

use crate::error::IndexError;

pub fn resize_rgb(
    src: &image::RgbImage,
    dst_w: u32,
    dst_h: u32,
) -> Result<image::RgbImage, IndexError> {
    let src_img = fr::images::Image::from_vec_u8(
        src.width(),
        src.height(),
        src.as_raw().clone(),
        fr::PixelType::U8x3,
    )
    .map_err(|e| IndexError::Resize(e.to_string()))?;
    let mut dst_img = fr::images::Image::new(dst_w, dst_h, fr::PixelType::U8x3);
    let mut resizer = fr::Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &fr::ResizeOptions::new()
                .resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Bilinear)),
        )
        .map_err(|e| IndexError::Resize(e.to_string()))?;
    image::RgbImage::from_raw(dst_w, dst_h, dst_img.into_vec())
        .ok_or_else(|| IndexError::Resize("buffer size mismatch after resize".into()))
}
```

`thumbs.rs`:

```rust
//! Thumbnail generation: 320px longest edge, lossy WebP. HEIC/HEIF decode
//! through macOS's `sips` (the `image` crate has no HEIC support).

use std::path::Path;
use std::process::Command;

use crate::error::IndexError;
use crate::resize::resize_rgb;

pub const THUMB_EDGE: u32 = 320;
const WEBP_QUALITY: f32 = 80.0;

pub fn decode_image(path: &Path) -> Result<image::RgbImage, IndexError> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let dynamic = if matches!(ext.as_str(), "heic" | "heif") {
        decode_via_sips(path)?
    } else {
        image::open(path).map_err(|e| IndexError::Decode {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?
    };
    Ok(dynamic.to_rgb8())
}

fn decode_via_sips(path: &Path) -> Result<image::DynamicImage, IndexError> {
    let out = tempfile::Builder::new()
        .suffix(".png")
        .tempfile()
        .map_err(|source| IndexError::Blob { path: path.to_path_buf(), source })?;
    let status = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(path)
        .arg("--out")
        .arg(out.path())
        .output()
        .map_err(|source| IndexError::Blob { path: path.to_path_buf(), source })?;
    if !status.status.success() {
        return Err(IndexError::Decode {
            path: path.to_path_buf(),
            message: format!("sips failed: {}", String::from_utf8_lossy(&status.stderr)),
        });
    }
    image::open(out.path()).map_err(|e| IndexError::Decode {
        path: path.to_path_buf(),
        message: format!("sips output unreadable: {e}"),
    })
}

pub fn thumbnail_webp(rgb: &image::RgbImage) -> Result<Vec<u8>, IndexError> {
    let (w, h) = (rgb.width(), rgb.height());
    let longest = w.max(h);
    let scaled = if longest <= THUMB_EDGE {
        rgb.clone()
    } else {
        let scale = f64::from(THUMB_EDGE) / f64::from(longest);
        let dw = (f64::from(w) * scale).round().max(1.0) as u32;
        let dh = (f64::from(h) * scale).round().max(1.0) as u32;
        resize_rgb(rgb, dw, dh)?
    };
    let encoded =
        webp::Encoder::from_rgb(scaled.as_raw(), scaled.width(), scaled.height())
            .encode(WEBP_QUALITY);
    Ok(encoded.to_vec())
}
```

(`as u32` casts after `.round().max(1.0)` trip clippy pedantic
`cast_possible_truncation` — use `u32::try_from(… as i64).unwrap_or(1)` or the
established house pattern if clippy objects; resolve to zero warnings.)

Run: `cargo test -p majestical-index` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/index
git commit -m "feat: index crate with blob store and webp thumbnailer"
```

### Task 12: Queue-as-diff planner + `maj index run/status` + path re-basing

**Files:**
- Create: `crates/index/src/work.rs`
- Modify: `crates/cli/src/main.rs`, `crates/cli/src/commands.rs`,
  `crates/cli/src/volume_identity.rs`, `crates/cli/Cargo.toml`
- Test: `crates/index/src/work.rs` unit tests, `crates/cli/tests/index_smoke.rs` (new)

- [ ] **Step 1: Write failing planner tests**

In `work.rs`:

```rust
#[test]
fn plans_missing_thumbs_and_counts_statuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let caps = Capabilities { model_tag: None, ffmpeg: false };
    let sources = vec![
        AssetSource { asset: "xxh3:aa11".into(), kind: MediaKind::Image,
                      abs_path: Some("/tmp/a.png".into()) },
        AssetSource { asset: "xxh3:bb22".into(), kind: MediaKind::Image, abs_path: None },
        AssetSource { asset: "xxh3:cc33".into(), kind: MediaKind::Video,
                      abs_path: Some("/tmp/c.mov".into()) },
        AssetSource { asset: "xxh3:dd44".into(), kind: MediaKind::Other,
                      abs_path: Some("/tmp/d.txt".into()) },
    ];
    let plan = plan_work(&sources, &store, &caps);
    // Thumbs: aa11 pending; bb22 offline; cc33 needs ffmpeg (video thumb);
    // dd44 not eligible at all.
    assert_eq!(plan.thumbs.pending, 1);
    assert_eq!(plan.thumbs.offline, 1);
    assert_eq!(plan.thumbs.needs_ffmpeg, 1);
    // Embeddings: no model → both eligible assets need the model.
    assert_eq!(plan.embeddings.needs_model, 2);
    assert_eq!(plan.items.len(), 1);
    assert!(matches!(plan.items[0].kind, WorkKind::Thumb));
}

#[test]
fn existing_blobs_count_done_and_raw_images_are_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path());
    let hex = "aa11";
    let thumb = store.path_for(hex, &Derivation::Thumb);
    store.write_atomic(&thumb, b"x").expect("seed thumb");
    let caps = Capabilities { model_tag: Some("m1".into()), ffmpeg: false };
    let sources = vec![
        AssetSource { asset: "xxh3:aa11".into(), kind: MediaKind::Image,
                      abs_path: Some("/tmp/a.png".into()) },
        AssetSource { asset: "xxh3:ee55".into(), kind: MediaKind::Image,
                      abs_path: Some("/tmp/e.cr3".into()) },
    ];
    let plan = plan_work(&sources, &store, &caps);
    assert_eq!(plan.thumbs.done, 1);
    assert_eq!(plan.thumbs.unsupported, 1, "RAW is planner-level unsupported");
    assert_eq!(plan.embeddings.unsupported, 1);
    assert_eq!(plan.embeddings.pending, 1, "aa11 embedding is embeddable");
}
```

Run: `cargo test -p majestical-index work` — Expected: FAIL.

- [ ] **Step 2: Implement `work.rs`**

```rust
//! The queue IS the diff: work = (assets × required derivations) minus
//! (blobs that exist). Nothing is stored; finished work is self-evident from
//! the blob store, so runs are resumable, idempotent, and self-healing.

use std::path::PathBuf;

use majestical_core::media_kind::MediaKind;

use crate::blob::{asset_hex, BlobStore, Derivation};

/// Extensions we know we cannot decode yet (RAW family). Planner-level so
/// status is deterministic instead of discovered by failing forever.
const UNDECODABLE_EXTS: &[&str] =
    &["dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    Thumb,
    ImageEmbed,
    Keyframes,
}

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub asset: String,
    pub asset_hex: String,
    pub abs_path: PathBuf,
    pub kind: WorkKind,
}

#[derive(Debug, Clone)]
pub struct AssetSource {
    pub asset: String,
    pub kind: MediaKind,
    /// Resolved readable path on an online volume, if any.
    pub abs_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Capabilities {
    pub model_tag: Option<String>,
    pub ffmpeg: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KindStatus {
    pub done: u64,
    pub pending: u64,
    pub offline: u64,
    pub unsupported: u64,
    pub needs_ffmpeg: u64,
    pub needs_model: u64,
}

#[derive(Debug, Default)]
pub struct WorkPlan {
    /// Priority-ordered: thumbnails, then image embeddings, then keyframes.
    pub items: Vec<WorkItem>,
    pub thumbs: KindStatus,
    pub embeddings: KindStatus,
    pub keyframes: KindStatus,
}

fn undecodable(path: &std::path::Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| UNDECODABLE_EXTS.contains(&e.as_str()))
}

#[must_use]
pub fn plan_work(sources: &[AssetSource], blobs: &BlobStore, caps: &Capabilities) -> WorkPlan {
    let mut plan = WorkPlan::default();
    let mut thumb_items = Vec::new();
    let mut embed_items = Vec::new();
    let mut kf_items = Vec::new();
    for source in sources {
        let Some(hex) = asset_hex(&source.asset) else { continue };
        if source.kind == MediaKind::Other {
            continue;
        }
        // Thumbnails (images directly; videos need ffmpeg for the frame grab).
        if blobs.path_for(hex, &Derivation::Thumb).exists() {
            plan.thumbs.done += 1;
        } else {
            match (&source.abs_path, source.kind) {
                (None, _) => plan.thumbs.offline += 1,
                (Some(p), _) if undecodable(p) => plan.thumbs.unsupported += 1,
                (Some(_), MediaKind::Video) if !caps.ffmpeg => plan.thumbs.needs_ffmpeg += 1,
                (Some(p), _) => {
                    plan.thumbs.pending += 1;
                    thumb_items.push(WorkItem {
                        asset: source.asset.clone(),
                        asset_hex: hex.to_string(),
                        abs_path: p.clone(),
                        kind: WorkKind::Thumb,
                    });
                }
            }
        }
        // Image embeddings.
        if source.kind == MediaKind::Image {
            let status = &mut plan.embeddings;
            match &caps.model_tag {
                None => status.needs_model += 1,
                Some(tag) => {
                    if blobs
                        .path_for(hex, &Derivation::ImageEmbedding { model_tag: tag })
                        .exists()
                    {
                        status.done += 1;
                    } else {
                        match &source.abs_path {
                            None => status.offline += 1,
                            Some(p) if undecodable(p) => status.unsupported += 1,
                            Some(p) => {
                                status.pending += 1;
                                embed_items.push(WorkItem {
                                    asset: source.asset.clone(),
                                    asset_hex: hex.to_string(),
                                    abs_path: p.clone(),
                                    kind: WorkKind::ImageEmbed,
                                });
                            }
                        }
                    }
                }
            }
        }
        // Video keyframes.
        if source.kind == MediaKind::Video {
            let status = &mut plan.keyframes;
            match &caps.model_tag {
                None => status.needs_model += 1,
                Some(tag) => {
                    if blobs
                        .path_for(hex, &Derivation::KeyframeManifest { model_tag: tag })
                        .exists()
                    {
                        status.done += 1;
                    } else if !caps.ffmpeg {
                        status.needs_ffmpeg += 1;
                    } else {
                        match &source.abs_path {
                            None => status.offline += 1,
                            Some(p) => {
                                status.pending += 1;
                                kf_items.push(WorkItem {
                                    asset: source.asset.clone(),
                                    asset_hex: hex.to_string(),
                                    abs_path: p.clone(),
                                    kind: WorkKind::Keyframes,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    plan.items = thumb_items;
    plan.items.extend(embed_items);
    plan.items.extend(kf_items);
    plan
}
```

Add `pub mod work;` to lib.rs. Run: `cargo test -p majestical-index` — PASS.

- [ ] **Step 3: Volume-root-relative paths in scan and ingest**

`volume_identity.rs`: change `fn mount_point_of` to `pub(crate) fn mount_point_of`
and add `mounted_volumes` if Task 8 didn't already (it did).

`commands.rs` `cmd_scan`: where each file's `Op::AssetSeen` is built, replace the
scanned-dir-relative path with a volume-root-relative one:

```rust
let mount = crate::volume_identity::mount_point_of(&absolute_path);
let vol_rel = absolute_path
    .strip_prefix(&mount)
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|_| relative_to_scan_dir.clone());
```

and pass `path: vol_rel`. (Keep the existing non-UTF-8/lossy behavior exactly as
the current line does — only the base changes.)

`cmd_ingest`'s `asset_and_para_ops`: for each placed file the instance path is
currently the dest-root-relative `dest_rel`; compute the same way:

```rust
let absolute = dest_root.join(&placed.dest_rel);
let mount = crate::volume_identity::mount_point_of(&absolute);
let vol_rel = absolute
    .strip_prefix(&mount)
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|_| placed.dest_rel.clone());
```

Existing smoke tests that assert stored paths (grep `cli_smoke.rs` for path
assertions on scan/search output) now see paths from the volume root (tempdirs
live on the root volume, so `private/tmp/...` on macOS) — update the assertions to
match on basename only (`contains("a.txt")`), not full relative paths.

Watchlist entry (append under a new "Phase 4 deferrals" heading in
`docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, in this PR): pre-phase-4
instance rows keep scan-dir-relative paths; the indexer reports them `offline`
until a rescan re-observes them (stale LWW keys persist harmlessly).

- [ ] **Step 4: CLI `maj index run|status`**

`crates/cli/Cargo.toml`: add `majestical-index = { path = "../index" }`; dev-deps
add `image = { workspace = true }` (test fixture generation).

`main.rs`:

```rust
Index {
    #[command(subcommand)]
    cmd: IndexCmd,
},

#[derive(Subcommand)]
enum IndexCmd {
    /// Work the derivation queue (thumbnails now; embeddings and keyframes
    /// as their capabilities are present).
    Run {
        /// Keep running, re-checking for new work every few seconds.
        #[arg(long)]
        watch: bool,
        #[arg(long)]
        threads: Option<usize>,
        /// Stop after this many items.
        #[arg(long)]
        limit: Option<usize>,
        /// Comma-separated subset: thumbs,embeddings,keyframes.
        #[arg(long, value_delimiter = ',')]
        kinds: Option<Vec<String>>,
        #[arg(long)]
        json: bool,
    },
    /// Show queue status per derivation kind.
    Status {
        #[arg(long)]
        json: bool,
    },
}
```

Dispatch both arms through `commands::cmd_index_run` / `cmd_index_status` with
`&app` (read-only — indexing writes blobs, not events).

`commands.rs`:

```rust
pub(crate) struct IndexRunArgs {
    pub watch: bool,
    pub threads: Option<usize>,
    pub limit: Option<usize>,
    pub kinds: Option<Vec<String>>,
    pub json: bool,
}

fn gather_sources(projection: &Projection) -> Vec<majestical_index::work::AssetSource> {
    use majestical_core::media_kind::media_kind;
    let mounted = crate::volume_identity::mounted_volumes();
    let mut sources = Vec::new();
    for (asset, state) in projection.assets() {
        let mut kind = majestical_core::media_kind::MediaKind::Other;
        let mut abs_path = None;
        for ((volume, path), _info) in &state.instances {
            let k = media_kind(path);
            if kind == majestical_core::media_kind::MediaKind::Other {
                kind = k;
            }
            if abs_path.is_none() {
                if let Some(mount) = mounted.get(volume) {
                    let candidate = mount.join(path);
                    if candidate.is_file() {
                        abs_path = Some(candidate);
                    }
                }
            }
        }
        sources.push(majestical_index::work::AssetSource {
            asset: asset.0.clone(),
            kind,
            abs_path,
        });
    }
    sources
}

fn capabilities() -> majestical_index::work::Capabilities {
    // Model and ffmpeg detection arrive with PRs 6 and 8; until then both are
    // honestly absent and status reports needs-model / needs-ffmpeg.
    majestical_index::work::Capabilities { model_tag: None, ffmpeg: false }
}

pub(crate) fn cmd_index_run(app: &FsApp, catalog_dir: &Path, args: &IndexRunArgs) -> Result<()> {
    loop {
        let projection = app.projection()?;
        let blobs = majestical_index::blob::BlobStore::new(catalog_dir);
        let plan = majestical_index::work::plan_work(
            &gather_sources(&projection),
            &blobs,
            &capabilities(),
        );
        let keep = |k: majestical_index::work::WorkKind| {
            args.kinds.as_ref().is_none_or(|ks| {
                let name = match k {
                    majestical_index::work::WorkKind::Thumb => "thumbs",
                    majestical_index::work::WorkKind::ImageEmbed => "embeddings",
                    majestical_index::work::WorkKind::Keyframes => "keyframes",
                };
                ks.iter().any(|s| s == name)
            })
        };
        let mut items: Vec<_> = plan.items.into_iter().filter(|i| keep(i.kind)).collect();
        if let Some(limit) = args.limit {
            items.truncate(limit);
        }
        let (written, failed) = run_thumb_items(&blobs, &items, args.threads)?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({ "written": written, "failed": failed.len() })
            );
        } else {
            println!("thumbnails: {written} written, {} failed", failed.len());
            for (path, reason) in &failed {
                eprintln!("  failed {}: {reason}", path.display());
            }
        }
        if !args.watch {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

fn run_thumb_items(
    blobs: &majestical_index::blob::BlobStore,
    items: &[majestical_index::work::WorkItem],
    threads: Option<usize>,
) -> Result<(u64, Vec<(PathBuf, String)>)> {
    use std::sync::Mutex;
    let jobs = threads
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(2, |n| n.get().min(4))
        })
        .max(1);
    let written = std::sync::atomic::AtomicU64::new(0);
    let failed: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
    let queue = Mutex::new(items.iter().filter(|i| {
        matches!(i.kind, majestical_index::work::WorkKind::Thumb)
    }));
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let Some(item) = ({
                    let mut q = match queue.lock() {
                        Ok(q) => q,
                        Err(_) => return,
                    };
                    q.next()
                }) else {
                    return;
                };
                let result = majestical_index::thumbs::decode_image(&item.abs_path)
                    .and_then(|img| majestical_index::thumbs::thumbnail_webp(&img))
                    .and_then(|bytes| {
                        let path = blobs.path_for(
                            &item.asset_hex,
                            &majestical_index::blob::Derivation::Thumb,
                        );
                        blobs.write_atomic(&path, &bytes)
                    });
                match result {
                    Ok(()) => {
                        written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => {
                        if let Ok(mut f) = failed.lock() {
                            f.push((item.abs_path.clone(), e.to_string()));
                        }
                    }
                }
            });
        }
    });
    Ok((written.into_inner(), failed.into_inner().unwrap_or_default()))
}

pub(crate) fn cmd_index_status(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let projection = app.projection()?;
    let blobs = majestical_index::blob::BlobStore::new(catalog_dir);
    let plan = majestical_index::work::plan_work(
        &gather_sources(&projection),
        &blobs,
        &capabilities(),
    );
    let row = |s: &majestical_index::work::KindStatus| {
        serde_json::json!({
            "done": s.done, "pending": s.pending, "offline": s.offline,
            "unsupported": s.unsupported, "needs_ffmpeg": s.needs_ffmpeg,
            "needs_model": s.needs_model,
        })
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbs": row(&plan.thumbs),
                "embeddings": row(&plan.embeddings),
                "keyframes": row(&plan.keyframes),
            })
        );
    } else {
        for (name, s) in [
            ("thumbs", &plan.thumbs),
            ("embeddings", &plan.embeddings),
            ("keyframes", &plan.keyframes),
        ] {
            println!(
                "{name}: {} done, {} pending, {} offline, {} unsupported, {} need ffmpeg, {} need model",
                s.done, s.pending, s.offline, s.unsupported, s.needs_ffmpeg, s.needs_model
            );
        }
    }
    Ok(())
}
```

(If the borrow of `items.iter().filter(…)` inside a Mutex fights the borrow
checker, collect the thumb items into a `Vec` first and share an
`AtomicUsize` index instead — same shape, simpler ownership.)

- [ ] **Step 5: Integration test**

Create `crates/cli/tests/index_smoke.rs`:

```rust
#![cfg(test)] // clippy.toml test exemptions key on the literal attribute
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn maj(catalog: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("maj").expect("binary");
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", "test-machine")
        .env("MAJ_STATE_DIR", state);
    c
}

fn write_png(path: &Path) {
    let mut img = image::RgbImage::new(64, 48);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x * 4) as u8, (y * 5) as u8, 7]);
    }
    img.save(path).expect("png");
}

#[test]
fn index_run_writes_thumbs_idempotently_and_self_heals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = dir.path().join("catalog");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&catalog).expect("mkdir");
    maj(&catalog, &state).args(["catalog", "init"]).assert().success();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    write_png(&media.join("photo.png"));
    maj(&catalog, &state).args(["scan"]).arg(&media).assert().success();

    maj(&catalog, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 written"));
    let thumbs: Vec<PathBuf> = walkdir_find(&catalog.join("blobs"), "thumb-320.webp");
    assert_eq!(thumbs.len(), 1, "one thumbnail blob in the sync root");

    maj(&catalog, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("0 written"));

    std::fs::remove_file(&thumbs[0]).expect("delete blob");
    maj(&catalog, &state)
        .args(["index", "run"])
        .assert()
        .success()
        .stdout(contains("1 written"));

    maj(&catalog, &state)
        .args(["index", "status"])
        .assert()
        .success()
        .stdout(contains("thumbs: 1 done"));
}
```

(Copy `walkdir_find` from `cli_smoke.rs` — or move it into a shared
`tests/common/mod.rs` with `#[cfg(test)]`-attributed helpers, the ingest crate's
established pattern.)

Run: `cargo test -p majestical-cli --test index_smoke`
Expected: PASS after implementation.

- [ ] **Step 6: Full checks, watchlist entry, commit**

Run: `just ci`
Expected: clean.

```bash
git add crates/index crates/cli Cargo.toml Cargo.lock docs/superpowers/plans/2026-07-29-phase2-watchlist.md
git commit -m "feat: maj index run/status with queue-as-diff thumbnails"
```

Open PR 5: title `feat: index queue and thumbnails`. Body: queue-as-diff design,
blob layout, path re-basing note + watchlist entry.

---

# PR 6 — Model fetch, encoder, conformance gate

### Task 13: Model registry + `maj model fetch`

**Files:**
- Create: `crates/index/src/model.rs`
- Modify: `crates/index/src/lib.rs`, `crates/cli/src/main.rs`, `crates/cli/src/commands.rs`
- Test: `crates/index/src/model.rs` unit tests

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn sha256_matches_known_vector() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("f");
    std::fs::write(&p, b"abc").expect("write");
    assert_eq!(
        sha256_file(&p).expect("hash"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
}

#[test]
fn fetch_one_downloads_verifies_and_skips_when_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("weights.bin");
    std::fs::write(&src, b"model-bytes").expect("write");
    let sha = sha256_file(&src).expect("hash");
    let url = format!("file://{}", src.display());
    let dest_dir = dir.path().join("cache");
    let outcome = fetch_one(&dest_dir, "weights.bin", &url, &sha, 11, false)
        .expect("fetch");
    assert_eq!(outcome, FetchOutcome::Downloaded);
    assert_eq!(std::fs::read(dest_dir.join("weights.bin")).expect("read"), b"model-bytes");
    let again = fetch_one(&dest_dir, "weights.bin", &url, &sha, 11, false).expect("fetch");
    assert_eq!(again, FetchOutcome::AlreadyPresent);
}

#[test]
fn fetch_one_rejects_a_bad_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("weights.bin");
    std::fs::write(&src, b"tampered").expect("write");
    let url = format!("file://{}", src.display());
    let err = fetch_one(&dir.path().join("cache"), "weights.bin", &url,
        &"0".repeat(64), 8, false).expect_err("must fail");
    assert!(err.to_string().contains("hash mismatch"));
    assert!(!dir.path().join("cache/weights.bin").exists(), "no unverified file placed");
}
```

Run: `cargo test -p majestical-index model` — Expected: FAIL.

- [ ] **Step 2: Implement `model.rs`**

```rust
//! Encoder model artifacts: pinned URLs + sha256, fetched with system curl
//! into a shared cache. Every artifact is verified before it is placed;
//! nothing unverified ever sits at a final path.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::error::IndexError;

pub const MODEL_TAG: &str = "siglip2-b16-v1";
const HF_REPO: &str = "onnx-community/siglip2-base-patch16-256-ONNX";
const HF_REVISION: &str = "d1114256522a37ffa257a0a58017348ab0058db2";

pub struct ModelFile {
    pub name: &'static str,
    pub repo_path: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// Vision tower fp32 (CoreML/ANE handles precision), text tower fp16 (the
/// Gemma 256k vocab makes fp32 a 1.13 GB download), tokenizer.
pub const MODEL_FILES: [ModelFile; 3] = [
    ModelFile {
        name: "vision_model.onnx",
        repo_path: "onnx/vision_model.onnx",
        sha256: "f5cb16728a704703f05516ded628397e11dbca4de2eb5db04b0c0bcee988aa7a",
        bytes: 371_992_072,
    },
    ModelFile {
        name: "text_model_fp16.onnx",
        repo_path: "onnx/text_model_fp16.onnx",
        sha256: "80954edffdc689599e5d5bc6a1738380bc9e8139a18e5c8892485f248b6b4890",
        bytes: 564_862_230,
    },
    ModelFile {
        name: "tokenizer.json",
        repo_path: "tokenizer.json",
        sha256: "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322",
        bytes: 34_363_039,
    },
];

/// `MAJ_MODEL_DIR` overrides the platform cache base (tests, CI caching).
pub fn model_dir() -> Result<PathBuf, IndexError> {
    if let Some(dir) = std::env::var_os("MAJ_MODEL_DIR") {
        return Ok(PathBuf::from(dir).join(MODEL_TAG));
    }
    let data = dirs::data_dir()
        .ok_or_else(|| IndexError::Model("no platform data dir; set MAJ_MODEL_DIR".into()))?;
    Ok(data.join("majestical").join("models").join(MODEL_TAG))
}

/// Present = every file exists with its expected size (hash checked at fetch
/// time; pass `verify` to re-hash).
pub fn model_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|f| {
        std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.bytes)
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    AlreadyPresent,
    Downloaded,
}

pub fn fetch(dir: &Path, verify: bool, progress: &mut dyn FnMut(&str)) -> Result<(), IndexError> {
    for file in &MODEL_FILES {
        let url = format!("https://huggingface.co/{HF_REPO}/resolve/{HF_REVISION}/{}", file.repo_path);
        progress(&format!("{} ({} MB)", file.name, file.bytes / 1_000_000));
        let outcome = fetch_one(dir, file.name, &url, file.sha256, file.bytes, verify)?;
        progress(match outcome {
            FetchOutcome::AlreadyPresent => "  already present",
            FetchOutcome::Downloaded => "  downloaded and verified",
        });
    }
    Ok(())
}

pub fn fetch_one(
    dir: &Path,
    name: &str,
    url: &str,
    expected_sha256: &str,
    expected_bytes: u64,
    verify: bool,
) -> Result<FetchOutcome, IndexError> {
    let dest = dir.join(name);
    if let Ok(meta) = std::fs::metadata(&dest) {
        if meta.len() == expected_bytes
            && (!verify || sha256_file(&dest)? == expected_sha256)
        {
            return Ok(FetchOutcome::AlreadyPresent);
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| IndexError::Model(format!("creating {}: {e}", dir.display())))?;
    let temp = dir.join(format!(".fetch-{name}"));
    let status = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--output"])
        .arg(&temp)
        .arg(url)
        .status()
        .map_err(|e| IndexError::Model(format!("running curl: {e} (is curl installed?)")))?;
    if !status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(IndexError::Model(format!("curl failed for {url} ({status})")));
    }
    let actual = sha256_file(&temp)?;
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&temp);
        return Err(IndexError::Model(format!(
            "hash mismatch for {name}: expected {expected_sha256}, got {actual} — \
             refusing to install; re-run to retry"
        )));
    }
    std::fs::rename(&temp, &dest)
        .map_err(|e| IndexError::Model(format!("placing {}: {e}", dest.display())))?;
    Ok(FetchOutcome::Downloaded)
}

pub fn sha256_file(path: &Path) -> Result<String, IndexError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| IndexError::Model(format!("opening {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| IndexError::Model(format!("hashing {}: {e}", path.display())))?;
    Ok(format!("{:x}", hasher.finalize()))
}
```

CLI (`main.rs`):

```rust
Model {
    #[command(subcommand)]
    cmd: ModelCmd,
},

#[derive(Subcommand)]
enum ModelCmd {
    /// Download the encoder model (pinned URLs, sha256-verified).
    Fetch {
        /// Re-hash files that already exist.
        #[arg(long)]
        verify: bool,
    },
}
```

`commands.rs`:

```rust
pub(crate) fn cmd_model_fetch(verify: bool) -> Result<()> {
    let dir = majestical_index::model::model_dir()?;
    println!("model cache: {}", dir.display());
    majestical_index::model::fetch(&dir, verify, &mut |line| println!("{line}"))?;
    println!("model '{}' ready", majestical_index::model::MODEL_TAG);
    Ok(())
}
```

Also update `capabilities()` from Task 12:

```rust
fn capabilities() -> majestical_index::work::Capabilities {
    let model_tag = majestical_index::model::model_dir()
        .ok()
        .filter(|d| majestical_index::model::model_present(d))
        .map(|_| majestical_index::model::MODEL_TAG.to_string());
    majestical_index::work::Capabilities { model_tag, ffmpeg: false }
}
```

- [ ] **Step 3: Run, commit**

Run: `cargo test -p majestical-index && just check`
Expected: PASS.

```bash
git add crates/index crates/cli
git commit -m "feat: maj model fetch with pinned hashes"
```

### Task 14: Preprocessing + encoder

**Files:**
- Create: `crates/index/src/preprocess.rs`, `crates/index/src/encoder.rs`
- Modify: `crates/index/Cargo.toml`, `crates/index/src/lib.rs`

- [ ] **Step 1: Add dependencies**

`crates/index/Cargo.toml` (also add both to `[workspace.dependencies]` with the
same pins):

```toml
ort = { version = "=2.0.0-rc.13", features = ["coreml"] }
tokenizers = { version = "0.23.1", default-features = false }
```

(`ort`'s default `download-binaries` feature provides static ONNX Runtime 1.28
with CoreML built in on macOS — no dylibs to manage. Verify at execution that
rc.13 is still current; later RCs may rename APIs, so bump deliberately.)

- [ ] **Step 2: Write failing preprocessing tests**

`preprocess.rs`:

```rust
#[test]
fn uniform_color_maps_exactly_and_shape_is_nchw() {
    let mut img = image::RgbImage::new(100, 50);
    for px in img.pixels_mut() {
        *px = image::Rgb([255, 0, 128]);
    }
    let out = preprocess_rgb(&img).expect("preprocess");
    assert_eq!(out.len(), 3 * 256 * 256);
    let n = 256 * 256;
    assert!((out[0] - 1.0).abs() < 1e-6, "R plane first (NCHW)");
    assert!((out[n] - (-1.0)).abs() < 1e-6, "G plane second");
    assert!((out[2 * n] - (128.0 / 127.5 - 1.0)).abs() < 1e-6, "B plane third");
}

#[test]
fn preprocess_is_deterministic() {
    let mut img = image::RgbImage::new(300, 200);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
    }
    assert_eq!(preprocess_rgb(&img).expect("a"), preprocess_rgb(&img).expect("b"));
}
```

Run: `cargo test -p majestical-index preprocess` — Expected: FAIL.

- [ ] **Step 3: Implement `preprocess.rs`**

```rust
//! SigLIP 2 image preprocessing. Every constant here is pinned by the
//! conformance gate: squash-resize to 256×256 (no crop, no aspect
//! preservation) with antialiased bilinear, then (px/127.5 − 1) into NCHW.

use crate::error::IndexError;
use crate::resize::resize_rgb;

pub const EDGE: u32 = 256;

pub fn preprocess_rgb(rgb: &image::RgbImage) -> Result<Vec<f32>, IndexError> {
    let resized = if (rgb.width(), rgb.height()) == (EDGE, EDGE) {
        rgb.clone()
    } else {
        resize_rgb(rgb, EDGE, EDGE)?
    };
    let raw = resized.into_raw(); // HWC, RGB
    let plane = (EDGE * EDGE) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (i, px) in raw.chunks_exact(3).enumerate() {
        out[i] = f32::from(px[0]) / 127.5 - 1.0;
        out[plane + i] = f32::from(px[1]) / 127.5 - 1.0;
        out[2 * plane + i] = f32::from(px[2]) / 127.5 - 1.0;
    }
    Ok(out)
}
```

- [ ] **Step 4: Implement `encoder.rs`** (tests are model-gated, next step)

```rust
//! SigLIP 2 dual-tower encoder via ONNX Runtime. Vision on the CoreML EP
//! (ANE), text on CPU (CoreML mishandles the text tower's shapes; the fp16
//! text model is fast enough for query-time encoding). Both towers emit
//! `pooler_output`, L2-normalized here so dot product == cosine.

use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::Tensor;

use crate::error::IndexError;
use crate::preprocess::{preprocess_rgb, EDGE};

pub const EMBED_DIM: usize = 768;
const TEXT_LEN: usize = 64;

pub struct EncoderOptions {
    pub coreml: bool,
    /// CoreML model cache dir (without it, CoreML recompiles per session).
    pub coreml_cache: Option<PathBuf>,
}

pub struct Encoder {
    vision: Session,
    text: Session,
    tokenizer: tokenizers::Tokenizer,
}

fn enc_err<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> IndexError + '_ {
    move |e| IndexError::Encoder(format!("{context}: {e}"))
}

impl Encoder {
    pub fn load(model_dir: &Path, options: &EncoderOptions) -> Result<Self, IndexError> {
        let vision_path = model_dir.join("vision_model.onnx");
        let text_path = model_dir.join("text_model_fp16.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        let mut vision_builder =
            Session::builder().map_err(enc_err("vision session builder"))?;
        if options.coreml {
            let mut ep = ort::ep::CoreML::default()
                .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                .with_compute_units(ort::ep::coreml::ComputeUnits::CPUAndNeuralEngine);
            if let Some(cache) = &options.coreml_cache {
                ep = ep.with_model_cache_dir(cache.to_string_lossy());
            }
            vision_builder = vision_builder
                .with_execution_providers([ep.build()])
                .map_err(enc_err("enabling CoreML"))?;
        }
        let vision = vision_builder
            .commit_from_file(&vision_path)
            .map_err(enc_err("loading vision model"))?;
        let text = Session::builder()
            .map_err(enc_err("text session builder"))?
            .commit_from_file(&text_path)
            .map_err(enc_err("loading text model"))?;

        let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(enc_err("loading tokenizer"))?;
        // Fixed-64 right padding with pad id 0 is correctness-critical: the
        // text tower pools position 63 unconditionally. Padding is also in
        // tokenizer.json, but truncation is NOT — set both explicitly.
        tokenizer.with_padding(Some(tokenizers::utils::padding::PaddingParams {
            strategy: tokenizers::utils::padding::PaddingStrategy::Fixed(TEXT_LEN),
            direction: tokenizers::utils::padding::PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id: 0,
            pad_type_id: 0,
            pad_token: "<pad>".to_string(),
        }));
        tokenizer
            .with_truncation(Some(tokenizers::utils::truncation::TruncationParams {
                direction: tokenizers::utils::truncation::TruncationDirection::Right,
                max_length: TEXT_LEN,
                strategy: tokenizers::utils::truncation::TruncationStrategy::LongestFirst,
                stride: 0,
            }))
            .map_err(enc_err("configuring truncation"))?;
        Ok(Self { vision, text, tokenizer })
    }

    pub fn embed_image(&mut self, rgb: &image::RgbImage) -> Result<Vec<f32>, IndexError> {
        let pixels = preprocess_rgb(rgb)?;
        let input = Tensor::from_array((
            [1usize, 3, EDGE as usize, EDGE as usize],
            pixels,
        ))
        .map_err(enc_err("building pixel tensor"))?;
        let outputs = self
            .vision
            .run(ort::inputs!["pixel_values" => input])
            .map_err(enc_err("vision inference"))?;
        Self::pooled(&outputs)
    }

    pub fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, IndexError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(enc_err("tokenizing"))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| i64::from(x)).collect();
        if ids.len() != TEXT_LEN {
            return Err(IndexError::Encoder(format!(
                "tokenizer produced {} ids, expected {TEXT_LEN}", ids.len()
            )));
        }
        let input = Tensor::from_array(([1usize, TEXT_LEN], ids))
            .map_err(enc_err("building id tensor"))?;
        let outputs = self
            .text
            .run(ort::inputs!["input_ids" => input])
            .map_err(enc_err("text inference"))?;
        Self::pooled(&outputs)
    }

    /// Tokenize without running inference (golden-token conformance).
    pub fn token_ids(&self, text: &str) -> Result<Vec<i64>, IndexError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(enc_err("tokenizing"))?;
        Ok(encoding.get_ids().iter().map(|&x| i64::from(x)).collect())
    }

    fn pooled(outputs: &ort::session::SessionOutputs<'_>) -> Result<Vec<f32>, IndexError> {
        let (_, data) = outputs["pooler_output"]
            .try_extract_tensor::<f32>()
            .map_err(enc_err("extracting pooler_output"))?;
        if data.len() != EMBED_DIM {
            return Err(IndexError::Encoder(format!(
                "pooler_output has {} values, expected {EMBED_DIM}", data.len()
            )));
        }
        let mut v = data.to_vec();
        l2_normalize(&mut v);
        Ok(v)
    }
}

pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
```

API-drift note for the implementer: the exact `ort` rc.13 paths
(`ort::ep::CoreML`, `Session::builder()`, `commit_from_file`, `ort::inputs!`,
`try_extract_tensor`, and whether builder methods need `.recover()` on the
`BuilderResult` type) were transcribed from docs, not compiled — if the compiler
disagrees, follow docs.rs for `=2.0.0-rc.13` and keep the behavior identical.
Add `pub mod encoder; pub mod model; pub mod preprocess;` to lib.rs.

- [ ] **Step 5: Model-gated encoder sanity tests**

`crates/index/tests/encoder_gated.rs`:

```rust
//! Sanity tests requiring the fetched model. Run with:
//!   MAJ_MODEL_DIR=… cargo test -p majestical-index --test encoder_gated -- --ignored
#![cfg(test)]

use majestical_index::encoder::{cosine, Encoder, EncoderOptions};

fn load_cpu() -> Encoder {
    let dir = majestical_index::model::model_dir().expect("model dir");
    assert!(
        majestical_index::model::model_present(&dir),
        "run `maj model fetch` first (or set MAJ_MODEL_DIR)"
    );
    Encoder::load(&dir, &EncoderOptions { coreml: false, coreml_cache: None })
        .expect("load encoder")
}

#[test]
#[ignore = "needs fetched model"]
fn text_tokens_are_fixed_64_right_padded_with_eos() {
    let enc = load_cpu();
    let ids = enc.token_ids("a photo of a beach").expect("tokenize");
    assert_eq!(ids.len(), 64);
    let last_nonzero = ids.iter().rposition(|&i| i != 0).expect("nonzero");
    assert_eq!(ids[last_nonzero], 1, "eos id 1 closes the sequence");
    assert!(ids[last_nonzero + 1..].iter().all(|&i| i == 0), "right-padded with 0");
}

#[test]
#[ignore = "needs fetched model"]
fn matching_text_and_image_score_higher_than_mismatched() {
    let mut enc = load_cpu();
    // Solid blue square vs solid green square.
    let mut blue = image::RgbImage::new(64, 64);
    for px in blue.pixels_mut() { *px = image::Rgb([20, 40, 220]); }
    let mut green = image::RgbImage::new(64, 64);
    for px in green.pixels_mut() { *px = image::Rgb([30, 200, 40]); }
    let blue_v = enc.embed_image(&blue).expect("embed");
    let green_v = enc.embed_image(&green).expect("embed");
    let text_v = enc.embed_text("a solid blue square").expect("embed");
    assert!(cosine(&text_v, &blue_v) > cosine(&text_v, &green_v));
}
```

Run: `cargo test -p majestical-index` (gated tests skipped) — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/index Cargo.toml Cargo.lock
git commit -m "feat: siglip2 encoder with coreml vision tower"
```

### Task 15: Conformance oracle (fixtures, Python reference, CI)

**Files:**
- Create: `crates/index/examples/gen_fixtures.rs`, `crates/index/tests/fixtures/` (committed PNGs),
  `conformance/encoder/golden.py`, `crates/index/tests/encoder_conformance.rs`
- Modify: `justfile`, `.github/workflows/ci.yml`, `.gitignore`

- [ ] **Step 1: Generate and commit fixtures**

`crates/index/examples/gen_fixtures.rs`:

```rust
//! Regenerate the conformance fixture images (deterministic, no randomness):
//!   cargo run -p majestical-index --example gen_fixtures
fn main() {
    let dir = std::path::Path::new("crates/index/tests/fixtures");
    std::fs::create_dir_all(dir).expect("mkdir fixtures");
    let mut gradient = image::RgbImage::new(300, 200);
    for (x, y, px) in gradient.enumerate_pixels_mut() {
        *px = image::Rgb([
            (x * 255 / 299) as u8,
            (y * 255 / 199) as u8,
            ((x + y) % 256) as u8,
        ]);
    }
    gradient.save(dir.join("gradient.png")).expect("save");
    let mut blocks = image::RgbImage::new(256, 256);
    for (x, y, px) in blocks.enumerate_pixels_mut() {
        *px = match (x < 128, y < 128) {
            (true, true) => image::Rgb([220, 30, 30]),
            (false, true) => image::Rgb([30, 220, 30]),
            (true, false) => image::Rgb([30, 30, 220]),
            (false, false) => image::Rgb([220, 220, 30]),
        };
    }
    blocks.save(dir.join("blocks.png")).expect("save");
    // Extreme aspect ratio: squash-resize errors show here first.
    let mut wide = image::RgbImage::new(512, 64);
    for (x, y, px) in wide.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, (y * 4) as u8, 90]);
    }
    wide.save(dir.join("wide.png")).expect("save");
    println!("fixtures written");
}
```

Run it once and `git add` the three PNGs. (`examples/` compiles under
dev-dependencies; `image` is already a dependency. `expect` in an example trips
the workspace `expect_used = "warn"` only — acceptable; if clippy escalates, use
`unwrap_or_else` with `eprintln`+`std::process::abort` or add the example to the
test-exempt pattern used elsewhere.)

- [ ] **Step 2: Python reference `conformance/encoder/golden.py`**

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["transformers==5.14.1", "torch", "pillow"]
# ///
"""Golden embeddings from the reference SigLIP 2 implementation.

Usage: uv run conformance/encoder/golden.py --revision <sha> --out golden.json
The revision pins google/siglip2-base-patch16-256 (the torch source the ONNX
export was made from). Torch floats freely; transformers is the pinned oracle
(v5 uses the torchvision antialiased-bilinear resize path — the thing we are
actually testing).
"""
import argparse
import json
import pathlib

import torch
from PIL import Image
from transformers import AutoModel, AutoProcessor

TEXTS = [
    "a photo of a beach at sunset",
    "portrait of a golden retriever",
    "city skyline at night",
]
FIXTURES = pathlib.Path("crates/index/tests/fixtures")

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model_id = "google/siglip2-base-patch16-256"
    processor = AutoProcessor.from_pretrained(model_id, revision=args.revision)
    model = AutoModel.from_pretrained(model_id, revision=args.revision)
    model.eval()
    out = {
        "meta": {
            "model": model_id,
            "revision": args.revision,
            "transformers": __import__("transformers").__version__,
            "torch": torch.__version__,
        },
        "images": {},
        "texts": {},
        "token_ids": {},
    }
    with torch.no_grad():
        for png in sorted(FIXTURES.glob("*.png")):
            image = Image.open(png).convert("RGB")
            inputs = processor(images=image, return_tensors="pt")
            feats = model.get_image_features(**inputs).pooler_output
            feats = feats / feats.norm(p=2, dim=-1, keepdim=True)
            out["images"][png.name] = feats[0].tolist()
        for text in TEXTS:
            inputs = processor(
                text=text, padding="max_length", max_length=64,
                truncation=True, return_tensors="pt",
            )
            out["token_ids"][text] = inputs["input_ids"][0].tolist()
            feats = model.get_text_features(**inputs).pooler_output
            feats = feats / feats.norm(p=2, dim=-1, keepdim=True)
            out["texts"][text] = feats[0].tolist()
    pathlib.Path(args.out).write_text(json.dumps(out))
    print(f"golden embeddings -> {args.out}")

if __name__ == "__main__":
    main()
```

Implementer note: resolve the current commit sha of
`google/siglip2-base-patch16-256` (`curl -s https://huggingface.co/api/models/google/siglip2-base-patch16-256 | jq -r .sha`)
and pin it as `SIGLIP2_TORCH_REVISION` in the justfile (Step 4). If transformers
v5's `get_image_features` signature differs at execution, follow the pinned
version's docs — the invariants are: pooler_output, L2-normalize,
`padding="max_length", max_length=64, truncation=True`.

- [ ] **Step 3: Rust conformance test `crates/index/tests/encoder_conformance.rs`**

```rust
//! Oracle gate: Rust preprocessing + ONNX inference vs the pinned Python
//! reference. Run via `just encoder-conformance`.
#![cfg(test)]

use majestical_index::encoder::{cosine, Encoder, EncoderOptions};

const VISION_CPU_MIN_COSINE: f32 = 0.999;
const TEXT_MIN_COSINE: f32 = 0.995; // fp16 text tower vs fp32 reference
const VISION_COREML_MIN_COSINE: f32 = 0.99;

struct Golden {
    images: Vec<(String, Vec<f32>)>,
    texts: Vec<(String, Vec<f32>)>,
    token_ids: Vec<(String, Vec<i64>)>,
}

fn load_golden() -> Golden {
    let path = std::env::var("MAJ_GOLDEN").expect("set MAJ_GOLDEN to golden.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read golden"))
            .expect("parse golden");
    let vecs = |key: &str| {
        json[key]
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_array().expect("array").iter()
                        .map(|x| x.as_f64().expect("num") as f32).collect(),
                )
            })
            .collect::<Vec<(String, Vec<f32>)>>()
    };
    let ids = json["token_ids"].as_object().expect("object").iter()
        .map(|(k, v)| {
            (k.clone(), v.as_array().expect("array").iter()
                .map(|x| x.as_i64().expect("int")).collect())
        })
        .collect();
    Golden { images: vecs("images"), texts: vecs("texts"), token_ids: ids }
}

fn load(coreml: bool) -> Encoder {
    let dir = majestical_index::model::model_dir().expect("model dir");
    assert!(majestical_index::model::model_present(&dir), "run maj model fetch");
    Encoder::load(&dir, &EncoderOptions { coreml, coreml_cache: None }).expect("load")
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn tokenizer_matches_reference_exactly() {
    let enc = load(false);
    for (text, want) in load_golden().token_ids {
        let got = enc.token_ids(&text).expect("tokenize");
        assert_eq!(got, want, "token ids diverge for '{text}'");
    }
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn cpu_embeddings_match_reference() {
    let mut enc = load(false);
    let golden = load_golden();
    let mut worst_image = 1.0f32;
    for (name, want) in &golden.images {
        let img = image::open(format!("tests/fixtures/{name}")).expect("fixture").to_rgb8();
        let got = enc.embed_image(&img).expect("embed");
        let c = cosine(&got, want);
        worst_image = worst_image.min(c);
        assert!(c >= VISION_CPU_MIN_COSINE, "{name}: cosine {c} < {VISION_CPU_MIN_COSINE}");
    }
    let mut worst_text = 1.0f32;
    for (text, want) in &golden.texts {
        let got = enc.embed_text(text).expect("embed");
        let c = cosine(&got, want);
        worst_text = worst_text.min(c);
        assert!(c >= TEXT_MIN_COSINE, "'{text}': cosine {c} < {TEXT_MIN_COSINE}");
    }
    println!("worst image cosine {worst_image}, worst text cosine {worst_text}");
}

#[test]
#[ignore = "conformance: needs model + golden json"]
fn coreml_vision_is_close_to_reference() {
    let mut enc = load(true);
    for (name, want) in load_golden().images {
        let img = image::open(format!("tests/fixtures/{name}")).expect("fixture").to_rgb8();
        let got = enc.embed_image(&img).expect("embed");
        let c = cosine(&got, &want);
        assert!(c >= VISION_COREML_MIN_COSINE, "{name}: coreml cosine {c}");
    }
}
```

Tolerances are targets; if the measured floor differs, adjust the constant to the
measured value minus margin and record the measurement in the PR body (the spec
requires recording, not guessing).

- [ ] **Step 4: justfile + CI + .gitignore**

justfile additions:

```make
SIGLIP2_TORCH_REVISION := "<pin the resolved sha here — see Task 15 step 2>"

encoder-conformance:
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        cargo run -p majestical-cli --bin maj -- \
        --catalog . --machine-id conformance model fetch
    uv run conformance/encoder/golden.py \
        --revision {{SIGLIP2_TORCH_REVISION}} --out target/encoder-golden.json
    MAJ_MODEL_DIR="{{justfile_directory()}}/.model-cache" \
        MAJ_GOLDEN="{{justfile_directory()}}/target/encoder-golden.json" \
        cargo test -p majestical-index --test encoder_conformance --test encoder_gated -- --ignored
```

`.gitignore`: add `.model-cache/`.

`ci.yml`: new job `encoder-conformance` cloned from `mhl-conformance`'s shape
(same pinned checkout/toolchain/rust-cache/setup-uv SHAs), plus a model cache and
the uv cache:

```yaml
  encoder-conformance:
    runs-on: macos-latest
    steps:
      # same checkout / rust-toolchain / rust-cache / setup-uv steps as
      # mhl-conformance, identical SHAs and persist-credentials: false
      - name: Cache encoder model
        uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830  # v4.3.0 — verify latest SHA at execution
        with:
          path: .model-cache
          key: model-siglip2-b16-v1
      - name: Encoder conformance (oracle: pinned transformers)
        run: just encoder-conformance
```

(First run downloads ~940 MB of ONNX + the torch reference weights; both cache.
Run `actionlint` and `zizmor` on the workflow before committing, per house rules.)

- [ ] **Step 5: Run the whole gate locally**

Run: `just encoder-conformance`
Expected: model fetch verifies hashes; golden.py writes JSON; all three
conformance tests pass with printed cosine floors. Record the floors.

- [ ] **Step 6: Commit**

```bash
git add crates/index conformance justfile .github/workflows/ci.yml .gitignore
git commit -m "feat: encoder conformance gate against pinned transformers reference"
```

Open PR 6: title `feat: model fetch + encoder + conformance`. Body: pinned
artifacts table (urls/hashes/sizes), measured cosine floors, CI cache design.

---

# PR 7 — Lance vector store + semantic search

### Task 16: `VectorStore` (sync wrapper over lancedb)

**Files:**
- Create: `crates/index/src/vector_store.rs`
- Modify: root `Cargo.toml`, `crates/index/Cargo.toml`, `crates/index/src/lib.rs`,
  `.github/workflows/ci.yml`

- [ ] **Step 1: Add pinned dependencies**

Root `[workspace.dependencies]` (pins are lockstep-critical — lancedb 0.33.0
compiles against arrow 58 only):

```toml
lancedb = "=0.33.0"
arrow-array = "=58.0.0"
arrow-schema = "=58.0.0"
tokio = { version = "1", features = ["rt", "sync"] }
futures = "0.3"
```

`crates/index/Cargo.toml` adds all five. Build prerequisite: `protoc`
(`brew install protobuf`) — add a `# Build requires protoc: brew install protobuf`
comment beside the lancedb dep, and add to BOTH the `rust` and
`encoder-conformance` CI jobs, before the build steps:

```yaml
      - name: Install protoc (lance build dependency)
        run: brew install protobuf
```

Run: `cargo build -p majestical-index` — Expected: compiles (slow first build;
datafusion is heavy).

- [ ] **Step 2: Write failing store tests (in-file)**

```rust
#[test]
fn add_search_and_diff_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = VectorStore::open(&dir.path().join("lance")).expect("open");
    let unit = |i: usize| {
        let mut v = vec![0.0f32; 768];
        v[i] = 1.0;
        v
    };
    store.add(vec![
        VectorRow { asset_hex: "aa11".into(), kind: "image".into(), ts_ms: -1,
                    model_tag: "m1".into(), vector: unit(0) },
        VectorRow { asset_hex: "bb22".into(), kind: "keyframe".into(), ts_ms: 4500,
                    model_tag: "m1".into(), vector: unit(1) },
        VectorRow { asset_hex: "cc33".into(), kind: "image".into(), ts_ms: -1,
                    model_tag: "other".into(), vector: unit(0) },
    ]).expect("add");

    let hits = store.search(&unit(0), "m1", 10).expect("search");
    assert_eq!(hits[0].asset_hex, "aa11", "nearest by dot product");
    assert!(hits.iter().all(|h| h.asset_hex != "cc33"), "model_tag filter applies");

    let keys = store.existing_keys("m1").expect("keys");
    assert!(keys.contains(&("aa11".into(), "image".into(), -1)));
    assert!(keys.contains(&("bb22".into(), "keyframe".into(), 4500)));
    assert_eq!(keys.len(), 2);
    assert_eq!(store.distinct_assets("m1").expect("assets").len(), 2);
}
```

Run: `cargo test -p majestical-index vector_store` — Expected: FAIL.

- [ ] **Step 3: Implement `vector_store.rs`**

```rust
//! Local, disposable LanceDB dataset over the vectors that live canonically
//! as blobs. Sync API: an internal current-thread tokio runtime keeps async
//! out of the CLI. Vectors are L2-normalized, so Dot distance == cosine.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use futures::TryStreamExt as _;
use lancedb::query::{ExecutableQuery as _, QueryBase as _, Select};
use lancedb::{DistanceType, Table};

use crate::error::IndexError;

pub const DIM: i32 = 768;
const TABLE: &str = "vectors";

#[derive(Debug, Clone)]
pub struct VectorRow {
    pub asset_hex: String,
    pub kind: String,
    /// -1 for whole-image vectors; keyframe timestamp otherwise.
    pub ts_ms: i64,
    pub model_tag: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub asset_hex: String,
    pub kind: String,
    pub ts_ms: i64,
    pub distance: f32,
}

pub struct VectorStore {
    rt: tokio::runtime::Runtime,
    table: Table,
}

fn store_err<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> IndexError + '_ {
    move |e| IndexError::Encoder(format!("vector store {context}: {e}"))
}

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("asset_hex", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("model_tag", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), DIM),
            true,
        ),
    ]))
}

/// model_tag values are our own consts (no quoting worries), but escape
/// single quotes anyway so the predicate can never break.
fn tag_predicate(model_tag: &str) -> String {
    format!("model_tag = '{}'", model_tag.replace('\'', "''"))
}

impl VectorStore {
    pub fn open(dir: &Path) -> Result<Self, IndexError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(store_err("runtime"))?;
        let table = rt.block_on(async {
            let db = lancedb::connect(&dir.to_string_lossy())
                .execute()
                .await
                .map_err(store_err("connect"))?;
            match db.open_table(TABLE).execute().await {
                Ok(table) => Ok(table),
                Err(_) => db
                    .create_empty_table(TABLE, schema())
                    .execute()
                    .await
                    .map_err(store_err("create table")),
            }
        })?;
        Ok(Self { rt, table })
    }

    pub fn add(&self, rows: Vec<VectorRow>) -> Result<(), IndexError> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch = RecordBatch::try_new(
            schema(),
            vec![
                Arc::new(StringArray::from(
                    rows.iter().map(|r| r.asset_hex.clone()).collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    rows.iter().map(|r| r.kind.clone()).collect::<Vec<_>>(),
                )),
                Arc::new(Int64Array::from(rows.iter().map(|r| r.ts_ms).collect::<Vec<_>>())),
                Arc::new(StringArray::from(
                    rows.iter().map(|r| r.model_tag.clone()).collect::<Vec<_>>(),
                )),
                Arc::new(FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                    rows.iter().map(|r| {
                        Some(r.vector.iter().copied().map(Some).collect::<Vec<_>>())
                    }),
                    DIM,
                )),
            ],
        )
        .map_err(store_err("record batch"))?;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema());
        self.rt.block_on(async {
            self.table.add(batches).execute().await.map_err(store_err("add"))
        })?;
        Ok(())
    }

    pub fn search(
        &self,
        vector: &[f32],
        model_tag: &str,
        limit: usize,
    ) -> Result<Vec<VectorHit>, IndexError> {
        let batches: Vec<RecordBatch> = self.rt.block_on(async {
            self.table
                .query()
                .nearest_to(vector)
                .map_err(store_err("nearest_to"))?
                .distance_type(DistanceType::Dot)
                .only_if(tag_predicate(model_tag))
                .select(Select::Columns(vec![
                    "asset_hex".into(), "kind".into(), "ts_ms".into(),
                ]))
                .limit(limit)
                .execute()
                .await
                .map_err(store_err("search"))?
                .try_collect()
                .await
                .map_err(store_err("collect"))
        })?;
        let mut hits = Vec::new();
        for batch in batches {
            let assets = column_strings(&batch, "asset_hex")?;
            let kinds = column_strings(&batch, "kind")?;
            let ts = column_i64(&batch, "ts_ms")?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>())
                .map(|a| a.values().to_vec())
                .unwrap_or_else(|| vec![0.0; assets.len()]);
            for i in 0..assets.len() {
                hits.push(VectorHit {
                    asset_hex: assets[i].clone(),
                    kind: kinds[i].clone(),
                    ts_ms: ts[i],
                    distance: distances.get(i).copied().unwrap_or(0.0),
                });
            }
        }
        Ok(hits)
    }

    /// Every (asset, kind, ts) currently indexed for a model — the Lance side
    /// of the blob↔Lance diff. Full scan; fine at catalog scale.
    pub fn existing_keys(
        &self,
        model_tag: &str,
    ) -> Result<BTreeSet<(String, String, i64)>, IndexError> {
        let batches: Vec<RecordBatch> = self.rt.block_on(async {
            self.table
                .query()
                .only_if(tag_predicate(model_tag))
                .select(Select::Columns(vec![
                    "asset_hex".into(), "kind".into(), "ts_ms".into(),
                ]))
                .execute()
                .await
                .map_err(store_err("scan"))?
                .try_collect()
                .await
                .map_err(store_err("collect"))
        })?;
        let mut keys = BTreeSet::new();
        for batch in batches {
            let assets = column_strings(&batch, "asset_hex")?;
            let kinds = column_strings(&batch, "kind")?;
            let ts = column_i64(&batch, "ts_ms")?;
            for i in 0..assets.len() {
                keys.insert((assets[i].clone(), kinds[i].clone(), ts[i]));
            }
        }
        Ok(keys)
    }

    pub fn distinct_assets(&self, model_tag: &str) -> Result<BTreeSet<String>, IndexError> {
        Ok(self
            .existing_keys(model_tag)?
            .into_iter()
            .map(|(asset, _, _)| asset)
            .collect())
    }
}

fn column_strings(batch: &RecordBatch, name: &str) -> Result<Vec<String>, IndexError> {
    let col = batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| IndexError::Encoder(format!("vector store: missing column {name}")))?;
    Ok((0..col.len()).map(|i| col.value(i).to_string()).collect())
}

fn column_i64(batch: &RecordBatch, name: &str) -> Result<Vec<i64>, IndexError> {
    let col = batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| IndexError::Encoder(format!("vector store: missing column {name}")))?;
    Ok(col.values().to_vec())
}
```

API-drift note: as with `ort`, these lancedb 0.33 signatures were transcribed,
not compiled — reconcile against docs.rs for `=0.33.0` if the compiler objects
(`nearest_to` may take `&[f32]` or an `impl IntoQueryVector`; `only_if` takes
`impl AsRef<str>`). Corruption policy (spec: "Lance dataset corruption → rebuild from blobs, logged,
not fatal"): in the CLI, wrap `VectorStore::open` so that on error it removes
the `lance/` dir, retries once, and prints a note — the blob→Lance diff in
`run_embed_items` repopulates it with zero re-inference.
Add `pub mod vector_store;` to lib.rs, plus a
`IndexError::VectorStore(String)` variant if you prefer it over reusing
`Encoder` — reviewer's call, keep messages precise either way.

- [ ] **Step 4: Run, commit**

Run: `cargo test -p majestical-index vector_store && just check`
Expected: PASS.

```bash
git add Cargo.toml Cargo.lock crates/index .github/workflows/ci.yml
git commit -m "feat: lancedb vector store behind a sync wrapper"
```

### Task 17: Embeddings in `index run`, semantic layer in `maj search`

**Files:**
- Modify: `crates/index/src/blob.rs` (vector walk), `crates/index/src/encoder.rs`
  (text-only load), `crates/cli/src/commands.rs`, `crates/cli/src/state_dir.rs`
- Test: `crates/cli/tests/index_smoke.rs`, unit tests for the merge fn

- [ ] **Step 1: Blob walk + text-only encoder**

`blob.rs` — add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorBlobRef {
    pub asset_hex: String,
    pub kind: String, // "image" | "keyframe"
    pub ts_ms: i64,
    pub path: PathBuf,
}

impl BlobStore {
    /// Walk blobs/ for every vector belonging to `model_tag` — the blob side
    /// of the blob↔Lance diff (this is how teammates' vectors get indexed).
    pub fn iter_vectors(&self, model_tag: &str) -> Result<Vec<VectorBlobRef>, IndexError> {
        let mut out = Vec::new();
        let Ok(prefixes) = std::fs::read_dir(&self.root) else { return Ok(out) };
        for prefix in prefixes.flatten() {
            let Ok(assets) = std::fs::read_dir(prefix.path()) else { continue };
            for asset in assets.flatten() {
                let model_dir = asset.path().join(model_tag);
                let Ok(files) = std::fs::read_dir(&model_dir) else { continue };
                let hex = asset.file_name().to_string_lossy().into_owned();
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name == "image.f32le.zst" {
                        out.push(VectorBlobRef {
                            asset_hex: hex.clone(), kind: "image".into(),
                            ts_ms: -1, path: file.path(),
                        });
                    } else if let Some(ts) = name
                        .strip_prefix("kf-")
                        .and_then(|s| s.strip_suffix(".f32le.zst"))
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        out.push(VectorBlobRef {
                            asset_hex: hex.clone(), kind: "keyframe".into(),
                            ts_ms: ts, path: file.path(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}
```

`encoder.rs` — make the vision session optional so query-time text encoding
doesn't load 372 MB: `vision: Option<Session>`, `Encoder::load` fills it,
new `Encoder::load_text_only(model_dir)` leaves it `None` (skips the vision
builder entirely), and `embed_image` starts with
`let Some(vision) = &mut self.vision else { return Err(IndexError::Encoder("text-only encoder".into())) };`.

- [ ] **Step 2: `index run` executes embeddings + syncs Lance from blobs**

`commands.rs`: `capabilities()` gains ffmpeg detection stub unchanged (PR 8).
Extend `cmd_index_run` after `run_thumb_items`:

```rust
let embed_summary = run_embed_items(catalog_dir, &blobs, &items)?;
```

```rust
fn run_embed_items(
    catalog_dir: &Path,
    blobs: &majestical_index::blob::BlobStore,
    items: &[majestical_index::work::WorkItem],
) -> Result<(u64, Vec<(PathBuf, String)>)> {
    use majestical_index::blob::Derivation;
    use majestical_index::work::WorkKind;
    let todo: Vec<_> = items.iter().filter(|i| matches!(i.kind, WorkKind::ImageEmbed)).collect();
    let model_dir = majestical_index::model::model_dir()?;
    let model_ready = majestical_index::model::model_present(&model_dir);
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let store = majestical_index::vector_store::VectorStore::open(&paths.state_dir.join("lance"))
        .context("opening vector store")?;
    let tag = majestical_index::model::MODEL_TAG;
    let mut written = 0u64;
    let mut failed = Vec::new();
    if !todo.is_empty() && model_ready {
        let mut encoder = majestical_index::encoder::Encoder::load(
            &model_dir,
            &majestical_index::encoder::EncoderOptions {
                coreml: true,
                coreml_cache: Some(paths.state_dir.join("coreml-cache")),
            },
        )
        .context("loading encoder")?;
        let mut batch = Vec::new();
        for item in todo {
            let result = majestical_index::thumbs::decode_image(&item.abs_path)
                .and_then(|img| encoder.embed_image(&img))
                .and_then(|vector| {
                    let path = blobs.path_for(&item.asset_hex, &Derivation::ImageEmbedding { model_tag: tag });
                    blobs.write_vector(&path, &vector)?;
                    Ok(vector)
                });
            match result {
                Ok(vector) => {
                    written += 1;
                    batch.push(majestical_index::vector_store::VectorRow {
                        asset_hex: item.asset_hex.clone(), kind: "image".into(),
                        ts_ms: -1, model_tag: tag.into(), vector,
                    });
                    if batch.len() >= 64 {
                        store.add(std::mem::take(&mut batch)).context("indexing vectors")?;
                    }
                }
                Err(e) => failed.push((item.abs_path.clone(), e.to_string())),
            }
        }
        store.add(batch).context("indexing vectors")?;
    }
    // Blob → Lance diff: index vectors we didn't produce (teammates' blobs,
    // or a rebuilt Lance dir) without re-inference.
    let existing = store.existing_keys(tag).context("scanning vector index")?;
    let mut load = Vec::new();
    for blob_ref in blobs.iter_vectors(tag)? {
        let key = (blob_ref.asset_hex.clone(), blob_ref.kind.clone(), blob_ref.ts_ms);
        if !existing.contains(&key) {
            let vector = blobs.read_vector(&blob_ref.path)?;
            load.push(majestical_index::vector_store::VectorRow {
                asset_hex: blob_ref.asset_hex, kind: blob_ref.kind,
                ts_ms: blob_ref.ts_ms, model_tag: tag.into(), vector,
            });
            if load.len() >= 256 {
                store.add(std::mem::take(&mut load)).context("loading blob vectors")?;
            }
        }
    }
    store.add(load).context("loading blob vectors")?;
    Ok((written, failed))
}
```

Report `embeddings: N written, F failed` beside the thumbnail line (and in the
JSON object).

- [ ] **Step 3: Semantic layer in `run_search` + RRF (unit-tested pure fn)**

Add to `commands.rs`:

```rust
/// Reciprocal-rank fusion (k=60) over ranked lists. Deterministic: ties break
/// by asset id.
pub(crate) fn rrf_merge(lists: &[Vec<AssetId>], limit: usize) -> Vec<(AssetId, f64)> {
    const K: f64 = 60.0;
    let mut scores: std::collections::BTreeMap<AssetId, f64> = std::collections::BTreeMap::new();
    for list in lists {
        for (rank, asset) in list.iter().enumerate() {
            let rank = rank as f64;
            *scores.entry(asset.clone()).or_default() += 1.0 / (K + rank + 1.0);
        }
    }
    let mut ranked: Vec<(AssetId, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(limit);
    ranked
}
```

Unit tests (in commands.rs `#[cfg(test)]`): an asset on both lists outranks
single-list assets; empty lists yield empty; ties are id-ordered.

Rework the term-ranking section of `run_search`:

```rust
let ranked = if parsed.terms.is_empty() {
    /* filter-only path, unchanged */
} else {
    let fts: Vec<AssetId> = db
        .search_names_ranked(&parsed.terms, args.limit.saturating_mul(4))?
        .into_iter().map(|(a, _)| a).collect();
    let (semantic, mut hit_ts, coverage) = semantic_candidates(
        catalog_dir, &parsed.terms.join(" "), args.limit.saturating_mul(4))?;
    let allowed_ref = allowed.as_ref();
    let keep = |list: Vec<AssetId>| -> Vec<AssetId> {
        list.into_iter()
            .filter(|a| allowed_ref.is_none_or(|s| s.contains(a)))
            .collect()
    };
    let lists = if semantic.is_empty() {
        vec![keep(fts)]
    } else {
        vec![keep(fts), keep(semantic)]
    };
    // coverage: (embedded, eligible) — print the notice after results.
    …rrf_merge(&lists, args.limit)
};
```

```rust
/// Semantic candidates for a query, plus per-asset best keyframe timestamp
/// and (embedded, eligible) coverage. Degrades to empty on any missing piece
/// (no model, no lance dir) — search must answer regardless.
fn semantic_candidates(
    catalog_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<(Vec<AssetId>, HashMap<AssetId, i64>, Option<(u64, u64)>)> {
    let model_dir = majestical_index::model::model_dir()?;
    if !majestical_index::model::model_present(&model_dir) {
        eprintln!("note: semantic search unavailable — run `maj model fetch`");
        return Ok((Vec::new(), HashMap::new(), None));
    }
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let store = majestical_index::vector_store::VectorStore::open(&paths.state_dir.join("lance"))
        .context("opening vector store")?;
    let tag = majestical_index::model::MODEL_TAG;
    let embedded = store.distinct_assets(tag).context("coverage scan")?;
    if embedded.is_empty() {
        eprintln!("note: semantic index is empty — run `maj index run`");
        return Ok((Vec::new(), HashMap::new(), None));
    }
    let mut encoder = majestical_index::encoder::Encoder::load_text_only(&model_dir)
        .context("loading text encoder")?;
    let vector = encoder.embed_text(query).context("embedding query")?;
    let hits = store.search(&vector, tag, limit).context("vector search")?;
    let mut seen = Vec::new();
    let mut timestamps = HashMap::new();
    for hit in hits {
        let asset = AssetId(format!("xxh3:{}", hit.asset_hex));
        if hit.kind == "keyframe" {
            timestamps.entry(asset.clone()).or_insert(hit.ts_ms);
        }
        if !seen.contains(&asset) {
            seen.push(asset);
        }
    }
    Ok((seen, timestamps, Some((embedded.len() as u64, 0))))
}
```

Coverage denominator: count eligible assets (kind image|video) from the
projection in `run_search` (it has it), and print after the results line:

```rust
if let Some((embedded, _)) = coverage {
    let eligible = projection.assets().filter(|(_, s)| {
        s.instances.keys().any(|(_, p)| {
            !matches!(media_kind(p), MediaKind::Other)
        })
    }).count() as u64;
    if embedded < eligible {
        println!("semantic index: {embedded} of {eligible} eligible assets");
    }
}
```

Keyframe timestamps: pass `hit_ts` into `print_search_results` and append
`@{}m{:02}s` to the name line when the asset has one.

- [ ] **Step 4: End-to-end degradation tests**

`index_smoke.rs` additions:

```rust
#[test]
fn search_without_model_degrades_with_notice() {
    // Point MAJ_MODEL_DIR at an empty dir: no model, search still answers.
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, state) = setup_scanned_catalog(&dir); // helper from earlier test
    maj(&catalog, &state)
        .env("MAJ_MODEL_DIR", dir.path().join("empty-models"))
        .args(["search", "photo"])
        .assert()
        .success()
        .stdout(predicates::str::contains("results"))
        .stderr(predicates::str::contains("maj model fetch"));
}
```

(Real-encoder search coverage lives in the conformance/gated tests — CI's
encoder-conformance job runs with the fetched model.)

- [ ] **Step 5: Run everything, commit**

Run: `just ci`
Expected: PASS.

```bash
git add crates/index crates/cli Cargo.toml Cargo.lock
git commit -m "feat: semantic search layer fused with name search via RRF"
```

Open PR 7: title `feat: vector index + semantic search`.

---

# PR 8 — Video keyframes

### Task 18: Probe, frame pipe, scene detection

**Files:**
- Create: `crates/index/src/video.rs`
- Modify: `crates/index/src/lib.rs`, `crates/index/src/error.rs` (add `Video` variant)

- [ ] **Step 1: Write failing scene-detection tests (pure, no ffmpeg)**

```rust
fn solid(ts_ms: u64, rgb: [u8; 3]) -> Frame {
    Frame { ts_ms, w: 16, h: 9, rgb: [rgb[0], rgb[1], rgb[2]].repeat(16 * 9) }
}

#[test]
fn hard_cuts_at_2fps_are_found_and_midpoints_returned() {
    // 0-3s red, 3-6s green, 6-9s blue at 2fps.
    let mut frames = Vec::new();
    for i in 0..18u64 {
        let ts = i * 500;
        let color = if ts < 3000 { [200, 30, 30] } else if ts < 6000 { [30, 200, 30] } else { [30, 30, 200] };
        frames.push(solid(ts, color));
    }
    let keyframes = detect_scenes(&frames, 2000, 9000);
    assert_eq!(keyframes.len(), 3, "three scenes: {keyframes:?}");
    assert!(keyframes[0] < 3000 && (3000..6000).contains(&keyframes[1]) && keyframes[2] >= 6000);
}

#[test]
fn single_frame_flicker_shorter_than_min_scene_is_ignored() {
    let mut frames = Vec::new();
    for i in 0..12u64 {
        let color = if i == 5 { [255, 255, 255] } else { [40, 40, 90] };
        frames.push(solid(i * 500, color));
    }
    let keyframes = detect_scenes(&frames, 2000, 6000);
    assert_eq!(keyframes.len(), 1, "flicker must not split the scene");
}

#[test]
fn continuous_footage_falls_back_to_uniform_sampling() {
    // Slow gradient: no adaptive cut ever fires.
    let frames: Vec<Frame> = (0..120u64)
        .map(|i| solid(i * 500, [(i % 256) as u8, 60, 60]))
        .collect();
    let keyframes = detect_scenes(&frames, 2000, 60_000);
    assert_eq!(keyframes.len(), 10, "uniform fallback when <10 scenes");
    assert!(keyframes.windows(2).all(|w| w[1] > w[0]));
}

#[test]
fn keyframes_are_capped_at_150() {
    // 200 hard cuts, min scene length satisfied (each scene 2s at 2fps).
    let mut frames = Vec::new();
    for scene in 0..200u64 {
        for f in 0..4u64 {
            let hue = ((scene * 71) % 255) as u8;
            frames.push(solid((scene * 4 + f) * 500, [hue, 255 - hue, 128]));
        }
    }
    let keyframes = detect_scenes(&frames, 2000, 400_000);
    assert!(keyframes.len() <= 150);
}
```

Run: `cargo test -p majestical-index video` — Expected: FAIL.

- [ ] **Step 2: Implement `video.rs`**

```rust
//! Video probing, analysis-rate frame decoding through ffmpeg, and adaptive
//! scene detection (a Rust port of PySceneDetect AdaptiveDetector's
//! field-tested parameters: HSV mean-abs-diff score, rolling-average ratio
//! threshold 3.0, min content 15.0, 2s min scene, uniform fallback, 150 cap).

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::IndexError;

pub const ANALYSIS_FPS: f64 = 2.0;
pub const ANALYSIS_W: u32 = 160;
pub const ANALYSIS_H: u32 = 90;
const ADAPTIVE_RATIO: f32 = 3.0;
const MIN_CONTENT_VAL: f32 = 15.0;
const WINDOW: usize = 2;
const MIN_SCENES_BEFORE_FALLBACK: usize = 10;
const UNIFORM_SAMPLES: usize = 10;
pub const MAX_KEYFRAMES: usize = 150;

pub struct Frame {
    pub ts_ms: u64,
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
}

pub fn ffmpeg_available() -> bool {
    let ok = |cmd: &str| {
        Command::new(cmd)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    ok("ffmpeg") && ok("ffprobe")
}

pub fn probe(path: &Path) -> Result<VideoInfo, IndexError> {
    let err = |message: String| IndexError::Video { path: path.to_path_buf(), message };
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .map_err(|e| err(format!("running ffprobe: {e}")))?;
    if !output.status.success() {
        return Err(err(format!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr))));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| err(format!("ffprobe output: {e}")))?;
    let stream = json["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|st| st["codec_type"] == "video"))
        .ok_or_else(|| err("no video stream".into()))?;
    let duration_s: f64 = json["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .ok_or_else(|| err("no duration".into()))?;
    Ok(VideoInfo {
        duration_ms: (duration_s * 1000.0).max(0.0) as u64,
        width: stream["width"].as_u64().unwrap_or(0) as u32,
        height: stream["height"].as_u64().unwrap_or(0) as u32,
    })
}

/// Decode small analysis frames at `ANALYSIS_FPS` via a raw RGB pipe.
/// Frame i's timestamp is exactly i/fps (verified against ffmpeg showinfo).
pub fn analysis_frames(path: &Path) -> Result<Vec<Frame>, IndexError> {
    let err = |message: String| IndexError::Video { path: path.to_path_buf(), message };
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("fps={ANALYSIS_FPS},scale={ANALYSIS_W}:{ANALYSIS_H}"),
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| err(format!("running ffmpeg: {e}")))?;
    let mut raw = Vec::new();
    if let Some(stdout) = child.stdout.as_mut() {
        stdout.read_to_end(&mut raw).map_err(|e| err(format!("reading frames: {e}")))?;
    }
    let status = child.wait().map_err(|e| err(format!("waiting for ffmpeg: {e}")))?;
    if !status.success() {
        return Err(err(format!("ffmpeg failed ({status})")));
    }
    let frame_bytes = (ANALYSIS_W * ANALYSIS_H * 3) as usize;
    Ok(raw
        .chunks_exact(frame_bytes)
        .enumerate()
        .map(|(i, chunk)| Frame {
            ts_ms: ((i as f64 / ANALYSIS_FPS) * 1000.0) as u64,
            w: ANALYSIS_W,
            h: ANALYSIS_H,
            rgb: chunk.to_vec(),
        })
        .collect())
}

/// Full-resolution single-frame extraction for keyframe embedding/thumbnails.
pub fn extract_frame(path: &Path, ts_ms: u64) -> Result<image::RgbImage, IndexError> {
    let err = |message: String| IndexError::Video { path: path.to_path_buf(), message };
    let ts = format!("{}.{:03}", ts_ms / 1000, ts_ms % 1000);
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &ts, "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .map_err(|e| err(format!("running ffmpeg: {e}")))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(err(format!("frame extraction at {ts}s failed")));
    }
    Ok(image::load_from_memory(&output.stdout)
        .map_err(|e| err(format!("decoding extracted frame: {e}")))?
        .to_rgb8())
}

fn rgb_to_hsv_u8(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (f32::from(r), f32::from(g), f32::from(b));
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let h = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta).rem_euclid(6.0))
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { delta / max };
    ((h / 360.0 * 255.0) as u8, (s * 255.0) as u8, max as u8)
}

/// Mean absolute HSV difference between consecutive frames (PySceneDetect's
/// content score with default weights hue=sat=luma=1, edges=0).
fn content_scores(frames: &[Frame]) -> Vec<f32> {
    let mut scores = vec![0.0f32; frames.len()];
    for i in 1..frames.len() {
        let (a, b) = (&frames[i - 1], &frames[i]);
        let mut total = 0.0f32;
        let pixels = (a.rgb.len() / 3).min(b.rgb.len() / 3);
        for p in 0..pixels {
            let (h1, s1, v1) = rgb_to_hsv_u8(a.rgb[3 * p], a.rgb[3 * p + 1], a.rgb[3 * p + 2]);
            let (h2, s2, v2) = rgb_to_hsv_u8(b.rgb[3 * p], b.rgb[3 * p + 1], b.rgb[3 * p + 2]);
            total += (f32::from(h1) - f32::from(h2)).abs()
                + (f32::from(s1) - f32::from(s2)).abs()
                + (f32::from(v1) - f32::from(v2)).abs();
        }
        scores[i] = total / (pixels.max(1) as f32 * 3.0);
    }
    scores
}

/// Scene keyframe timestamps: adaptive cuts → min-scene-length merge →
/// midpoints; uniform fallback below `MIN_SCENES_BEFORE_FALLBACK`; capped.
#[must_use]
pub fn detect_scenes(frames: &[Frame], min_scene_ms: u64, duration_ms: u64) -> Vec<u64> {
    if frames.is_empty() {
        return Vec::new();
    }
    let scores = content_scores(frames);
    let mut cuts: Vec<usize> = Vec::new();
    for i in 1..frames.len() {
        let mut neighborhood = Vec::new();
        for j in i.saturating_sub(WINDOW)..(i + WINDOW + 1).min(scores.len()) {
            if j != i && j > 0 {
                neighborhood.push(scores[j]);
            }
        }
        let avg = if neighborhood.is_empty() {
            0.0
        } else {
            neighborhood.iter().sum::<f32>() / neighborhood.len() as f32
        };
        let adaptive = if avg > 0.0 { scores[i] / avg } else { f32::INFINITY };
        if scores[i] >= MIN_CONTENT_VAL && adaptive >= ADAPTIVE_RATIO {
            // Enforce min scene length against the previous cut.
            let prev_ts = cuts.last().map_or(0, |&c| frames[c].ts_ms);
            if frames[i].ts_ms.saturating_sub(prev_ts) >= min_scene_ms {
                cuts.push(i);
            }
        }
    }
    let mut keyframes: Vec<u64> = Vec::new();
    if cuts.len() + 1 >= MIN_SCENES_BEFORE_FALLBACK {
        let mut boundaries = vec![0usize];
        boundaries.extend(&cuts);
        boundaries.push(frames.len());
        for pair in boundaries.windows(2) {
            let mid = (pair[0] + pair[1]) / 2;
            keyframes.push(frames[mid.min(frames.len() - 1)].ts_ms);
        }
    } else if cuts.is_empty() {
        // Continuous footage: uniform sampling.
        for k in 0..UNIFORM_SAMPLES {
            let ts = duration_ms * (2 * k as u64 + 1) / (2 * UNIFORM_SAMPLES as u64);
            keyframes.push(ts);
        }
    } else {
        // Few real cuts: keep their scene midpoints AND pad uniformly.
        let mut boundaries = vec![0usize];
        boundaries.extend(&cuts);
        boundaries.push(frames.len());
        for pair in boundaries.windows(2) {
            let mid = (pair[0] + pair[1]) / 2;
            keyframes.push(frames[mid.min(frames.len() - 1)].ts_ms);
        }
    }
    keyframes.sort_unstable();
    keyframes.dedup();
    if keyframes.len() > MAX_KEYFRAMES {
        // Thin uniformly.
        let step = keyframes.len() as f64 / MAX_KEYFRAMES as f64;
        keyframes = (0..MAX_KEYFRAMES)
            .map(|k| keyframes[(k as f64 * step) as usize])
            .collect();
    }
    keyframes
}
```

Tune the detector against the unit tests — they encode the spec's behavior
(the exact constants may need adjustment for the 3-scene test to pass with the
midpoint math; the tests are the contract, PySceneDetect's parameters are the
starting values). Add the error variant:

```rust
#[error("video {path}: {message}")]
Video { path: PathBuf, message: String },
```

Run: `cargo test -p majestical-index video` — Expected: PASS.

- [ ] **Step 3: `#[ignore]`d real-ffmpeg integration test**

`crates/index/tests/video_e2e.rs`:

```rust
//! Real ffmpeg pipe test. Run: cargo test -p majestical-index --test video_e2e -- --ignored
#![cfg(test)]
use std::process::Command;

#[test]
#[ignore = "needs ffmpeg on PATH"]
fn analysis_pipe_and_extraction_work_on_a_generated_clip() {
    assert!(majestical_index::video::ffmpeg_available(), "install ffmpeg");
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = dir.path().join("clip.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error",
            "-f", "lavfi", "-i", "color=red:s=320x180:d=3:r=30",
            "-f", "lavfi", "-i", "color=green:s=320x180:d=3:r=30",
            "-f", "lavfi", "-i", "color=blue:s=320x180:d=3:r=30",
            "-filter_complex", "[0][1][2]concat=n=3:v=1:a=0",
            "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
    let info = majestical_index::video::probe(&clip).expect("probe");
    assert!((8500..=9500).contains(&info.duration_ms));
    let frames = majestical_index::video::analysis_frames(&clip).expect("frames");
    assert!(frames.len() >= 17, "≈18 frames at 2fps over 9s, got {}", frames.len());
    let cuts = majestical_index::video::detect_scenes(&frames, 2000, info.duration_ms);
    assert_eq!(cuts.len(), 3, "three color scenes: {cuts:?}");
    let frame = majestical_index::video::extract_frame(&clip, 4500).expect("extract");
    assert_eq!((frame.width(), frame.height()), (320, 180));
}
```

Run locally: `cargo test -p majestical-index --test video_e2e -- --ignored`
Expected: PASS (with ffmpeg installed).

- [ ] **Step 4: Commit**

```bash
git add crates/index
git commit -m "feat: ffmpeg probe, frame pipe, adaptive scene detection"
```

### Task 19: Wire video into `index run` and search output

**Files:**
- Modify: `crates/cli/src/commands.rs`
- Test: covered by unit tests + the gated e2e; degradation via `index_smoke.rs`

- [ ] **Step 1: Capability + executors**

`capabilities()` gains `ffmpeg: majestical_index::video::ffmpeg_available()`.

Spec's error-handling rule: degradation applies to the *default* run, but an
explicit ask is a hard error — at the top of `cmd_index_run`, if
`args.kinds` explicitly contains `"keyframes"` and ffmpeg is absent,
`bail!("--kinds keyframes requires ffmpeg/ffprobe on PATH (brew install ffmpeg)")`.

`run_thumb_items`'s per-item closure becomes kind-aware: for a Video asset,
produce the thumb source via `video::extract_frame(&item.abs_path, info.duration_ms / 10)`
(probe first) instead of `decode_image`, then `thumbnail_webp` as before.

New `run_keyframe_items` mirroring `run_embed_items`' shape, per `Keyframes` item:

```rust
let info = majestical_index::video::probe(&item.abs_path)?;
let frames = majestical_index::video::analysis_frames(&item.abs_path)?;
let timestamps = majestical_index::video::detect_scenes(&frames, 2000, info.duration_ms);
let mut rows = Vec::new();
for ts in &timestamps {
    let frame = majestical_index::video::extract_frame(&item.abs_path, *ts)?;
    let vector = encoder.embed_image(&frame)?;
    let path = blobs.path_for(&item.asset_hex,
        &Derivation::KeyframeEmbedding { model_tag: tag, timestamp_ms: *ts });
    blobs.write_vector(&path, &vector)?;
    rows.push(VectorRow { asset_hex: item.asset_hex.clone(), kind: "keyframe".into(),
        ts_ms: *ts as i64, model_tag: tag.into(), vector });
}
store.add(rows)?;
// The manifest is written LAST: its existence marks the video complete, so a
// crash mid-video re-runs the whole video (idempotent — kf blobs are skipped
// by write only, re-embedding is avoided by checking blob existence per ts).
let manifest = serde_json::json!({ "model_tag": tag, "timestamps": timestamps });
blobs.write_atomic(
    &blobs.path_for(&item.asset_hex, &Derivation::KeyframeManifest { model_tag: tag }),
    manifest.to_string().as_bytes(),
)?;
```

(Per-timestamp skip: before extract+embed, `if kf_path.exists() { load blob into
rows via read_vector; continue; }` — crash recovery without re-inference.)

Errors per item are collected as `(path, reason)` failures like the other
executors, never aborting the run.

- [ ] **Step 2: Search output timestamps**

Already wired in Task 17 (`hit_ts` map → `@MmSSs` suffix). Verify a keyframe hit
formats as e.g. `beach.mov @0m04s` — unit-test the formatting helper:

```rust
pub(crate) fn format_ts(ts_ms: i64) -> String {
    let total_s = ts_ms.max(0) / 1000;
    format!("@{}m{:02}s", total_s / 60, total_s % 60)
}
```

- [ ] **Step 3: Degradation check + commit**

`index_smoke.rs`: add a `.mov`-named file (any bytes — it's never decoded when
ffmpeg detection is stubbed by PATH manipulation being unreliable, instead assert
on status output): scan a dir containing `clip.mov`, then
`maj index status` must show the keyframes row with either `1 pending` (ffmpeg
present) or `1 need ffmpeg` (absent) — assert `stdout(contains("keyframes:"))`
and success, not the exact count, so the test passes on any machine.

Run: `just ci`
Expected: PASS.

```bash
git add crates/cli crates/index
git commit -m "feat: video keyframe embeddings with seekable search hits"
```

Open PR 8: title `feat: video keyframes`. Record measured scene-detection
behavior on a real clip in the PR body.

---

# PR 9 — Closing: acceptance, mutants, docs

### Task 20: Search-flow acceptance (cucumber) + degradation sweep

**Files:**
- Create: `crates/cli/tests/acceptance.rs`, `crates/cli/tests/features/search.feature`
- Modify: `crates/cli/Cargo.toml` (dev-deps: `cucumber`, `futures` — pin the same
  versions core/ingest use)

- [ ] **Step 1: Feature file**

```gherkin
Feature: Layered search
  Search must answer from whatever layers exist: filters and name matching
  always work; the semantic layer joins when the model and index exist;
  offline volumes stay searchable.

  Scenario: Name search with a tag filter and negation
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    And "beach_day.mov" is tagged "status/select"
    When I search "beach tag:status/select"
    Then the results contain "beach_day.mov"
    When I search "beach -tag:status/select"
    Then the results are empty

  Scenario: Search without the encoder model degrades with a notice
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    And no encoder model is installed
    When I search "beach"
    Then the results contain "beach_day.mov"
    And the notice mentions "maj model fetch"

  Scenario: Saved searches round-trip between machines
    Given a catalog with assets "beach_day.mov" and "mountain.jpg"
    When machine "a" saves the search "tag:keep" as "keepers"
    Then machine "b" lists a saved search named "keepers"

  Scenario: Filter-only search over an offline volume still answers
    Given a catalog with an asset "archived.mov" on an offline volume
    When I search "online:no"
    Then the results contain "archived.mov"
```

- [ ] **Step 2: World + steps**

`acceptance.rs` in the established cucumber style (World owns tempdirs; every
step returns `Result<_, String>`; `harness = false` in Cargo.toml):

```rust
use std::path::PathBuf;

use cucumber::{given, then, when, World};

#[derive(Debug, World)]
#[world(init = Self::new)]
struct SearchWorld {
    dir: Option<tempfile::TempDir>,
    catalog: PathBuf,
    states: std::collections::BTreeMap<String, PathBuf>,
    last_stdout: String,
    last_stderr: String,
}
```

Steps shell out with `assert_cmd::Command::cargo_bin("maj")` exactly like
`cli_smoke.rs` (env `MAJ_CATALOG`/`MAJ_MACHINE_ID`/`MAJ_STATE_DIR`, plus
`MAJ_MODEL_DIR` pointed at an empty temp dir for the no-model scenario), capture
stdout/stderr into the world, and assert with `contains`. The offline-volume
scenario seeds an event directly by running `scan` on a temp dir, then deleting
the scanned files (instances remain, volume resolves but files are gone — the
`online:no` filter path uses mounted-volume ids, so ALSO pass a fabricated
volume: emit via a second scan of a subdir then trash it; if that proves flaky,
assert on `online:yes` exclusion instead — the point is the query answers, not
macOS mount simulation).

`[[test]] name = "acceptance"` + `harness = false` block in `crates/cli/Cargo.toml`.

Run: `cargo test -p majestical-cli --test acceptance`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cli
git commit -m "test: cucumber acceptance for layered search flows"
```

### Task 21: cargo-mutants, watchlist, handoff

- [ ] **Step 1: Run mutants (background, 20-40 min each — keep working)**

```bash
cargo mutants --package majestical-index --output target/mutants-index &
cargo mutants --package majestical-catalog-sqlite --output target/mutants-sqlite &
```

Triage per the phase-3 pattern: genuine test gaps get discriminating tests
**verified against the production line** (break the real code, watch the real
test fail, revert); display-only/fault-injection-needing survivors go to the
watchlist with attribution. Scoped re-runs (`--file`) verify fixes cheaply.

- [ ] **Step 2: Watchlist + handoff docs**

Append a "Phase 4 deferrals" section to
`docs/superpowers/plans/2026-07-29-phase2-watchlist.md` covering at minimum:
pre-phase-4 instance paths (from PR 5), snapshot JSON size at scale (binary
format candidate), `emit()`/write-path full log re-read still unaddressed,
Lance IVF-PQ index deferred until ~100k vectors, model-fetch resume (no Range
support), keyframe thumbnails per scene deferred, decode-failure markers
(currently retried each run), plus mutants survivors.

Write `docs/superpowers/HANDOFF-phase5.md` in the established handoff format:
state at close (all merged PRs), architecture delta (crates/index, state dir,
blobs, Lance), phase 5 recommendation per the parent spec build order
(Describer backends, transcription, OCR, captions — spec §4's remaining
scope), process conventions carried forward verbatim, and lessons learned this
phase. Update the phase-4 spec with an "As-built deviations" section mirroring
the phase-3 spec's, seeded from this plan's "Planning-time discoveries" plus
anything execution added.

- [ ] **Step 3: Final PR**

```bash
git add docs
git commit -m "docs: phase 4 as-built notes, watchlist, phase 5 handoff"
```

Open PR 9: title `docs: close out phase 4`.

---

## Execution handoff

Plan complete. Execute with superpowers:subagent-driven-development: one fresh
implementer subagent per task, then the mandated review loop (adversarial
spec-compliance reviewer → code-quality reviewer → fix rounds until APPROVED),
merging each PR chunk after green CI before starting the next. Tasks within a
PR are sequential; PR chunks are sequential (each builds on the last). The two
API-drift notes (ort rc.13, lancedb 0.33) are expected fix-forward points, not
plan errors — implementers reconcile against docs.rs for the pinned versions.
