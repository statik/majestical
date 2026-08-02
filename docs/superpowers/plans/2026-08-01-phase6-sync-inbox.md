# Phase 6: Multi-location Sync + Inbox Contributions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `maj sync location|push|pull|status` (git-remote model, stateless set-union sync of event segments + derivation blobs) and `maj inbox process` (validated `contribution.json` ingest with provenance tags).

**Architecture:** The transfer engine lives in `crates/sync` (new `transfer.rs`) operating on two plain filesystem roots — no `SyncTransport` port this phase. Segments sync longer-wins whole-file via temp+rename; blobs sync by path presence, priority-ordered. CLI orchestration in new `crates/cli/src/sync_cmd.rs` and `inbox_cmd.rs`. Inbox reuses the existing verified-ingest pipeline via a small refactor of `cmd_ingest`.

**Tech Stack:** Rust workspace edition 2024, strict clippy (`unwrap_used` denied — use `expect` only in tests), thiserror in crates, anyhow in CLI, clap, serde/toml, proptest, cucumber + assert_cmd for acceptance.

**Spec:** `docs/superpowers/specs/2026-08-01-phase6-sync-design.md` — read it first.

**Process conventions (mandatory):**
- TDD every task: failing test → verify fail → implement → verify pass → commit.
- Stage ONLY your files, never `git add -A`. No Claude-Session trailers. Never push to main.
- `just check` runs fmt + clippy (also the prek hook). `cargo test -p <crate>` for the crate you touched.
- PR chunks (squash-merge after green CI): Tasks 1-2 = PR1, Task 3 = PR2, Tasks 4-5 = PR3, Task 6 = PR4, Tasks 7-8 = PR5, Tasks 9-10 = PR6, Tasks 11-12 = PR7, Task 13 = PR8.

## File structure

- `crates/sync/src/lib.rs` — modify: `LogError::io` constructor, unified read walk, segment rotation + `SegmentOverflow`, `list_segments` stays private but is reused by the new module.
- `crates/sync/src/transfer.rs` — create: blob classes, transfer plan/execute, `TransferError`. Declared `pub mod transfer;` in lib.rs.
- `crates/sync/tests/convergence.rs` — create: proptest convergence property.
- `crates/cli/src/sync_cmd.rs` — create: `SyncConfig` (sync.toml), location add/list/rm, push, pull, status.
- `crates/cli/src/inbox_cmd.rs` — create: manifest types + validation, `cmd_inbox_process`, failure markers.
- `crates/cli/src/commands.rs` — modify: extract `run_ingest` (returns `engine::Outcome`) from `cmd_ingest`.
- `crates/cli/src/main.rs` — modify: `Sync` + `Inbox` subcommands, dispatchers.
- `crates/cli/tests/sync_smoke.rs` — create: shuttle e2e + sabotage probes through the real binary.
- `crates/cli/tests/inbox_smoke.rs` — create: inbox e2e with real MHL.
- `crates/cli/tests/inbox_acceptance.rs` + `crates/cli/tests/features/inbox.feature` — create: cucumber flows.

---

### Task 1: `LogError::io` constructor + unified read walk

**Files:**
- Modify: `crates/sync/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/sync/src/lib.rs`:

```rust
#[test]
fn read_all_reports_non_utf8_line_and_keeps_reading() {
    // Previously read_all_reporting used fs::read_to_string, so one bad
    // byte failed the WHOLE segment. Unified with the read_since walk it
    // must degrade per line instead.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
    log.append(&[ev(1)]).expect("append");
    let seg = dir.path().join("events/m1/0001.jsonl");
    let mut bytes = std::fs::read(&seg).expect("read seg");
    bytes.extend_from_slice(&[0xFF, 0xFE, b'\n']);
    std::fs::write(&seg, bytes).expect("write");
    let mut bad = 0;
    let all = log.read_all_reporting(|_| bad += 1).expect("read must not fail");
    assert_eq!((all.len(), bad), (1, 1));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p majestical-sync read_all_reports_non_utf8 -- --nocapture`
Expected: FAIL — `read must not fail` panics with a `LogError::Io` (read_to_string chokes on invalid UTF-8).

- [ ] **Step 3: Implement**

In `crates/sync/src/lib.rs`:

1. Add the constructor to `impl LogError` (new impl block after the enum):

```rust
impl LogError {
    /// `map_err(LogError::io(&path))` — replaces the hand-built closure at
    /// every I/O call site.
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}
```

2. Replace every `map_err(|source| LogError::Io { path: X.clone()-or-to_path_buf(), source })` in the file with `.map_err(LogError::io(&X))` (13 sites: `init`, `open`, `append` ×2, `read_all_reporting` ×2 until deleted below, `list_segments` ×3, `read_segment_since` ×3, `read_since_reporting` ×3). Where the borrowed path is `machine.path()` or `entry.path()` (a temporary), bind it first: `let path = entry.path();` then `.map_err(LogError::io(&path))`.

3. Replace the body of `read_all_reporting` — it becomes the read-since walk from zero:

```rust
    /// Corrupt lines are skipped and reported, never fatal: one bad byte
    /// on a shuttle drive must not take down the whole catalog. Shares the
    /// segment walk with [`Self::read_since_reporting`] (this is that read
    /// with empty cursors, cursors discarded), so the two paths can no
    /// longer diverge in walk order or UTF-8 handling. A torn tail (a
    /// write in progress) is left unread rather than reported — it parses
    /// on the next read once the write completes.
    ///
    /// Returned order is grouped by machine (directory iteration order,
    /// which is unspecified), with segments sorted within each machine —
    /// there is no global HLC order. Callers must not assume one; the CRDT
    /// projection this feeds is order-independent by design.
    ///
    /// # Errors
    /// Returns [`LogError::Io`] if the events directory or a machine's
    /// segments can't be read.
    pub fn read_all_reporting(
        &self,
        on_bad_line: impl FnMut(&str),
    ) -> Result<Vec<Event>, LogError> {
        let (events, _cursors) = self.read_since_reporting(&[], on_bad_line)?;
        Ok(events)
    }
```

4. Delete the module-level `//! Known divergence:` paragraph (lines 5-10) — it no longer exists. Update `append`'s doc comment sentence about the two readers: both now defer the torn tail.

- [ ] **Step 4: Run the whole crate's tests**

Run: `cargo test -p majestical-sync`
Expected: PASS, including the pre-existing `corrupt_line_is_skipped_and_reported` (a complete corrupt line is still reported) and `a_torn_tail_is_not_consumed_until_completed`.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/sync/src/lib.rs
git commit -m "refactor: unify sync read paths and add LogError::io constructor"
```

---

### Task 2: Segment rotation + overflow error

**Files:**
- Modify: `crates/sync/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn append_rotates_past_the_size_threshold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    log.append(&[ev(1)]).expect("append");
    // Grow 0001.jsonl past the threshold without writing 4MiB of real
    // events: pad with newlines, which every reader skips as empty lines.
    let seg = dir.path().join("events/m1/0001.jsonl");
    let f = std::fs::OpenOptions::new().append(true).open(&seg).expect("open");
    f.set_len(ROTATE_BYTES + 1).expect("grow");
    log.append(&[ev(2)]).expect("append after threshold");
    assert!(
        dir.path().join("events/m1/0002.jsonl").is_file(),
        "append past the threshold must start 0002.jsonl"
    );
    let mut bad = 0;
    let all = log.read_all_reporting(|_| bad += 1).expect("read");
    assert_eq!((all.len(), bad), (2, 0), "events merge across both segments");
}

#[test]
fn segment_overflow_is_a_hard_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = MachineId("m1".into());
    let mut log = FileEventLog::init(dir.path(), &m).expect("init");
    let seg = dir.path().join("events/m1/9999.jsonl");
    let f = std::fs::File::create(&seg).expect("create");
    f.set_len(ROTATE_BYTES + 1).expect("grow");
    assert!(matches!(
        log.append(&[ev(1)]),
        Err(LogError::SegmentOverflow { .. })
    ));
}
```

Note: `set_len` pads with NUL bytes, not newlines. NUL-padded regions form one long line that reports as ONE bad line on read. Amend the first test's expectation: the padded region parses as a single bad line, so assert `(all.len(), bad) == (2, 1)` and name why in the assert message: `"the NUL padding reads as one reported bad line"`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p majestical-sync rotat overflow`
Expected: COMPILE FAIL — `ROTATE_BYTES` and `SegmentOverflow` don't exist.

- [ ] **Step 3: Implement**

In `crates/sync/src/lib.rs`:

1. Constants (module level, under the imports) — `pub(crate)` so tests and `transfer.rs` can see them:

```rust
/// Rotation threshold: an append that would grow the active segment past
/// this starts the next `NNNN.jsonl` instead, bounding the whole-file
/// re-copy cost of `maj sync push` (segments transfer longer-wins as whole
/// files). Rotated segments are immutable thereafter.
pub(crate) const ROTATE_BYTES: u64 = 4 * 1024 * 1024;
/// Segment names are zero-padded width-4 so lexicographic order is numeric
/// order (see `list_segments`); 9999 is therefore the namespace's end.
const MAX_SEGMENT: u32 = 9999;
```

2. New `LogError` variant:

```rust
    #[error(
        "machine {machine} reached segment 9999 — the log segment namespace is exhausted; this catalog needs a new machine id"
    )]
    SegmentOverflow { machine: String },
```

3. Replace the hardcoded segment line in `append` (`let seg = self.machine_dir.join("0001.jsonl");`) with `let seg = self.active_segment(batch.len() as u64)?;` — note `batch` must be built first, so move the serialization loop above the segment choice. Wait — serialization can fail per event before any I/O; keep order: build `batch` (serde), then choose segment, then open/write. Add the chooser:

```rust
    /// The segment this append should write: the highest-numbered existing
    /// `NNNN.jsonl` unless this batch would push it past [`ROTATE_BYTES`],
    /// in which case the next number starts fresh. A brand-new machine dir
    /// starts at `0001.jsonl`. Non-numeric `.jsonl` names (a sync tool's
    /// "conflicted copy") are ignored for numbering, same as readers ignore
    /// nothing — they still read, they just never become the active tip.
    fn active_segment(&self, batch_len: u64) -> Result<PathBuf, LogError> {
        let segments = Self::list_segments(&self.machine_dir)?;
        let current = segments
            .iter()
            .filter_map(|(name, path)| {
                let n: u32 = name.strip_suffix(".jsonl")?.parse().ok()?;
                Some((n, path.clone()))
            })
            .next_back();
        let Some((num, path)) = current else {
            return Ok(self.machine_dir.join("0001.jsonl"));
        };
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len == 0 || len + batch_len <= ROTATE_BYTES {
            return Ok(path);
        }
        let next = num + 1;
        if next > MAX_SEGMENT {
            return Err(LogError::SegmentOverflow {
                machine: self
                    .machine_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });
        }
        Ok(self.machine_dir.join(format!("{next:04}.jsonl")))
    }
```

4. Update `append`'s doc comment: delete "(0001.jsonl for phase 1; segment rotation arrives with sync push/pull in a later phase)" — describe rotation instead.

- [ ] **Step 4: Run tests**

Run: `cargo test -p majestical-sync`
Expected: PASS (all pre-existing tests still pass — `append_then_read_all_machines` etc. exercise the `0001` fresh-dir path).

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/sync/src/lib.rs
git commit -m "feat: rotate log segments at 4 MiB with a hard 9999 overflow error"
```

---

### Task 3: `sync.toml` config + `maj sync location add|list|rm`

**Files:**
- Create: `crates/cli/src/sync_cmd.rs`
- Modify: `crates/cli/src/main.rs` (module decl, `Sync` subcommand, dispatcher)
- Test: unit tests inside `sync_cmd.rs` + CLI-level checks land in Task 8's smoke file

- [ ] **Step 1: Write the failing tests**

Create `crates/cli/src/sync_cmd.rs` containing ONLY the test module first:

```rust
//! `maj sync`: location config plus push/pull/status orchestration over
//! `crates/sync`'s transfer engine. Locations are per-machine config
//! (mount points differ per machine) in the state dir's `sync.toml`,
//! never synced.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_and_defaults_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loaded = SyncConfig::load(&path).expect("load missing");
        assert!(!loaded.readonly);
        assert!(loaded.locations.is_empty());
        let config = SyncConfig {
            readonly: true,
            locations: vec![Location {
                name: "nas".into(),
                path: "/Volumes/Team/sync".into(),
            }],
        };
        config.store(&path).expect("store");
        let loaded = SyncConfig::load(&path).expect("load");
        assert_eq!(loaded, config);
    }

    #[test]
    fn add_rejects_duplicate_names_and_rm_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "nas", &loc).expect("first add");
        assert!(
            loc.join("events").is_dir() && loc.join("blobs").is_dir(),
            "add initializes the events/ + blobs/ skeleton"
        );
        let err = add_location(&path, "nas", &loc).expect_err("dup must fail");
        assert!(err.to_string().contains("already configured"));
        let err = remove_location(&path, "ghost").expect_err("unknown rm");
        assert!(err.to_string().contains("no sync location named"));
    }

    #[test]
    fn add_rejects_an_unreachable_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let err = add_location(&path, "nas", &dir.path().join("missing"))
            .expect_err("unreachable path must fail");
        assert!(err.to_string().contains("not an accessible directory"));
    }
}
```

- [ ] **Step 2: Wire the module and run tests to verify they fail**

In `crates/cli/src/main.rs` add `mod sync_cmd;` to the module list (alphabetical: after `state_dir`).

Run: `cargo test -p majestical-cli sync_cmd`
Expected: COMPILE FAIL — `SyncConfig` etc. undefined.

- [ ] **Step 3: Implement config + location ops**

Above the test module in `sync_cmd.rs`:

```rust
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncConfig {
    /// The read-only-member switch: a machine with `readonly = true` never
    /// pushes (events already carry author identity, so this is the whole
    /// feature — a policy on the push side, not a data concept).
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Location {
    pub name: String,
    pub path: PathBuf,
}

impl SyncConfig {
    /// Missing file = empty config (a catalog that has never configured
    /// sync), never an error.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub(crate) fn store(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing sync config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// The per-catalog `sync.toml` path in this machine's state dir.
pub(crate) fn config_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("sync.toml"))
}

