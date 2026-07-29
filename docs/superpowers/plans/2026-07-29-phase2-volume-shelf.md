# Phase 2: Volume Shelf and Ports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the offline-catalog baseline: real volume identity ("which shelf is this file on, when was it last seen"), the hexagon's EventLog/CatalogStore port traits, a drift-bounded HLC, and the watch-list hygiene items — ending with `maj volumes list` answering the NeoFinder question.

**Architecture:** Extends Phase 1 in place. Core gains a `ports` module (traits + PortError) and a `VolumeSeen` event; sync and catalog-sqlite implement the ports; the CLI becomes generic over them. Spec: `docs/superpowers/specs/2026-07-28-majestical-design.md` §1–§2; watch list: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`.

**Tech Stack:** unchanged (Rust stable, workspace from Phase 1) plus the `plist` crate for `diskutil` output parsing.

---

### Task 1: Port traits in core; adapters implement them

**Files:**
- Create: `crates/core/src/ports.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod ports;`)
- Modify: `crates/sync/src/lib.rs` (impl EventLog for FileEventLog)
- Modify: `crates/catalog-sqlite/src/lib.rs` (split open/rebuild; impl CatalogStore)
- Modify: `crates/cli/src/main.rs` (App generic over EventLog)

- [ ] **Step 1: Write failing port tests** — in `ports.rs`, a `#[cfg(test)]` module with an in-memory fake proving the traits are implementable and object-safe:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op};

    #[derive(Default)]
    struct MemLog(Vec<Event>);
    impl EventLog for MemLog {
        fn append(&mut self, events: &[Event]) -> Result<(), PortError> {
            self.0.extend(events.iter().cloned());
            Ok(())
        }
        fn read_all_reporting(
            &self,
            _on_bad_line: &mut dyn FnMut(&str),
        ) -> Result<Vec<Event>, PortError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn event_log_port_is_object_safe_and_round_trips() {
        let mut log: Box<dyn EventLog> = Box::<MemLog>::default();
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc { wall_ms: 1, counter: 0, machine: MachineId("m".into()) },
            author: "t".into(),
            op: Op::TagAdd { asset: AssetId("xxh3:aa".into()), tag: "t".into() },
        };
        log.append(std::slice::from_ref(&e)).expect("append");
        let mut bad = 0;
        let all = log.read_all_reporting(&mut |_| bad += 1).expect("read");
        assert_eq!((all.len(), bad), (1, 0));
    }
}
```

Add `ulid.workspace = true` to core dev-dependencies if not already present (it is, via existing tests — verify).

- [ ] **Step 2: Implement `ports.rs`**

```rust
//! Ports: the traits adapters implement. The core knows these shapes,
//! never the concrete adapters behind them.
use crate::event::{AssetId, Event};
use crate::projection::Projection;

/// Adapter errors crossing a port boundary keep their message and source
/// but drop the concrete type, so core-level code never names an adapter.
#[derive(Debug, thiserror::Error)]
#[error("{context}: {source}")]
pub struct PortError {
    pub context: String,
    #[source]
    pub source: Box<dyn std::error::Error + Send + Sync>,
}

impl PortError {
    pub fn new(
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self { context: context.into(), source: Box::new(source) }
    }
}

