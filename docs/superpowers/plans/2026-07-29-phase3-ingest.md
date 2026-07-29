# Phase 3: Ingest Engine + ASC MHL + PARA Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verified multi-destination ingest with ASC MHL histories, full PARA model, and content-hash dedupe — the OffShoot baseline per spec §3.

**Architecture:** New `crates/ingest` (planner, file-parallel copy engine, transfer journal, ASC MHL) over `core` ports. Six new additive CRDT ops. CLI grows `para`, `ingest`, `verify` command families after the mandated main.rs extraction.

**Tech Stack:** Rust edition 2024, xxhash-rust (xxh3 + xxh64), walkdir, quick-xml, rusqlite 0.37 (pinned), cucumber, proptest, Python `ascmhl` as CI conformance oracle.

**Spec:** `docs/superpowers/specs/2026-07-29-phase3-ingest-design.md` — read it first.

---

## Non-negotiable execution rules (from HANDOFF-phase3.md)

- `just ci` must pass before every PR; zero warnings.
- NO Claude-Session trailers in commits. Plain git (no submitting-changes skill).
- Never push to main. One branch + PR per task below; squash-merge after CI green.
- Push via: `git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git <branch>`
- Stage ONLY your files — never `git add -A`. Use `trash`, never `rm -rf`.
- Verify current versions of any new dep/action at execution time — do not trust versions written here.
- The CLI crate hand-copies the workspace clippy table (Cargo can't merge them); if you add lints, update both.
- rusqlite is PINNED at 0.37 — do not bump.
- Wire format is pinned by golden tests — additive changes only. Every new op extends the proptest generator in `crates/core/tests/crdt_properties.rs`.

## Task → PR map

| Task | Branch | PR title |
|---|---|---|
| 1 | `refactor/cli-commands-module` | `refactor: extract CLI commands module and catch up CatalogStore port` |
| 2 | `feat/para-ingest-ops` | `feat: PARA, verification, and manifest ops in core` |
| 3 | `feat/para-cli` | `feat: maj para command family` |
| 4 | `feat/ingest-planner` | `feat: ingest planner with dedupe and PARA routing` |
| 5 | `feat/ingest-engine` | `feat: verified copy engine with resume journal` |
| 6 | `feat/asc-mhl` | `feat: ASC MHL create/verify with conformance CI` |
| 7 | `feat/ingest-cli` | `feat: maj ingest end to end` |

---

### Task 1: CLI commands module extraction + CatalogStore port catch-up

Pure refactor + port additions. No behavior change; the existing e2e smoke tests
(`crates/cli/tests/cli_smoke.rs`) are the safety net.

**Files:**
- Create: `crates/cli/src/app.rs`
- Create: `crates/cli/src/commands.rs`
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/core/src/ports.rs`
- Modify: `crates/catalog-sqlite/src/lib.rs`

- [ ] **Step 1: Write the failing port test**

In `crates/core/src/ports.rs` tests module, extend `MemLog`-style testing with a
memory CatalogStore proving the trait carries the volume queries:

```rust
    #[derive(Default)]
    struct MemStore {
        vols: Vec<(String, String, u64)>,
    }
    impl CatalogStore for MemStore {
        fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError> {
            self.vols = projection
                .volumes()
                .map(|(id, st)| {
                    (
                        id.clone(),
                        st.label().unwrap_or("").to_string(),
                        st.last_seen().map_or(0, |h| h.wall_ms),
                    )
                })
                .collect();
            Ok(())
        }
        fn search_by_tag(&self, _tag: &str) -> Result<Vec<AssetId>, PortError> {
            Ok(Vec::new())
        }
        fn search_by_name(&self, _needle: &str) -> Result<Vec<AssetId>, PortError> {
            Ok(Vec::new())
        }
        fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError> {
            Ok(self.vols.clone())
        }
        fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn catalog_store_port_serves_volume_queries() {
        let mut p = Projection::default();
        p.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 7,
                counter: 0,
                machine: MachineId("m".into()),
            },
            author: "t".into(),
            op: Op::VolumeSeen {
                volume: "V1".into(),
                label: "card-a".into(),
            },
        });
        let mut store: Box<dyn CatalogStore> = Box::<MemStore>::default();
        store.rebuild(&p).expect("rebuild");
        assert_eq!(
            store.volumes().expect("volumes"),
            vec![("V1".to_string(), "card-a".to_string(), 7)]
        );
    }
```

- [ ] **Step 2: Run it — expect FAIL** (`volumes` not on the trait)

Run: `cargo test -p majestical-core catalog_store_port_serves_volume_queries`
Expected: compile error "not a member of trait `CatalogStore`".

- [ ] **Step 3: Add the trait methods**

In `crates/core/src/ports.rs`, extend `CatalogStore`:

```rust
    /// Every volume ever seen: (id, label, last-seen wall ms), ordered by id.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError>;
    /// Distinct asset count per volume, ordered by volume.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError>;
```

In `crates/catalog-sqlite/src/lib.rs`, extend `impl CatalogStore for SqliteCatalog`
(the inherent methods already exist — delegate exactly like `search_by_tag` does):

```rust
    fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError> {
        Self::volumes(self).map_err(|e| PortError::new("catalog store", e))
    }

    fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError> {
        Self::volume_asset_counts(self).map_err(|e| PortError::new("catalog store", e))
    }
```

- [ ] **Step 4: Run core + sqlite tests — expect PASS**

Run: `cargo test -p majestical-core -p majestical-catalog-sqlite`

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/ports.rs crates/catalog-sqlite/src/lib.rs
git commit -m "feat: CatalogStore port carries volume queries"
```

- [ ] **Step 6: Extract `app.rs`**

Move from `main.rs` into new `crates/cli/src/app.rs`, unchanged: `physical_now_ms`,
`SystemClock`, `struct App<L>`, `type FsApp`, both `impl` blocks. Add at top:

```rust
//! CLI application state: adapter wiring, event emission, projection loading.
use anyhow::{Context, Result};
use majestical_core::clock::{Clock, HlcClock, MachineId, ObserveOutcome};
use majestical_core::event::{Event, EventId, Op};
use majestical_core::ports::EventLog;
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::path::{Path, PathBuf};
```

Make `pub(crate)`: `physical_now_ms`, `App` (and its fields used by commands:
keep fields private, methods `pub(crate)`), `FsApp`, `SystemClock`.

- [ ] **Step 7: Extract `commands.rs`**

Move from `main.rs` into new `crates/cli/src/commands.rs`, unchanged:
`cmd_catalog_init`, `resolve_volume`, `cmd_scan`, `ensure_asset_known`, `cmd_tag`,
`cmd_meta`, `print_meta_get`, `cmd_search`, `volume_is_online`, `cmd_volumes_list`,
`print_volumes_table`. All `pub(crate)`. Module doc:

```rust
//! One `cmd_*` handler per CLI verb. main.rs owns clap definitions and
//! dispatch; handlers own behavior.
```

`cmd_catalog_init` takes `(catalog: &Path, machine_id: &str, author: &str)`
instead of `&Cli` so commands.rs never imports clap types.

- [ ] **Step 8: Shrink `main.rs`**

`main.rs` keeps: `mod app; mod commands; mod iso8601; mod volume_identity;`, the
clap structs (`Cli`, `Cmd`, all subcommand enums — these stay), and `main()`
dispatching to `commands::cmd_*`. Nothing else.

- [ ] **Step 9: Full gate, then commit**

Run: `just ci`
Expected: everything green; main.rs now ~170 lines.

```bash
git add crates/cli/src/main.rs crates/cli/src/app.rs crates/cli/src/commands.rs
git commit -m "refactor: extract CLI app and commands modules"
```

- [ ] **Step 10: Branch, push, PR, merge**

```bash
git checkout -b refactor/cli-commands-module   # (create at task start, actually — see rules)
git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git refactor/cli-commands-module
gh pr create --repo statik/majestical --head refactor/cli-commands-module \
  --title "refactor: extract CLI commands module and catch up CatalogStore port" \
  --body "main.rs is clap definitions + dispatch; cmd_* handlers move to commands.rs, App wiring to app.rs. CatalogStore trait gains volumes() and volume_asset_counts() (watchlist). No behavior change."
# after CI green:
gh pr merge --repo statik/majestical --squash --delete-branch <PR#>
```

---

### Task 2: PARA, verification, and manifest ops in core

Six additive ops + projection + proptest + sqlite tables. Branch from fresh main
after Task 1 merges: `git checkout main && git pull && git checkout -b feat/para-ingest-ops`.

**Files:**
- Modify: `crates/core/src/event.rs`
- Modify: `crates/core/src/projection.rs`
- Modify: `crates/core/tests/crdt_properties.rs`
- Modify: `crates/catalog-sqlite/src/lib.rs`

- [ ] **Step 1: Write failing golden wire-format tests**

In `crates/core/src/event.rs` tests (pattern: copy `volume_seen_wire_format_is_stable`):

```rust
    fn golden(op: Op) -> String {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "elliot".into(),
            op,
        };
        serde_json::to_string(&e).expect("serialize")
    }

    const PREFIX: &str = r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":"#;

    #[test]
    fn para_and_ingest_ops_wire_formats_are_stable() {
        let node = "00000000010000000000000002".to_string();
        for (op, want) in [
            (
                Op::ParaNodeCreate {
                    node: node.clone(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                r#"{"type":"para_node_create","node":"00000000010000000000000002","kind":"project","name":"client-x"}"#,
            ),
            (
                Op::ParaNodeRename {
                    node: node.clone(),
                    name: "client-y".into(),
                },
                r#"{"type":"para_node_rename","node":"00000000010000000000000002","name":"client-y"}"#,
            ),
            (
                Op::ParaNodeArchive { node: node.clone() },
                r#"{"type":"para_node_archive","node":"00000000010000000000000002"}"#,
            ),
            (
                Op::AssetParaSet {
                    asset: AssetId("xxh3:aa".into()),
                    node: node.clone(),
                },
                r#"{"type":"asset_para_set","asset":"xxh3:aa","node":"00000000010000000000000002"}"#,
            ),
            (
                Op::VerificationRecorded {
                    asset: AssetId("xxh3:aa".into()),
                    volume: "uuid:abc".into(),
                    path: "clips/a.mov".into(),
                    algo: "xxh64".into(),
                    value: "0011223344556677".into(),
                    outcome: VerifyOutcome::Verified,
                    hashdate_ms: 42,
                },
                r#"{"type":"verification_recorded","asset":"xxh3:aa","volume":"uuid:abc","path":"clips/a.mov","algo":"xxh64","value":"0011223344556677","outcome":"verified","hashdate_ms":42}"#,
            ),
            (
                Op::ManifestRecorded {
                    volume: "uuid:abc".into(),
                    mhl_path: "ascmhl/0001_dest_2026-07-29_120000.mhl".into(),
                    generation: 1,
                    roothash: "xxh64:8899aabbccddeeff".into(),
                },
                r#"{"type":"manifest_recorded","volume":"uuid:abc","mhl_path":"ascmhl/0001_dest_2026-07-29_120000.mhl","generation":1,"roothash":"xxh64:8899aabbccddeeff"}"#,
            ),
        ] {
            assert_eq!(golden(op), format!("{PREFIX}{want}}}"));
        }
    }
```