fn add_location(config: &Path, name: &str, location: &Path) -> Result<()> {
    anyhow::ensure!(
        location.is_dir(),
        "{} is not an accessible directory — mount it or check the path",
        location.display()
    );
    let mut cfg = SyncConfig::load(config)?;
    anyhow::ensure!(
        !cfg.locations.iter().any(|l| l.name == name),
        "sync location '{name}' is already configured — remove it first with `maj sync location rm {name}`"
    );
    // Git-init style: idempotently create the layout so the first push
    // has somewhere to land. Never touches existing files.
    for sub in ["events", "blobs"] {
        let dir = location.join(sub);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("initializing {}", dir.display()))?;
    }
    cfg.locations.push(Location {
        name: name.to_string(),
        path: location.to_path_buf(),
    });
    cfg.store(config)
}

fn remove_location(config: &Path, name: &str) -> Result<()> {
    let mut cfg = SyncConfig::load(config)?;
    let before = cfg.locations.len();
    cfg.locations.retain(|l| l.name != name);
    anyhow::ensure!(
        cfg.locations.len() < before,
        "no sync location named '{name}' — see `maj sync location list`"
    );
    cfg.store(config)
}

pub(crate) fn cmd_location_add(catalog: &Path, name: &str, location: &Path) -> Result<()> {
    add_location(&config_path(catalog)?, name, location)?;
    println!("added sync location '{name}' at {}", location.display());
    Ok(())
}

pub(crate) fn cmd_location_rm(catalog: &Path, name: &str) -> Result<()> {
    remove_location(&config_path(catalog)?, name)?;
    println!("removed sync location '{name}' (its files were not touched)");
    Ok(())
}

pub(crate) fn cmd_location_list(catalog: &Path, json: bool) -> Result<()> {
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    if json {
        let rows: Vec<serde_json::Value> = cfg
            .locations
            .iter()
            .map(|l| serde_json::json!({ "name": l.name, "path": l.path }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "readonly": cfg.readonly,
                "locations": rows,
            }))?
        );
        return Ok(());
    }
    if cfg.locations.is_empty() {
        println!("no sync locations configured — add one with `maj sync location add <name> <path>`");
        return Ok(());
    }
    for l in &cfg.locations {
        println!("{}\t{}", l.name, l.path.display());
    }
    if cfg.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}
```

Check `crates/cli/Cargo.toml` has `toml` as a dependency (the describe crate uses it; the CLI may not yet). If missing: `toml.workspace = true` under `[dependencies]` — verify `toml` is in the workspace `[workspace.dependencies]` first; add the current stable there if not.

- [ ] **Step 4: Wire clap in `main.rs`**

Add to `enum Cmd`:

```rust
    /// Sync the catalog with configured locations (NAS, Dropbox folder,
    /// shuttle drive).
    Sync {
        #[command(subcommand)]
        cmd: SyncCmd,
    },
```

New subcommand enums (Push/Pull/Status variants are wired in Tasks 5-7; declare them now so the surface compiles once):

```rust
#[derive(Subcommand)]
enum SyncCmd {
    /// Manage this machine's sync locations (stored in the state dir,
    /// never synced).
    Location {
        #[command(subcommand)]
        cmd: SyncLocationCmd,
    },
}

#[derive(Subcommand)]
enum SyncLocationCmd {
    /// Add a named location and initialize its events/ + blobs/ layout.
    Add { name: String, path: PathBuf },
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a location from config (never touches its files).
    Rm { name: String },
}
```

Dispatcher + match arm (mirrors `dispatch_describer` — no catalog open needed for config-only ops, but `state_dir_for` canonicalizes the catalog root, so the root must exist):

```rust
/// Dispatches `maj sync`'s subcommands. Split out of `main` purely to stay
/// under the crate's max-function-length lint, matching [`dispatch_index`].
fn dispatch_sync(catalog: &Path, cmd: SyncCmd) -> Result<()> {
    match cmd {
        SyncCmd::Location { cmd } => match cmd {
            SyncLocationCmd::Add { name, path } => sync_cmd::cmd_location_add(catalog, &name, &path),
            SyncLocationCmd::List { json } => sync_cmd::cmd_location_list(catalog, json),
            SyncLocationCmd::Rm { name } => sync_cmd::cmd_location_rm(catalog, &name),
        },
    }
}
```

In `main`'s match: `Cmd::Sync { cmd } => dispatch_sync(&cli.catalog, cmd)?,`

- [ ] **Step 5: Run tests, lint, commit**

Run: `cargo test -p majestical-cli sync_cmd` — Expected: PASS.

```bash
just check
git add crates/cli/src/sync_cmd.rs crates/cli/src/main.rs crates/cli/Cargo.toml
git commit -m "feat: sync location config and maj sync location add|list|rm"
```

---

### Task 4: Transfer engine (`crates/sync/src/transfer.rs`)

**Files:**
- Create: `crates/sync/src/transfer.rs`
- Modify: `crates/sync/src/lib.rs` (add `pub mod transfer;` after the imports; make `list_segments` reachable by moving nothing — transfer re-walks directories itself, keeping the engine self-contained)

- [ ] **Step 1: Write the failing tests**

Create `crates/sync/src/transfer.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileEventLog;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};

    fn ev(n: u64) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(n, u128::from(n))),
            hlc: Hlc {
                wall_ms: n,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: AssetId("xxh3:aa".into()),
                tag: "t".into(),
            },
        }
    }

    fn write_blob(root: &std::path::Path, rel: &str, bytes: &[u8]) {
        let path = root.join("blobs").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write blob");
    }

    #[test]
    fn classify_covers_every_blob_shape() {
        assert_eq!(classify_blob("thumb-320.webp"), BlobClass::Thumbs);
        assert_eq!(classify_blob("transcript.json.zst"), BlobClass::Transcripts);
        assert_eq!(classify_blob("image.f32le.zst"), BlobClass::Vectors);
        assert_eq!(classify_blob("kf-1500.f32le.zst"), BlobClass::Vectors);
        assert_eq!(classify_blob("chunk-0.f32le.zst"), BlobClass::Vectors);
        for name in [
            "keyframes.json",
            "image.json.zst",
            "kf-1500.json.zst",
            "text.json.zst",
            "caption.json.zst",
            "captions.json.zst",
            "tags.json.zst",
            "ocr-complete.json",
            "chunks-empty.json",
            "chunks-complete.json",
        ] {
            assert_eq!(classify_blob(name), BlobClass::Metadata, "{name}");
        }
    }

    #[test]
    fn plan_is_priority_ordered_and_execute_converges() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1), ev(2)]).expect("append");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        write_blob(src.path(), "aa/aahex/siglip2-b16-v1/transcript.json.zst", b"t");
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"w");
        write_blob(src.path(), "aa/aahex/siglip2-b16-v1/image.f32le.zst", b"v");
        write_blob(src.path(), "aa/aahex/tags.json.zst", b"j");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(plan.segments.len(), 1);
        let classes: Vec<BlobClass> = plan.blobs.iter().map(|b| b.class).collect();
        assert_eq!(
            classes,
            vec![
                BlobClass::Thumbs,
                BlobClass::Metadata,
                BlobClass::Vectors,
                BlobClass::Transcripts
            ],
            "blob plan must be priority-ordered"
        );

        let outcome = execute(src.path(), dst.path(), &plan).expect("execute");
        assert_eq!(outcome.segments_copied, 1);
        assert_eq!(outcome.blobs_copied, 4);
        let events_new: usize = outcome.events_added.iter().map(|(_, n)| *n).sum();
        assert_eq!(events_new, 2);

        let replan = plan_transfer(src.path(), dst.path()).expect("replan");
        assert!(
            replan.segments.is_empty() && replan.blobs.is_empty(),
            "a second plan after execute must be empty — sync converged"
        );
    }

    #[test]
    fn longer_destination_segment_is_never_truncated() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut src_log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        src_log.append(&[ev(1)]).expect("append");
        let mut dst_log = FileEventLog::init(dst.path(), &MachineId("m1".into())).expect("init");
        dst_log.append(&[ev(1), ev(2)]).expect("append");
        let dst_seg = dst.path().join("events/m1/0001.jsonl");
        let longer = std::fs::metadata(&dst_seg).expect("meta").len();

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert!(plan.segments.is_empty(), "destination is ahead — nothing to push");
        execute(src.path(), dst.path(), &plan).expect("execute");
        assert_eq!(
            std::fs::metadata(&dst_seg).expect("meta").len(),
            longer,
            "sync must never truncate"
        );
    }

    #[test]
    fn truncated_destination_segment_is_restored_whole() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1), ev(2), ev(3)]).expect("append");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        execute(src.path(), dst.path(), &plan).expect("execute");
        // Sabotage: an external tool truncates the replica.
        let dst_seg = dst.path().join("events/m1/0001.jsonl");
        let full = std::fs::metadata(&dst_seg).expect("meta").len();
        let f = std::fs::OpenOptions::new().write(true).open(&dst_seg).expect("open");
        f.set_len(10).expect("truncate");
        let plan = plan_transfer(src.path(), dst.path()).expect("replan");
        assert_eq!(plan.segments.len(), 1, "shorter replica must be re-planned");
        execute(src.path(), dst.path(), &plan).expect("re-execute");
        assert_eq!(std::fs::metadata(&dst_seg).expect("meta").len(), full);
    }

    #[test]
    fn size_mismatched_blob_is_recopied_and_temp_leftovers_are_ignored() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(src.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"full-content");
        write_blob(dst.path(), "aa/aahex/thumb-320.webp", b"torn");
        // A leftover temp file from a killed sync must not appear in a plan.
        let tmp = dst.path().join("tmp");
        std::fs::create_dir_all(&tmp).expect("mkdir tmp");
        std::fs::write(tmp.join("12345-0.part"), b"junk").expect("write junk");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(plan.blobs.len(), 1, "size mismatch = torn copy = re-copy");
        execute(src.path(), dst.path(), &plan).expect("execute");
        let healed =
            std::fs::read(dst.path().join("blobs/aa/aahex/thumb-320.webp")).expect("read");
        assert_eq!(healed, b"full-content");
        let plan_back = plan_transfer(dst.path(), src.path()).expect("reverse plan");
        assert!(
            plan_back.blobs.is_empty(),
            "tmp/ leftovers must never be planned as blobs"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p majestical-sync transfer`
Expected: COMPILE FAIL — nothing implemented yet.

- [ ] **Step 3: Implement the engine**

Top of `crates/sync/src/transfer.rs`:

```rust
//! Stateless set-union transfer between two sync roots (`events/` +
//! `blobs/`). Every plan is a fresh diff of real files — no cached sync
//! state anywhere, so an interrupted transfer converges on the next run by
//! construction (the same diff-as-queue shape as `maj index run`).
//!
//! Rules: segments are append-only with a single appender, so a shorter
//! copy is a strict prefix — transfer longer-wins, whole-file, via
//! temp+rename (atomic; a race between two pushers leaves one complete
//! valid file and the next sync restores any missing tail). Blobs are
//! immutable and derivation-keyed — presence is the diff, a size mismatch
//! is a torn copy from some non-atomic tool and is re-copied. Nothing is
//! ever deleted or truncated, in either direction.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("sync transfer io at {}: {source} — check the location is accessible", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl TransferError {
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Priority classes, in transfer order: an interrupted first sync should
/// leave the destination browsable (thumbs, then the small JSON) before it
/// is semantically searchable (vectors) or transcript-complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlobClass {
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

impl BlobClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thumbs => "thumbs",
            Self::Metadata => "metadata",
            Self::Vectors => "vectors",
            Self::Transcripts => "transcripts",
        }
    }
}

