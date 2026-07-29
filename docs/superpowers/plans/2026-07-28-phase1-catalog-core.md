# Phase 1: Catalog Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The event-sourced CRDT catalog core with a file-based event log, SQLite projection, and a minimal `maj` CLI that can scan a folder into a catalog, tag assets, and search — with two-machine convergence proven by acceptance tests.

**Architecture:** Hexagonal Rust core (`crates/core`) defining ports; adapters for SQLite projection (`crates/catalog-sqlite`), file event log (`crates/sync`), and CLI (`crates/cli`). All state derives from an append-only event log; the SQLite catalog is a rebuildable projection. Spec: `docs/superpowers/specs/2026-07-28-majestical-design.md` §1–§2, build-order 1–2 (filename search slice).

**Tech Stack:** Rust stable, ulid, serde/serde_json, rusqlite (bundled), xxhash-rust, clap, thiserror, proptest, cucumber, tempfile.

---

### Task 1: Workspace scaffolding

**Files:**
- Create: `Cargo.toml`, `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `justfile`, `.gitignore`

- [ ] **Step 1: Create root workspace `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/core"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"

[workspace.lints.clippy]
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

[profile.release]
lto = true
codegen-units = 1
```

- [ ] **Step 2: Create `crates/core/Cargo.toml` and empty lib**

```toml
[package]
name = "majestical-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
ulid = { version = "1", features = ["serde"] }

[dev-dependencies]
proptest = "1"
```

`crates/core/src/lib.rs`:

```rust
//! Majestical domain core: events, CRDT merge, projection, ports.
pub mod clock;
pub mod event;
pub mod projection;
```

Create empty `clock.rs`, `event.rs`, `projection.rs` (contents in Tasks 2–4; for now each file contains only `//! placeholder module` so the workspace compiles — Tasks 2–4 replace them entirely).

- [ ] **Step 3: Create `justfile` and `.gitignore`**

```make
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

ci: check test
```

`.gitignore`: `target/`

- [ ] **Step 4: Verify build**

Run: `cargo check --workspace && just ci`
Expected: clean pass (no tests yet).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: scaffold cargo workspace with lint policy"
```

---

### Task 2: Clock port + Hybrid Logical Clock

**Files:**
- Create: `crates/core/src/clock.rs` (replace placeholder)

- [ ] **Step 1: Write failing tests** (bottom of `clock.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn wall_ms(&self) -> u64 {
            self.0
        }
    }

    #[test]
    fn hlc_is_monotonic_when_wall_clock_stalls() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let a = hlc.now();
        let b = hlc.now();
        assert!(b > a, "same wall ms must bump counter");
    }

    #[test]
    fn hlc_observe_advances_past_remote() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let remote = Hlc { wall_ms: 5000, counter: 3, machine: MachineId("m2".into()) };
        hlc.observe(&remote);
        assert!(hlc.now() > remote, "local must order after observed remote");
    }

    #[test]
    fn hlc_orders_by_wall_then_counter_then_machine() {
        let a = Hlc { wall_ms: 1, counter: 0, machine: MachineId("a".into()) };
        let b = Hlc { wall_ms: 1, counter: 1, machine: MachineId("a".into()) };
        let c = Hlc { wall_ms: 1, counter: 1, machine: MachineId("b".into()) };
        assert!(a < b && b < c);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-core clock`
Expected: compile error — types not defined.

- [ ] **Step 3: Implement** (top of `clock.rs`)

```rust
//! Wall-clock port and Hybrid Logical Clock.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId(pub String);

/// Port: injected so HLC logic is deterministic under test.
pub trait Clock: Send {
    fn wall_ms(&self) -> u64;
}

/// HLC timestamp. Derived ordering (wall, counter, machine) is the total
/// order used for LWW merges; machine id is the deterministic tiebreaker.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub machine: MachineId,
}

pub struct HlcClock {
    machine: MachineId,
    clock: Box<dyn Clock>,
    last_wall: u64,
    last_counter: u32,
}

impl HlcClock {
    #[must_use]
    pub fn new(machine: MachineId, clock: Box<dyn Clock>) -> Self {
        Self { machine, clock, last_wall: 0, last_counter: 0 }
    }

    pub fn now(&mut self) -> Hlc {
        let wall = self.clock.wall_ms();
        if wall > self.last_wall {
            self.last_wall = wall;
            self.last_counter = 0;
        } else {
            self.last_counter += 1;
        }
        Hlc { wall_ms: self.last_wall, counter: self.last_counter, machine: self.machine.clone() }
    }