- [ ] **Step 2: Run — expect FAIL** (variants don't exist)

Run: `cargo test -p majestical-core para_and_ingest_ops`

- [ ] **Step 3: Add the op variants**

In `crates/core/src/event.rs`, above `Op`:

```rust
/// PARA node kind. Serialized lowercase; pinned by golden tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParaKind {
    Project,
    Area,
    Resource,
    Archive,
}

impl ParaKind {
    /// The on-disk directory a node of this kind materializes under.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Project => "Projects",
            Self::Area => "Areas",
            Self::Resource => "Resources",
            Self::Archive => "Archives",
        }
    }
}

/// Outcome of one hash verification of one file instance (spec §2 hash history).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    Original,
    Verified,
    Failed,
}
```

Append variants to `Op` (order within the enum doesn't affect the wire, but field
order within each variant does — match the golden test exactly):

```rust
    /// A PARA node exists. `node` is a ULID minted once at creation, so
    /// kind is immutable; name participates in LWW with `ParaNodeRename`.
    ParaNodeCreate {
        node: String,
        kind: ParaKind,
        name: String,
    },
    /// HLC-LWW rename of a node.
    ParaNodeRename { node: String, name: String },
    /// Marks a node archived. Monotonic: no unarchive op this phase.
    ParaNodeArchive { node: String },
    /// HLC-LWW assignment of an asset to a PARA node.
    AssetParaSet { asset: AssetId, node: String },
    /// Physical observation: this instance's bytes hashed to `value` at
    /// `hashdate_ms`, with `outcome` per the ASC MHL action model.
    VerificationRecorded {
        asset: AssetId,
        volume: String,
        path: String,
        algo: String,
        value: String,
        outcome: VerifyOutcome,
        hashdate_ms: u64,
    },
    /// An ASC MHL generation was written for `volume`; `roothash` is the
    /// xxh64 of the manifest file itself, so on-disk tampering is detectable.
    ManifestRecorded {
        volume: String,
        mhl_path: String,
        generation: u32,
        roothash: String,
    },
```

- [ ] **Step 4: Run — golden tests PASS; projection fails to compile**

Run: `cargo test -p majestical-core`
Expected: `projection.rs` non-exhaustive match error. That's the next test's driver.

- [ ] **Step 5: Write failing projection tests**

In `crates/core/src/projection.rs` tests:

```rust
    use crate::event::{ParaKind, VerifyOutcome};

    #[test]
    fn para_node_create_rename_archive_are_lww_and_order_independent() {
        let node = "N1".to_string();
        let create = ev(1, 1, "bob", Op::ParaNodeCreate {
            node: node.clone(),
            kind: ParaKind::Project,
            name: "client-x".into(),
        });
        // Later rename from lexically-smaller machine: discriminates real
        // (wall, counter) LWW from a machine-id tiebreak bug (the same
        // confound the volume-label test guards).
        let rename = ev(2, 2, "amy", Op::ParaNodeRename {
            node: node.clone(),
            name: "client-y".into(),
        });
        let archive = ev(3, 3, "bob", Op::ParaNodeArchive { node: node.clone() });
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in [&create, &rename, &archive] {
            fwd.apply(e);
        }
        for e in [&archive, &rename, &create] {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        let st = fwd.para_node(&node).expect("node exists");
        assert_eq!(st.kind(), Some(ParaKind::Project));
        assert_eq!(st.name(), Some("client-y"));
        assert!(st.archived());
    }

    #[test]
    fn stale_rename_loses_to_newer_name() {
        let node = "N1".to_string();
        let create = ev(1, 5, "m1", Op::ParaNodeCreate {
            node: node.clone(),
            kind: ParaKind::Area,
            name: "newer".into(),
        });
        let stale = ev(2, 1, "m2", Op::ParaNodeRename {
            node: node.clone(),
            name: "older".into(),
        });
        let mut p = Projection::default();
        p.apply(&create);
        p.apply(&stale);
        assert_eq!(p.para_node(&node).expect("node").name(), Some("newer"));
    }

    #[test]
    fn asset_para_assignment_is_lww() {
        let a = asset();
        let first = ev(1, 1, "m1", Op::AssetParaSet {
            asset: a.clone(),
            node: "N1".into(),
        });
        let second = ev(2, 2, "m2", Op::AssetParaSet {
            asset: a.clone(),
            node: "N2".into(),
        });
        let mut fwd = Projection::default();
        fwd.apply(&first);
        fwd.apply(&second);
        let mut rev = Projection::default();
        rev.apply(&second);
        rev.apply(&first);
        assert_eq!(fwd, rev);
        assert_eq!(fwd.asset_para(&a), Some("N2"));
    }

    #[test]
    fn verifications_and_manifests_accumulate_as_sets() {
        let a = asset();
        let v1 = ev(1, 1, "m1", Op::VerificationRecorded {
            asset: a.clone(),
            volume: "V1".into(),
            path: "a.mov".into(),
            algo: "xxh64".into(),
            value: "00".into(),
            outcome: VerifyOutcome::Original,
            hashdate_ms: 1,
        });
        let v2 = ev(2, 2, "m1", Op::VerificationRecorded {
            asset: a.clone(),
            volume: "V1".into(),
            path: "a.mov".into(),
            algo: "xxh64".into(),
            value: "00".into(),
            outcome: VerifyOutcome::Verified,
            hashdate_ms: 2,
        });
        let m = ev(3, 3, "m1", Op::ManifestRecorded {
            volume: "V1".into(),
            mhl_path: "ascmhl/0001_d_x.mhl".into(),
            generation: 1,
            roothash: "xxh64:aa".into(),
        });
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in [&v1, &v2, &m] {
            fwd.apply(e);
        }
        for e in [&m, &v2, &v1] {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        assert_eq!(fwd.verifications(&a).count(), 2);
        assert_eq!(fwd.manifests("V1").count(), 1);
    }
```

- [ ] **Step 6: Run — expect FAIL/compile errors**

Run: `cargo test -p majestical-core`

- [ ] **Step 7: Implement projection state**

In `crates/core/src/projection.rs`:

```rust
use crate::event::{ParaKind, VerifyOutcome};

/// One verification observation; a plain fact, deduped by full value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerificationRecord {
    pub volume: String,
    pub path: String,
    pub algo: String,
    pub value: String,
    pub outcome: VerifyOutcome,
    pub hashdate_ms: u64,
}

/// One recorded ASC MHL generation; a plain fact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestRecord {
    pub generation: u32,
    pub mhl_path: String,
    pub roothash: String,
}

/// PARA node folded state. Kind is immutable (node ids are minted once);
/// name is LWW across create+rename; archived is monotonic.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParaNodeState {
    kind: Option<ParaKind>,
    name: Option<(Hlc, String)>,
    archived: bool,
}

impl ParaNodeState {
    #[must_use]
    pub fn kind(&self) -> Option<ParaKind> {
        self.kind
    }
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|(_, n)| n.as_str())
    }
    #[must_use]
    pub fn archived(&self) -> bool {
        self.archived
    }
}
```

Extend `AssetState` with:

```rust
    /// PARA assignment: (hlc, node id); higher tuple wins.
    para: Option<(Hlc, String)>,
    /// Hash-history facts observed for this asset's instances.
    verifications: BTreeSet<VerificationRecord>,
```

Extend `Projection` with:

```rust
    para_nodes: BTreeMap<String, ParaNodeState>,
    /// volume id -> recorded manifest generations.
    manifests: BTreeMap<String, BTreeSet<ManifestRecord>>,
```

New `apply` arms (LWW arms use the exact `Some(current) if *current >= candidate`
shape `FieldSet` uses):

```rust
            Op::ParaNodeCreate { node, kind, name } => {
                let st = self.para_nodes.entry(node.clone()).or_default();
                st.kind.get_or_insert(*kind);
                let candidate = (event.hlc.clone(), name.clone());
                match &st.name {
                    Some(current) if *current >= candidate => {}
                    _ => st.name = Some(candidate),
                }
            }
            Op::ParaNodeRename { node, name } => {
                let st = self.para_nodes.entry(node.clone()).or_default();
                let candidate = (event.hlc.clone(), name.clone());
                match &st.name {
                    Some(current) if *current >= candidate => {}
                    _ => st.name = Some(candidate),
                }
            }
            Op::ParaNodeArchive { node } => {
                self.para_nodes.entry(node.clone()).or_default().archived = true;
            }
            Op::AssetParaSet { asset, node } => {
                let st = self.assets.entry(asset.clone()).or_default();
                let candidate = (event.hlc.clone(), node.clone());
                match &st.para {
                    Some(current) if *current >= candidate => {}
                    _ => st.para = Some(candidate),
                }
            }
            Op::VerificationRecorded {
                asset,
                volume,
                path,
                algo,
                value,
                outcome,
                hashdate_ms,
            } => {
                self.assets
                    .entry(asset.clone())
                    .or_default()
                    .verifications
                    .insert(VerificationRecord {
                        volume: volume.clone(),
                        path: path.clone(),
                        algo: algo.clone(),
                        value: value.clone(),
                        outcome: *outcome,
                        hashdate_ms: *hashdate_ms,
                    });
            }
            Op::ManifestRecorded {
                volume,
                mhl_path,
                generation,
                roothash,
            } => {
                self.manifests
                    .entry(volume.clone())
                    .or_default()
                    .insert(ManifestRecord {
                        generation: *generation,
                        mhl_path: mhl_path.clone(),
                        roothash: roothash.clone(),
                    });
            }
```

Accessors on `Projection`:

```rust
    #[must_use]
    pub fn para_node(&self, node: &str) -> Option<&ParaNodeState> {
        self.para_nodes.get(node)
    }

    pub fn para_nodes(&self) -> impl Iterator<Item = (&String, &ParaNodeState)> {
        self.para_nodes.iter()
    }

    /// The asset's current PARA node id (LWW winner).
    #[must_use]
    pub fn asset_para<'a>(&'a self, asset: &AssetId) -> Option<&'a str> {
        self.assets
            .get(asset)?
            .para
            .as_ref()
            .map(|(_, n)| n.as_str())
    }

    pub fn verifications<'a>(
        &'a self,
        asset: &AssetId,
    ) -> impl Iterator<Item = &'a VerificationRecord> {
        self.assets
            .get(asset)
            .into_iter()
            .flat_map(|s| s.verifications.iter())
    }

    pub fn manifests<'a>(&'a self, volume: &str) -> impl Iterator<Item = &'a ManifestRecord> {
        self.manifests
            .get(volume)
            .into_iter()
            .flat_map(|s| s.iter())
    }
```

- [ ] **Step 8: Run — expect PASS**

Run: `cargo test -p majestical-core`

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/event.rs crates/core/src/projection.rs
git commit -m "feat: PARA, verification, and manifest ops with CRDT projection"
```

- [ ] **Step 10: Extend the proptest generator**

Open `crates/core/tests/crdt_properties.rs`, find the `Op` strategy (a
`prop_oneof!` over the existing five variants), and add arms generating every
new variant. Node ids draw from a small pool (`"N1" | "N2"`) so ops collide on
the same node; names from `"[a-z]{1,8}"`; kinds from
`prop_oneof![Just(ParaKind::Project), Just(ParaKind::Area), Just(ParaKind::Resource), Just(ParaKind::Archive)]`;
`VerifyOutcome` likewise; `hashdate_ms`/`generation` from small ranges so
records dedupe sometimes. Follow the existing arms' style exactly — the
property functions themselves (`apply` order-independence + idempotence over
shuffled event vectors) need no change; they must now pass over the widened
generator.

- [ ] **Step 11: Run properties — expect PASS**

Run: `cargo test -p majestical-core --test crdt_properties`
If a property fails, the projection arm is wrong — fix projection, not the test.

- [ ] **Step 12: Commit**

```bash
git add crates/core/tests/crdt_properties.rs
git commit -m "test: property coverage for PARA and ingest ops"
```

- [ ] **Step 13: Write failing sqlite test**

In `crates/catalog-sqlite/src/lib.rs` tests (reuse the `Event` scaffolding of
`rebuild_populates_volumes_and_asset_counts`):

```rust
    #[test]
    fn rebuild_populates_para_tables() {
        let mut p = Projection::default();
        for (n, op) in [
            Op::ParaNodeCreate {
                node: "N1".into(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            },
            Op::AssetSeen {
                asset: AssetId("xxh3:aa".into()),
                volume: "V1".into(),
                path: "a.mov".into(),
                size: 1,
            },
            Op::AssetParaSet {
                asset: AssetId("xxh3:aa".into()),
                node: "N1".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            p.apply(&Event {
                id: EventId(ulid::Ulid::from_parts(1, n as u128)),
                hlc: Hlc {
                    wall_ms: u64::try_from(n).expect("small") + 1,
                    counter: 0,
                    machine: MachineId("m1".into()),
                },
                author: "t".into(),
                op,
            });
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = SqliteCatalog::open(&dir.path().join("catalog.db")).expect("open");
        db.rebuild(&p).expect("rebuild");
        assert_eq!(
            db.para_nodes().expect("para query"),
            vec![("N1".to_string(), "project".to_string(), "client-x".to_string(), false)]
        );
    }
```

(`ParaKind` needs importing in the tests module.)

- [ ] **Step 14: Run — expect FAIL, then implement**

Extend `rebuild`'s `execute_batch` with drops + creates:

```sql
             DROP TABLE IF EXISTS para_nodes;
             DROP TABLE IF EXISTS asset_para;
             DROP TABLE IF EXISTS verifications;
             DROP TABLE IF EXISTS manifests;
             ...
             CREATE TABLE para_nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL,
               name TEXT NOT NULL, archived INTEGER NOT NULL
             );
             CREATE TABLE asset_para (
               asset TEXT NOT NULL PRIMARY KEY REFERENCES assets(id),
               node TEXT NOT NULL
             );
             CREATE TABLE verifications (
               asset TEXT NOT NULL, volume TEXT NOT NULL, path TEXT NOT NULL,
               algo TEXT NOT NULL, value TEXT NOT NULL, outcome TEXT NOT NULL,
               hashdate_ms INTEGER NOT NULL
             );
             CREATE TABLE manifests (
               volume TEXT NOT NULL, generation INTEGER NOT NULL,
               mhl_path TEXT NOT NULL, roothash TEXT NOT NULL,
               PRIMARY KEY (volume, generation, mhl_path)
             );
```

Populate inside the existing loops (asset_para + verifications inside the asset
loop via `projection.asset_para(asset)` / `projection.verifications(asset)`;
para_nodes + manifests in their own loops over the new iterators). Serialize
`ParaKind`/`VerifyOutcome` to their wire strings via
`serde_json::to_value(kind)` → `as_str` (or a small `kind_str` helper matching
the serde rename — keep it in one place). Add the inherent query used by the test:

```rust
    /// Every PARA node: (id, kind, name, archived), ordered by id.
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn para_nodes(&self) -> Result<Vec<(String, String, String, bool)>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, name, archived FROM para_nodes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
```

Nodes with no `ParaNodeCreate` seen yet (rename arriving first) have
`kind = None` / `name = None` — skip those rows in rebuild (they materialize
once the create arrives; document with a comment).

- [ ] **Step 15: Run full gate, commit**

Run: `just ci`

```bash
git add crates/catalog-sqlite/src/lib.rs
git commit -m "feat: PARA and ingest tables in sqlite projection"
```

- [ ] **Step 16: Push, PR, merge** (same commands as Task 1, branch `feat/para-ingest-ops`)

---

### Task 3: `maj para` command family

Branch `feat/para-cli` from fresh main. CLI-level behavior is covered by the
smoke-test pattern in `crates/cli/tests/cli_smoke.rs` — follow its existing
helpers for spawning `maj` against a temp catalog.

**Files:**
- Modify: `crates/cli/src/main.rs` (clap)
- Modify: `crates/cli/src/commands.rs`
- Test: `crates/cli/tests/cli_smoke.rs`

**Node reference format (pinned):** commands take `<kind>/<name>` (e.g.
`project/client-x`), resolved against non-archived nodes. If two concurrent
creates produced duplicate `(kind, name)`, resolution errors and lists the full
node ids; a raw node ULID is always accepted as a reference too.

- [ ] **Step 1: Write failing smoke tests**

```rust
#[test]
fn para_add_list_rename_archive_round_trip() {
    let t = TestCatalog::init(); // follow the existing smoke-test helper name
    t.maj(&["para", "add", "project", "client-x"]).success();
    let out = t.maj(&["para", "list", "--json"]).success_stdout();
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let nodes = v["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["kind"], "project");
    assert_eq!(nodes[0]["name"], "client-x");
    assert_eq!(nodes[0]["archived"], false);
    let node_ref = "project/client-x";
    t.maj(&["para", "rename", node_ref, "client-y"]).success();
    t.maj(&["para", "archive", "project/client-y"]).success();
    let out = t.maj(&["para", "list", "--json"]).success_stdout();
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["nodes"][0]["archived"], true);
}

#[test]
fn para_add_rejects_duplicate_active_name() {
    let t = TestCatalog::init();
    t.maj(&["para", "add", "project", "client-x"]).success();
    t.maj(&["para", "add", "project", "client-x"])
        .failure_stderr_contains("already exists");
}

#[test]
fn para_archive_moves_materialized_dir_with_root() {
    let t = TestCatalog::init();
    t.maj(&["para", "add", "project", "client-x"]).success();
    let root = t.tempdir_path("dest");
    std::fs::create_dir_all(root.join("Projects/client-x")).expect("materialize");
    std::fs::write(root.join("Projects/client-x/a.txt"), b"x").expect("file");
    t.maj(&[
        "para", "archive", "project/client-x",
        "--root", root.to_str().expect("utf8"),
    ])
    .success();
    assert!(!root.join("Projects/client-x").exists());
    assert!(root.join("Archives/client-x/a.txt").exists());
}
```

Adapt helper names to what `cli_smoke.rs` actually provides; add a
`failure_stderr_contains` helper if missing.

- [ ] **Step 2: Run — expect FAIL** (`para` unknown subcommand)

Run: `cargo test -p majestical-cli --test cli_smoke para_`

- [ ] **Step 3: Add clap surface**

In `main.rs`:

```rust
    /// Manage PARA organization nodes.
    Para {
        #[command(subcommand)]
        cmd: ParaCmd,
    },
```

```rust
#[derive(Subcommand)]
enum ParaCmd {
    /// Create a node: `maj para add project client-x`.
    Add { kind: String, name: String },
    List {
        #[arg(long)]
        json: bool,
    },
    /// Rename a node (last-write-wins across machines).
    Rename { node: String, name: String },
    /// Archive a node; with --root, also moves the materialized directory.
    Archive {
        node: String,
        /// Destination root(s) where the node is materialized on disk.
        #[arg(long)]
        root: Vec<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}
```

Dispatch in `main()` follows the `Cmd::Meta` pattern → `commands::cmd_para`.

- [ ] **Step 4: Implement handlers in `commands.rs`**

```rust
fn parse_kind(kind: &str) -> Result<ParaKind> {
    match kind {
        "project" => Ok(ParaKind::Project),
        "area" => Ok(ParaKind::Area),
        "resource" => Ok(ParaKind::Resource),
        "archive" => Ok(ParaKind::Archive),
        other => anyhow::bail!("unknown PARA kind '{other}' — one of: project, area, resource, archive"),
    }
}

/// Resolves `<kind>/<name>` or a raw node ULID against non-archived nodes.
pub(crate) fn resolve_para_node(projection: &Projection, reference: &str) -> Result<String> {
    if projection.para_node(reference).is_some() {
        return Ok(reference.to_string());
    }
    let Some((kind_str, name)) = reference.split_once('/') else {
        anyhow::bail!("unknown PARA node '{reference}' — use <kind>/<name> or a node id from `maj para list`");
    };
    let kind = parse_kind(kind_str)?;
    let matches: Vec<&String> = projection
        .para_nodes()
        .filter(|(_, st)| {
            !st.archived() && st.kind() == Some(kind) && st.name() == Some(name)
        })
        .map(|(id, _)| id)
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no active PARA node '{reference}' — see `maj para list`"),
        [id] => Ok((*id).clone()),
        many => anyhow::bail!(
            "'{reference}' is ambiguous (concurrent creates); use a node id: {}",
            many.iter().map(String::as_str).collect::<Vec<_>>().join(", ")
        ),
    }
}

pub(crate) fn cmd_para(app: &mut FsApp, catalog_dir: &Path, cmd: ParaCmd) -> Result<()> {
    match cmd {
        ParaCmd::Add { kind, name } => {
            let kind = parse_kind(&kind)?;
            let p = app.projection()?;
            let duplicate = p.para_nodes().any(|(_, st)| {
                !st.archived() && st.kind() == Some(kind) && st.name() == Some(name.as_str())
            });
            anyhow::ensure!(!duplicate, "active {}/{} already exists", kind.dir_name(), name);
            let node = ulid::Ulid::new().to_string();
            app.emit(vec![Op::ParaNodeCreate { node: node.clone(), kind, name }])?;
            println!("{node}");
        }
        ParaCmd::List { json } => cmd_para_list(app, catalog_dir, json)?,
        ParaCmd::Rename { node, name } => {
            let p = app.projection()?;
            let node = resolve_para_node(&p, &node)?;
            app.emit(vec![Op::ParaNodeRename { node, name }])?;
            println!("ok");
        }
        ParaCmd::Archive { node, root, dry_run } => {
            let p = app.projection()?;
            let node_id = resolve_para_node(&p, &node)?;
            let st = p.para_node(&node_id).expect("resolved above");
            let (kind, name) = (
                st.kind().context("node has no create event yet")?,
                st.name().context("node has no name yet")?.to_string(),
            );
            for r in &root {
                let from = r.join(kind.dir_name()).join(&name);
                let to = r.join("Archives").join(&name);
                if dry_run {
                    println!("would move {} -> {}", from.display(), to.display());
                    continue;
                }
                anyhow::ensure!(from.is_dir(), "not materialized at {}", from.display());
                anyhow::ensure!(!to.exists(), "{} already exists — resolve manually", to.display());
                std::fs::create_dir_all(r.join("Archives")).context("creating Archives/")?;
                std::fs::rename(&from, &to)
                    .with_context(|| format!("moving {} -> {}", from.display(), to.display()))?;
                println!("moved {} -> {}", from.display(), to.display());
            }
            if dry_run {
                return Ok(());
            }
            app.emit(vec![Op::ParaNodeArchive { node: node_id }])?;
            if root.is_empty() {
                println!("archived in catalog; no --root given, no directories moved");
            }
        }
    }
    Ok(())
}
```

`cmd_para_list` follows `cmd_volumes_list`: projection → sqlite rebuild →
`db.para_nodes()` → JSON `{"nodes":[{"id","kind","name","archived"}]}` or an
aligned table (follow `print_volumes_table`'s width pattern).

Note `expect("resolved above")` violates the workspace `expect_used = "warn"`
budget only as a warn — restructure with `let Some(st) = ... else` to keep zero
warnings.

- [ ] **Step 5: Run smoke tests — expect PASS**

Run: `cargo test -p majestical-cli --test cli_smoke para_`

- [ ] **Step 6: Full gate, commit, push, PR, merge**

Run: `just ci`

```bash
git add crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/tests/cli_smoke.rs
git commit -m "feat: maj para add/list/rename/archive"
```

Branch `feat/para-cli`; PR body: what the commands do + the pinned reference
format. Squash-merge after CI green.

---

### Task 4: Ingest planner with dedupe and PARA routing

Branch `feat/ingest-planner`. New crate; no CLI wiring yet.

**Files:**
- Create: `crates/ingest/Cargo.toml`
- Create: `crates/ingest/src/lib.rs`
- Create: `crates/ingest/src/template.rs`
- Create: `crates/ingest/src/plan.rs`
- Modify: `Cargo.toml` (workspace members — follow how `crates/sync` is listed)

- [ ] **Step 1: Crate scaffolding**

`crates/ingest/Cargo.toml` (copy the lint table reference and edition from
`crates/sync/Cargo.toml`; use workspace deps where they exist):

```toml
[package]
name = "majestical-ingest"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

[dependencies]
majestical-core = { path = "../core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
ulid = "1"
walkdir = "2"
xxhash-rust = { version = "0.8", features = ["xxh3", "xxh64"] }

[dev-dependencies]
proptest = "1"
tempfile = "3"

[lints]
workspace = true
```

Match versions to what the workspace already uses (check the root `Cargo.toml`
and other crates — do NOT trust the numbers above).

`crates/ingest/src/lib.rs`:

```rust
//! Ingest engine: plan, verified copy, transfer journal, ASC MHL.
pub mod plan;
pub mod template;

/// Errors from the ingest engine. Every variant names the operation and the
/// path so a failure is actionable without a debugger.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("walking source {path}: {source}")]
    Walk {
        path: std::path::PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("reading {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "non-UTF-8 file name at {} — ASC MHL cannot represent it; rename the file to ingest it",
        path.display()
    )]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("template: {0}")]
    Template(String),
}
```

- [ ] **Step 2: Write failing template tests**

`crates/ingest/src/template.rs`:

```rust
//! `{token}` layout templates for the destination path inside a PARA node.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_known_tokens() {
        let ctx = TemplateCtx {
            date: "2026-07-29".into(),
            source_label: "card-a".into(),
        };
        assert_eq!(
            render("{date}/{source-label}", &ctx).expect("render"),
            "2026-07-29/card-a"
        );
    }

    #[test]
    fn unknown_token_is_an_error() {
        let ctx = TemplateCtx {
            date: "d".into(),
            source_label: "s".into(),
        };
        let err = render("{nope}", &ctx).expect_err("must fail");
        assert!(err.to_string().contains("unknown token 'nope'"));
    }

    #[test]
    fn unbalanced_brace_is_an_error() {
        let ctx = TemplateCtx {
            date: "d".into(),
            source_label: "s".into(),
        };
        render("{date", &ctx).expect_err("must fail");
    }

    #[test]
    fn traversal_and_absolute_segments_are_rejected() {
        let ctx = TemplateCtx {
            date: "..".into(),
            source_label: "s".into(),
        };
        render("{date}/{source-label}", &ctx).expect_err("dot-dot segment must fail");
        let ctx = TemplateCtx {
            date: "d".into(),
            source_label: "/abs".into(),
        };
        render("{date}/{source-label}", &ctx).expect_err("separator inside a value must fail");
    }
}
```

And a property test in the same file:

```rust
    proptest::proptest! {
        #[test]
        fn rendered_paths_are_always_safe_relative(
            date in "[a-zA-Z0-9._ -]{1,12}",
            label in "[a-zA-Z0-9._ -]{1,12}",
        ) {
            let ctx = TemplateCtx { date, source_label: label };
            if let Ok(out) = render("{date}/{source-label}", &ctx) {
                proptest::prop_assert!(!out.starts_with('/'));
                for seg in out.split('/') {
                    proptest::prop_assert!(!seg.is_empty());
                    proptest::prop_assert!(seg != "..");
                }
            }
        }
    }