/// Classifies a blob file by name. The names are pinned by
/// `crates/index/src/blob.rs::path_for`; anything unrecognized lands in
/// `Metadata` (small JSON) rather than being skipped — sync must move
/// every blob, known shape or not.
#[must_use]
pub fn classify_blob(file_name: &str) -> BlobClass {
    if file_name == "thumb-320.webp" {
        return BlobClass::Thumbs;
    }
    if file_name == "transcript.json.zst" {
        return BlobClass::Transcripts;
    }
    if file_name.ends_with(".f32le.zst") {
        return BlobClass::Vectors;
    }
    BlobClass::Metadata
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCopy {
    pub machine: String,
    pub segment: String,
    pub src_len: u64,
    /// Destination length before the copy (0 when absent) — the offset new
    /// events start at, used by pull to count what arrived.
    pub dst_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobCopy {
    /// Path relative to `blobs/`.
    pub rel: PathBuf,
    pub class: BlobClass,
    pub size: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub segments: Vec<SegmentCopy>,
    pub blobs: Vec<BlobCopy>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferOutcome {
    pub segments_copied: usize,
    pub segment_bytes: u64,
    pub blobs_copied: usize,
    pub blob_bytes: u64,
    /// `(machine, events)` counted from each copied segment's new byte
    /// range — what a pull reports as "applied N events from M machines".
    pub events_added: Vec<(String, usize)>,
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Temp leftovers older than this are swept at the start of `execute` —
/// young ones may belong to a concurrent pusher and are left alone.
const STALE_TEMP_MS: u128 = 60 * 60 * 1000;
```

Planning (same file):

```rust
/// Diff `src` against `dst`: segments where the destination is missing or
/// shorter, blobs where the destination is missing or size-mismatched.
/// Blobs come back priority-ordered ([`BlobClass`] then path). A `src`
/// with no `events/` or `blobs/` contributes nothing for that half — a
/// fresh location is a valid, empty peer.
///
/// # Errors
/// Returns [`TransferError::Io`] if a directory that exists can't be read.
pub fn plan_transfer(src: &Path, dst: &Path) -> Result<TransferPlan, TransferError> {
    let mut plan = TransferPlan::default();
    plan_segments(src, dst, &mut plan)?;
    plan_blobs(src, dst, &mut plan)?;
    plan.blobs.sort_by(|a, b| (a.class, &a.rel).cmp(&(b.class, &b.rel)));
    Ok(plan)
}

/// DESTINATION-side length only: absent (or unreadable) = 0 = "needs the
/// copy", which is self-correcting — if the file is unreadable rather than
/// absent, the planned copy fails loudly in `execute`. Never use this for
/// SOURCE lengths: a source metadata failure must propagate, or the file
/// silently drops out of the plan (Task 2's review caught exactly this
/// swallow-and-degrade pattern in rotation).
fn dst_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

fn plan_segments(src: &Path, dst: &Path, plan: &mut TransferPlan) -> Result<(), TransferError> {
    let events = src.join("events");
    let Ok(machines) = std::fs::read_dir(&events) else {
        return Ok(());
    };
    for machine in machines {
        let machine = machine.map_err(TransferError::io(&events))?;
        let machine_path = machine.path();
        let is_dir = machine
            .file_type()
            .map_err(TransferError::io(&machine_path))?
            .is_dir();
        if !is_dir {
            continue;
        }
        let machine_name = machine.file_name().to_string_lossy().into_owned();
        let entries = std::fs::read_dir(&machine_path).map_err(TransferError::io(&machine_path))?;
        for entry in entries {
            let entry = entry.map_err(TransferError::io(&machine_path))?;
            let path = entry.path();
            let is_seg = entry.file_type().map_err(TransferError::io(&path))?.is_file()
                && path.extension().is_some_and(|x| x == "jsonl");
            if !is_seg {
                continue;
            }
            let segment = entry.file_name().to_string_lossy().into_owned();
            let src_len = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_len = dst_len(&dst.join("events").join(&machine_name).join(&segment));
            if src_len > dst_len {
                plan.segments.push(SegmentCopy {
                    machine: machine_name.clone(),
                    segment,
                    src_len,
                    dst_len,
                });
            }
        }
    }
    plan.segments
        .sort_by(|a, b| (&a.machine, &a.segment).cmp(&(&b.machine, &b.segment)));
    Ok(())
}

/// Recursive walk of `blobs/` collecting files as `blobs/`-relative paths.
/// The destination's `tmp/` staging dir never appears because temp files
/// live under `<root>/tmp`, a sibling of `blobs/`, not inside it.
fn plan_blobs(src: &Path, dst: &Path, plan: &mut TransferPlan) -> Result<(), TransferError> {
    let src_blobs = src.join("blobs");
    let dst_blobs = dst.join("blobs");
    let mut stack = vec![src_blobs.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue; // absent tree half — a fresh location
        };
        for entry in entries {
            let entry = entry.map_err(TransferError::io(&dir))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(TransferError::io(&path))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&src_blobs) else {
                continue;
            };
            let size = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_path = dst_blobs.join(rel);
            if !dst_path.is_file() || dst_len(&dst_path) != size {
                let name = entry.file_name().to_string_lossy().into_owned();
                plan.blobs.push(BlobCopy {
                    rel: rel.to_path_buf(),
                    class: classify_blob(&name),
                    size,
                });
            }
        }
    }
    Ok(())
}
```

Execution (same file):

```rust
/// Copies everything in `plan` from `src` to `dst` via `<dst>/tmp` staging
/// + atomic rename, sweeping stale temp leftovers first. Segment copies
/// re-check the destination length right before renaming? No — deliberately
/// not: a concurrent pusher racing us leaves one complete valid file
/// either way, and the next sync restores any missing tail (see the module
/// doc). Re-checking would only narrow, never close, the window.
///
/// # Errors
/// Returns [`TransferError::Io`] on any staging, copy, or rename failure.
pub fn execute(src: &Path, dst: &Path, plan: &TransferPlan) -> Result<TransferOutcome, TransferError> {
    let staging = dst.join("tmp");
    std::fs::create_dir_all(&staging).map_err(TransferError::io(&staging))?;
    sweep_stale_temps(&staging);
    let mut outcome = TransferOutcome::default();
    for seg in &plan.segments {
        let from = src.join("events").join(&seg.machine).join(&seg.segment);
        let to = dst.join("events").join(&seg.machine).join(&seg.segment);
        copy_via_temp(&from, &to, &staging)?;
        outcome.segments_copied += 1;
        outcome.segment_bytes += seg.src_len.saturating_sub(seg.dst_len);
        let mut count = 0usize;
        let (events, _) = crate::FileEventLog::read_segment_since(&from, seg.dst_len, |_| {})
            .map_err(|e| match e {
                crate::LogError::Io { path, source } => TransferError::Io { path, source },
                other => TransferError::Io {
                    path: from.clone(),
                    source: std::io::Error::other(other.to_string()),
                },
            })?;
        count += events.len();
        if count > 0 {
            outcome.events_added.push((seg.machine.clone(), count));
        }
    }
    for blob in &plan.blobs {
        let from = src.join("blobs").join(&blob.rel);
        let to = dst.join("blobs").join(&blob.rel);
        copy_via_temp(&from, &to, &staging)?;
        outcome.blobs_copied += 1;
        outcome.blob_bytes += blob.size;
    }
    Ok(outcome)
}

/// Best-effort: a leftover that can't be inspected or removed is skipped,
/// never fatal — it is invisible to planning either way.
fn sweep_stale_temps(staging: &Path) {
    let Ok(entries) = std::fs::read_dir(staging) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let age_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(0, |d| d.as_millis());
        if age_ms > STALE_TEMP_MS {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn copy_via_temp(from: &Path, to: &Path, staging: &Path) -> Result<(), TransferError> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = staging.join(format!("{}-{seq}.part", std::process::id()));
    std::fs::copy(from, &tmp).map_err(TransferError::io(from))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(TransferError::io(parent))?;
    }
    std::fs::rename(&tmp, to).map_err(TransferError::io(to))?;
    Ok(())
}
```

`read_segment_since` is currently a private method on `FileEventLog` — change its visibility in `lib.rs` to `pub(crate)` (same crate, still not public API). Add `pub mod transfer;` to `lib.rs` after the `use` block.

- [ ] **Step 4: Run tests**

Run: `cargo test -p majestical-sync`
Expected: PASS — all five new transfer tests plus every existing lib test.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/sync/src/lib.rs crates/sync/src/transfer.rs
git commit -m "feat: stateless set-union transfer engine for sync roots"
```

---

### Task 5: `maj sync push`

**Files:**
- Modify: `crates/cli/src/sync_cmd.rs`, `crates/cli/src/main.rs`
- Test: `crates/cli/tests/sync_smoke.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/cli/tests/sync_smoke.rs`. Follow `cli_smoke.rs`'s harness pattern (`assert_cmd::Command::cargo_bin("maj")`, per-test temp dirs, `MAJ_STATE_DIR` + `MAJ_CATALOG` + `MAJ_MACHINE_ID` env). Helper + first tests:

```rust
//! `maj sync` end to end over real temp-dir catalogs and locations.
use assert_cmd::Command;
use std::path::Path;

fn maj(catalog: &Path, state: &Path, machine: &str) -> Command {
    let mut cmd = Command::cargo_bin("maj").expect("binary");
    cmd.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", machine)
        .env("MAJ_STATE_DIR", state);
    cmd
}

fn init_catalog(catalog: &Path, state: &Path, machine: &str) {
    maj(catalog, state, machine)
        .args(["catalog", "init"])
        .assert()
        .success();
}

#[test]
fn push_replicates_segments_and_blobs_to_a_location() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    init_catalog(&catalog, &state, "m1");
    // One real event past init: a tag on a hand-declared asset id is the
    // cheapest op the CLI can emit... but tag requires a known asset, so
    // scan one real file instead.
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&catalog, &state, "m1")
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    // A blob to carry along.
    let blob_dir = catalog.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"w").expect("write");

    maj(&catalog, &state, "m1")
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    let out = maj(&catalog, &state, "m1")
        .args(["sync", "push"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("nas"), "report names the location: {stdout}");
    assert!(
        location.join("events/m1/0001.jsonl").is_file(),
        "segments replicated"
    );
    assert!(
        location.join("blobs/ab/abcd/thumb-320.webp").is_file(),
        "blobs replicated"
    );
}

#[test]
fn readonly_refuses_push_naming_the_config_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    init_catalog(&catalog, &state, "m1");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    maj(&catalog, &state, "m1")
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    // Flip readonly by rewriting the config file the CLI just created.
    let config = find_sync_toml(&state);
    let text = std::fs::read_to_string(&config).expect("read");
    std::fs::write(&config, format!("readonly = true\n{text}")).expect("write");
    let out = maj(&catalog, &state, "m1")
        .args(["sync", "push"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("readonly = true") && stderr.contains("sync.toml"),
        "refusal must name the setting and the file: {stderr}"
    );
}

fn find_sync_toml(state: &Path) -> std::path::PathBuf {
    // state/catalogs/<key>/sync.toml — exactly one catalog key exists.
    let catalogs = state.join("catalogs");
    let entry = std::fs::read_dir(&catalogs)
        .expect("state dir")
        .next()
        .expect("one catalog key")
        .expect("entry");
    entry.path().join("sync.toml")
}

#[test]
fn unreachable_location_is_skipped_with_a_notice_not_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    init_catalog(&catalog, &state, "m1");
    let good = root.path().join("nas");
    let gone = root.path().join("shuttle");
    std::fs::create_dir_all(&good).expect("mkdir");
    std::fs::create_dir_all(&gone).expect("mkdir");
    for (name, path) in [("nas", &good), ("shuttle", &gone)] {
        maj(&catalog, &state, "m1")
            .args(["sync", "location", "add", name])
            .arg(path)
            .assert()
            .success();
    }
    std::fs::remove_dir_all(&gone).expect("eject the shuttle");
    let out = maj(&catalog, &state, "m1")
        .args(["sync", "push"])
        .assert()
        .success(); // one location succeeded — exit 0
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("shuttle") && stdout.contains("skipped"),
        "skip notice must name the location: {stdout}"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test sync_smoke`
Expected: FAIL — `maj sync push` is not a known subcommand.

- [ ] **Step 3: Implement push in `sync_cmd.rs`**

```rust
use majestical_sync::transfer::{self, BlobClass, TransferPlan};

/// `--only` surface, shared by push and pull.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OnlyArg {
    Segments,
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

fn filter_plan(plan: TransferPlan, only: Option<OnlyArg>) -> TransferPlan {
    let Some(only) = only else { return plan };
    let class = match only {
        OnlyArg::Segments => {
            return TransferPlan {
                segments: plan.segments,
                blobs: Vec::new(),
            };
        }
        OnlyArg::Thumbs => BlobClass::Thumbs,
        OnlyArg::Metadata => BlobClass::Metadata,
        OnlyArg::Vectors => BlobClass::Vectors,
        OnlyArg::Transcripts => BlobClass::Transcripts,
    };
    TransferPlan {
        segments: Vec::new(),
        blobs: plan.blobs.into_iter().filter(|b| b.class == class).collect(),
    }
}

/// Locations to operate on: all configured, or exactly the named one.
fn resolve_targets(cfg: &SyncConfig, name: Option<&str>) -> Result<Vec<&Location>> {
    anyhow::ensure!(
        !cfg.locations.is_empty(),
        "no sync locations configured — add one with `maj sync location add <name> <path>`"
    );
    let Some(name) = name else {
        return Ok(cfg.locations.iter().collect());
    };
    let found = cfg.locations.iter().find(|l| l.name == name);
    found.map(|l| vec![l]).ok_or_else(|| {
        let known: Vec<&str> = cfg.locations.iter().map(|l| l.name.as_str()).collect();
        anyhow::anyhow!("no sync location named '{name}' — configured: {}", known.join(", "))
    })
}

/// One location's push/pull result, for the per-row report.
struct LocationResult {
    name: String,
    outcome: Option<transfer::TransferOutcome>,
    skipped: Option<String>,
}

pub(crate) fn cmd_push(
    catalog: &Path,
    location: Option<&str>,
    only: Option<OnlyArg>,
    json: bool,
) -> Result<()> {
    anyhow::ensure!(
        catalog.join("events").is_dir(),
        "no catalog at {} — run `maj catalog init` first",
        catalog.display()
    );
    let config = config_path(catalog)?;
    let cfg = SyncConfig::load(&config)?;
    anyhow::ensure!(
        !cfg.readonly,
        "readonly = true in {} — this machine is a read-only member and never pushes",
        config.display()
    );
    let targets = resolve_targets(&cfg, location)?;
    let mut results = Vec::new();
    for loc in targets {
        results.push(transfer_one(catalog, loc, only, Direction::Push));
    }
    report_and_check(&results, "push", json)
}

enum Direction {
    Push,
    Pull,
}

/// Runs one location's transfer in the given direction, converting an
/// unreachable location into a skip row rather than an error.
fn transfer_one(
    catalog: &Path,
    loc: &Location,
    only: Option<OnlyArg>,
    direction: Direction,
) -> LocationResult {
    if !loc.path.is_dir() {
        return LocationResult {
            name: loc.name.clone(),
            outcome: None,
            skipped: Some(format!("unreachable at {} — skipped", loc.path.display())),
        };
    }
    let (src, dst) = match direction {
        Direction::Push => (catalog, loc.path.as_path()),
        Direction::Pull => (loc.path.as_path(), catalog),
    };
    let run = transfer::plan_transfer(src, dst)
        .map(|plan| filter_plan(plan, only))
        .and_then(|plan| transfer::execute(src, dst, &plan));
    match run {
        Ok(outcome) => LocationResult {
            name: loc.name.clone(),
            outcome: Some(outcome),
            skipped: None,
        },
        Err(e) => LocationResult {
            name: loc.name.clone(),
            outcome: None,
            skipped: Some(format!("failed: {e}")),
        },
    }
}

/// Prints per-location rows and enforces the exit policy: nonzero when
/// EVERY requested location failed or was skipped, and ALSO when any
/// per-file `failures` occurred (partial progress is kept and reported —
/// the engine records and continues past per-file errors, Task 4
/// as-built — but a sync that could not move everything must not exit 0
/// under cron). A failure-carrying row appends ", N failed" and each
/// failure prints to stderr as `<location>: failed <path>: <reason>`; the
/// final ensure message names the failing locations and says progress was
/// kept and the next run retries.
fn report_and_check(results: &[LocationResult], verb: &str, json: bool) -> Result<()> {
    if json {
        let rows: Vec<serde_json::Value> = results
            .iter()
            .map(|r| match (&r.outcome, &r.skipped) {
                (Some(o), _) => serde_json::json!({
                    "location": r.name,
                    "segments": o.segments_copied,
                    "segment_bytes": o.segment_bytes,
                    "blobs": o.blobs_copied,
                    "blob_bytes": o.blob_bytes,
                    "failures": o.failures.iter().map(|(p, e)| {
                        serde_json::json!({ "path": p, "error": e })
                    }).collect::<Vec<_>>(),
                }),
                (None, skipped) => serde_json::json!({
                    "location": r.name,
                    "skipped": skipped,
                }),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in results {
            match (&r.outcome, &r.skipped) {
                (Some(o), _) => println!(
                    "{}: {verb}ed {} segment(s) ({} bytes), {} blob(s) ({} bytes)",
                    r.name, o.segments_copied, o.segment_bytes, o.blobs_copied, o.blob_bytes
                ),
                (None, Some(reason)) => println!("{}: {reason}", r.name),
                (None, None) => {}
            }
        }
    }
    anyhow::ensure!(
        results.iter().any(|r| r.outcome.is_some()),
        "sync {verb} failed for every requested location"
    );
    Ok(())
}
```

- [ ] **Step 4: Wire clap**

Add to `enum SyncCmd` in `main.rs`:

```rust
    /// Replicate everything this catalog has (segments + blobs) to
    /// configured locations.
    Push {
        #[arg(long)]
        location: Option<String>,
        /// Restrict to one transfer class.
        #[arg(long, value_enum)]
        only: Option<sync_cmd::OnlyArg>,
        #[arg(long)]
        json: bool,
    },
```

And in `dispatch_sync`:

```rust
        SyncCmd::Push { location, only, json } => {
            sync_cmd::cmd_push(catalog, location.as_deref(), only, json)
        }
```

- [ ] **Step 5: Run tests, lint, commit**

Run: `cargo test -p majestical-cli --test sync_smoke` — Expected: PASS (all three).
Run: `cargo test -p majestical-cli` — Expected: PASS (no regressions).

```bash
just check
git add crates/cli/src/sync_cmd.rs crates/cli/src/main.rs crates/cli/tests/sync_smoke.rs
git commit -m "feat: maj sync push with per-location rows and readonly refusal"
```

---

### Task 6: `maj sync pull` + incremental apply + remedy notice

**Files:**
- Modify: `crates/cli/src/sync_cmd.rs`, `crates/cli/src/main.rs`
- Test: `crates/cli/tests/sync_smoke.rs`

- [ ] **Step 1: Write the failing test**

Add to `sync_smoke.rs`:

```rust
#[test]
fn pull_applies_a_teammates_events_and_names_the_index_remedy() {
    let root = tempfile::tempdir().expect("tempdir");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    // Machine 1: its own catalog root + state, scans a file, pushes.
    let cat1 = root.path().join("cat1");
    let state1 = root.path().join("state1");
    init_catalog(&cat1, &state1, "m1");
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("a.jpg"), b"jpeg-bytes").expect("write");
    maj(&cat1, &state1, "m1")
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol1"])
        .assert()
        .success();
    let blob_dir = cat1.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"w").expect("write");
    maj(&cat1, &state1, "m1")
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    maj(&cat1, &state1, "m1").args(["sync", "push"]).assert().success();

    // Machine 2: separate catalog root + state, pulls from the same location.
    let cat2 = root.path().join("cat2");
    let state2 = root.path().join("state2");
    init_catalog(&cat2, &state2, "m2");
    maj(&cat2, &state2, "m2")
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    let out = maj(&cat2, &state2, "m2")
        .args(["sync", "pull"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("applied") && stdout.contains("m1"),
        "pull must report applied events and the machines they came from: {stdout}"
    );
    assert!(
        stdout.contains("maj index run"),
        "fetched blobs must carry the index remedy notice: {stdout}"
    );
    assert!(cat2.join("blobs/ab/abcd/thumb-320.webp").is_file());
    // The pulled events are actually in machine 2's catalog: search sees
    // the scanned asset.
    let out = maj(&cat2, &state2, "m2")
        .args(["search", "a.jpg"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("a.jpg"), "pulled catalog must be searchable: {stdout}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test sync_smoke pull_applies`
Expected: FAIL — `maj sync pull` unknown.

- [ ] **Step 3: Implement pull**

REQUIRED PRE-WORK (Task 5 review finding): split `report_and_check` into
`render_rows` (build the report rows) and `check_exit_policy` (the two
`ensure`s) BEFORE wiring pull. Reason: pull must (a) apply pulled events to
the local catalog even when per-file blob failures occurred — the current
fused fn would `?`-short-circuit before the apply, leaving landed segments
unapplied while claiming "the next run retries" (it won't: the transfer
already converged); and (b) emit ONE JSON document — rows plus the
`{applied_events, machines, blobs_fetched}` summary in a single object —
not two concatenated documents that break every JSON consumer. Order in
`cmd_pull`: transfer → render (don't print yet in json mode) → apply →
print combined output → check_exit_policy LAST. `cmd_push` calls the two
split fns back-to-back (no behavior change; existing tests must stay
green untouched).

In `sync_cmd.rs` (uses `FsApp` + `open_catalog` for the incremental apply):

```rust
use crate::app::FsApp;

pub(crate) fn cmd_pull(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    args: &PullArgs,
) -> Result<()> {
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    let targets = resolve_targets(&cfg, args.location.as_deref())?;
    let mut results = Vec::new();
    for loc in targets {
        results.push(transfer_one(catalog, loc, args.only, Direction::Pull));
    }
    report_and_check(&results, "pull", args.json)?;

    // New events land by the existing incremental apply — opening the
    // sqlite catalog applies past the saved cursor. Counts come from the
    // transfer outcomes (events in each copied segment's new byte range).
    let app = FsApp::open(catalog, machine_id, author)?;
    crate::commands::open_catalog(&app, catalog)?;
    let mut per_machine: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut blobs_fetched = 0usize;
    for r in &results {
        let Some(o) = &r.outcome else { continue };
        blobs_fetched += o.blobs_copied;
        for (machine, n) in &o.events_added {
            *per_machine.entry(machine.as_str()).or_default() += n;
        }
    }
    let applied: usize = per_machine.values().sum();
    let machines: Vec<&str> = per_machine.keys().copied().collect();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied_events": applied,
                "machines": machines,
                "blobs_fetched": blobs_fetched,
            }))?
        );
    } else {
        println!(
            "applied {applied} new event(s) from {} machine(s){}",
            machines.len(),
            if machines.is_empty() { String::new() } else { format!(" ({})", machines.join(", ")) }
        );
        if blobs_fetched > 0 {
            println!(
                "fetched {blobs_fetched} blob(s); run `maj index run` to make fetched vectors and text searchable"
            );
        }
    }
    Ok(())
}

/// Bundles pull's flags within the house 5-positional-parameter limit.
pub(crate) struct PullArgs {
    pub location: Option<String>,
    pub only: Option<OnlyArg>,
    pub json: bool,
}
```

Note: pulled events arrive as DUPLICATE-free counts only when a segment's new byte range is genuinely new — which longer-wins guarantees (the range past `dst_len` was absent locally). A pulled event may still double-count across two locations holding the same segment tail; the diff prevents it (the second location's `dst_len` is re-measured after the first location's copy landed, so its range shrinks to nothing). State this in a code comment on `events_added` aggregation.

- [ ] **Step 4: Wire clap**

`SyncCmd::Pull` mirrors `Push` exactly (same three flags). Dispatch needs the machine/author, so `dispatch_sync` gains parameters — change its signature to `fn dispatch_sync(catalog: &Path, machine_id: &str, author: &str, cmd: SyncCmd)` and pass `&cli.machine_id, &author` from `main`:

```rust
        SyncCmd::Pull { location, only, json } => sync_cmd::cmd_pull(
            catalog,
            machine_id,
            author,
            &sync_cmd::PullArgs { location, only, json },
        ),
```

- [ ] **Step 5: Run tests, lint, commit**

Run: `cargo test -p majestical-cli --test sync_smoke` — Expected: PASS.

```bash
just check
git add crates/cli/src/sync_cmd.rs crates/cli/src/main.rs crates/cli/tests/sync_smoke.rs
git commit -m "feat: maj sync pull with incremental apply and index remedy notice"
```

---

### Task 7: `maj sync status`

**Files:**
- Modify: `crates/cli/src/sync_cmd.rs`, `crates/cli/src/main.rs`
- Test: `crates/cli/tests/sync_smoke.rs`

- [ ] **Step 1: Write the failing test (a sabotage probe)**

```rust
#[test]
fn status_counts_are_walked_not_cached() {
    let root = tempfile::tempdir().expect("tempdir");
    let catalog = root.path().join("cat");
    let state = root.path().join("state");
    let location = root.path().join("nas");
    std::fs::create_dir_all(&location).expect("mkdir");
    init_catalog(&catalog, &state, "m1");
    let blob_dir = catalog.join("blobs/ab/abcd");
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"w").expect("write");
    maj(&catalog, &state, "m1")
        .args(["sync", "location", "add", "nas"])
        .arg(&location)
        .assert()
        .success();
    maj(&catalog, &state, "m1").args(["sync", "push"]).assert().success();
    let synced = maj(&catalog, &state, "m1")
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let synced: serde_json::Value =
        serde_json::from_slice(&synced.get_output().stdout).expect("json");
    assert_eq!(synced[0]["ahead"]["blobs"]["thumbs"], 0, "in sync after push");

    // Sabotage: delete the remote blob. Status must see it — no cache.
    std::fs::remove_file(location.join("blobs/ab/abcd/thumb-320.webp")).expect("rm");
    let after = maj(&catalog, &state, "m1")
        .args(["sync", "status", "--json"])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&after.get_output().stdout).expect("json");
    assert_eq!(
        after[0]["ahead"]["blobs"]["thumbs"], 1,
        "a deleted remote blob must reappear in ahead-counts: {after}"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test sync_smoke status_counts`
Expected: FAIL — `maj sync status` unknown.

- [ ] **Step 3: Implement status**

In `sync_cmd.rs` — status plans BOTH directions and never executes:

```rust
fn class_counts(blobs: &[transfer::BlobCopy]) -> serde_json::Value {
    let mut counts = std::collections::BTreeMap::from([
        ("thumbs", 0usize),
        ("metadata", 0),
        ("vectors", 0),
        ("transcripts", 0),
    ]);
    for b in blobs {
        *counts.entry(b.class.as_str()).or_default() += 1;
    }
    serde_json::json!(counts)
}

fn direction_json(plan: &TransferPlan) -> serde_json::Value {
    let segment_bytes: u64 = plan.segments.iter().map(|s| s.src_len - s.dst_len).sum();
    serde_json::json!({
        "segments": plan.segments.len(),
        "segment_bytes": segment_bytes,
        "blobs": class_counts(&plan.blobs),
    })
}

pub(crate) fn cmd_status(catalog: &Path, json: bool) -> Result<()> {
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    anyhow::ensure!(
        !cfg.locations.is_empty(),
        "no sync locations configured — add one with `maj sync location add <name> <path>`"
    );
    let mut rows = Vec::new();
    for loc in &cfg.locations {
        if !loc.path.is_dir() {
            rows.push(serde_json::json!({
                "location": loc.name,
                "reachable": false,
                "path": loc.path,
            }));
            continue;
        }
        let ahead = transfer::plan_transfer(catalog, &loc.path)?;
        let behind = transfer::plan_transfer(&loc.path, catalog)?;
        rows.push(serde_json::json!({
            "location": loc.name,
            "reachable": true,
            "ahead": direction_json(&ahead),
            "behind": direction_json(&behind),
        }));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for row in &rows {
        print_status_row(row);
    }
    if cfg.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}

fn print_status_row(row: &serde_json::Value) {
    let name = row["location"].as_str().unwrap_or("?");
    if row["reachable"] == false {
        println!(
            "{name}: unreachable at {} — mount it and retry",
            row["path"].as_str().unwrap_or("?")
        );
        return;
    }
    for (label, key) in [("ahead (push would send)", "ahead"), ("behind (pull would fetch)", "behind")] {
        let d = &row[key];
        println!(
            "{name}: {label}: {} segment(s), blobs: thumbs {} / metadata {} / vectors {} / transcripts {}",
            d["segments"],
            d["blobs"]["thumbs"], d["blobs"]["metadata"], d["blobs"]["vectors"], d["blobs"]["transcripts"]
        );
    }
}
```

`plan_transfer` returns `TransferError`, which needs `From` conversion for anyhow — it already implements `std::error::Error` via thiserror, so `?` works inside `Result<_, anyhow::Error>` directly.

- [ ] **Step 4: Wire clap**

`SyncCmd::Status { #[arg(long)] json: bool }`; dispatch arm `SyncCmd::Status { json } => sync_cmd::cmd_status(catalog, json)`.

- [ ] **Step 5: Run tests, lint, commit**

Run: `cargo test -p majestical-cli --test sync_smoke` — Expected: PASS.

```bash
just check
git add crates/cli/src/sync_cmd.rs crates/cli/src/main.rs crates/cli/tests/sync_smoke.rs
git commit -m "feat: maj sync status with walked ahead/behind counts per location"
```

---

### Task 8: Convergence property test + shuttle e2e

**Files:**
- Create: `crates/sync/tests/convergence.rs`
- Modify: `crates/sync/Cargo.toml` (dev-dependency `proptest.workspace = true`)
- Test: `crates/cli/tests/sync_smoke.rs` (shuttle e2e)

- [ ] **Step 1: Write the convergence property test**

`crates/sync/tests/convergence.rs`:

```rust
//! The sync acceptance criterion, per the phase 6 spec: random
//! interleavings of append/push/pull across machines and locations, then a
//! final full round — every machine converges to the same event set and
//! the same blob set. Reuses nothing but the public transfer API; if this
//! holds, projection equality follows from the already-proven commutative
//! idempotent apply.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_sync::transfer::{execute, plan_transfer};
use majestical_sync::FileEventLog;
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::path::Path;

const MACHINES: usize = 3;
const LOCATIONS: usize = 2;

#[derive(Debug, Clone)]
enum Step {
    Append { machine: usize, events: u8 },
    Push { machine: usize, location: usize },
    Pull { machine: usize, location: usize },
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..MACHINES, 1u8..4).prop_map(|(machine, events)| Step::Append { machine, events }),
        (0..MACHINES, 0..LOCATIONS).prop_map(|(machine, location)| Step::Push { machine, location }),
        (0..MACHINES, 0..LOCATIONS).prop_map(|(machine, location)| Step::Pull { machine, location }),
    ]
}

fn ev(machine: usize, n: u64) -> Event {
    let unique = u128::from(n) << 8 | machine as u128;
    Event {
        id: EventId(ulid::Ulid::from_parts(n, unique)),
        hlc: Hlc {
            wall_ms: n,
            counter: 0,
            machine: MachineId(format!("m{machine}")),
        },
        author: "prop".into(),
        op: Op::TagAdd {
            asset: AssetId("xxh3:aa".into()),
            tag: format!("t{machine}-{n}"),
        },
    }
}

fn event_ids(root: &Path, machine: &MachineId) -> BTreeSet<String> {
    FileEventLog::open(root, machine)
        .expect("open")
        .read_all()
        .expect("read")
        .into_iter()
        .map(|e| e.id.0.to_string())
        .collect()
}

fn sync_pair(src: &Path, dst: &Path) {
    let plan = plan_transfer(src, dst).expect("plan");
    execute(src, dst, &plan).expect("execute");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn machines_converge_after_a_final_full_round(script in prop::collection::vec(step_strategy(), 0..40)) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut counters = [0u64; MACHINES];
        let machine_roots: Vec<_> = (0..MACHINES).map(|m| {
            let root = dir.path().join(format!("machine{m}"));
            FileEventLog::init(&root, &MachineId(format!("m{m}"))).expect("init");
            root
        }).collect();
        let location_roots: Vec<_> = (0..LOCATIONS).map(|l| {
            let root = dir.path().join(format!("location{l}"));
            std::fs::create_dir_all(root.join("events")).expect("mkdir");
            std::fs::create_dir_all(root.join("blobs")).expect("mkdir");
            root
        }).collect();

        for step in script {
            match step {
                Step::Append { machine, events } => {
                    let id = MachineId(format!("m{machine}"));
                    let mut log = FileEventLog::open(&machine_roots[machine], &id).expect("open");
                    let batch: Vec<Event> = (0..events).map(|_| {
                        counters[machine] += 1;
                        ev(machine, counters[machine])
                    }).collect();
                    log.append(&batch).expect("append");
                }
                Step::Push { machine, location } =>
                    sync_pair(&machine_roots[machine], &location_roots[location]),
                Step::Pull { machine, location } =>
                    sync_pair(&location_roots[location], &machine_roots[machine]),
            }
        }

        // Final full round: everyone pushes everywhere, then everyone
        // pulls everywhere — one round suffices because push carries
        // gossiped segments, not just the pusher's own.
        for root in &machine_roots {
            for loc in &location_roots {
                sync_pair(root, loc);
            }
        }
        for root in &machine_roots {
            for loc in &location_roots {
                sync_pair(loc, root);
            }
        }

        let reference = event_ids(&machine_roots[0], &MachineId("m0".into()));
        let total: u64 = counters.iter().sum();
        prop_assert_eq!(reference.len() as u64, total, "no event may be lost");
        for (m, root) in machine_roots.iter().enumerate().skip(1) {
            let ids = event_ids(root, &MachineId(format!("m{m}")));
            prop_assert_eq!(&ids, &reference, "machine {} diverged", m);
        }
    }
}
```

Wait — one push+pull round does NOT suffice for full convergence through locations when a machine's events reached a location only during the final push round: machine A pushes to L after machine B already pulled from L. The loop above pushes ALL machines first, THEN pulls — so every pull sees every push. That ordering is the sufficiency argument; keep the loops in that order and say so in the comment (already does).

- [ ] **Step 2: Add proptest dev-dependency and run**

In `crates/sync/Cargo.toml` `[dev-dependencies]`: add `proptest.workspace = true`.

Run: `cargo test -p majestical-sync --test convergence`
Expected: PASS (this is a property of code already written in Tasks 4; the test is new coverage, not new behavior — if it fails, the transfer engine has a real bug: STOP and debug with the shrunken case).

- [ ] **Step 3: Write the shuttle e2e**

Add to `crates/cli/tests/sync_smoke.rs`:

```rust
#[test]
fn a_shuttle_drive_converges_two_sites_that_never_meet() {
    let root = tempfile::tempdir().expect("tempdir");
    let shuttle = root.path().join("shuttle");
    std::fs::create_dir_all(&shuttle).expect("mkdir");
    let cat_a = root.path().join("site-a");
    let state_a = root.path().join("state-a");
    let cat_b = root.path().join("site-b");
    let state_b = root.path().join("state-b");
    init_catalog(&cat_a, &state_a, "site-a");
    init_catalog(&cat_b, &state_b, "site-b");
    for (cat, state, machine) in [(&cat_a, &state_a, "site-a"), (&cat_b, &state_b, "site-b")] {
        maj(cat, state, machine)
            .args(["sync", "location", "add", "shuttle"])
            .arg(&shuttle)
            .assert()
            .success();
    }
    // Site A catalogs a file and pushes to the shuttle.
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("mkdir");
    std::fs::write(media.join("interview.mov"), b"mov-bytes").expect("write");
    maj(&cat_a, &state_a, "site-a")
        .args(["scan"])
        .arg(&media)
        .args(["--volume", "vol-a"])
        .assert()
        .success();
    maj(&cat_a, &state_a, "site-a").args(["sync", "push"]).assert().success();

    // The drive travels. Site B pulls, sees the asset, tags it, pushes.
    maj(&cat_b, &state_b, "site-b").args(["sync", "pull"]).assert().success();
    let out = maj(&cat_b, &state_b, "site-b")
        .args(["search", "interview"])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&out.get_output().stdout).contains("interview.mov"),
        "site B must see site A's asset"
    );
    let asset = asset_id_of(&cat_b, &state_b, "site-b", "interview");
    maj(&cat_b, &state_b, "site-b")
        .args(["tag", "add", &asset, "status/select"])
        .assert()
        .success();
    maj(&cat_b, &state_b, "site-b").args(["sync", "push"]).assert().success();

    // The drive travels back. Site A pulls and sees site B's tag.
    maj(&cat_a, &state_a, "site-a").args(["sync", "pull"]).assert().success();
    let out = maj(&cat_a, &state_a, "site-a")
        .args(["search", "tag:status/select"])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&out.get_output().stdout).contains("interview.mov"),
        "site A must see site B's tag after the shuttle round trip"
    );
}

/// The asset id of the first search hit, via --json output.
fn asset_id_of(catalog: &Path, state: &Path, machine: &str, query: &str) -> String {
    let out = maj(catalog, state, machine)
        .args(["search", query, "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).expect("json");
    v["results"][0]["asset"]
        .as_str()
        .expect("asset id in search json")
        .to_string()
}
```

Before running, check the actual `search --json` output shape with `rg '"results"' crates/cli/src/search.rs` — if the key differs (e.g. `hits`), match the real shape, and verify the tag-filter syntax `tag:status/select` against `crates/cli/src/query.rs`.

- [ ] **Step 4: Run tests, lint, commit**

Run: `cargo test -p majestical-sync --test convergence && cargo test -p majestical-cli --test sync_smoke`
Expected: PASS.

```bash
just check
git add crates/sync/Cargo.toml crates/sync/tests/convergence.rs crates/cli/tests/sync_smoke.rs
git commit -m "test: sync convergence property and shuttle-drive e2e"
```

---

### Task 9: `contribution.json` manifest types + validation

**Files:**
- Create: `crates/cli/src/inbox_cmd.rs`
- Modify: `crates/cli/src/main.rs` (`mod inbox_cmd;`)

- [ ] **Step 1: Write the failing tests**

`crates/cli/src/inbox_cmd.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_json(files: &str) -> String {
        format!(
            r#"{{"version":1,"contributor":"dana","para_target":"Projects/spring","source":"iphone","files":{files}}}"#
        )
    }

    #[test]
    fn a_complete_contribution_is_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"abcd").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        assert_eq!(manifest.contributor, "dana");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert!(check.waiting.is_empty());
        assert!(check.unlisted.is_empty());
    }

    #[test]
    fn short_and_missing_files_mean_still_uploading() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"ab").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(
                r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4},
                    {"name":"IMG_2.HEIC","xxh64":"deadbeef00000001","size":9}]"#,
            ),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(check.waiting.len(), 2, "one short, one missing: {:?}", check.waiting);
    }

    #[test]
    fn unlisted_files_are_reported_never_absorbed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("IMG_1.HEIC"), b"abcd").expect("write");
        std::fs::write(dir.path().join("stray.mov"), b"x").expect("write");
        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"IMG_1.HEIC","xxh64":"deadbeef00000000","size":4}]"#),
        )
        .expect("write");
        let manifest = load_manifest(dir.path()).expect("load").expect("present");
        let check = check_files(dir.path(), &manifest).expect("check");
        assert_eq!(check.unlisted, vec!["stray.mov".to_string()]);
    }

    #[test]
    fn unknown_version_and_traversal_names_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("contribution.json"),
            r#"{"version":99,"contributor":"dana","files":[]}"#,
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("unknown version must fail");
        assert!(err.to_string().contains("version 99"), "{err}");
        assert!(err.to_string().contains("supports version 1"), "{err}");

        std::fs::write(
            dir.path().join("contribution.json"),
            manifest_json(r#"[{"name":"../escape.mov","xxh64":"00","size":1}]"#),
        )
        .expect("write");
        let err = load_manifest(dir.path()).expect_err("traversal must fail");
        assert!(err.to_string().contains("escape"), "{err}");
    }

    #[test]
    fn a_folder_without_a_manifest_loads_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_manifest(dir.path()).expect("load").is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `mod inbox_cmd;` to `main.rs` (alphabetical, after `index_cmd`).
Run: `cargo test -p majestical-cli inbox_cmd`
Expected: COMPILE FAIL.

- [ ] **Step 3: Implement**

```rust
//! `maj inbox process`: one converging pass over a shared drop folder.
//! Contribution = a subfolder with a `contribution.json` manifest (the
//! documented integration point for the share-sheet Shortcut and future
//! iOS app); manifest-less drops go to a triage PARA node after a
//! quiescence check. Reuses the verified-ingest pipeline end to end.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_NAME: &str = "contribution.json";
const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ContributionManifest {
    pub version: u32,
    pub contributor: String,
    #[serde(default)]
    pub para_target: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // capture context, carried for future surfacing
    pub note: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ManifestFile {
    pub name: String,
    pub xxh64: String,
    pub size: u64,
}

/// `Ok(None)` when the folder has no manifest (the manifest-less path).
/// Unknown versions and path-traversal names are hard errors — a manifest
/// we cannot fully honor must never be half-honored.
pub(crate) fn load_manifest(dir: &Path) -> Result<Option<ContributionManifest>> {
    let path = dir.join(MANIFEST_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let manifest: ContributionManifest =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    anyhow::ensure!(
        manifest.version == SUPPORTED_VERSION,
        "{} is version {} — this maj supports version {SUPPORTED_VERSION}; upgrade maj or re-export the contribution",
        path.display(),
        manifest.version
    );
    for file in &manifest.files {
        let name_path = Path::new(&file.name);
        let escapes = name_path.is_absolute()
            || name_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        anyhow::ensure!(
            !escapes,
            "manifest entry '{}' escapes the contribution folder — refusing the whole contribution",
            file.name
        );
    }
    Ok(Some(manifest))
}

/// Presence/size gate ("still uploading" detection) + the unlisted-file
/// report. Hash checking is deliberately NOT here — it reads every byte,
/// so it runs once, later, only on contributions that pass this gate.
pub(crate) struct FileCheck {
    /// Human-readable per-file reasons the contribution isn't ready.
    pub waiting: Vec<String>,
    /// Files in the folder the manifest doesn't list (reported, left
    /// untouched, never ingested from a manifested contribution).
    pub unlisted: Vec<String>,
}

pub(crate) fn check_files(dir: &Path, manifest: &ContributionManifest) -> Result<FileCheck> {
    let mut waiting = Vec::new();
    let mut listed = std::collections::BTreeSet::new();
    for file in &manifest.files {
        listed.insert(file.name.clone());
        let path = dir.join(&file.name);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() == file.size => {}
            Ok(meta) => waiting.push(format!(
                "{}: {} of {} bytes present — still uploading?",
                file.name,
                meta.len(),
                file.size
            )),
            Err(_) => waiting.push(format!("{}: not yet present", file.name)),
        }
    }
    let mut unlisted = Vec::new();
    collect_unlisted(dir, dir, &listed, &mut unlisted)?;
    Ok(FileCheck { waiting, unlisted })
}

fn collect_unlisted(
    root: &Path,
    dir: &Path,
    listed: &std::collections::BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_unlisted(root, &path, listed, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if rel != MANIFEST_NAME && !listed.contains(&rel) {
            out.push(rel);
        }
    }
    out.sort();
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p majestical-cli inbox_cmd` — Expected: PASS.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/cli/src/inbox_cmd.rs crates/cli/src/main.rs
git commit -m "feat: contribution.json manifest schema and validation"
```

---

### Task 10: `maj inbox process` — manifested flow

> **CONTRACT CHANGES from Task 9's review (supersede the sketches below):**
> 1. `load_manifest` now validates ALL contributor-controlled strings
>    (contributor/source/para_target: no absolute, no `..`, non-empty;
>    xxh64: 16 lowercase hex) — Task 10 must NOT re-validate, and the
>    `subdir` construction interpolating `manifest.contributor` is safe
>    ONLY because of that load-time guard; do not bypass `load_manifest`.
> 2. `check_files` returns `FileCheck { waiting, unlisted, refused }` —
>    refusals are VALUES (map to the contribution's Failed outcome with
>    the reasons), and `Err` from it is pass-fatal I/O only (propagate).
>    The sketch's `check_files(dir, manifest)?` + waiting-only handling
>    below predates this; adapt accordingly.

**Files:**
- Modify: `crates/cli/src/commands.rs` (extract `run_ingest` from `cmd_ingest`)
- Modify: `crates/cli/src/inbox_cmd.rs`, `crates/cli/src/main.rs`
- Test: `crates/cli/tests/inbox_smoke.rs` (create)

- [ ] **Step 1: Refactor `cmd_ingest` (no behavior change)**

In `commands.rs`: split `cmd_ingest` so the engine-running middle is callable with its outcome returned. Move everything from the `run_id` line through `print_ingest_outcome` into:

```rust
/// The verified-copy pipeline shared by `maj ingest` and `maj inbox
/// process`: journal + engine + ASC MHL generations + catalog events +
/// outcome print. The caller has already planned and resolved the PARA
/// node. Returns the outcome so callers can act on what was placed
/// (inbox adds provenance tags); the failure `ensure` stays with the
/// caller so inbox can fail one contribution without aborting its pass.
pub(crate) struct ExecuteIngest<'a> {
    pub plan: &'a plan::IngestPlan,
    pub dest: &'a [PathBuf],
    pub subdir: &'a str,
    pub node_id: &'a str,
    pub source_volume: (&'a str, &'a str),
    pub jobs: Option<usize>,
    pub resume: Option<&'a str>,
    pub json: bool,
}

pub(crate) fn run_ingest(
    app: &mut FsApp,
    catalog_dir: &Path,
    exec: &ExecuteIngest<'_>,
) -> Result<engine::Outcome> {
    let run_id = exec
        .resume
        .map(str::to_string)
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());
    if exec.resume.is_some() {
        check_resume_journal_exists(catalog_dir, &run_id)?;
    }
    eprintln!("run {run_id} — resume with: --resume {run_id}");
    let dests = build_dest_specs(exec.dest, exec.subdir);
    let outcome = run_ingest_engine(catalog_dir, &run_id, exec.plan, &dests, exec.jobs)?;
    let hashdate_ms = physical_now_ms();
    let hashdate = iso8601_ms(hashdate_ms);
    let generations = write_ingest_generations(&dests, &outcome, &hashdate)
        .context("writing ASC MHL generations")?;
    let dest_volumes = dest_volume_identities(exec.dest);
    let mut ops = volume_seen_ops(
        (exec.source_volume.0, exec.source_volume.1),
        &dest_volumes,
    );
    ops.extend(asset_and_para_ops(&outcome, &dest_volumes, exec.node_id, hashdate_ms));
    ops.extend(manifest_ops(&dest_volumes, &generations));
    app.emit(ops)?;
    print_ingest_outcome(&run_id, &outcome, &generations, exec.json);
    anyhow::ensure!(
        outcome.failed.is_empty() && outcome.rejected.is_empty() && outcome.diagnostics.is_empty(),
        "ingest run {run_id}: {} failed, {} rejected, {} diagnostic(s)",
        outcome.failed.len(),
        outcome.rejected.len(),
        outcome.diagnostics.len()
    );
    Ok(outcome)
}
```

`cmd_ingest` keeps its plan/dry-run head, then delegates:

```rust
    let outcome = run_ingest(
        app,
        catalog_dir,
        &ExecuteIngest {
            plan: &ingest_plan,
            dest: &args.dest,
            subdir: &subdir,
            node_id: &node_id,
            source_volume: (&source_volume_id, &source_volume_label),
            jobs: args.jobs,
            resume: args.resume.as_deref(),
            json: args.json,
        },
    )?;
    let _ = outcome;
    Ok(())
```

(Adjust exactly to what the current tail of `cmd_ingest` holds — the `ensure` moved INTO `run_ingest`, so `cmd_ingest` ends at the delegate call. Match signatures precisely; `volume_seen_ops` currently takes `(&str, &str)` via `(&source_volume_id, &source_volume_label)` — check its real signature before wiring.)

Run: `cargo test -p majestical-cli` — Expected: PASS (pure refactor; the ingest smoke tests in `cli_smoke.rs` are the guard).

Commit:

```bash
just check
git add crates/cli/src/commands.rs
git commit -m "refactor: extract run_ingest so inbox can reuse verified ingest"
```

- [ ] **Step 2: Write the failing e2e test**

Create `crates/cli/tests/inbox_smoke.rs`:

```rust
//! `maj inbox process` end to end: real manifests, real verified ingest,
//! real ASC MHL, real provenance tags.
use assert_cmd::Command;
use std::path::Path;

fn maj(catalog: &Path, state: &Path) -> Command {
    let mut cmd = Command::cargo_bin("maj").expect("binary");
    cmd.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", "inbox-m1")
        .env("MAJ_STATE_DIR", state);
    cmd
}

fn xxh64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh64::xxh64(bytes, 0))
}

struct Setup {
    root: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let s = Self { root };
        maj(&s.catalog(), &s.state())
            .args(["catalog", "init"])
            .assert()
            .success();
        maj(&s.catalog(), &s.state())
            .args(["para", "add", "project", "spring"])
            .assert()
            .success();
        std::fs::create_dir_all(s.inbox()).expect("mkdir");
        std::fs::create_dir_all(s.dest()).expect("mkdir");
        s
    }
    fn catalog(&self) -> std::path::PathBuf { self.root.path().join("cat") }
    fn state(&self) -> std::path::PathBuf { self.root.path().join("state") }
    fn inbox(&self) -> std::path::PathBuf { self.root.path().join("inbox") }
    fn dest(&self) -> std::path::PathBuf { self.root.path().join("dest") }

    fn write_contribution(&self, folder: &str, payload: &[u8], hash: &str) {
        let dir = self.inbox().join(folder);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("clip.mov"), payload).expect("write");
        let manifest = format!(
            r#"{{"version":1,"contributor":"dana","para_target":"project/spring","source":"iphone","files":[{{"name":"clip.mov","xxh64":"{hash}","size":{}}}]}}"#,
            payload.len()
        );
        std::fs::write(dir.join("contribution.json"), manifest).expect("write manifest");
    }

    fn process(&self) -> assert_cmd::assert::Assert {
        maj(&self.catalog(), &self.state())
            .args(["inbox", "process"])
            .arg(self.inbox())
            .args(["--dest"])
            .arg(self.dest())
            .assert()
    }
}