    /// Fold a remote timestamp in so subsequent local events order after it.
    pub fn observe(&mut self, remote: &Hlc) {
        if remote.wall_ms > self.last_wall
            || (remote.wall_ms == self.last_wall && remote.counter > self.last_counter)
        {
            self.last_wall = remote.wall_ms;
            self.last_counter = remote.counter;
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p majestical-core clock`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: add clock port and hybrid logical clock"
```

---

### Task 3: Domain events

**Files:**
- Create: `crates/core/src/event.rs` (replace placeholder)

- [ ] **Step 1: Write failing round-trip test** (bottom of `event.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};

    #[test]
    fn event_json_round_trips() {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc { wall_ms: 1, counter: 0, machine: MachineId("m1".into()) },
            author: "elliot".into(),
            op: Op::TagAdd { asset: AssetId("xxh3:aa".into()), tag: "person/dana".into() },
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-core event`
Expected: compile error.

- [ ] **Step 3: Implement** (top of `event.rs`)

```rust
//! Append-only catalog events. Events are immutable once written; the
//! catalog is a projection of the merged event set.
use crate::clock::Hlc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub ulid::Ulid);

/// Content-hash identity, e.g. "xxh3:9f2a…". Same bytes = same asset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub hlc: Hlc,
    pub author: String,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    /// Physical observation: a file with this content hash exists here.
    AssetSeen { asset: AssetId, volume: String, path: String, size: u64 },
    /// OR-Set add.
    TagAdd { asset: AssetId, tag: String },
    /// OR-Set remove: tombstones only the add-events it observed.
    TagRemove { asset: AssetId, tag: String, observed: Vec<EventId> },
    /// HLC-LWW scalar (rating, title, para node…).
    FieldSet { asset: AssetId, field: String, value: String },
}
```

- [ ] **Step 4: Run tests** — `cargo test -p majestical-core event` → 1 passed.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat: add domain event model"`

---

### Task 4: CRDT projection (order-independent apply)

**Files:**
- Create: `crates/core/src/projection.rs` (replace placeholder)

- [ ] **Step 1: Write failing unit tests** (bottom of `projection.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op};

    fn ev(n: u128, wall: u64, machine: &str, op: Op) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(wall, n)),
            hlc: Hlc { wall_ms: wall, counter: 0, machine: MachineId(machine.into()) },
            author: machine.into(),
            op,
        }
    }
    fn asset() -> AssetId {
        AssetId("xxh3:aa".into())
    }

    #[test]
    fn concurrent_add_wins_over_remove() {
        let add1 = ev(1, 1, "m1", Op::TagAdd { asset: asset(), tag: "keep".into() });
        // m2 removes having observed add1; m1 concurrently re-adds (add2 unobserved by m2).
        let rm = ev(2, 2, "m2", Op::TagRemove {
            asset: asset(), tag: "keep".into(), observed: vec![add1.id],
        });
        let add2 = ev(3, 2, "m1", Op::TagAdd { asset: asset(), tag: "keep".into() });
        let mut p = Projection::default();
        for e in [&add1, &rm, &add2] {
            p.apply(e);
        }
        assert!(p.tags(&asset()).contains("keep"), "unobserved add survives remove");
    }

    #[test]
    fn apply_is_idempotent_and_order_independent() {
        let a = asset();
        let events = vec![
            ev(1, 1, "m1", Op::TagAdd { asset: a.clone(), tag: "t".into() }),
            ev(2, 2, "m2", Op::TagRemove { asset: a.clone(), tag: "t".into(),
                observed: vec![EventId(ulid::Ulid::from_parts(1, 1))] }),
            ev(3, 3, "m1", Op::FieldSet { asset: a.clone(), field: "rating".into(), value: "5".into() }),
            ev(4, 1, "m2", Op::FieldSet { asset: a.clone(), field: "rating".into(), value: "2".into() }),
        ];
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in &events {
            fwd.apply(e);
            fwd.apply(e); // duplicate delivery
        }
        for e in events.iter().rev() {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        assert_eq!(fwd.field(&a, "rating"), Some("5")); // hlc wall 3 beats wall 1
        assert!(!fwd.tags(&a).contains("t")); // remove observed the only add
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p majestical-core projection` → compile error.

- [ ] **Step 3: Implement** (top of `projection.rs`)

```rust
//! In-memory CRDT projection of an event set. Apply is commutative and
//! idempotent: tombstoned add-ids are remembered so a remove arriving
//! before its add still wins over exactly that add and nothing else.
use crate::clock::Hlc;
use crate::event::{AssetId, Event, EventId, Op};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AssetState {
    /// (volume, path, size) instances observed for this content hash.
    pub instances: BTreeSet<(String, String, u64)>,
    /// tag -> live add-event ids.
    tag_adds: BTreeMap<String, BTreeSet<EventId>>,
    /// add-event ids tombstoned by observed removes.
    removed_adds: BTreeSet<EventId>,
    /// field -> (hlc, value); higher HLC wins deterministically.
    fields: BTreeMap<String, (Hlc, String)>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Projection {
    assets: BTreeMap<AssetId, AssetState>,
    applied: BTreeSet<EventId>,
}

impl Projection {
    pub fn apply(&mut self, event: &Event) {
        if !self.applied.insert(event.id) {
            return;
        }
        match &event.op {
            Op::AssetSeen { asset, volume, path, size } => {
                self.assets.entry(asset.clone()).or_default().instances.insert((
                    volume.clone(),
                    path.clone(),
                    *size,
                ));
            }
            Op::TagAdd { asset, tag } => {
                let st = self.assets.entry(asset.clone()).or_default();
                if !st.removed_adds.contains(&event.id) {
                    st.tag_adds.entry(tag.clone()).or_default().insert(event.id);
                }
            }
            Op::TagRemove { asset, tag, observed } => {
                let st = self.assets.entry(asset.clone()).or_default();
                for add_id in observed {
                    st.removed_adds.insert(*add_id);
                    if let Some(ids) = st.tag_adds.get_mut(tag) {
                        ids.remove(add_id);
                    }
                }
            }
            Op::FieldSet { asset, field, value } => {
                let st = self.assets.entry(asset.clone()).or_default();
                let candidate = (event.hlc.clone(), value.clone());
                match st.fields.get(field) {
                    Some(current) if *current >= candidate => {}
                    _ => {
                        st.fields.insert(field.clone(), candidate);
                    }
                }
            }
        }
    }

    #[must_use]
    pub fn tags(&self, asset: &AssetId) -> BTreeSet<String> {
        self.assets
            .get(asset)
            .map(|s| {
                s.tag_adds
                    .iter()
                    .filter(|(_, ids)| !ids.is_empty())
                    .map(|(t, _)| t.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Live add-event ids for a tag — what a remove must cite as observed.
    #[must_use]
    pub fn tag_add_ids(&self, asset: &AssetId, tag: &str) -> Vec<EventId> {
        self.assets
            .get(asset)
            .and_then(|s| s.tag_adds.get(tag))
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn field<'a>(&'a self, asset: &AssetId, field: &str) -> Option<&'a str> {
        self.assets.get(asset)?.fields.get(field).map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn assets(&self) -> impl Iterator<Item = (&AssetId, &AssetState)> {
        self.assets.iter()
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test -p majestical-core projection` → 2 passed.

- [ ] **Step 5: Add property tests** — Create `crates/core/tests/crdt_properties.rs`:

```rust
//! Algebraic laws: applying any event set in any order, with any
//! duplication, yields the same projection.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use proptest::prelude::*;

fn arb_op(asset: AssetId, pool: Vec<EventId>) -> impl Strategy<Value = Op> {
    let a2 = asset.clone();
    let a3 = asset.clone();
    prop_oneof![
        ("[a-c]{1,3}", any::<u8>()).prop_map(move |(tag, _)| Op::TagAdd {
            asset: asset.clone(),
            tag,
        }),
        ("[a-c]{1,3}", proptest::sample::subsequence(pool, 0..3)).prop_map(
            move |(tag, observed)| Op::TagRemove { asset: a2.clone(), tag, observed }
        ),
        ("[a-c]{1,3}", "[x-z]{1,3}").prop_map(move |(field, value)| Op::FieldSet {
            asset: a3.clone(),
            field,
            value,
        }),
    ]
}

proptest! {
    #[test]
    fn projection_is_order_independent(seed in proptest::collection::vec((1u64..50, 0u128..1000), 1..30)) {
        let asset = AssetId("xxh3:p".into());
        let pool: Vec<EventId> =
            seed.iter().map(|(w, n)| EventId(ulid::Ulid::from_parts(*w, *n))).collect();
        let ops = seed
            .iter()
            .map(|(w, n)| {
                arb_op(asset.clone(), pool.clone())
                    .new_tree(&mut proptest::test_runner::TestRunner::deterministic())
                    .map(|t| t.current())
                    .map(|op| Event {
                        id: EventId(ulid::Ulid::from_parts(*w, *n)),
                        hlc: Hlc { wall_ms: *w, counter: 0, machine: MachineId(format!("m{n}")) },
                        author: "p".into(),
                        op,
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TestCaseError::fail("gen"))?;
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in &ops { fwd.apply(e); fwd.apply(e); }
        for e in ops.iter().rev() { rev.apply(e); }
        prop_assert_eq!(fwd, rev);
    }
}
```

- [ ] **Step 6: Run** — `cargo test -p majestical-core` → all pass (if the generator plumbing fights proptest's API, simplify to generating `(tag, kind, observed-subset)` tuples directly inside the `proptest!` macro — the law being tested is what matters, not the strategy style).

- [ ] **Step 7: Commit** — `git add -A && git commit -m "feat: add order-independent crdt projection with property tests"`

---

### Task 5: File-based event log

**Files:**
- Create: `crates/sync/Cargo.toml`, `crates/sync/src/lib.rs`
- Modify: root `Cargo.toml` members → add `"crates/sync"`

- [ ] **Step 1: Crate manifest**

```toml
[package]
name = "majestical-sync"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
majestical-core = { path = "../core" }
serde_json = "1"
thiserror = "2"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write failing test** (bottom of `lib.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};

    fn ev(n: u128) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(n as u64, n)),
            hlc: Hlc { wall_ms: n as u64, counter: 0, machine: MachineId("m1".into()) },
            author: "t".into(),
            op: Op::TagAdd { asset: AssetId("xxh3:aa".into()), tag: "t".into() },
        }
    }

    #[test]
    fn append_then_read_all_machines() {
        let dir = tempfile::tempdir().unwrap();
        let mut log1 = FileEventLog::open(dir.path(), &MachineId("m1".into())).unwrap();
        let mut log2 = FileEventLog::open(dir.path(), &MachineId("m2".into())).unwrap();
        log1.append(&[ev(1), ev(2)]).unwrap();
        log2.append(&[ev(3)]).unwrap();
        // A third participant reads everything both machines wrote.
        let all = FileEventLog::open(dir.path(), &MachineId("m3".into()))
            .unwrap()
            .read_all()
            .unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn corrupt_line_is_skipped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = FileEventLog::open(dir.path(), &MachineId("m1".into())).unwrap();
        log.append(&[ev(1)]).unwrap();
        let seg = dir.path().join("events/m1/0001.jsonl");
        std::fs::write(&seg, format!("{}\nnot json\n", std::fs::read_to_string(&seg).unwrap().trim())).unwrap();
        let mut skipped = 0;
        let all = log.read_all_reporting(|_line| skipped += 1).unwrap();
        assert_eq!((all.len(), skipped), (1, 1));
    }
}
```

- [ ] **Step 3: Run** — `cargo test -p majestical-sync` → compile error.

- [ ] **Step 4: Implement** (top of `lib.rs`)

```rust
//! File-based event log: `events/<machine-id>/NNNN.jsonl` under a sync
//! root. Append-only; reading merges every machine's segments. Designed
//! so dumb transports (Dropbox, rsync, a shuttle drive) can carry it.
use majestical_core::clock::MachineId;
use majestical_core::event::Event;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("event log io at {path}: {source} — check the sync root is writable")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("serializing event: {0}")]
    Serde(#[from] serde_json::Error),
}