```

- [ ] **Step 3: Run — expect FAIL, then implement**

```rust
use crate::IngestError;

/// Values substituted into a layout template.
pub struct TemplateCtx {
    pub date: String,
    pub source_label: String,
}

/// Renders `{token}` templates. Tokens: `{date}`, `{source-label}`. The
/// result is a relative path fragment; values must not smuggle separators
/// or traversal segments into it.
///
/// # Errors
/// Returns `IngestError::Template` on unknown tokens, unbalanced braces, or
/// values that would produce an absolute, empty, or `..` path segment.
pub fn render(template: &str, ctx: &TemplateCtx) -> Result<String, IngestError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(IngestError::Template(format!(
                "unbalanced '{{' in template '{template}'"
            )));
        };
        let token = &after[..close];
        let value = match token {
            "date" => &ctx.date,
            "source-label" => &ctx.source_label,
            other => {
                return Err(IngestError::Template(format!(
                    "unknown token '{other}' — known: date, source-label"
                )));
            }
        };
        if value.contains('/') || value.contains('\\') {
            return Err(IngestError::Template(format!(
                "value for '{{{token}}}' contains a path separator: '{value}'"
            )));
        }
        out.push_str(value);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    for seg in out.split('/') {
        if seg.is_empty() || seg == ".." || seg == "." {
            return Err(IngestError::Template(format!(
                "template '{template}' rendered an unsafe segment '{seg}'"
            )));
        }
    }
    Ok(out)
}
```

Run: `cargo test -p majestical-ingest template`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/ingest/Cargo.toml crates/ingest/src/lib.rs crates/ingest/src/template.rs
git commit -m "feat: ingest crate with layout templates"
```