#[test]
fn a_valid_contribution_ingests_with_provenance_and_moves_to_processed() {
    let s = Setup::new();
    let payload = b"mov-bytes-for-clip";
    s.write_contribution("drop-1", payload, &xxh64_hex(payload));
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("drop-1"), "report names the contribution: {stdout}");
    assert!(
        s.inbox().join(".processed/drop-1/clip.mov").is_file(),
        "success moves the contribution to .processed/"
    );
    assert!(!s.inbox().join("drop-1").exists());
    // Real MHL was written at the destination.
    let ascmhl = s.dest().join("ascmhl");
    assert!(ascmhl.is_dir(), "verified ingest writes an ASC MHL history");
    // Provenance tags are searchable.
    for query in ["tag:contributor/dana", "tag:source/iphone"] {
        let out = maj(&s.catalog(), &s.state())
            .args(["search", query])
            .assert()
            .success();
        let found = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(found.contains("clip.mov"), "{query} must find the clip: {found}");
    }
    // A second pass is a clean no-op: .processed/ is skipped.
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("nothing to process"), "{stdout}");
}

#[test]
fn keep_leaves_the_contribution_and_a_redrop_dedupes() {
    let s = Setup::new();
    let payload = b"mov-bytes-for-clip";
    s.write_contribution("drop-keep", payload, &xxh64_hex(payload));
    maj(&s.catalog(), &s.state())
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .args(["--keep"])
        .assert()
        .success();
    assert!(
        s.inbox().join("drop-keep/clip.mov").is_file(),
        "--keep must leave the contribution in place"
    );
    // The same content dropped again (new folder) dedupes: the planner's
    // content-hash prefilter marks it duplicate/skip, so nothing re-copies
    // but the pass still succeeds.
    s.write_contribution("drop-again", payload, &xxh64_hex(payload));
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("drop-again"), "{stdout}");
    let copies = walkdir_count(&s.dest(), "clip.mov");
    assert_eq!(copies, 1, "a re-dropped duplicate must not copy again");
}