pub struct FileEventLog {
    root: PathBuf,
    machine_dir: PathBuf,
}

impl FileEventLog {
    pub fn open(root: &Path, machine: &MachineId) -> Result<Self, LogError> {
        let machine_dir = root.join("events").join(&machine.0);
        fs::create_dir_all(&machine_dir)
            .map_err(|source| LogError::Io { path: machine_dir.clone(), source })?;
        Ok(Self { root: root.to_path_buf(), machine_dir })
    }

    /// Append to this machine's current segment (0001.jsonl for phase 1;
    /// segment rotation arrives with sync push/pull in a later phase).
    pub fn append(&mut self, events: &[Event]) -> Result<(), LogError> {
        let seg = self.machine_dir.join("0001.jsonl");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg)
            .map_err(|source| LogError::Io { path: seg.clone(), source })?;
        for e in events {
            let line = serde_json::to_string(e)?;
            writeln!(f, "{line}").map_err(|source| LogError::Io { path: seg.clone(), source })?;
        }
        Ok(())
    }

    pub fn read_all(&self) -> Result<Vec<Event>, LogError> {
        self.read_all_reporting(|_| {})
    }

    /// Corrupt lines are skipped and reported, never fatal: one bad byte
    /// on a shuttle drive must not take down the whole catalog.
    pub fn read_all_reporting(
        &self,
        mut on_bad_line: impl FnMut(&str),
    ) -> Result<Vec<Event>, LogError> {
        let events_dir = self.root.join("events");
        let mut out = Vec::new();
        let machines = fs::read_dir(&events_dir)
            .map_err(|source| LogError::Io { path: events_dir.clone(), source })?;
        for machine in machines {
            let machine = machine.map_err(|source| LogError::Io { path: events_dir.clone(), source })?;
            let mut segments: Vec<PathBuf> = fs::read_dir(machine.path())
                .map_err(|source| LogError::Io { path: machine.path(), source })?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            segments.sort();
            for seg in segments {
                let text = fs::read_to_string(&seg)
                    .map_err(|source| LogError::Io { path: seg.clone(), source })?;
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    match serde_json::from_str::<Event>(line) {
                        Ok(e) => out.push(e),
                        Err(_) => on_bad_line(line),
                    }
                }
            }
        }
        Ok(out)
    }
}
```

Add `ulid = { version = "1", features = ["serde"] }` to `[dev-dependencies]` (tests construct ids).

- [ ] **Step 5: Run** — `cargo test -p majestical-sync` → 2 passed.

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: add file-based append-only event log"`