/// Durable append-only event storage.
pub trait EventLog {
    /// # Errors
    /// Returns `PortError` when the underlying storage cannot be written.
    fn append(&mut self, events: &[Event]) -> Result<(), PortError>;
    /// Reads every event from every machine. Corrupt entries are skipped
    /// and reported through `on_bad_line`, never fatal.
    /// # Errors
    /// Returns `PortError` when the underlying storage cannot be read.
    fn read_all_reporting(
        &self,
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<Vec<Event>, PortError>;
}

/// Queryable projection storage, disposable and rebuildable.
pub trait CatalogStore {
    /// # Errors
    /// Returns `PortError` when the store cannot be rebuilt.
    fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError>;
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn search_by_tag(&self, tag: &str) -> Result<Vec<AssetId>, PortError>;
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn search_by_name(&self, needle: &str) -> Result<Vec<AssetId>, PortError>;
}
```

Add `thiserror.workspace = true` to core `[dependencies]`.

- [ ] **Step 3: Implement the ports in the adapters.**
  - sync: `impl majestical_core::ports::EventLog for FileEventLog` delegating to the inherent methods, mapping `LogError` via `PortError::new("event log", e)`. Keep the inherent methods (existing tests use them). The inherent `read_all_reporting` takes `impl FnMut`; the trait takes `&mut dyn FnMut` — delegate with a closure.
  - catalog-sqlite: refactor `SqliteCatalog::rebuild(path, projection) -> Self` into `SqliteCatalog::open(path) -> Result<Self, CatalogError>` (opens/creates the connection only) plus inherent `fn rebuild(&mut self, projection: &Projection) -> Result<(), CatalogError>` (drops tables if present — `DROP TABLE IF EXISTS` in the same batch before CREATE — then rebuilds inside the existing transaction; the delete-the-file behavior moves out: `open` no longer deletes). Update the existing tests to the two-step shape; keep the stale-removal error path only where still reachable (RemoveStale variant may be deleted if nothing removes files anymore — prefer deleting it and the file-removal entirely: DROP TABLE makes it unnecessary). Then `impl CatalogStore for SqliteCatalog` delegating and wrapping errors.
- [ ] **Step 4: CLI: `struct App<L: EventLog>` with `type FsApp = App<FileEventLog>`; `App::open` returns `FsApp`. Search keeps constructing `SqliteCatalog` but through `open` + `rebuild` + trait-typed local `let db: &dyn CatalogStore = &catalog;` is unnecessary ceremony — call inherent methods; the trait exists for future adapters and tests. Behavior unchanged; e2e must pass untouched.**
- [ ] **Step 5: Run `just ci` — everything green, zero warnings.**
- [ ] **Step 6: Commit** — `git add` the five files + manifests, `git commit -m "refactor: add event log and catalog store port traits"`.

---

### Task 2: HLC drift bound

**Files:**
- Modify: `crates/core/src/clock.rs`
- Modify: `crates/cli/src/main.rs` (surface clamp warnings)

- [ ] **Step 1: Failing tests** (in clock.rs tests module):

```rust
#[test]
fn observe_within_drift_is_adopted() {
    let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
    let remote = Hlc { wall_ms: 2000, counter: 3, machine: MachineId("m2".into()) };
    assert!(matches!(hlc.observe(&remote), ObserveOutcome::Adopted));
    assert!(hlc.now() > remote);
}

#[test]
fn observe_far_future_is_clamped() {
    let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
    let poison = Hlc {
        wall_ms: 1000 + MAX_DRIFT_MS + 5000,
        counter: 0,
        machine: MachineId("bad".into()),
    };
    let outcome = hlc.observe(&poison);
    assert!(matches!(outcome, ObserveOutcome::ClampedFuture { .. }));
    let next = hlc.now();
    assert!(next.wall_ms <= 1000 + MAX_DRIFT_MS, "local clock must not adopt poison");
}
```

- [ ] **Step 2: Implement.** Add `pub const MAX_DRIFT_MS: u64 = 24 * 60 * 60 * 1000;` (24h — generous for catalogs synced by shuttle drive across time zones; the point is stopping year-scale poison, not millisecond skew) and:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Remote was ahead and within drift; adopted.
    Adopted,
    /// Remote was behind or equal; nothing to do.
    AlreadyCurrent,
    /// Remote wall time exceeded physical-now + MAX_DRIFT_MS; local state
    /// advanced only to the clamp. New local events may order before the
    /// poisoned events — deliberately, so one bad peer clock cannot
    /// permanently win every LWW merge.
    ClampedFuture { remote_wall_ms: u64 },
}
```

`observe` reads `self.clock.wall_ms()` for physical-now, clamps adoption to `physical_now + MAX_DRIFT_MS` (counter resets to 0 on clamp), returns the outcome. `#[must_use]` on the return. Update existing callers/tests for the new return type.

- [ ] **Step 3: CLI** — in `emit`'s fold loop, count `ClampedFuture` outcomes; if any, print one stderr warning: `warning: {n} event(s) carry timestamps more than 24h in the future — a peer's clock may be wrong; ordering was clamped locally`.
- [ ] **Step 4: `just ci` green. Commit** — `fix: bound hlc observation drift to stop clock poisoning`.

---

### Task 3: Volume identity, VolumeSeen events, `maj volumes list`

**Files:**
- Modify: `crates/core/src/event.rs` (new Op variant)
- Modify: `crates/core/src/projection.rs` (volumes map + accessor)
- Modify: `crates/catalog-sqlite/src/lib.rs` (volumes table + query)
- Create: `crates/cli/src/volume_identity.rs`
- Modify: `crates/cli/src/main.rs` (scan emits VolumeSeen; new `volumes list` subcommand)

- [ ] **Step 1: Event.** Add to `Op` (after AssetSeen):

```rust
/// Physical observation: a volume was present. `volume` is the stable
/// identity AssetSeen.volume refers to; `label` is the human name at
/// observation time.
VolumeSeen { volume: String, label: String },
```

Update the golden wire-format expectations only if the existing golden test breaks (it should not — new variants don't change old encodings). Add one wire test for VolumeSeen: `{"type":"volume_seen","volume":"…","label":"…"}`.

- [ ] **Step 2: Projection.** `AssetState` untouched. Add to `Projection`: `volumes: BTreeMap<String, VolumeState>` with

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VolumeState {
    /// Label from the highest-HLC observation (LWW by (Hlc, label) tuple).
    label: Option<(Hlc, String)>,
    /// Highest HLC at which this volume was observed.
    pub last_seen: Option<Hlc>,
}
```

Apply for VolumeSeen: update `last_seen = max(last_seen, event.hlc)`, label via the same LWW-max pattern as fields. Accessors: `pub fn volumes(&self) -> impl Iterator<Item = (&String, &VolumeState)>` and on VolumeState `pub fn label(&self) -> Option<&str>`. Unit test: two machines observe the same volume with different labels at different HLCs → higher HLC's label wins, last_seen is the max, order-independent (add a VolumeSeen generator arm to the property test's OpKind so the law covers it).