/// Counts files named `name` under `root`, recursively.
fn walkdir_count(root: &Path, name: &str) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if entry.file_name().to_string_lossy() == name {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn hash_mismatch_fails_the_contribution_records_it_and_skips_next_pass() {
    let s = Setup::new();
    s.write_contribution("drop-bad", b"actual-bytes", "0000000000000000");
    let out = s.process().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("clip.mov") && stderr.contains("0000000000000000"),
        "failure must name the file and both hashes: {stderr}"
    );
    assert!(
        s.inbox().join("drop-bad/clip.mov").is_file(),
        "a failed contribution is left untouched in the inbox"
    );
    assert!(
        !s.dest().join("ascmhl").exists(),
        "nothing from a failed contribution may be ingested"
    );
    // Second pass: skipped via the recorded marker, not re-hashed; the
    // pass itself succeeds (a recorded failure is a notice, not an error).
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("drop-bad") && stdout.contains("recorded failure"),
        "second pass must skip with the recorded reason: {stdout}"
    );
}

#[test]
fn incomplete_upload_is_skipped_and_converges_when_complete() {
    let s = Setup::new();
    let payload = b"full-payload-bytes";
    let dir = s.inbox().join("drop-slow");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("clip.mov"), &payload[..4]).expect("partial write");
    let manifest = format!(
        r#"{{"version":1,"contributor":"dana","para_target":"project/spring","files":[{{"name":"clip.mov","xxh64":"{}","size":{}}}]}}"#,
        xxh64_hex(payload),
        payload.len()
    );
    std::fs::write(dir.join("contribution.json"), manifest).expect("write");
    let out = s.process().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("still uploading") || stdout.contains("not yet present"),
        "incomplete upload skips with the reason: {stdout}"
    );
    // Upload completes; the next pass converges.
    std::fs::write(dir.join("clip.mov"), payload).expect("complete write");
    s.process().success();
    assert!(s.inbox().join(".processed/drop-slow/clip.mov").is_file());
}
```

Add `xxhash-rust` to `crates/cli/Cargo.toml` `[dev-dependencies]` if not present (it is a workspace dependency already — `xxhash-rust.workspace = true`; check the workspace declares the `xxh64` feature; `state_dir.rs` uses `xxh3`, so the features list may need `"xxh64"` added in the workspace `Cargo.toml`).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p majestical-cli --test inbox_smoke`
Expected: FAIL — `maj inbox` unknown.