- [ ] **Step 5: Write failing planner tests**

`crates/ingest/src/plan.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &std::path::Path, rel: &str, bytes: &[u8]) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, bytes).expect("write");
    }

    #[test]
    fn plans_new_files_and_confirms_duplicates_by_content_hash() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "clips/a.mov", b"AAAA");
        write(src.path(), "clips/b.mov", b"BBBBBB");
        // Known catalog: an asset with a's exact bytes (size 4) and an
        // unrelated same-size-as-b asset whose hash won't match b.
        let known = KnownAssets::from_pairs(vec![
            (hash_bytes(b"AAAA"), 4),
            (hash_bytes(b"XXXXXX"), 6),
        ]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        let by_rel: std::collections::BTreeMap<_, _> = plan
            .files
            .iter()
            .map(|f| (f.rel.clone(), f))
            .collect();
        // a: size matched AND pre-hash confirmed -> duplicate, skipped.
        match &by_rel["clips/a.mov"].decision {
            Decision::Duplicate { asset, action } => {
                assert_eq!(asset.0, format!("xxh3:{}", hash_bytes(b"AAAA")));
                assert_eq!(*action, DedupeMode::Skip);
            }
            other => panic!("expected duplicate, got {other:?}"),
        }
        // b: size matched but hash differs -> copies.
        assert!(matches!(by_rel["clips/b.mov"].decision, Decision::Copy));
    }

    #[test]
    fn size_prefilter_avoids_hashing_unmatched_sizes() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "c.mov", b"CCCCCCCC");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert!(matches!(plan.files[0].decision, Decision::Copy));
        assert!(
            plan.files[0].prehash.is_none(),
            "no known asset of size 8 — planner must not have hashed the source"
        );
    }

    #[test]
    fn zero_byte_file_is_flagged() {
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "empty.bin", b"");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert!(matches!(plan.files[0].decision, Decision::Rejected { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_name_is_rejected_per_file_not_fatally() {
        use std::os::unix::ffi::OsStrExt;
        let src = tempfile::tempdir().expect("tempdir");
        write(src.path(), "ok.mov", b"OK");
        let bad = src
            .path()
            .join(std::ffi::OsStr::from_bytes(b"bad\xFFname"));
        fs::write(&bad, b"BAD").expect("write raw-byte name");
        let known = KnownAssets::from_pairs(vec![]);
        let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
        assert_eq!(plan.files.len(), 2);
        let rejected: Vec<_> = plan
            .files
            .iter()
            .filter(|f| matches!(f.decision, Decision::Rejected { .. }))
            .collect();
        assert_eq!(rejected.len(), 1, "only the raw-byte name is rejected");
    }
}
```

Note: APFS rejects invalid-UTF-8 names, so the `#[cfg(unix)]` test may be
unable to create the file on a Mac dev machine — if `fs::write` fails with
`InvalidInput`/`IlSeq`, `return` early with an eprintln note (the sync crate has
precedent for environment-dependent tests; follow whatever pattern exists, and
keep the test meaningful on filesystems that do allow raw bytes).

- [ ] **Step 6: Run — expect FAIL, then implement the planner**

```rust
//! Ingest planning: walk the source, decide per file, hash only when the
//! size prefilter says a duplicate is possible.
use crate::IngestError;
use majestical_core::event::AssetId;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// What the catalog already knows: content hashes grouped by file size, so
/// the planner can skip hashing sources whose size matches nothing.
#[derive(Debug, Default)]
pub struct KnownAssets {
    by_size: BTreeMap<u64, BTreeSet<String>>,
}

impl KnownAssets {
    #[must_use]
    pub fn from_pairs(pairs: Vec<(String, u64)>) -> Self {
        let mut by_size: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for (hash, size) in pairs {
            by_size.entry(size).or_default().insert(hash);
        }
        Self { by_size }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupeMode {
    Skip,
    CopyAnyway,
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// New content: copy and verify.
    Copy,
    /// Content hash already in the catalog; `action` is the run's mode.
    Duplicate { asset: AssetId, action: DedupeMode },
    /// Not ingestable; the run continues without it.
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedFile {
    pub source: PathBuf,
    /// Path relative to the source root, `/`-separated.
    pub rel: String,
    pub size: u64,
    /// xxh3-128 hex computed during planning, only when the size prefilter
    /// matched (dedupe confirmation); the engine reuses it when present.
    pub prehash: Option<String>,
    pub decision: Decision,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestPlan {
    pub files: Vec<PlannedFile>,
}

/// Streams xxh3-128 over a file. 1 MiB chunks: media files are large and
/// sequential; bigger buffers than scan's 64 KiB measurably help here.
pub(crate) fn hash_file(path: &Path) -> Result<String, IngestError> {
    let file = std::fs::File::open(path).map_err(|source| IngestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader.read(&mut buf).map_err(|source| IngestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:032x}", hasher.digest128()))
}

#[cfg(test)]
pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:032x}", xxhash_rust::xxh3::xxh3_128(bytes))
}

/// Walks `source` and produces the run plan.
///
/// # Errors
/// Fails only on unwalkable directories or unreadable candidate files;
/// per-file conditions (0-byte, non-UTF-8 name) become `Decision::Rejected`.
pub fn plan_source(
    source: &Path,
    known: &KnownAssets,
    mode: DedupeMode,
) -> Result<IngestPlan, IngestError> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(source).sort_by_file_name() {
        let entry = entry.map_err(|source_err| IngestError::Walk {
            path: source.to_path_buf(),
            source: source_err,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let size = entry
            .metadata()
            .map_err(|e| IngestError::Walk {
                path: path.clone(),
                source: e,
            })?
            .len();
        let rel_os = path.strip_prefix(source).unwrap_or(&path);
        let Some(rel) = rel_os.to_str().map(|s| s.replace('\\', "/")) else {
            files.push(PlannedFile {
                source: path.clone(),
                rel: rel_os.to_string_lossy().replace('\\', "/"),
                size,
                prehash: None,
                decision: Decision::Rejected {
                    reason: IngestError::NonUtf8Path { path }.to_string(),
                },
            });
            continue;
        };
        if size == 0 {
            files.push(PlannedFile {
                source: path,
                rel,
                size,
                prehash: None,
                decision: Decision::Rejected {
                    reason: "0-byte file — nothing to verify; ingest refuses it".into(),
                },
            });
            continue;
        }
        let (prehash, decision) = match known.by_size.get(&size) {
            None => (None, Decision::Copy),
            Some(candidates) => {
                let hash = hash_file(&path)?;
                if candidates.contains(&hash) {
                    let asset = AssetId(format!("xxh3:{hash}"));
                    (Some(hash), Decision::Duplicate { asset, action: mode })
                } else {
                    (Some(hash), Decision::Copy)
                }
            }
        };
        files.push(PlannedFile {
            source: path,
            rel,
            size,
            prehash,
            decision,
        });
    }
    Ok(IngestPlan { files })
}
```

Run: `cargo test -p majestical-ingest plan`
Expected: PASS.

- [ ] **Step 7: Full gate, commit, push, PR, merge**

Run: `just ci`

```bash
git add crates/ingest/src/plan.rs crates/ingest/src/lib.rs
git commit -m "feat: ingest planner with size-prefiltered content dedupe"
```

Branch `feat/ingest-planner`, PR, squash-merge after green.

---

### Task 5: Verified copy engine with resume journal

Branch `feat/ingest-engine`.

**Files:**
- Create: `crates/ingest/src/journal.rs`
- Create: `crates/ingest/src/engine.rs`
- Modify: `crates/ingest/src/lib.rs` (add `pub mod engine; pub mod journal;` and new `IngestError` variants)
- Test: `crates/ingest/tests/engine.rs`

- [ ] **Step 1: Write failing journal tests**