- [ ] **Step 3: SQLite.** Add `volumes (id TEXT PRIMARY KEY, label TEXT, last_seen_ms INTEGER NOT NULL)` to the rebuild schema + inserts (label empty-string when None; last_seen_ms from the Hlc wall_ms). Query: `fn volumes(&self) -> Result<Vec<(String, String, u64)>, CatalogError>` ordered by id. Test: rebuild with a projection containing volumes, assert the rows.
- [ ] **Step 4: Volume identity helper** (`crates/cli/src/volume_identity.rs`):

```rust
//! Resolve a stable identity for the volume containing a path.
//! macOS: `diskutil info -plist <mount>` → VolumeUUID (stable across
//! renames and machines). Anything else (or diskutil failure): fall back
//! to the mount's last path component as both id and label — weaker, but
//! scan must never fail because identity resolution did.
use std::path::Path;
use std::process::Command;

pub struct VolumeIdentity {
    pub id: String,
    pub label: String,
}

pub fn resolve(path: &Path) -> VolumeIdentity {
    let mount = mount_point_of(path);
    let label = mount
        .file_name()
        .map_or_else(|| "root".to_string(), |n| n.to_string_lossy().into_owned());
    #[cfg(target_os = "macos")]
    if let Some(uuid) = diskutil_volume_uuid(&mount) {
        return VolumeIdentity { id: format!("uuid:{uuid}"), label };
    }
    VolumeIdentity { id: format!("label:{label}"), label }
}

fn mount_point_of(path: &Path) -> std::path::PathBuf {
    // Walk up until the device id changes; the last path before the
    // change is the mount point. Root fallback: "/".
    use std::os::unix::fs::MetadataExt;
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let Ok(start_meta) = std::fs::metadata(&start) else {
        return start;
    };
    let dev = start_meta.dev();
    let mut current = start.clone();
    while let Some(parent) = current.parent() {
        match std::fs::metadata(parent) {
            Ok(m) if m.dev() == dev => current = parent.to_path_buf(),
            _ => break,
        }
    }
    current
}

#[cfg(target_os = "macos")]
fn diskutil_volume_uuid(mount: &Path) -> Option<String> {
    let out = Command::new("diskutil").args(["info", "-plist"]).arg(mount).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let value: plist::Value = plist::from_bytes(&out.stdout).ok()?;
    value
        .as_dictionary()?
        .get("VolumeUUID")?
        .as_string()
        .map(str::to_owned)
}
```