- [ ] **Step 4: Implement the manifested flow**

In `inbox_cmd.rs`:

```rust
use crate::app::FsApp;
use crate::commands::{self, ExecuteIngest};
use majestical_ingest::{hashing, plan};

/// Bundles the flags within the house 5-positional-parameter limit.
pub(crate) struct InboxArgs {
    pub inbox: PathBuf,
    pub dest: Vec<PathBuf>,
    pub triage_target: Option<String>,
    pub keep: bool,
    pub json: bool,
}

/// Per-machine record of contributions that failed hash validation, so a
/// later pass skips them with a notice instead of re-hashing forever.
/// Keyed by (inbox identity, folder name) — inbox identity = xxh3-128 of
/// the canonicalized inbox path, the state_dir pattern — because two
/// inboxes sharing one catalog with same-named folders would otherwise
/// evict each other's markers and oscillate (Task 10 review finding;
/// as-built supersedes the bare-name sketch below). Cleared automatically
/// when the manifest OR any listed file's (mtime, size) changes — a
/// re-upload re-validates.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FailureMarkers {
    #[serde(default)]
    failures: std::collections::BTreeMap<String, FailureMarker>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FailureMarker {
    reason: String,
    manifest_mtime_ms: u64,
    manifest_size: u64,
}

fn markers_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("inbox-failures.json"))
}

fn load_markers(catalog: &Path) -> Result<FailureMarkers> {
    let path = markers_path(catalog)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FailureMarkers::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn store_markers(catalog: &Path, markers: &FailureMarkers) -> Result<()> {
    let path = markers_path(catalog)?;
    std::fs::write(&path, serde_json::to_string_pretty(markers)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn manifest_fingerprint(dir: &Path) -> (u64, u64) {
    std::fs::metadata(dir.join(MANIFEST_NAME))
        .map(|m| (crate::commands::mtime_ms_of(&m), m.len()))
        .unwrap_or((0, 0))
}

/// One contribution's outcome for the pass report.
enum ContribOutcome {
    Ingested { files: usize },
    Waiting { reasons: Vec<String> },
    RecordedFailure { reason: String },
    Failed { reason: String },
}

pub(crate) fn cmd_inbox_process(
    app: &mut FsApp,
    catalog: &Path,
    args: &InboxArgs,
) -> Result<()> {
    anyhow::ensure!(
        args.inbox.is_dir(),
        "inbox must be a directory: {}",
        args.inbox.display()
    );
    let mut markers = load_markers(catalog)?;
    let mut report: Vec<(String, ContribOutcome)> = Vec::new();
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&args.inbox)
        .with_context(|| format!("reading {}", args.inbox.display()))?
    {
        let entry = entry.with_context(|| format!("reading {}", args.inbox.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // .processed/, .DS_Store, sync-tool droppings
        }
        entries.push(path);
    }
    entries.sort();
    for path in &entries {
        if !path.is_dir() {
            continue; // bare files: Task 11's manifest-less flow
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load_manifest(path) {
            Ok(Some(manifest)) => {
                let outcome =
                    process_contribution(app, catalog, args, path, &manifest, &mut markers)?;
                report.push((name, outcome));
            }
            Ok(None) => {} // manifest-less: Task 11
            Err(e) => report.push((name, ContribOutcome::Failed { reason: format!("{e:#}") })),
        }
    }
    store_markers(catalog, &markers)?;
    print_report(&report, args.json)
}

fn process_contribution(
    app: &mut FsApp,
    catalog: &Path,
    args: &InboxArgs,
    dir: &Path,
    manifest: &ContributionManifest,
    markers: &mut FailureMarkers,
) -> Result<ContribOutcome> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fingerprint = manifest_fingerprint(dir);
    if let Some(marker) = markers.failures.get(&name) {
        if (marker.manifest_mtime_ms, marker.manifest_size) == fingerprint {
            return Ok(ContribOutcome::RecordedFailure { reason: marker.reason.clone() });
        }
        markers.failures.remove(&name); // manifest changed — re-validate
    }
    let check = check_files(dir, manifest)?;
    if !check.waiting.is_empty() {
        return Ok(ContribOutcome::Waiting { reasons: check.waiting });
    }
    for unlisted in &check.unlisted {
        eprintln!(
            "note: {name}/{unlisted} is not in the manifest — left untouched, not ingested"
        );
    }
    // End-to-end hash gate: the contributor's client-side xxh64 against a
    // fresh read of what actually arrived. Any mismatch fails the WHOLE
    // contribution before a single byte is copied.
    for file in &manifest.files {
        let path = dir.join(&file.name);
        let computed = hashing::xxh64_file(&path)
            .with_context(|| format!("hashing {}", path.display()))?;
        if computed != file.xxh64 {
            let reason = format!(
                "{}: manifest says xxh64 {} but the file hashes to {computed} — corrupt in transit or a stale manifest; re-upload it or remove the folder",
                file.name, file.xxh64
            );
            markers.failures.insert(
                name.clone(),
                FailureMarker {
                    reason: reason.clone(),
                    manifest_mtime_ms: fingerprint.0,
                    manifest_size: fingerprint.1,
                },
            );
            return Ok(ContribOutcome::Failed { reason });
        }
    }
    let para = manifest.para_target.as_deref().with_context(|| {
        format!("{name}: manifest has no para_target and no default exists — add one or use the manifest-less triage path")
    })?;
    ingest_contribution(app, catalog, args, dir, manifest, para)?;
    if !args.keep {
        move_to_processed(&args.inbox, dir)?;
    }
    Ok(ContribOutcome::Ingested { files: manifest.files.len() })
}
```