---

### Task 6: SQLite projection

**Files:**
- Create: `crates/catalog-sqlite/Cargo.toml`, `crates/catalog-sqlite/src/lib.rs`
- Modify: root `Cargo.toml` members → add `"crates/catalog-sqlite"`

- [ ] **Step 1: Crate manifest**

```toml
[package]
name = "majestical-catalog-sqlite"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
majestical-core = { path = "../core" }
rusqlite = { version = "0.37", features = ["bundled"] }
thiserror = "2"

[dev-dependencies]
tempfile = "3"
ulid = { version = "1", features = ["serde"] }
```

- [ ] **Step 2: Write failing test** (bottom of `lib.rs`) — rebuild from a projection, query by tag and name:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};
    use majestical_core::projection::Projection;

    #[test]
    fn rebuild_then_query_by_tag_and_name() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:aa".into());
        for (n, op) in [
            Op::AssetSeen { asset: a.clone(), volume: "card1".into(), path: "clips/sunset.mov".into(), size: 42 },
            Op::TagAdd { asset: a.clone(), tag: "topic/drone".into() },
        ]
        .into_iter()
        .enumerate()
        {
            p.apply(&Event {
                id: EventId(ulid::Ulid::from_parts(1, n as u128)),
                hlc: Hlc { wall_ms: 1, counter: n as u32, machine: MachineId("m1".into()) },
                author: "t".into(),
                op,
            });
        }
        let dir = tempfile::tempdir().unwrap();
        let db = SqliteCatalog::rebuild(&dir.path().join("catalog.db"), &p).unwrap();
        assert_eq!(db.search_by_tag("topic/drone").unwrap(), vec![a.clone()]);
        assert_eq!(db.search_by_name("sunset").unwrap(), vec![a.clone()]);
        assert_eq!(db.search_by_name("nothing").unwrap(), Vec::<AssetId>::new());
    }
}
```

- [ ] **Step 3: Run** — `cargo test -p majestical-catalog-sqlite` → compile error.

- [ ] **Step 4: Implement** (top of `lib.rs`)

```rust
//! SQLite projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection` (incremental apply and
//! FTS5/sqlite-vec arrive in later phases).
use majestical_core::event::AssetId;
use majestical_core::projection::Projection;
use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog db: {0} — delete the file and rebuild from the event log")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct SqliteCatalog {
    conn: Connection,
}