Add `plist = "1"` to `[workspace.dependencies]` (verify current version) and cli deps. Unit test for `mount_point_of` (a tempdir resolves to the same mount as its parent chain — assert it returns an ancestor of the input and exists); diskutil path is covered by the manual smoke run, not unit tests.

- [ ] **Step 5: CLI wiring.** `scan`: `--volume` becomes optional; when omitted, `volume_identity::resolve(&dir)` supplies id + label; when given, it is used as both id and label (explicit override, keeps e2e tests deterministic). Scan emits one `VolumeSeen { volume, label }` event before the AssetSeen batch, and AssetSeen.volume carries the same id. New subcommand:

```rust
/// List every volume the catalog has ever seen.
Volumes {
    #[command(subcommand)]
    cmd: VolumesCmd,
}
// VolumesCmd::List { #[arg(long)] json: bool }
```

`cmd_volumes_list`: rebuild projection, for each volume row compute `online` = does a mount whose resolved identity matches exist right now (cheap heuristic for phase 2: for `label:` ids check `/Volumes/<label>` exists or the id is the root volume; for `uuid:` ids run resolve on `/Volumes/<label>` when present and compare) — keep the heuristic in one small function with a doc comment stating its limits. Output human table (id, label, last seen as ISO-8601 from wall ms, online/offline, asset count) and `--json` array. Asset count = distinct assets with an instance on that volume (SQL: `SELECT volume, COUNT(DISTINCT asset) FROM instances GROUP BY volume`, new store method `fn volume_asset_counts`).
- [ ] **Step 6: e2e test** in cli_smoke.rs: scan with explicit `--volume card1`, then `volumes list --json` → one volume, id card1, count 2, online false (no /Volumes/card1). Scan without `--volume` in a tempdir → volumes list contains a volume whose id is non-empty (uuid: or label: prefixed) — assert shape, not the machine-specific value.
- [ ] **Step 7: `just ci` green. Commit** — `feat: add volume identity and the volumes list shelf view`.

---

### Task 4: Hygiene — strict init, tag validation, author identity

**Files:**
- Modify: `crates/cli/src/main.rs`, `crates/cli/tests/cli_smoke.rs`