The ingest + tag step (same file):

```rust
fn ingest_contribution(
    app: &mut FsApp,
    catalog: &Path,
    args: &InboxArgs,
    dir: &Path,
    manifest: &ContributionManifest,
    para: &str,
) -> Result<()> {
    let projection = app.projection()?;
    let node_id = commands::resolve_ingest_node_pub(&projection, para)?;
    let known = commands::known_assets_pub(&projection);
    let ingest_plan = plan::plan_source(dir, &known, plan::DedupeMode::Skip)
        .with_context(|| format!("planning contribution {}", dir.display()))?;
    let (vol_id, vol_label) = commands::resolve_volume(dir, None);
    let subdir = format!(
        "{}/{}",
        node_dir_prefix(&node_id.1, &node_id.2),
        manifest.contributor
    );
    let outcome = commands::run_ingest(
        app,
        catalog,
        &ExecuteIngest {
            plan: &ingest_plan,
            dest: &args.dest,
            subdir: &subdir,
            node_id: &node_id.0,
            source_volume: (&vol_id, &vol_label),
            jobs: None,
            resume: None,
            json: args.json,
        },
    )?;
    // Provenance: contributor + optional source, as plain TagAdds on every
    // distinct placed asset — no new op variants this phase.
    let mut ops = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for placed in &outcome.placed {
        let asset = majestical_core::event::AssetId(format!("xxh3:{}", placed.xxh3));
        if !seen.insert(asset.clone()) {
            continue;
        }
        ops.push(majestical_core::event::Op::TagAdd {
            asset: asset.clone(),
            tag: format!("contributor/{}", manifest.contributor),
        });
        if let Some(source) = &manifest.source {
            ops.push(majestical_core::event::Op::TagAdd {
                asset,
                tag: format!("source/{source}"),
            });
        }
    }
    app.emit(ops)?;
    Ok(())
}

/// Atomic rename into `.processed/`, numeric suffix on collision.
fn move_to_processed(inbox: &Path, dir: &Path) -> Result<()> {
    let processed = inbox.join(".processed");
    std::fs::create_dir_all(&processed)
        .with_context(|| format!("creating {}", processed.display()))?;
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut target = processed.join(&name);
    let mut suffix = 2u32;
    while target.exists() {
        target = processed.join(format!("{name}-{suffix}"));
        suffix += 1;
    }
    std::fs::rename(dir, &target)
        .with_context(|| format!("moving {} to {}", dir.display(), target.display()))
}
```

Supporting changes in `commands.rs`: `resolve_ingest_node` and `known_assets_from_projection` are private — export thin `pub(crate)` aliases `resolve_ingest_node_pub` / `known_assets_pub`, OR (better, house style "replace, don't wrap") just change their visibility to `pub(crate)` and use the originals from `inbox_cmd`. Do the latter; the `_pub` names above then become `commands::resolve_ingest_node` / `commands::known_assets_from_projection`. `resolve_ingest_node` returns `(node_id, kind, name)` — the `node_dir_prefix(&node_id.1, &node_id.2)` call above is the same kind/name → directory mapping `render_ingest_subdir` uses internally; reuse the existing template machinery instead if simpler: `render_ingest_subdir(kind, &name, "{date}/{source-label}", &vol_label)` mirrors `maj ingest`'s default layout. Prefer that — delete `node_dir_prefix` from the sketch and pass the rendered subdir. The contributor lands as a tag, not a directory, keeping inbox layout identical to manual ingest.

`print_report` (same file):

```rust
fn print_report(report: &[(String, ContribOutcome)], json: bool) -> Result<()> {
    let mut any_failed = false;
    if json {
        let rows: Vec<serde_json::Value> = report
            .iter()
            .map(|(name, outcome)| match outcome {
                ContribOutcome::Ingested { files } =>
                    serde_json::json!({"contribution": name, "status": "ingested", "files": files}),
                ContribOutcome::Waiting { reasons } =>
                    serde_json::json!({"contribution": name, "status": "waiting", "reasons": reasons}),
                ContribOutcome::RecordedFailure { reason } =>
                    serde_json::json!({"contribution": name, "status": "recorded_failure", "reason": reason}),
                ContribOutcome::Failed { reason } => {
                    serde_json::json!({"contribution": name, "status": "failed", "reason": reason})
                }
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if report.is_empty() {
        println!("nothing to process");
    } else {
        for (name, outcome) in report {
            match outcome {
                ContribOutcome::Ingested { files } => println!("{name}: ingested {files} file(s)"),
                ContribOutcome::Waiting { reasons } => {
                    println!("{name}: waiting — {}", reasons.join("; "));
                }
                ContribOutcome::RecordedFailure { reason } => {
                    println!("{name}: skipped (recorded failure) — {reason}");
                }
                ContribOutcome::Failed { reason } => println!("{name}: FAILED — {reason}"),
            }
        }
    }
    for (_, outcome) in report {
        if matches!(outcome, ContribOutcome::Failed { .. }) {
            any_failed = true;
        }
    }
    anyhow::ensure!(!any_failed, "one or more contributions failed — see the report above");
    Ok(())
}
```

Note the exit policy the tests pin: a FRESH failure fails the pass (nonzero); a previously RECORDED failure only notices (zero). Also fix the JSON path: with `--json` the fresh-failure detail must reach stderr too for the smoke test's stderr assertions — simplest is `eprintln!("{name}: {reason}")` at the point of `ContribOutcome::Failed` creation in `process_contribution` (before returning), unconditionally. The e2e asserts stderr regardless of `--json`.

Wire clap in `main.rs`:

```rust
    /// Process a shared inbox folder: validated contributions plus
    /// manifest-less drops.
    Inbox {
        #[command(subcommand)]
        cmd: InboxCmd,
    },
```

```rust
#[derive(Subcommand)]
enum InboxCmd {
    /// One converging pass: validate, verified-ingest, tag provenance,
    /// move to .processed/.
    Process {
        inbox: PathBuf,
        /// Destination root(s), like `maj ingest --dest`.
        #[arg(long, required = true)]
        dest: Vec<PathBuf>,
        /// PARA node for manifest-less drops (required if any exist).
        #[arg(long)]
        triage_target: Option<String>,
        /// Leave processed contributions in place.
        #[arg(long)]
        keep: bool,
        #[arg(long)]
        json: bool,
    },
}
```

Dispatch: open `FsApp` and call `inbox_cmd::cmd_inbox_process(&mut app, &cli.catalog, &args)`.

- [ ] **Step 5: Run tests, lint, commit**

Run: `cargo test -p majestical-cli --test inbox_smoke` — Expected: PASS (all four).
Run: `cargo test -p majestical-cli` — Expected: PASS.