impl SqliteCatalog {
    pub fn rebuild(path: &Path, projection: &Projection) -> Result<Self, CatalogError> {
        if path.exists() {
            // Projection files are disposable; the event log is the truth.
            let _ = std::fs::remove_file(path);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE assets (id TEXT PRIMARY KEY);
             CREATE TABLE instances (
               asset TEXT NOT NULL REFERENCES assets(id),
               volume TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
               PRIMARY KEY (asset, volume, path)
             );
             CREATE TABLE tags (
               asset TEXT NOT NULL REFERENCES assets(id),
               tag TEXT NOT NULL, PRIMARY KEY (asset, tag)
             );
             CREATE INDEX tags_by_tag ON tags (tag);",
        )?;
        for (asset, state) in projection.assets() {
            conn.execute("INSERT INTO assets (id) VALUES (?1)", [&asset.0])?;
            for (volume, path, size) in &state.instances {
                conn.execute(
                    "INSERT INTO instances (asset, volume, path, size) VALUES (?1, ?2, ?3, ?4)",
                    (&asset.0, volume, path, size),
                )?;
            }
            for tag in projection.tags(asset) {
                conn.execute("INSERT INTO tags (asset, tag) VALUES (?1, ?2)", (&asset.0, &tag))?;
            }
        }
        Ok(Self { conn })
    }

    pub fn search_by_tag(&self, tag: &str) -> Result<Vec<AssetId>, CatalogError> {
        self.query("SELECT asset FROM tags WHERE tag = ?1 ORDER BY asset", tag)
    }

    /// Case-insensitive filename substring match (FTS arrives with search phase).
    pub fn search_by_name(&self, needle: &str) -> Result<Vec<AssetId>, CatalogError> {
        self.query(
            "SELECT DISTINCT asset FROM instances WHERE path LIKE '%' || ?1 || '%' ORDER BY asset",
            needle,
        )
    }

    fn query(&self, sql: &str, param: &str) -> Result<Vec<AssetId>, CatalogError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([param], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(AssetId(row?));
        }
        Ok(out)
    }
}
```

- [ ] **Step 5: Run** — `cargo test -p majestical-catalog-sqlite` → 1 passed.

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: add sqlite catalog projection with tag and name search"`

---

### Task 7: `maj` CLI — init, scan, tag, search

**Files:**
- Create: `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`
- Modify: root `Cargo.toml` members → add `"crates/cli"`

- [ ] **Step 1: Crate manifest**

```toml
[package]
name = "majestical-cli"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "maj"
path = "src/main.rs"

[lints]
workspace = true

[dependencies]
majestical-core = { path = "../core" }
majestical-sync = { path = "../sync" }
majestical-catalog-sqlite = { path = "../catalog-sqlite" }
anyhow = "1"
clap = { version = "4", features = ["derive"] }
serde_json = "1"
ulid = "1"
walkdir = "2"
xxhash-rust = { version = "0.8", features = ["xxh3"] }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

- [ ] **Step 2: Write failing e2e test** — Create `crates/cli/tests/cli_smoke.rs`:

```rust
//! End-to-end: init a catalog, scan a folder, tag by name-match, search.
use assert_cmd::Command;
use predicates::str::contains;

fn maj(catalog: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env("MAJ_CATALOG", catalog).env("MAJ_MACHINE_ID", "test-machine");
    c
}