`crates/ingest/src/journal.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Decision, PlannedFile};

    fn planned(rel: &str) -> PlannedFile {
        PlannedFile {
            source: std::path::PathBuf::from(format!("/src/{rel}")),
            rel: rel.into(),
            size: 4,
            prehash: None,
            decision: Decision::Copy,
        }
    }

    #[test]
    fn journal_round_trips_and_folds_placed_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        let mut j = Journal::create(&path).expect("create");
        j.append(&Record::FilePlanned { file: planned("a.mov") }).expect("w");
        j.append(&Record::FilePlanned { file: planned("b.mov") }).expect("w");
        j.append(&Record::FileCopied { rel: "a.mov".into() }).expect("w");
        j.append(&Record::FileVerified { rel: "a.mov".into() }).expect("w");
        j.append(&Record::FilePlaced { rel: "a.mov".into() }).expect("w");
        j.append(&Record::FileFailed {
            rel: "b.mov".into(),
            reason: "verify mismatch".into(),
        })
        .expect("w");
        let folded = Journal::load(&path).expect("load");
        assert!(folded.placed.contains("a.mov"));
        assert!(!folded.placed.contains("b.mov"));
        assert_eq!(folded.failed.get("b.mov").map(String::as_str), Some("verify mismatch"));
        assert_eq!(folded.planned.len(), 2);
    }

    #[test]
    fn corrupt_trailing_line_is_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        let mut j = Journal::create(&path).expect("create");
        j.append(&Record::FilePlanned { file: planned("a.mov") }).expect("w");
        let mut bytes = std::fs::read(&path).expect("read");
        bytes.extend_from_slice(b"{\"rec\":\"file_pl"); // torn write
        std::fs::write(&path, bytes).expect("write");
        let folded = Journal::load(&path).expect("torn tail must not be fatal");
        assert_eq!(folded.planned.len(), 1);
    }

    proptest::proptest! {
        /// Any prefix of a journal folds without panicking, and its placed
        /// set is a subset of the full journal's placed set.
        #[test]
        fn any_prefix_folds_consistently(cut in 0usize..6) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("run.jsonl");
            let mut j = Journal::create(&path).expect("create");
            let records = [
                Record::FilePlanned { file: planned("a.mov") },
                Record::FileCopied { rel: "a.mov".into() },
                Record::FileVerified { rel: "a.mov".into() },
                Record::FilePlaced { rel: "a.mov".into() },
                Record::FilePlanned { file: planned("b.mov") },
            ];
            for r in &records {
                j.append(r).expect("w");
            }
            let full = Journal::load(&path).expect("load");
            let text = std::fs::read_to_string(&path).expect("read");
            let prefix: String = text.lines().take(cut).map(|l| format!("{l}\n")).collect();
            std::fs::write(&path, prefix).expect("truncate");
            let part = Journal::load(&path).expect("prefix folds");
            proptest::prop_assert!(part.placed.is_subset(&full.placed));
        }
    }
}
```

- [ ] **Step 2: Run — expect FAIL, then implement the journal**

```rust
//! Transfer journal: JSONL checkpoints making a run resumable at file
//! granularity. One line per state transition; the tail line may be torn
//! (crash mid-write) and is skipped on load.
use crate::IngestError;
use crate::plan::PlannedFile;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub enum Record {
    RunStarted {
        run: String,
        source: String,
        dests: Vec<String>,
    },
    FilePlanned { file: PlannedFile },
    FileCopied { rel: String },
    FileVerified { rel: String },
    FilePlaced { rel: String },
    FileFailed { rel: String, reason: String },
}

/// Folded view of a journal: what a resume needs to know.
#[derive(Debug, Default)]
pub struct Folded {
    pub planned: BTreeMap<String, PlannedFile>,
    pub placed: BTreeSet<String>,
    pub failed: BTreeMap<String, String>,
}

pub struct Journal {
    file: std::fs::File,
}

impl Journal {
    /// # Errors
    /// Fails when the journal file can't be created.
    pub fn create(path: &Path) -> Result<Self, IngestError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self { file })
    }

    /// Appends one record and flushes it to disk — a checkpoint is only a
    /// checkpoint if it survives the crash that follows it.
    /// # Errors
    /// Fails when the record can't be serialized or written.
    pub fn append(&mut self, record: &Record) -> Result<(), IngestError> {
        let mut line = serde_json::to_string(record).map_err(IngestError::JournalEncode)?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|source| IngestError::Journal {
                path: std::path::PathBuf::from("<journal>"),
                source,
            })
    }

    /// Loads and folds a journal. A torn (unparseable) line ends the fold —
    /// everything before it counts, matching append-then-crash reality.
    /// # Errors
    /// Fails only when the file itself can't be read.
    pub fn load(path: &Path) -> Result<Folded, IngestError> {
        let text = std::fs::read_to_string(path).map_err(|source| IngestError::Journal {
            path: path.to_path_buf(),
            source,
        })?;
        let mut folded = Folded::default();
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                break;
            };
            match record {
                Record::RunStarted { .. }
                | Record::FileCopied { .. }
                | Record::FileVerified { .. } => {}
                Record::FilePlanned { file } => {
                    folded.planned.insert(file.rel.clone(), file);
                }
                Record::FilePlaced { rel } => {
                    folded.placed.insert(rel);
                }
                Record::FileFailed { rel, reason } => {
                    folded.failed.insert(rel, reason);
                }
            }
        }
        Ok(folded)
    }
}
```

Add to `IngestError` in `lib.rs`:

```rust
    #[error("journal {path}: {source}")]
    Journal {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("journal encode: {0}")]
    JournalEncode(#[source] serde_json::Error),
    #[error("writing {path}: {source}")]
    WriteDest {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
```

Run: `cargo test -p majestical-ingest journal`
Expected: PASS. Commit:

```bash
git add crates/ingest/src/journal.rs crates/ingest/src/lib.rs
git commit -m "feat: resumable transfer journal"
```

- [ ] **Step 3: Write failing engine tests**

`crates/ingest/tests/engine.rs`:

```rust
//! Engine acceptance: real files in temp dirs, fault injection via SinkFactory.
use majestical_ingest::engine::{run, DestSpec, EngineConfig, RealSinks, Sink, SinkFactory};
use majestical_ingest::journal::Journal;
use majestical_ingest::plan::{plan_source, DedupeMode, KnownAssets};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// No prior journal: empty resume set.
fn fresh() -> BTreeSet<String> {
    BTreeSet::new()
}

fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, bytes).expect("write");
}

fn setup(files: &[(&str, &[u8])]) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().expect("src");
    for (rel, bytes) in files {
        write(src.path(), rel, bytes);
    }
    (src, tempfile::tempdir().expect("d1"), tempfile::tempdir().expect("d2"))
}

fn dests(d1: &Path, d2: &Path) -> Vec<DestSpec> {
    vec![
        DestSpec { root: d1.to_path_buf(), subdir: "Projects/x/day1".into() },
        DestSpec { root: d2.to_path_buf(), subdir: "Projects/x/day1".into() },
    ]
}

#[test]
fn copies_verifies_and_places_to_every_destination() {
    let (src, d1, d2) = setup(&[("clips/a.mov", b"AAAA"), ("b.wav", b"BBBBBB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::create(&jpath).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 2 },
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 2);
    assert!(outcome.failed.is_empty());
    for d in [d1.path(), d2.path()] {
        assert_eq!(
            std::fs::read(d.join("Projects/x/day1/clips/a.mov")).expect("placed"),
            b"AAAA"
        );
        assert_eq!(
            std::fs::read(d.join("Projects/x/day1/b.wav")).expect("placed"),
            b"BBBBBB"
        );
    }
    let placed_a = outcome
        .placed
        .iter()
        .find(|p| p.rel == "clips/a.mov")
        .expect("a placed");
    assert_eq!(placed_a.xxh64, format!("{:016x}", xxhash_rust::xxh64::xxh64(b"AAAA", 0)));
    assert_eq!(placed_a.xxh3, format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"AAAA")));
}

/// Flips the first byte it writes for paths containing `target`, corrupting
/// the destination between write and read-back — exactly the failure
/// read-back verification exists to catch.
struct CorruptingSinks {
    target: &'static str,
}

struct CorruptingSink {
    inner: Box<dyn Sink>,
    corrupt: bool,
    done: bool,
}

impl Write for CorruptingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.corrupt && !self.done && !buf.is_empty() {
            self.done = true;
            let mut flipped = buf.to_vec();
            flipped[0] ^= 0xFF;
            return self.inner.write(&flipped);
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Sink for CorruptingSink {
    fn finish(&mut self) -> std::io::Result<()> {
        self.inner.finish()
    }
}

impl SinkFactory for CorruptingSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        let corrupt = path.to_string_lossy().contains(self.target);
        Ok(Box::new(CorruptingSink {
            inner: RealSinks.open(path)?,
            corrupt,
            done: false,
        }))
    }
}

#[test]
fn corrupted_destination_fails_verification_and_stays_quarantined() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::create(&d1.path().join("run.jsonl")).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        // Corrupt only destination 1's copy.
        &CorruptingSinks { target: d1.path().to_str().expect("utf8") },
        &EngineConfig { jobs: 1 },
    )
    .expect("run");
    assert_eq!(outcome.failed.len(), 1, "corrupted dest fails");
    assert!(
        !d1.path().join("Projects/x/day1/a.mov").exists(),
        "corrupt copy must never be renamed into place"
    );
    let quarantined: Vec<_> = walkdir::WalkDir::new(d1.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".maj-partial-"))
        .collect();
    assert_eq!(quarantined.len(), 1, "partial stays under its temp name");
    // The healthy destination is independent: it still gets its verified copy.
    assert!(d2.path().join("Projects/x/day1/a.mov").exists());
}

#[test]
fn resume_skips_placed_files() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::create(&jpath).expect("journal");
    run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
    )
    .expect("first run");
    // Delete one placed output, then resume from the journal: the deleted
    // file was Placed, so resume must NOT redo it (resume trusts the
    // journal, `maj verify` catches later damage); an un-journaled file
    // would be redone.
    let folded = Journal::load(&jpath).expect("fold");
    assert_eq!(folded.placed.len(), 2);
    let mut journal = Journal::create(&jpath).expect("reopen appends");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &folded.placed,
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
    )
    .expect("resume");
    assert!(outcome.placed.is_empty(), "everything already placed");
    assert_eq!(outcome.skipped_resumed, 2);
}

#[test]
fn duplicate_skip_does_not_copy() {
    let (src, d1, d2) = setup(&[("dup.mov", b"AAAA")]);
    let known = KnownAssets::from_pairs(vec![(
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"AAAA")),
        4,
    )]);
    let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
    let mut journal = Journal::create(&d1.path().join("run.jsonl")).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
    )
    .expect("run");
    assert!(outcome.placed.is_empty());
    assert_eq!(outcome.skipped_duplicates.len(), 1);
    assert!(!d1.path().join("Projects/x/day1/dup.mov").exists());
}
```

Add `walkdir` and `xxhash-rust` to `[dev-dependencies]` if not already visible
to tests.

- [ ] **Step 4: Run — expect FAIL, then implement the engine**

`crates/ingest/src/engine.rs`:

```rust
//! File-parallel verified copy: stream the source once, hash xxh64+xxh3 in
//! the same pass, fan chunks out to every destination, fsync, read back
//! each destination independently, and only then rename into place.
use crate::journal::{Journal, Record};
use crate::plan::{Decision, DedupeMode, IngestPlan, PlannedFile};
use crate::IngestError;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One verified destination: files land under `root/subdir/<rel>`.
#[derive(Debug, Clone)]
pub struct DestSpec {
    pub root: PathBuf,
    /// `/`-separated, pre-rendered (PARA dir + template), safe-relative.
    pub subdir: String,
}

pub struct EngineConfig {
    pub jobs: usize,
}

/// Destination write handle; `finish` must not return until bytes are
/// durable (fsync) — read-back verification is only meaningful after it.
pub trait Sink: Write + Send {
    /// # Errors
    /// Fails when flushing or syncing the destination fails.
    fn finish(&mut self) -> std::io::Result<()>;
}

/// Opens destination sinks. The seam fault-injection tests wrap.
pub trait SinkFactory: Sync {
    /// # Errors
    /// Fails when the destination file can't be created.
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>>;
}

pub struct RealSinks;

struct FileSink(std::fs::File);

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Sink for FileSink {
    fn finish(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        self.0.sync_all()
    }
}

impl SinkFactory for RealSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Box::new(FileSink(std::fs::File::create(path)?)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub rel: String,
    pub size: u64,
    pub xxh3: String,
    pub xxh64: String,
    /// Final path under each destination root, `/`-separated relative.
    pub dest_rel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFile {
    pub rel: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub placed: Vec<PlacedFile>,
    pub failed: Vec<FailedFile>,
    pub skipped_duplicates: Vec<String>,
    pub rejected: Vec<FailedFile>,
    pub skipped_resumed: usize,
}

/// Runs the plan against every destination. `resume` is the set of rel
/// paths a prior run's journal already recorded as Placed (empty for a
/// fresh run) — the CLI folds the journal and passes it in.
///
/// # Errors
/// Fails only on journal I/O; per-file copy and verification problems land
/// in `Outcome::failed` so one bad file never aborts the card.
pub fn run(
    plan: &IngestPlan,
    dests: &[DestSpec],
    resume: &std::collections::BTreeSet<String>,
    journal: &mut Journal,
    sinks: &dyn SinkFactory,
    config: &EngineConfig,
) -> Result<Outcome, IngestError> {
    let journal = Mutex::new(journal);
    let mut outcome = Outcome::default();
    let already_placed = resume;
    let mut queue = VecDeque::new();
    for file in &plan.files {
        match &file.decision {
            Decision::Rejected { reason } => {
                outcome.rejected.push(FailedFile {
                    rel: file.rel.clone(),
                    reason: reason.clone(),
                });
            }
            Decision::Duplicate { action: DedupeMode::Skip, .. } => {
                outcome.skipped_duplicates.push(file.rel.clone());
            }
            Decision::Duplicate { .. } | Decision::Copy => {
                if already_placed.contains(&file.rel) {
                    outcome.skipped_resumed += 1;
                } else {
                    queue.push_back(file.clone());
                }
            }
        }
    }
    let queue = Mutex::new(queue);
    let results = Mutex::new((Vec::new(), Vec::new())); // (placed, failed)
    std::thread::scope(|scope| {
        for _ in 0..config.jobs.max(1) {
            scope.spawn(|| loop {
                let Some(file) = queue.lock().expect("queue lock").pop_front() else {
                    return;
                };
                let result = copy_one(&file, dests, &journal, sinks);
                let mut results = results.lock().expect("results lock");
                match result {
                    Ok(placed) => results.0.push(placed),
                    Err(reason) => results.1.push(FailedFile { rel: file.rel, reason }),
                }
            });
        }
    });
    let (placed, failed) = results.into_inner().expect("results");
    outcome.placed = placed;
    outcome.failed = failed;
    outcome.placed.sort_by(|a, b| a.rel.cmp(&b.rel));
    outcome.failed.sort_by(|a, b| a.rel.cmp(&b.rel));
    // End-of-run missing-file sweep (spec §3): every file this run believes
    // is placed must actually exist at every destination — a rename that
    // "succeeded" onto a yanked drive must not stay a silent success.
    let mut still_placed = Vec::new();
    for p in outcome.placed.drain(..) {
        let missing: Vec<String> = dests
            .iter()
            .filter(|d| !d.root.join(&d.subdir).join(&p.rel).is_file())
            .map(|d| d.root.display().to_string())
            .collect();
        if missing.is_empty() {
            still_placed.push(p);
        } else {
            outcome.failed.push(FailedFile {
                rel: p.rel,
                reason: format!("placed but missing at end-of-run sweep: {}", missing.join(", ")),
            });
        }
    }
    outcome.placed = still_placed;
    Ok(outcome)
}
```

`copy_one` (same file; journal transitions bracket each stage — errors return
a human `reason` string, and every failure path appends `FileFailed`):

```rust
fn copy_one(
    file: &PlannedFile,
    dests: &[DestSpec],
    journal: &Mutex<&mut Journal>,
    sinks: &dyn SinkFactory,
) -> Result<PlacedFile, String> {
    let log = |record: Record| -> Result<(), String> {
        journal
            .lock()
            .expect("journal lock")
            .append(&record)
            .map_err(|e| format!("journal: {e}"))
    };
    log(Record::FilePlanned { file: file.clone() })?;
    let fail = |reason: String| {
        let _ = log(Record::FileFailed { rel: file.rel.clone(), reason: reason.clone() });
        reason
    };
    let dest_rel = format!("{}/{}", dests_subdir_of(file, dests), file.rel);
    // 1. Open a temp sink per destination.
    let token = ulid::Ulid::new().to_string();
    let mut open_dests: Vec<(PathBuf, PathBuf, Box<dyn Sink>)> = Vec::new();
    for d in dests {
        let final_path = d.root.join(&d.subdir).join(&file.rel);
        let temp_path = temp_path_for(&final_path, &token);
        let sink = sinks
            .open(&temp_path)
            .map_err(|e| fail(format!("creating {}: {e}", temp_path.display())))?;
        open_dests.push((final_path, temp_path, sink));
    }
    // 2. Single read pass: hash both algorithms, fan out writes.
    let source = std::fs::File::open(&file.source)
        .map_err(|e| fail(format!("reading {}: {e}", file.source.display())))?;
    let mut reader = std::io::BufReader::new(source);
    let mut xxh3 = xxhash_rust::xxh3::Xxh3::new();
    let mut xxh64 = xxhash_rust::xxh64::Xxh64::new(0);
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| fail(format!("reading {}: {e}", file.source.display())))?;
        if n == 0 {
            break;
        }
        xxh3.update(&buf[..n]);
        xxh64.update(&buf[..n]);
        for (_, temp, sink) in &mut open_dests {
            sink.write_all(&buf[..n])
                .map_err(|e| fail(format!("writing {}: {e}", temp.display())))?;
        }
    }
    let want64 = format!("{:016x}", xxh64.digest());
    let want3 = format!("{:032x}", xxh3.digest128());
    if let Some(pre) = &file.prehash {
        if *pre != want3 {
            return Err(fail(format!(
                "{} changed between planning and copy (hash {} then {want3}) — source is being written; re-run when it settles",
                file.source.display(),
                pre
            )));
        }
    }
    for (_, temp, sink) in &mut open_dests {
        sink.finish()
            .map_err(|e| fail(format!("syncing {}: {e}", temp.display())))?;
    }
    log(Record::FileCopied { rel: file.rel.clone() })?;
    // 3. Read back every destination independently.
    for (_, temp, _) in &open_dests {
        let got = read_back_xxh64(temp)
            .map_err(|e| fail(format!("reading back {}: {e}", temp.display())))?;
        if got != want64 {
            return Err(fail(format!(
                "verification FAILED at {}: wrote xxh64 {want64}, read back {got} — partial kept for inspection; delete it and re-run to retry",
                temp.display()
            )));
        }
    }
    log(Record::FileVerified { rel: file.rel.clone() })?;
    // 4. Rename into place — only verified bytes ever get a final name.
    for (final_path, temp, _) in &open_dests {
        std::fs::rename(temp, final_path)
            .map_err(|e| fail(format!("placing {}: {e}", final_path.display())))?;
    }
    log(Record::FilePlaced { rel: file.rel.clone() })?;
    Ok(PlacedFile {
        rel: file.rel.clone(),
        size: file.size,
        xxh3: want3,
        xxh64: want64,
        dest_rel,
    })
}
```

Helpers in the same file:

```rust
fn temp_path_for(final_path: &Path, token: &str) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    final_path.with_file_name(format!(".maj-partial-{token}-{name}"))
}

fn read_back_xxh64(path: &Path) -> std::io::Result<String> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut hasher = xxhash_rust::xxh64::Xxh64::new(0);
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}
```

Notes for the implementer:
- `dests_subdir_of` in the sketch is wrong-headed — `dest_rel` is just
  `format!("{}/{}", d.subdir, file.rel)` and it's per-destination identical by
  construction (all dests share one subdir string this phase); store it once.
  Simplify while keeping the test green.
- `expect(...)` on mutex locks: workspace denies `unwrap_used`, warns
  `expect_used`. Poisoned locks here mean a panicked worker — crossing that
  bridge silently would hide a bug. Use `#[allow]`-free handling: match on the
  `LockResult` and propagate a reason string instead of `expect`.
- Per-file failure isolation across destinations: the corruption test pins the
  healthy-destination behavior — when only dest 1's read-back fails, dest 2's
  verified copy must still be placed. Restructure the read-back/rename loops
  per destination (verify+rename each dest independently, collect per-dest
  failures) to satisfy it; the file counts as failed if ANY dest failed, but
  healthy dests keep their placed copy.

- [ ] **Step 5: Run engine tests — expect PASS**

Run: `cargo test -p majestical-ingest --test engine`

- [ ] **Step 6: `just ci`, commit, push, PR, merge**

```bash
git add crates/ingest/src/engine.rs crates/ingest/src/lib.rs crates/ingest/tests/engine.rs crates/ingest/Cargo.toml
git commit -m "feat: verified copy engine with fan-out, read-back, and resume"
```

Branch `feat/ingest-engine`, PR, squash-merge after green.

---

### Task 6: ASC MHL create/verify + conformance CI + `maj verify`

Branch `feat/asc-mhl`. The Python reference implementation is the conformance
oracle — where this plan's XML shape and the oracle disagree, THE ORACLE WINS.