```bash
just check
git add crates/cli/src/inbox_cmd.rs crates/cli/src/commands.rs crates/cli/src/main.rs crates/cli/tests/inbox_smoke.rs crates/cli/Cargo.toml Cargo.toml
git commit -m "feat: maj inbox process for manifested contributions"
```

---

### Task 11: Manifest-less triage flow + quiescence

**Files:**
- Modify: `crates/cli/src/inbox_cmd.rs`
- Test: `crates/cli/tests/inbox_smoke.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn manifest_less_drops_triage_after_quiescence() {
    let s = Setup::new();
    maj(&s.catalog(), &s.state())
        .args(["para", "add", "resource", "inbox-triage"])
        .assert()
        .success();
    // A manifest-less folder and a bare top-level file.
    let folder = s.inbox().join("beach-shoot");
    std::fs::create_dir_all(&folder).expect("mkdir");
    std::fs::write(folder.join("wave.heic"), b"heic-bytes").expect("write");
    std::fs::write(s.inbox().join("loose.jpg"), b"jpg-bytes").expect("write");

    // Not yet quiescent (default 5 min): both are skipped.
    let out = maj(&s.catalog(), &s.state())
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .args(["--triage-target", "resource/inbox-triage"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("quiesce"), "young files must wait: {stdout}");

    // Quiescence window forced to zero: both ingest to triage.
    maj(&s.catalog(), &s.state())
        .env("MAJ_INBOX_QUIESCENCE_MS", "0")
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .args(["--triage-target", "resource/inbox-triage"])
        .assert()
        .success();
    for query in ["wave.heic", "loose.jpg"] {
        let out = maj(&s.catalog(), &s.state())
            .args(["search", &format!("{query} tag:source/inbox")])
            .assert()
            .success();
        let found = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(found.contains(query), "{query} must be triaged: {found}");
    }
}

#[test]
fn manifest_less_items_without_a_triage_target_are_an_error() {
    let s = Setup::new();
    std::fs::write(s.inbox().join("loose.jpg"), b"jpg-bytes").expect("write");
    let out = maj(&s.catalog(), &s.state())
        .env("MAJ_INBOX_QUIESCENCE_MS", "0")
        .args(["inbox", "process"])
        .arg(s.inbox())
        .args(["--dest"])
        .arg(s.dest())
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("--triage-target"),
        "the error must name the missing flag: {stderr}"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-cli --test inbox_smoke manifest_less`
Expected: FAIL — manifest-less entries are currently silently skipped.

- [ ] **Step 3: Implement**

In `inbox_cmd.rs`:

```rust
/// Default quiescence window for manifest-less drops: nothing younger
/// than this is touched, so a mid-upload file is never grabbed.
/// `MAJ_INBOX_QUIESCENCE_MS` overrides (tests; impatient users).
const QUIESCENCE_MS: u64 = 5 * 60 * 1000;

fn quiescence_ms() -> u64 {
    std::env::var("MAJ_INBOX_QUIESCENCE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(QUIESCENCE_MS)
}

/// Newest mtime under `path` (a file's own, or the max across a folder's
/// contents). `u64::MAX` when unreadable — unreadable means "not ready".
fn newest_mtime_ms(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return u64::MAX;
    };
    if meta.is_file() {
        return crate::commands::mtime_ms_of(&meta);
    }
    let mut newest = crate::commands::mtime_ms_of(&meta);
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            newest = newest.max(newest_mtime_ms(&entry.path()));
        }
    }
    newest
}

fn is_quiescent(path: &Path) -> bool {
    let newest = newest_mtime_ms(path);
    let now = crate::app::physical_now_ms();
    now.saturating_sub(newest) >= quiescence_ms()
}
```

In `cmd_inbox_process`'s entry loop, replace the two skip comments:

- Bare top-level files: collect into `let mut loose_files: Vec<PathBuf>`.
- `Ok(None)` manifest-less folders: collect into `let mut triage_dirs: Vec<PathBuf>`.

After the manifested loop, process them:

```rust
    let quiescent_dirs: Vec<&PathBuf> = triage_dirs.iter().filter(|d| is_quiescent(d)).collect();
    let quiescent_files: Vec<&PathBuf> = loose_files.iter().filter(|f| is_quiescent(f)).collect();
    let waiting_count =
        (triage_dirs.len() - quiescent_dirs.len()) + (loose_files.len() - quiescent_files.len());
    if waiting_count > 0 {
        report.push((
            "(manifest-less)".to_string(),
            ContribOutcome::Waiting {
                reasons: vec![format!(
                    "{waiting_count} item(s) modified within the last {}s — letting them quiesce",
                    quiescence_ms() / 1000
                )],
            },
        ));
    }
    if !quiescent_dirs.is_empty() || !quiescent_files.is_empty() {
        let triage = args.triage_target.as_deref().context(
            "manifest-less items are in the inbox but no --triage-target was given — pass one (e.g. --triage-target resource/inbox-triage); nothing is invented silently",
        )?;
        for dir in quiescent_dirs {
            let outcome = triage_ingest(app, catalog, args, dir, triage)?;
            report.push((display_name(dir), outcome));
            if !args.keep && !matches!(report.last(), Some((_, ContribOutcome::Failed { .. }))) {
                move_to_processed(&args.inbox, dir)?;
            }
        }
        if !quiescent_files.is_empty() {
            let outcome = triage_loose_files(app, catalog, args, &quiescent_files, triage)?;
            report.push(("(loose files)".to_string(), outcome));
        }
    }
```

`triage_ingest` mirrors `ingest_contribution` with `source/inbox` as the only tag and no contributor. For loose files, `plan_source` needs a directory — plan the INBOX ROOT and filter the plan to exactly the quiescent top-level files:

```rust
fn triage_loose_files(
    app: &mut FsApp,
    catalog: &Path,
    args: &InboxArgs,
    files: &[&PathBuf],
    triage: &str,
) -> Result<ContribOutcome> {
    let projection = app.projection()?;
    let node = commands::resolve_ingest_node(&projection, triage)?;
    let known = commands::known_assets_from_projection(&projection);
    let mut ingest_plan =
        plan::plan_source(&args.inbox, &known, plan::DedupeMode::Skip)
            .with_context(|| format!("planning inbox {}", args.inbox.display()))?;
    let wanted: std::collections::BTreeSet<String> = files
        .iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    ingest_plan.files.retain(|f| wanted.contains(&f.rel));
    run_triage_ingest(app, catalog, args, &ingest_plan, &node)?;
    for file in files {
        if !args.keep {
            move_file_to_processed(&args.inbox, file)?;
        }
    }
    Ok(ContribOutcome::Ingested { files: wanted.len() })
}
```

`run_triage_ingest` = the shared tail (render subdir with the node's kind/name + `{date}/{source-label}` template, `commands::run_ingest`, then one `TagAdd source/inbox` per distinct placed asset). `move_file_to_processed` renames a single file into `.processed/` with the same collision suffixing — extract the suffix loop from `move_to_processed` into a `fn processed_target(processed: &Path, name: &str) -> PathBuf` both use. Factor `ingest_contribution` and `run_triage_ingest` to share their common body (projection → node → plan → run → tags) as one function taking the tag list — three near-copies of ingest orchestration in one file is exactly what the code-quality reviewer will flag.

- [ ] **Step 4: Run tests, lint, commit**

Run: `cargo test -p majestical-cli --test inbox_smoke` — Expected: PASS (all six).

```bash
just check
git add crates/cli/src/inbox_cmd.rs crates/cli/tests/inbox_smoke.rs
git commit -m "feat: manifest-less inbox triage with quiescence gate"
```

---

### Task 12: Inbox cucumber acceptance

**Files:**
- Create: `crates/cli/tests/features/inbox.feature`, `crates/cli/tests/inbox_acceptance.rs`
- Modify: `crates/cli/Cargo.toml` (`[[test]]` entry)

- [ ] **Step 1: Write the feature file**

Check how the existing cucumber binary is declared: `rg -n 'acceptance' crates/cli/Cargo.toml` — mirror that `[[test]]` block (`name = "inbox_acceptance"`, `harness = false`). `crates/cli/tests/features/inbox.feature`:

```gherkin
Feature: Inbox contributions
  A shared drop folder becomes cataloged, verified, provenance-tagged media.

  Scenario: A manifested contribution is ingested with provenance
    Given a catalog with a PARA project "spring"
    And a contribution "drop-1" of 2 files from contributor "dana" targeting "project/spring"
    When I process the inbox
    Then the report says "drop-1" was ingested with 2 files
    And searching "tag:contributor/dana" finds both files
    And the contribution folder has moved to ".processed"

  Scenario: An incomplete upload waits and converges
    Given a catalog with a PARA project "spring"
    And a contribution "drop-2" whose manifest promises a file that is short on disk
    When I process the inbox
    Then the report says "drop-2" is waiting
    When the file finishes uploading
    And I process the inbox
    Then the report says "drop-2" was ingested with 1 files

  Scenario: A hash mismatch is recorded once and skipped after
    Given a catalog with a PARA project "spring"
    And a contribution "drop-3" whose manifest hash does not match the file
    When I process the inbox expecting failure
    Then the report names the mismatched file and both hashes
    When I process the inbox
    Then the report says "drop-3" was skipped with a recorded failure

  Scenario: An unknown manifest version is skipped with a named remedy
    Given a catalog with a PARA project "spring"
    And a contribution "drop-4" with manifest version 99
    When I process the inbox expecting failure
    Then the report names version 99 and the supported version 1

  Scenario: Manifest-less drops triage after quiescence
    Given a catalog with a PARA resource "inbox-triage"
    And a quiescent manifest-less folder "beach" holding 1 file
    When I process the inbox with triage target "resource/inbox-triage"
    Then searching "tag:source/inbox" finds the file
```

- [ ] **Step 2: Write the step definitions**

`crates/cli/tests/inbox_acceptance.rs` — follow `acceptance.rs`'s structure exactly (World holding a `TempDir`, `assert_cmd` invocations, steps returning `Result<(), String>`, `harness = false`, a `main` that runs `InboxWorld::cucumber().fail_on_skipped().run_and_exit("tests/features/inbox.feature")` — copy the exact `main` shape from `acceptance.rs`). The World needs: catalog/state/inbox/dest paths, `last_stdout`, `last_stderr`, `last_exit_ok`. Steps compute real xxh64 hashes with `xxhash_rust::xxh64` (dev-dependency added in Task 10) and set `MAJ_INBOX_QUIESCENCE_MS=0` for the quiescent step. "expecting failure" steps assert the command failed; plain "I process" steps assert success. Every `Then` matches on `last_stdout`/`last_stderr` content.

- [ ] **Step 3: Run, lint, commit**

Run: `cargo test -p majestical-cli --test inbox_acceptance`
Expected: PASS, `fail_on_skipped` proving every step ran.

```bash
just check
git add crates/cli/tests/features/inbox.feature crates/cli/tests/inbox_acceptance.rs crates/cli/Cargo.toml
git commit -m "test: cucumber acceptance for inbox contribution flows"
```

---

### Task 13: Closing — wire-format assertion, mutants triage, watchlist

**Files:**
- Modify: `crates/core/tests/` (locate the op-variant assertion), `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`

- [ ] **Step 1: Assert zero new op variants**

Find phase 5's absence assertion: `rg -n "op variants|variant" crates/core/tests/`. Update its count/comment to say phase 6 also added none (sync moves files; inbox emits only pre-existing ops). If the test asserts an exhaustive variant list, no change is needed — verify it still passes and add "phase 6: no additions" to its comment.

Run: `cargo test -p majestical-core`
Expected: PASS.

- [ ] **Step 2: Full gate**

Run: `just ci`
Expected: green. Fix anything, however small — zero warnings.

- [ ] **Step 3: Mutation triage**

Run cargo-mutants on the touched crates (mirror the phase 5 triage flow):

```bash
cargo mutants -p majestical-sync --in-place -j 2 -- --all-features
cargo mutants -p majestical-cli --in-place -j 2 -- --all-features
```

For each missed mutant: add a discriminating test, or record it in the watchlist under a new `cargo-mutants triage (phase 6)` section with the reason it is untestable (display-only, durability-only, etc.).

- [ ] **Step 4: Watchlist + commit**

Add to `docs/superpowers/plans/2026-07-29-phase2-watchlist.md` a "Phase 6 deferrals" section from the spec's Deferred list (SyncTransport port, divergence detection, share-sheet Shortcut, resident watcher, auto-index on pull) plus anything reviewers deferred during execution. Mark the phase-2 "Segment rotation" and "sync's two read paths diverge" open items as "(Done in phase 6)".

```bash
git add crates/core/tests docs/superpowers/plans/2026-07-29-phase2-watchlist.md
git commit -m "test: phase 6 closing — wire-format assertion and mutants triage"
```

---

## Plan self-review notes (already applied)

- Task 2's first test narrative mentions newline padding then corrects to `set_len` NUL padding — the committed test must use the corrected `(2, 1)` expectation.
- Task 8's shuttle test and Task 10/11's search assertions depend on the real `search --json` shape and `tag:` filter syntax — implementers must verify against `crates/cli/src/search.rs` / `query.rs` before trusting the sketched assertions (`maj search` takes ONE positional query string; phase 5's plan made this exact mistake).
- Task 10's `resolve_ingest_node`/`known_assets_from_projection` visibility change replaces the sketched `_pub` aliases — use the original names.
- `scan` positional arg: the smoke tests pass `.args(["scan"]).arg(&media)` — matches the real `Scan { dir }` clap shape.