#[test]
fn init_scan_tag_search_round_trip() {
    let media = tempfile::tempdir().unwrap();
    std::fs::write(media.path().join("sunset.mov"), b"fake video bytes").unwrap();
    std::fs::write(media.path().join("notes.txt"), b"hello").unwrap();
    let catalog = tempfile::tempdir().unwrap();
    let root = catalog.path().join("cat");

    maj(&root).args(["catalog", "init"]).assert().success();
    maj(&root)
        .args(["scan"])
        .arg(media.path())
        .args(["--volume", "card1"])
        .assert()
        .success()
        .stdout(contains("2 assets"));
    // Find the asset id for sunset.mov via name search (json output).
    let out = maj(&root).args(["search", "--name", "sunset", "--json"]).output().unwrap();
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = hits["results"][0]["asset"].as_str().unwrap().to_string();
    assert_eq!(hits["count"], 1);

    maj(&root).args(["tag", "add", &id, "topic/drone"]).assert().success();
    maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .assert()
        .success()
        .stdout(contains(&id));
    maj(&root).args(["tag", "rm", &id, "topic/drone"]).assert().success();
    maj(&root)
        .args(["search", "--tag", "topic/drone", "--json"])
        .assert()
        .success()
        .stdout(contains("\"count\":0"));
}
```

- [ ] **Step 3: Run** — `cargo test -p majestical-cli` → compile error (no binary).

- [ ] **Step 4: Implement `crates/cli/src/main.rs`**

```rust
//! `maj`: agent-first CLI over the catalog core. JSON-first output.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::{Clock, HlcClock, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::path::{Path, PathBuf};

struct SystemClock;
impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
}