- [ ] **Step 1: Strict catalog opening.** `App::open` no longer creates directories implicitly; it errors ("no catalog at {path} — run `maj catalog init` first") unless `<root>/events` exists. `cmd_catalog_init` creates it (via a new `FileEventLog::init(root, machine)` in sync that create_dir_all's, while `open` now errors on missing root — adjust sync tests). e2e: any command against a fresh tempdir without init → exit 1 with the message; after init, works.
- [ ] **Step 2: Tag-add validation.** `tag add` errors when the asset id has no instances in the projection: "unknown asset {id} — scan its volume first, or check `maj search`". e2e: tag add on a bogus id fails with exit 1; catalog unchanged (search by that tag returns 0 after).
- [ ] **Step 3: Author identity.** New global `--author` arg (env `MAJ_AUTHOR`), defaulting to the machine id when absent. Event.author uses it. e2e: emit with MAJ_AUTHOR=elliot, then read the raw event log line and assert `"author":"elliot"`.
- [ ] **Step 4: `just ci` green. Commit** — `feat: strict catalog init, tag validation, and author identity`.

---

### Task 5: FieldSet CLI surface

**Files:**
- Modify: `crates/cli/src/main.rs`, `crates/cli/tests/cli_smoke.rs`

- [ ] **Step 1:** New subcommand `Meta { cmd: MetaCmd }` with `Set { asset, field, value }` and `Get { asset, field: Option<String>, #[arg(long)] json: bool }`. `set` validates the asset exists (same rule as tag add) then emits `Op::FieldSet`. `get` prints the field value (or all fields as `field\tvalue` lines / JSON object; requires a `pub fn fields(&self, asset)` accessor on Projection returning an iterator of (name, value) — add it with a unit test).
- [ ] **Step 2:** e2e: `meta set <id> rating 5`, `meta get <id> rating` → "5"; second machine sets rating 3 *before* (HLC-earlier) — verify via two-machine flow that the later write wins on both.
- [ ] **Step 3: `just ci` green. Commit** — `feat: add meta set and get for lww fields`.

---

### Task 6: Acceptance scenarios for the shelf and the clamp

**Files:**
- Modify: `crates/core/tests/features/convergence.feature`, `crates/core/tests/acceptance.rs`

- [ ] **Step 1:** Two new scenarios:

```gherkin
Scenario: Volume observations converge to the freshest label
  Given machine "amy" observes volume "V1" labeled "card-a"
  And machine "bob" observes volume "V1" labeled "card-a-renamed"
  When the machines exchange event logs
  Then both machines see volume "V1" labeled "card-a-renamed" 

Scenario: A poisoned future clock cannot dominate ordering
  Given machine "amy" tags asset "A" with "good"
  And machine "bob" has a clock far in the future
  And machine "bob" tags asset "A" with "poison"
  And the machines exchange event logs
  When machine "amy" removes tag "poison" from asset "A"
  And the machines exchange event logs
  Then both machines see tags "good" on asset "A"
```

(The label scenario needs bob's observation to be HLC-later: bob's TickClock starts higher, or bob observes after an exchange — pick the deterministic construction and comment it. The clamp scenario's step "has a clock far in the future" swaps bob's clock for a far-future TickClock; the remove works because OR-Set removal is causal, not LWW — the scenario documents that tags survive clock poison, and amy's clamp keeps her own subsequent HLCs sane.)
- [ ] **Step 2:** Step defs: `observes volume` emits VolumeSeen; `see volume {v} labeled {l}` asserts via `projection.volumes()`; the future-clock step replaces the machine's HlcClock. Machines seeded set grows only if a scenario needs a third name (it does not).
- [ ] **Step 3: `just ci` green (5 scenarios). Commit** — `test: acceptance scenarios for volume shelf and clock clamp`.

---

## Chunking for PRs

Task 1–2 (ports + drift) → one PR. Task 3 (volumes) → one PR. Tasks 4–5 (hygiene + meta) → one PR. Task 6 (acceptance) → one PR, or fold into the volumes PR if small.

## Deferred (phase 3+)

Incremental SQLite apply, FTS5/sqlite-vec, thumbnails/derived data, ingest engine + ASC MHL, sync push/pull + segment rotation, local-state vs sync-root split, Tauri/MCP adapters, cargo-mutants run (do opportunistically if a chunk finishes early).

## Self-review notes

- Spec coverage: finishes build-order 2's volume story; ports from §1; drift bound and hygiene from the watch list. FieldSet surface closes the "implemented but unreachable" gap.
- Type consistency: `ObserveOutcome`, `VolumeState::last_seen`, `PortError::new`, `SqliteCatalog::open/rebuild` split are used consistently across tasks; Task 3 depends on Task 1's rebuild split.
- Versions (plist crate) and diskutil plist key name must be verified at execution time.