**Files:**
- Create: `crates/ingest/src/mhl.rs`
- Modify: `crates/ingest/src/lib.rs` (`pub mod mhl;` + error variants)
- Modify: `crates/ingest/Cargo.toml` (add `quick-xml` — look up current version)
- Create: `crates/ingest/tests/conformance.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `justfile` (new `conformance` recipe)
- Test: unit tests inside `mhl.rs`

- [ ] **Step 0: Install the oracle locally and study its output**

```bash
cd /tmp && uv venv ascmhl-venv && ./ascmhl-venv/bin/pip install ascmhl
mkdir -p /tmp/mhl-sample/clips && printf 'AAAA' > /tmp/mhl-sample/clips/a.mov
./ascmhl-venv/bin/ascmhl create -v -h xxh64 /tmp/mhl-sample
cat /tmp/mhl-sample/ascmhl/*.mhl
ls /tmp/mhl-sample/ascmhl/
```

Read the generated XML carefully: root element + namespace, `creatorinfo` /
`processinfo` children, `hash` entry shape, path relativity, hashdate format,
the chain file's name and format, and the generation-file naming pattern.
**The code below is a starting shape — correct it to match what you just saw
before writing tests, and pin the observed shapes in the tests.**

- [ ] **Step 1: Write failing writer/reader unit tests**

In `crates/ingest/src/mhl.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HashList {
        HashList {
            creation_date: "2026-07-29T12:00:00Z".into(),
            hostname: "testhost".into(),
            tool_version: "0.2.0".into(),
            entries: vec![MhlEntry {
                rel: "clips/a.mov".into(),
                size: 4,
                xxh64: "0011223344556677".into(),
                action: HashAction::Original,
                hashdate: "2026-07-29T12:00:00Z".into(),
            }],
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let written = write_generation(dir.path(), &sample()).expect("write");
        assert_eq!(written.generation, 1);
        assert!(written.path.starts_with(dir.path().join("ascmhl")));
        let read = read_generation(&written.path).expect("read");
        assert_eq!(read.entries, sample().entries);
    }

    #[test]
    fn generations_number_sequentially_and_chain_records_each() {
        let dir = tempfile::tempdir().expect("tempdir");
        let g1 = write_generation(dir.path(), &sample()).expect("g1");
        let g2 = write_generation(dir.path(), &sample()).expect("g2");
        assert_eq!((g1.generation, g2.generation), (1, 2));
        let chain = std::fs::read_to_string(latest_chain_path(dir.path()).expect("chain"))
            .expect("chain readable");
        assert!(chain.contains(g1.file_name()));
        assert!(chain.contains(g2.file_name()));
    }

    #[test]
    fn verify_dir_reports_ok_missing_altered_and_new() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("clips")).expect("mkdir");
        std::fs::write(dir.path().join("clips/a.mov"), b"AAAA").expect("a");
        std::fs::write(dir.path().join("clips/b.mov"), b"BB").expect("b");
        let list = hash_dir(dir.path(), "2026-07-29T12:00:00Z").expect("hash dir");
        assert_eq!(list.entries.len(), 2);
        write_generation(dir.path(), &list).expect("g1");
        // Mutate the tree: alter a, delete b, add c.
        std::fs::write(dir.path().join("clips/a.mov"), b"AAAX").expect("alter");
        std::fs::remove_file(dir.path().join("clips/b.mov")).expect("delete");
        std::fs::write(dir.path().join("clips/c.mov"), b"CC").expect("new");
        let report = verify_dir(dir.path(), "2026-07-29T13:00:00Z").expect("verify");
        assert_eq!(report.altered, vec!["clips/a.mov".to_string()]);
        assert_eq!(report.missing, vec!["clips/b.mov".to_string()]);
        assert_eq!(report.new_files, vec!["clips/c.mov".to_string()]);
        assert!(report.verified.is_empty());
        // Verification appended generation 2 recording outcomes.
        assert_eq!(report.written.generation, 2);
    }
}
```

- [ ] **Step 2: Run — expect FAIL, then implement**

Model + API in `crates/ingest/src/mhl.rs` (shape only — align element names,
namespace, file naming, and chain format with the oracle output from Step 0):

```rust
//! ASC MHL create + verify (standard tier, flat histories). Conformance
//! oracle: the Python reference implementation — CI round-trips both ways.
use crate::IngestError;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAction {
    Original,
    Verified,
    Failed,
}

impl HashAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MhlEntry {
    /// `/`-separated path relative to the history root.
    pub rel: String,
    pub size: u64,
    pub xxh64: String,
    pub action: HashAction,
    pub hashdate: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashList {
    pub creation_date: String,
    pub hostname: String,
    pub tool_version: String,
    pub entries: Vec<MhlEntry>,
}

#[derive(Debug, Clone)]
pub struct WrittenGeneration {
    pub path: PathBuf,
    pub generation: u32,
    /// xxh64 of the manifest file's bytes — what `ManifestRecorded` stores.
    pub roothash: String,
}

impl WrittenGeneration {
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub verified: Vec<String>,
    pub altered: Vec<String>,
    pub missing: Vec<String>,
    pub new_files: Vec<String>,
    pub written: WrittenGeneration,
}
```

Functions to implement (each is a focused unit):

1. `next_generation(root: &Path) -> Result<u32, IngestError>` — scan
   `root/ascmhl/*.mhl`, parse the leading `NNNN_`, return max+1 (1 when the
   directory doesn't exist).
2. `write_generation(root, list) -> Result<WrittenGeneration, IngestError>` —
   build the XML with `quick_xml::Writer` (elements per the oracle sample),
   write `ascmhl/<NNNN>_<rootname>_<date>_<time>.mhl` (naming per oracle),
   xxh64 the written bytes for `roothash`, append the chain entry (format per
   oracle).
3. `read_generation(path) -> Result<HashList, IngestError>` — `quick_xml::Reader`
   pull loop; unknown elements are skipped (forward compatibility), missing
   required ones are errors naming the element and file.
4. `latest_generation_path(root)` / `latest_chain_path(root)` — highest `NNNN`.
5. `hash_dir(root, hashdate) -> Result<HashList, IngestError>` — walk root,
   skip `ascmhl/` and dotfiles/`.DS_Store` and `.maj-partial-*`, xxh64 each
   file (reuse a `read_back_xxh64`-style helper — lift that helper from
   `engine.rs` into a shared `pub(crate) fn stream_xxh64` in `lib.rs` rather
   than duplicating it), action `Original`.
6. `verify_dir(root, hashdate) -> Result<VerifyReport, IngestError>` — read
   latest generation, `hash_dir` the present state, diff by rel path:
   same hash → `verified` (action `Verified`), different → `altered`
   (action `Failed`), absent on disk → `missing` (kept in the new list with
   action `Failed`), on disk but not in manifest → `new_files` (action
   `Original`). Write the resulting list as a new generation.

New `IngestError` variants: `Mhl { path, msg }` and `MhlXml { path, source: quick_xml::Error }`.

Run: `cargo test -p majestical-ingest mhl`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/ingest/src/mhl.rs crates/ingest/src/lib.rs crates/ingest/src/engine.rs crates/ingest/Cargo.toml
git commit -m "feat: ASC MHL generation writer, reader, and directory verify"
```

- [ ] **Step 4: Write the conformance tests (ignored by default)**

`crates/ingest/tests/conformance.rs`:

```rust
//! Two-way conformance against the Python reference implementation.
//! Ignored by default; CI runs them with `ascmhl` on PATH (see justfile).
use majestical_ingest::mhl::{hash_dir, verify_dir, write_generation};
use std::path::Path;
use std::process::Command;

fn ascmhl() -> Command {
    let bin = std::env::var("ASCMHL_BIN").unwrap_or_else(|_| "ascmhl".into());
    Command::new(bin)
}

fn fixture(dir: &Path) {
    std::fs::create_dir_all(dir.join("clips")).expect("mkdir");
    std::fs::write(dir.join("clips/a.mov"), b"AAAA").expect("a");
    std::fs::write(dir.join("b space.wav"), b"BBBBBB").expect("b");
}

#[test]
#[ignore = "needs python ascmhl on PATH (CI: just conformance)"]
fn our_manifest_passes_reference_verify() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let list = hash_dir(dir.path(), "2026-07-29T12:00:00Z").expect("hash");
    write_generation(dir.path(), &list).expect("write");
    let out = ascmhl()
        .args(["verify"])
        .arg(dir.path())
        .output()
        .expect("run ascmhl verify");
    assert!(
        out.status.success(),
        "reference verify rejected our manifest:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "needs python ascmhl on PATH (CI: just conformance)"]
fn reference_manifest_passes_our_verify() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let out = ascmhl()
        .args(["create", "-h", "xxh64"])
        .arg(dir.path())
        .output()
        .expect("run ascmhl create");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report = verify_dir(dir.path(), "2026-07-29T13:00:00Z").expect("our verify");
    assert!(report.altered.is_empty(), "altered: {:?}", report.altered);
    assert!(report.missing.is_empty(), "missing: {:?}", report.missing);
    assert_eq!(report.verified.len(), 2);
}
```

Iterate on `mhl.rs` until both pass locally against your Step 0 venv:

```bash
ASCMHL_BIN=/tmp/ascmhl-venv/bin/ascmhl cargo test -p majestical-ingest --test conformance -- --ignored
```

Expect this to take a few rounds — element order, namespace, and chain-file
details are exactly what the oracle exists to pin down.

- [ ] **Step 5: Wire conformance into `just` and CI**

`justfile` (match existing recipe style):

```make
# Two-way ASC MHL conformance against the Python reference implementation.
conformance:
    uv venv --allow-existing .ascmhl-venv
    ./.ascmhl-venv/bin/pip install --quiet ascmhl
    ASCMHL_BIN=./.ascmhl-venv/bin/ascmhl cargo test -p majestical-ingest --test conformance -- --ignored
```

Add `.ascmhl-venv/` to `.gitignore`.

`.github/workflows/ci.yml`: add a job following the existing jobs' hygiene
(SHA-pinned actions with version comments, `persist-credentials: false`,
minimal permissions). Look up the CURRENT full SHA for `astral-sh/setup-uv`
(or install uv via the checkout image if the repo already does elsewhere — 
follow existing precedent; plain `pip` via actions/setup-python is fine too):

```yaml
  mhl-conformance:
    runs-on: macos-15
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@<same-sha-the-other-jobs-use>  # vX.Y.Z
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@<current-sha>  # match existing rust setup precedent
      - uses: actions/setup-python@<current-sha>  # vX.Y.Z — look up at execution time
        with:
          python-version: "3.13"
      - run: pipx install ascmhl || pip install --user ascmhl
      - run: cargo test -p majestical-ingest --test conformance -- --ignored
```

Reuse whatever runner + Rust-toolchain setup the existing test job uses —
copy that job and swap the run steps. Lint: `actionlint` + `zizmor` must stay
green (`just ci` runs them).

- [ ] **Step 6: Add `maj verify`**

clap in `main.rs`:

```rust
    /// Re-verify a destination against its ASC MHL history; appends a generation.
    Verify {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
```

`commands.rs`:

```rust
pub(crate) fn cmd_verify(dir: &Path, json: bool) -> Result<()> {
    let hashdate = iso8601_ms(app::physical_now_ms());
    let report = majestical_ingest::mhl::verify_dir(dir, &hashdate)
        .with_context(|| format!("verifying {}", dir.display()))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "verified": report.verified,
                "altered": report.altered,
                "missing": report.missing,
                "new": report.new_files,
                "generation": report.written.generation,
            })
        );
    } else {
        for f in &report.altered {
            println!("ALTERED  {f}");
        }
        for f in &report.missing {
            println!("MISSING  {f}");
        }
        for f in &report.new_files {
            println!("NEW      {f}");
        }
        println!(
            "{} verified, {} altered, {} missing, {} new — wrote generation {}",
            report.verified.len(),
            report.altered.len(),
            report.missing.len(),
            report.new_files.len(),
            report.written.generation
        );
    }
    anyhow::ensure!(
        report.altered.is_empty() && report.missing.is_empty(),
        "verification found problems — see above"
    );
    Ok(())
}
```

`maj verify` intentionally does NOT need a catalog (`--catalog`), it operates
on the directory's own history — note this means dispatch happens before
`FsApp::open`, like `catalog init`. Add a smoke test: ingest-less flow —
create a dir, `hash_dir`+`write_generation` via a helper binary is overkill;
instead have the smoke test shell `maj verify` against a dir prepared with
`write_generation` through a tiny `#[test]`-side dependency on
`majestical-ingest` (add it to the CLI's `[dev-dependencies]`).

- [ ] **Step 7: `just ci && just conformance`, commit, push, PR, merge**

```bash
git add crates/ingest crates/cli justfile .github/workflows/ci.yml .gitignore
git commit -m "feat: maj verify and ASC MHL conformance gate"
```

Branch `feat/asc-mhl`, PR (note in body: oracle-derived XML shape), squash-merge
after green — including the new conformance job.

---

### Task 7: `maj ingest` end to end + acceptance + cargo-mutants

Branch `feat/ingest-cli`.

**Files:**
- Modify: `crates/cli/src/main.rs`, `crates/cli/src/commands.rs`, `crates/cli/Cargo.toml`
- Test: `crates/cli/tests/cli_smoke.rs`
- Create: `crates/ingest/tests/features/ingest.feature` + `crates/ingest/tests/acceptance.rs`
- Modify: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`

- [ ] **Step 1: Write the acceptance feature file**

`crates/ingest/tests/features/ingest.feature` (cucumber, mirroring
`crates/core/tests/features/convergence.feature` style):

```gherkin
Feature: Verified multi-destination ingest

  Scenario: A card ingests verified to two destinations
    Given a source card with files
      | path        | bytes  |
      | clips/a.mov | AAAA   |
      | b.wav       | BBBBBB |
    And 2 destination roots
    When the card is ingested to "Projects/x/day1"
    Then every destination holds identical verified copies
    And every destination has an ASC MHL generation covering 2 files

  Scenario: A duplicate is skipped without copying
    Given a source card with files
      | path    | bytes |
      | dup.mov | AAAA  |
    And the catalog already knows content "AAAA"
    And 1 destination root
    When the card is ingested to "Projects/x/day1"
    Then no files are placed
    And 1 duplicate is reported

  Scenario: A corrupted write never reaches a final path
    Given a source card with files
      | path  | bytes |
      | a.mov | AAAA  |
    And 2 destination roots where destination 1 corrupts writes
    When the card is ingested to "Projects/x/day1"
    Then destination 1 reports a verification failure and holds only a quarantined partial
    And destination 2 holds an identical verified copy

  Scenario: An interrupted run resumes without re-copying placed files
    Given a source card with files
      | path  | bytes |
      | a.mov | AAAA  |
      | b.mov | BB    |
    And 1 destination root
    And a previous run already placed "a.mov"
    When the card is ingested to "Projects/x/day1"
    Then only "b.mov" is copied
```

- [ ] **Step 2: Implement the step definitions**

`crates/ingest/tests/acceptance.rs`: cucumber `World` holding tempdirs, plan,
outcome, and a sink-factory choice; steps call `plan_source`, `run`,
`write_generation`/`read_generation` directly (hexagon boundary — no CLI).
Follow `crates/core/tests/acceptance.rs` for the cucumber harness setup
(runner main, features path). Reuse the `CorruptingSinks` fixture — lift it
from `tests/engine.rs` into a shared `tests/common/mod.rs` used by both, and
have "a previous run already placed" pre-run the engine for just that file
(same journal path), mirroring `resume_skips_placed_files`.
Register the test in `crates/ingest/Cargo.toml`:

```toml
[[test]]
name = "acceptance"
harness = false
```

Run: `cargo test -p majestical-ingest --test acceptance`
Expected: PASS (iterate steps until green).

Commit:

```bash
git add crates/ingest/tests crates/ingest/Cargo.toml
git commit -m "test: acceptance scenarios for verified ingest"
```

- [ ] **Step 3: Write failing CLI smoke test for `maj ingest`**

```rust
#[test]
fn ingest_end_to_end_places_verifies_and_catalogs() {
    let t = TestCatalog::init();
    t.maj(&["para", "add", "project", "shoot"]).success();
    let src = t.tempdir_path("card");
    std::fs::create_dir_all(src.join("clips")).expect("mkdir");
    std::fs::write(src.join("clips/a.mov"), b"AAAA").expect("a");
    let d1 = t.tempdir_path("nas");
    let d2 = t.tempdir_path("shuttle");
    t.maj(&[
        "ingest", src.to_str().expect("utf8"),
        "--dest", d1.to_str().expect("utf8"),
        "--dest", d2.to_str().expect("utf8"),
        "--para", "project/shoot",
        "--template", "{date}/{source-label}",
        "--json",
    ])
    .success();
    // Placed at PARA-routed paths under both roots.
    let placed: Vec<_> = walkdir::WalkDir::new(&d1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == "a.mov")
        .collect();
    assert_eq!(placed.len(), 1);
    let path = placed[0].path().to_string_lossy().into_owned();
    assert!(path.contains("Projects/shoot/"), "PARA routing: {path}");
    // Each destination has its own history.
    assert!(d1.join("ascmhl").is_dir());
    assert!(d2.join("ascmhl").is_dir());
    // Catalog knows the asset, its PARA assignment, and the verification.
    let out = t.maj(&["search", "--name", "a.mov", "--json"]).success_stdout();
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["count"], 1);
    // Re-verify both destinations.
    t.maj(&["verify", d1.to_str().expect("utf8")]).success();
    t.maj(&["verify", d2.to_str().expect("utf8")]).success();
    // Second ingest of the same card: everything dedupes, nothing recopied.
    let out = t
        .maj(&[
            "ingest", src.to_str().expect("utf8"),
            "--dest", d1.to_str().expect("utf8"),
            "--para", "project/shoot",
            "--json",
        ])
        .success_stdout();
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["skipped_duplicates"], 1);
    assert_eq!(v["placed"], 0);
}
```

- [ ] **Step 4: Run — expect FAIL, then implement `cmd_ingest`**

clap in `main.rs`:

```rust
    /// Verified copy from a source into PARA-routed destinations.
    Ingest {
        source: PathBuf,
        /// Destination root(s); each gets an independently verified copy
        /// and its own ASC MHL history.
        #[arg(long, required = true)]
        dest: Vec<PathBuf>,
        /// Target PARA node (<kind>/<name> or node id).
        #[arg(long)]
        para: String,
        /// Layout inside the node. Tokens: {date}, {source-label}.
        #[arg(long, default_value = "{date}/{source-label}")]
        template: String,
        #[arg(long, value_enum, default_value_t = DedupeArg::Skip)]
        dedupe: DedupeArg,
        /// Parallel copy workers (default: CPU cores, max 8).
        #[arg(long)]
        jobs: Option<usize>,
        /// Print the plan and exit without copying.
        #[arg(long)]
        dry_run: bool,
        /// Resume a previous run's journal (run id printed at start).
        #[arg(long)]
        resume: Option<String>,
        #[arg(long)]
        json: bool,
    },
```

(`DedupeArg` is a small `clap::ValueEnum` mapping to `plan::DedupeMode` —
`Link` maps to `CopyAnyway` this phase? NO — see the note below; wire `link`
through as planned or drop the flag value. Decision, pinned: `--dedupe link`
is NOT exposed this phase — the engine's Duplicate handling implements Skip
and CopyAnyway; hard-linking needs per-destination instance lookup that adds
real complexity for an unrequested mode. `DedupeArg { Skip, Copy }`. The spec
names link mode: record the deferral in the watchlist with attribution to this
task, per handoff rule 9.)

`cmd_ingest` flow in `commands.rs` (~120 lines; split helpers to stay under
the 100-line function limit):

```rust
pub(crate) fn cmd_ingest(app: &mut FsApp, catalog_dir: &Path, args: IngestArgs) -> Result<()> {
    let projection = app.projection()?;
    let node_id = resolve_para_node(&projection, &args.para)?;
    let node = projection
        .para_node(&node_id)
        .filter(|st| st.kind().is_some() && st.name().is_some());
    let Some(node) = node else {
        anyhow::bail!("PARA node {node_id} is incomplete — its create event has not synced");
    };
    // Known assets for dedupe: every (hash, size) the catalog has observed.
    let known = KnownAssets::from_pairs(
        projection
            .assets()
            .flat_map(|(id, st)| {
                st.instances
                    .iter()
                    .map(|(_, _, size)| (id.0.trim_start_matches("xxh3:").to_string(), *size))
                    .collect::<Vec<_>>()
            })
            .collect(),
    );
    let plan = plan_source(&args.source, &known, args.dedupe.into())
        .context("planning ingest")?;
    let (src_volume_id, src_label) = resolve_volume(&args.source, None);
    let date = iso8601_ms(app::physical_now_ms())[..10].to_string(); // YYYY-MM-DD
    let subdir = format!(
        "{}/{}/{}",
        node.kind().map(ParaKind::dir_name).unwrap_or("Projects"),
        node.name().unwrap_or_default(),
        majestical_ingest::template::render(
            &args.template,
            &TemplateCtx { date, source_label: src_label.clone() },
        )?,
    );
    if args.dry_run {
        return print_plan(&plan, &args.dest, &subdir, args.json);
    }
    let run_id = args.resume.clone().unwrap_or_else(|| ulid::Ulid::new().to_string());
    let journal_path = catalog_dir.join("runs").join(format!("{run_id}.jsonl"));
    let resume_placed = if args.resume.is_some() {
        Journal::load(&journal_path).map(|f| f.placed).unwrap_or_default()
    } else {
        std::collections::BTreeSet::new()
    };
    eprintln!("run {run_id} — resume with: maj ingest ... --resume {run_id}");
    let mut journal = Journal::create(&journal_path).context("opening journal")?;
    let dests: Vec<DestSpec> = args
        .dest
        .iter()
        .map(|root| DestSpec { root: root.clone(), subdir: subdir.clone() })
        .collect();
    let jobs = args.jobs.unwrap_or_else(default_jobs);
    let outcome = run_engine(&plan, &dests, &resume_placed, &mut journal, jobs)?;
    let events = ingest_events(app, &projection, &outcome, &dests, &node_id, &src_volume_id, &src_label)?;
    print_outcome(&outcome, events, args.json)
}
```

The pieces (each its own function; complete them from the types already built):

- `default_jobs()` — `std::thread::available_parallelism()` clamped to 8.
- `run_engine` — adapts to the `run(...)` signature Task 5 settled (resume set
  as a parameter), maps `IngestError` via `.context("copy engine")`.
- `ingest_events` — after the engine, per destination root: resolve volume
  identity (`volume_identity::resolve(&d.root)` — this revisits the watchlist
  root-lumping item: destination roots get REAL identity, with the documented
  fallback), then `hash_dir` + `write_generation` for the MHL, then emit in
  one `app.emit(...)` batch:
  - `Op::VolumeSeen` per destination (+ one for the source),
  - per placed file × destination: `Op::AssetSeen { asset: xxh3:<hash>, volume, path: <subdir>/<rel>, size }`,
  - per placed file × destination: `Op::VerificationRecorded { outcome: Verified, algo: "xxh64", value: <xxh64>, hashdate_ms: physical_now_ms() }`,
  - per failed file × destination: `Op::VerificationRecorded { outcome: Failed, .. }`
    (value = expected hash; the failure reason already went to stderr),
  - per unique placed asset: `Op::AssetParaSet { asset, node: node_id }`,
  - per destination: `Op::ManifestRecorded { volume, mhl_path: "ascmhl/<file>", generation, roothash }`.
- `print_outcome` — text: placed/failed/skipped counts + per-failure lines
  with the re-copy hint; JSON: `{"run", "placed", "failed": [{"rel","reason"}],
  "skipped_duplicates", "rejected", "resumed"}` (counts as numbers, failures
  as arrays — match what the smoke test asserts).
- IMPORTANT engine/MHL boundary: `hash_dir` walks the WHOLE destination root.
  For a fresh destination that's correct. For a root that already had content,
  generation 1 must still cover everything present (that's how ASC MHL works —
  the history speaks for the tree). Keep `hash_dir` as-is; do not special-case.
- `maj ingest` exits non-zero when `outcome.failed` or `outcome.rejected` is
  non-empty (`anyhow::bail!` after printing) — partial success must be loud.

Add `majestical-ingest = { path = "../ingest" }` to the CLI's `[dependencies]`.

- [ ] **Step 5: Run smoke tests — expect PASS**

Run: `cargo test -p majestical-cli --test cli_smoke ingest`

- [ ] **Step 6: cargo-mutants on verification-critical modules**

```bash
cargo install cargo-mutants --locked   # verify current version
cargo mutants --package majestical-ingest -- --all-targets 2>&1 | tail -40
cargo mutants --package majestical-core -- --all-targets 2>&1 | tail -40
```

For each MISSED mutant in `engine.rs` read-back/rename logic, `mhl.rs` verify
diffing, or `projection.rs` new arms: add a discriminating test. Mutants in
display/formatting code may be triaged to the watchlist instead — record each
with attribution ("phase 3 Task 7 mutants run").

- [ ] **Step 7: Update the watchlist**

In `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, move to "Done":
non-UTF-8 handling (planner rejects per-file, engine byte-exact), commands
module extraction (Task 1), CatalogStore port lag (Task 1), root-volume
revisit (destination identity, Task 7). Add new deferred items: `--dedupe
link` mode (deferred from Task 7), plus anything reviewers deferred during
the phase.

- [ ] **Step 8: Full gate, commit, push, PR, merge**

Run: `just ci && just conformance`

```bash
git add crates/cli crates/ingest docs/superpowers/plans/2026-07-29-phase2-watchlist.md
git commit -m "feat: maj ingest end to end with catalog events"
```

Branch `feat/ingest-cli`, PR, squash-merge after green.

---

## Definition of done (whole phase)

- All 7 PRs squash-merged, CI green including the conformance job.
- `maj para add/list/rename/archive`, `maj ingest` (multi-dest, dedupe,
  dry-run, resume), `maj verify` all work end to end on a real machine.
- Both conformance directions pass. cargo-mutants run recorded.
- Watchlist updated. Wire-format golden tests cover all six new ops; proptest
  generator extended; no warnings anywhere.