#[derive(Parser)]
#[command(name = "maj", version, about = "Majestical media catalog")]
struct Cli {
    /// Catalog directory (env MAJ_CATALOG).
    #[arg(long, env = "MAJ_CATALOG")]
    catalog: PathBuf,
    /// Stable machine identity (env MAJ_MACHINE_ID).
    #[arg(long, env = "MAJ_MACHINE_ID")]
    machine_id: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the catalog directory.
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
    /// Hash every file under a directory into the catalog as AssetSeen events.
    Scan {
        dir: PathBuf,
        #[arg(long)]
        volume: String,
    },
    /// Add or remove folksonomy tags.
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Search the catalog projection.
    Search {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CatalogCmd {
    Init,
}

#[derive(Subcommand)]
enum TagCmd {
    Add { asset: String, tag: String },
    Rm { asset: String, tag: String },
}

struct App {
    log: FileEventLog,
    hlc: HlcClock,
    author: String,
}

impl App {
    fn open(root: &Path, machine: &str) -> Result<Self> {
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::open(root, &machine)
            .with_context(|| format!("opening catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine.clone(), Box::new(SystemClock)),
            author: machine.0,
        })
    }

    fn projection(&self) -> Result<Projection> {
        let mut p = Projection::default();
        for e in self.log.read_all().context("reading event log")? {
            self.hlc_observe_hint(&e);
            p.apply(&e);
        }
        Ok(p)
    }

    // HLC observation of read events happens on write paths via reload;
    // reads alone don't need clock updates. Kept as a hint hook.
    fn hlc_observe_hint(&self, _e: &Event) {}

    fn emit(&mut self, ops: Vec<Op>) -> Result<Vec<Event>> {
        // Fold existing log into the clock so new events order after it.
        for e in self.log.read_all().context("reading event log")? {
            self.hlc.observe(&e.hlc);
        }
        let events: Vec<Event> = ops
            .into_iter()
            .map(|op| {
                let hlc = self.hlc.now();
                Event {
                    id: EventId(ulid::Ulid::new()),
                    hlc,
                    author: self.author.clone(),
                    op,
                }
            })
            .collect();
        self.log.append(&events).context("appending events")?;
        Ok(events)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Catalog { cmd: CatalogCmd::Init } => {
            App::open(&cli.catalog, &cli.machine_id)?;
            println!("initialized catalog at {}", cli.catalog.display());
        }
        Cmd::Scan { dir, volume } => {
            let mut app = App::open(&cli.catalog, &cli.machine_id)?;
            let mut ops = Vec::new();
            for entry in walkdir::WalkDir::new(&dir).sort_by_file_name() {
                let entry = entry.context("walking scan directory")?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let bytes = std::fs::read(entry.path())
                    .with_context(|| format!("reading {}", entry.path().display()))?;
                let hash = xxhash_rust::xxh3::xxh3_128(&bytes);
                let rel = entry
                    .path()
                    .strip_prefix(&dir)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                ops.push(Op::AssetSeen {
                    asset: AssetId(format!("xxh3:{hash:032x}")),
                    volume: volume.clone(),
                    path: rel,
                    size: bytes.len() as u64,
                });
            }
            let n = ops.len();
            app.emit(ops)?;
            println!("scanned: {n} assets");
        }
        Cmd::Tag { cmd } => {
            let mut app = App::open(&cli.catalog, &cli.machine_id)?;
            match cmd {
                TagCmd::Add { asset, tag } => {
                    app.emit(vec![Op::TagAdd { asset: AssetId(asset), tag }])?;
                }
                TagCmd::Rm { asset, tag } => {
                    let p = app.projection()?;
                    let asset = AssetId(asset);
                    let observed = p.tag_add_ids(&asset, &tag);
                    anyhow::ensure!(
                        !observed.is_empty(),
                        "tag '{tag}' is not set on {} — nothing to remove",
                        asset.0
                    );
                    app.emit(vec![Op::TagRemove { asset, tag, observed }])?;
                }
            }
            println!("ok");
        }
        Cmd::Search { name, tag, json } => {
            let app = App::open(&cli.catalog, &cli.machine_id)?;
            let projection = app.projection()?;
            let db_path = cli.catalog.join("catalog.db");
            let db = SqliteCatalog::rebuild(&db_path, &projection)
                .context("rebuilding sqlite projection")?;
            let ids = match (&name, &tag) {
                (Some(n), None) => db.search_by_name(n)?,
                (None, Some(t)) => db.search_by_tag(t)?,
                _ => anyhow::bail!("pass exactly one of --name or --tag"),
            };
            if json {
                let results: Vec<_> = ids
                    .iter()
                    .map(|a| serde_json::json!({ "asset": a.0 }))
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({ "count": ids.len(), "results": results })
                );
            } else {
                for a in &ids {
                    println!("{}", a.0);
                }
                println!("{} results", ids.len());
            }
        }
    }
    Ok(())
}
```

Note: `print_stdout` is denied by workspace lints — a CLI legitimately prints. In `crates/cli/Cargo.toml`, replace the `[lints]\nworkspace = true` block from Step 1 with a per-crate table (Cargo does not merge the two):

```toml
[lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
panic = "deny"
print_stdout = "allow"   # CLI output is the product
module_name_repetitions = "allow"
```

- [ ] **Step 5: Run** — `cargo test -p majestical-cli` → e2e passes. Also run `just ci`.

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: add maj cli with scan, tag, and search"`

---

### Task 8: Cucumber acceptance harness — two-machine convergence

**Files:**
- Create: `crates/core/tests/acceptance.rs`, `crates/core/tests/features/convergence.feature`
- Modify: `crates/core/Cargo.toml` (dev-deps + harness registration)

- [ ] **Step 1: Add cucumber to `crates/core/Cargo.toml`**

```toml
[dev-dependencies]
proptest = "1"
cucumber = "0.21"
futures = "0.3"
tokio = { version = "1", features = ["macros", "rt"] }

[[test]]
name = "acceptance"
harness = false
```

- [ ] **Step 2: Write the feature file** `crates/core/tests/features/convergence.feature`:

```gherkin
Feature: Two machines converge through any exchange of event logs
  The catalog is a projection of merged event logs. Whatever order logs
  arrive in, and however often they are replayed, both machines see the
  same catalog.

  Scenario: Concurrent tagging merges as a union
    Given machine "amy" tags asset "A" with "topic/drone"
    And machine "bob" tags asset "A" with "status/select"
    When the machines exchange event logs
    Then both machines see tags "status/select, topic/drone" on asset "A"

  Scenario: A tag removed on one machine while re-added on another survives
    Given machine "amy" tags asset "A" with "keep"
    And the machines exchange event logs
    And machine "bob" removes tag "keep" from asset "A"
    And machine "amy" removes tag "keep" from asset "A"
    And machine "amy" tags asset "A" with "keep"
    When the machines exchange event logs
    Then both machines see tags "keep" on asset "A"

  Scenario: Replaying the same log twice changes nothing
    Given machine "amy" tags asset "A" with "topic/drone"
    When the machines exchange event logs
    And the machines exchange event logs
    Then both machines see tags "topic/drone" on asset "A"
```

- [ ] **Step 3: Write the harness** `crates/core/tests/acceptance.rs`:

```rust
//! Acceptance tests at the hexagon boundary: fake clock, in-memory
//! machines, real CRDT semantics.
use cucumber::{given, then, when, World};
use majestical_core::clock::{Clock, Hlc, HlcClock, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use std::collections::BTreeMap;

struct TickClock(u64);
impl Clock for TickClock {
    fn wall_ms(&self) -> u64 {
        self.0
    }
}

struct Machine {
    hlc: HlcClock,
    log: Vec<Event>,
    projection: Projection,
    seq: u128,
}

impl Machine {
    fn new(name: &str) -> Self {
        Self {
            hlc: HlcClock::new(MachineId(name.into()), Box::new(TickClock(1))),
            log: Vec::new(),
            projection: Projection::default(),
            seq: 0,
        }
    }
    fn emit(&mut self, op: Op) {
        self.seq += 1;
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(self.seq as u64, self.seq)),
            hlc: self.hlc.now(),
            author: "test".into(),
            op,
        };
        self.projection.apply(&e);
        self.log.push(e);
    }
    fn ingest(&mut self, events: &[Event]) {
        for e in events {
            self.hlc.observe(&e.hlc);
            self.projection.apply(e);
            if !self.log.iter().any(|x| x.id == e.id) {
                self.log.push(e.clone());
            }
        }
    }
}

#[derive(World)]
#[world(init = Self::new)]
struct CatalogWorld {
    machines: BTreeMap<String, Machine>,
}

impl CatalogWorld {
    fn new() -> Self {
        Self { machines: BTreeMap::new() }
    }
    fn machine(&mut self, name: &str) -> &mut Machine {
        self.machines.entry(name.to_string()).or_insert_with(|| Machine::new(name))
    }
}

impl std::fmt::Debug for CatalogWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CatalogWorld({} machines)", self.machines.len())
    }
}

fn asset(name: &str) -> AssetId {
    AssetId(format!("xxh3:{name}"))
}

#[given(expr = "machine {string} tags asset {string} with {string}")]
fn tag_add(w: &mut CatalogWorld, m: String, a: String, tag: String) {
    w.machine(&m).emit(Op::TagAdd { asset: asset(&a), tag });
}

#[given(expr = "machine {string} removes tag {string} from asset {string}")]
fn tag_rm(w: &mut CatalogWorld, m: String, tag: String, a: String) {
    let machine = w.machine(&m);
    let observed = machine.projection.tag_add_ids(&asset(&a), &tag);
    machine.emit(Op::TagRemove { asset: asset(&a), tag, observed });
}

#[given("the machines exchange event logs")]
#[when("the machines exchange event logs")]
fn exchange(w: &mut CatalogWorld) {
    let all: Vec<Event> =
        w.machines.values().flat_map(|m| m.log.iter().cloned()).collect();
    for m in w.machines.values_mut() {
        m.ingest(&all);
    }
}

#[then(expr = "both machines see tags {string} on asset {string}")]
fn assert_tags(w: &mut CatalogWorld, expected: String, a: String) {
    let want: Vec<&str> = expected.split(", ").collect();
    for (name, m) in &w.machines {
        let got: Vec<String> = m.projection.tags(&asset(&a)).into_iter().collect();
        assert_eq!(got, want, "machine {name} diverged");
    }
}

fn main() {
    futures::executor::block_on(CatalogWorld::run("tests/features"));
}
```

- [ ] **Step 4: Run** — `cargo test -p majestical-core --test acceptance`
Expected: 3 scenarios, all steps pass. (If cucumber's current API differs — it moves — `cargo doc -p cucumber --no-deps` or docs.rs for the pinned version is the reference; keep the World/steps shape.)

- [ ] **Step 5: Commit** — `git add -A && git commit -m "test: add cucumber acceptance harness for two-machine convergence"`

---

### Task 9: CI, hooks, and hygiene

**Files:**
- Create: `.github/workflows/ci.yml`, `.pre-commit-config.yaml`, `.github/dependabot.yml`

- [ ] **Step 1: CI workflow** — SHA pins copied from cuesheet (documented in `docs/research/cuesheet-patterns.md`); verify each is still latest before merging:

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:
env:
  CARGO_TERM_COLOR: always
permissions:
  contents: read
jobs:
  rust:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0  # v7.0.0
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32  # v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
  actions-lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0  # v7.0.0
        with:
          persist-credentials: false
      - run: |
          bash <(curl -sSfL https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash)
          ./actionlint -color
      - uses: astral-sh/setup-uv@0eabbc9066e28dd4c8813c18d4d3d3b8e0f2ad4a  # v7.1.6
      - run: uvx zizmor .github/workflows/
```

- [ ] **Step 2: prek config** `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: local
    hooks:
      - id: fmt
        name: cargo fmt
        entry: cargo fmt --all -- --check
        language: system
        pass_filenames: false
      - id: clippy
        name: cargo clippy
        entry: cargo clippy --workspace --all-targets -- -D warnings
        language: system
        pass_filenames: false
```

Run: `prek install && prek run`

- [ ] **Step 3: Dependabot** `.github/dependabot.yml`:

```yaml
version: 2
updates:
  - package-ecosystem: cargo
    directory: /
    schedule:
      interval: weekly
    cooldown:
      default-days: 7
    groups:
      cargo-all:
        patterns: ["*"]
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: weekly
    cooldown:
      default-days: 7
```

- [ ] **Step 4: Verify** — `actionlint .github/workflows/ && uvx zizmor .github/workflows/` locally; `just ci`.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "ci: add rust ci, actions linting, prek hooks, dependabot"`

---

## Deferred to later phases (deliberately absent here)

Ingest/verified copy + ASC MHL, embeddings + semantic search, describer backends, sync push/pull + segment rotation, Tauri app, MCP server, FTS5/sqlite-vec, PARA node operations. Each gets its own plan per the spec's build order.

Design note carried forward from Task 2 quality review: `HlcClock::observe` accepts
arbitrarily-future remote timestamps. The sync-phase plan must add a max-drift bound
(clamp or reject remotes far ahead of local wall time) before observe() is fed
remote event data, so one bad peer clock cannot permanently poison local ordering.

## Self-review notes

- Spec coverage: this plan implements spec §1 (workspace/ports subset), §2 (event/CRDT/projection model), and the filename-search slice of build-order 2. Deferred items listed above.
- Type consistency verified: `Projection::apply/tags/tag_add_ids/field`, `FileEventLog::open/append/read_all/read_all_reporting`, `SqliteCatalog::rebuild/search_by_tag/search_by_name` are used with the same signatures everywhere.
- Version numbers for crates (rusqlite 0.37, cucumber 0.21, etc.) and action SHAs must be re-verified as current at execution time per global standards.
