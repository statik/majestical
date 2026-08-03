# Phase 7A: Service Layer + `maj mcp` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract every CLI verb's "do the operation, return a structured outcome" half into a new `crates/services` workspace crate, then ship `maj mcp` — a stdio MCP server with full CLI parity — over that layer.

**Architecture:** `crates/services` owns operations (request struct → serde-serializable outcome → thiserror error); `crates/cli` keeps clap + rendering and calls services; `maj mcp` is a subcommand in the same binary built on the official `rmcp` SDK with a tokio runtime started only for that subcommand. CLI text/JSON output stays **byte-identical** through the extraction: the existing `json!` rendering code stays in the CLI, fed from outcome structs instead of local variables. MCP serializes the outcome structs directly (its JSON is a new surface, not required to match the CLI's).

**Tech Stack:** Rust workspace edition 2024, strict clippy (`unwrap_used` denied — `expect` only in tests), thiserror in crates / anyhow in CLI, clap, serde, rmcp + tokio (mcp subcommand only), schemars for tool schemas.

**Spec:** `docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md` — read it first. This plan covers spec delivery chunks 1-5 (Plan B = GUI slice, Plan C = release pipeline; written after this plan ships).

**Process conventions (mandatory):**
- TDD every task: failing test → verify fail → implement → verify pass → commit.
- Stage ONLY your files, never `git add -A`. No Claude-Session trailers. Never push to main. Push via `git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git <branch>`.
- `just check` runs fmt + clippy (also the prek hook). `cargo test -p <crate>` for the crate you touched.
- Verify current stable versions of every new dependency (`rmcp`, `tokio`, `schemars`) at execution time — never assume from memory. Use context7 or crates.io.
- PR chunks (squash-merge after green CI): Tasks 1-2 = PR1, Tasks 3-4 = PR2, Task 5 = PR3, Task 6 = PR4, Tasks 7-8 = PR5, Task 9 = PR6.

## The byte-identical rule (applies to every extraction task)

The CLI's stdout/stderr for every existing invocation must not change. The mechanism, used in every task below:

1. Before touching code, build a reference binary: `cargo build -p majestical-cli && cp target/debug/maj /tmp/maj-ref` (first task of each PR chunk).
2. After the extraction, the task's smoke tests run both binaries against the same fixture catalog and diff stdout byte-for-byte for `--json` and text modes. Phase 6 proved this technique on `run_ingest`.
3. The existing smoke/cucumber suites (`sync_smoke`, `inbox_smoke`, `index` and `search` suites) are the regression net — they run green in every task.

## File structure

- `crates/services/Cargo.toml` — create: new workspace member `majestical-services`.
- `crates/services/src/lib.rs` — create: module declarations + crate doc.
- `crates/services/src/app.rs` — moved from `crates/cli/src/app.rs` (items made `pub`).
- `crates/services/src/state_dir.rs`, `iso8601.rs`, `volume_identity.rs` — moved from `crates/cli/src/` (items made `pub`).
- `crates/services/src/catalog.rs` — create: `open_catalog` (moved from `commands.rs`), `catalog_init`, `get_asset`.
- `crates/services/src/search.rs` — create: query/search compute moved from `crates/cli/src/search.rs` + `query.rs`.
- `crates/services/src/volumes.rs`, `meta.rs`, `para.rs`, `tags.rs`, `scan.rs`, `verify.rs`, `ingest.rs`, `sync.rs`, `inbox.rs`, `index.rs`, `describer.rs` — create per-verb service modules (moved compute).
- `crates/services/src/error.rs` — create: `ServiceError`.
- `crates/cli/src/mcp_cmd.rs` — create: the MCP server (tools, resources, confirm gate).
- `crates/cli/src/main.rs` — modify: `Mcp` subcommand; imports move to `majestical_services`.
- `crates/cli/src/commands.rs`, `search.rs`, `sync_cmd.rs`, `inbox_cmd.rs`, `index_cmd.rs`, `tags_cmd.rs`, `describer_cmd.rs` — modify: shrink to arg-parsing + rendering.
- `crates/cli/tests/common/mod.rs` — create: shared `fixture_catalog` + `asset_id_of` helpers (integration test files are separate crates; both suites declare `mod common;`).
- `crates/cli/tests/services_parity.rs` — create: byte-identical reference-binary diffs.
- `crates/cli/tests/mcp_smoke.rs` — create: stdio JSON-RPC integration suite.

---

### Task 1: `crates/services` scaffold — move the shared foundation

**Files:**
- Create: `crates/services/Cargo.toml`, `crates/services/src/lib.rs`, `crates/services/src/error.rs`
- Move: `crates/cli/src/{app.rs,state_dir.rs,iso8601.rs,volume_identity.rs}` → `crates/services/src/`
- Modify: workspace `Cargo.toml` (add member), `crates/cli/Cargo.toml` (depend on services), `crates/cli/src/main.rs` (drop moved `mod` decls, import from `majestical_services`), every `crate::app::`/`crate::state_dir::`/`crate::iso8601::`/`crate::volume_identity::` path in the cli crate

- [ ] **Step 1: Build the reference binary**

```bash
cargo build -p majestical-cli && cp target/debug/maj /tmp/maj-ref
```

- [ ] **Step 2: Create the crate**

`crates/services/Cargo.toml` (versions must match the workspace's existing pins — copy them from `crates/cli/Cargo.toml`, do not invent):

```toml
[package]
name = "majestical-services"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
majestical-core = { path = "../core" }
majestical-catalog-sqlite = { path = "../catalog-sqlite" }
majestical-sync = { path = "../sync" }
majestical-ingest = { path = "../ingest" }
majestical-index = { path = "../index" }
majestical-describe = { path = "../describe" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
ulid = { workspace = true }
walkdir = { workspace = true }
xxhash-rust = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }

[lints]
workspace = true
```

If any of those deps are not yet `workspace = true` entries in the root `Cargo.toml`, hoist them (the phase-6 `rusqlite` single-pin precedent). Add `"crates/services"` to `[workspace] members`.

`crates/services/src/lib.rs`:

```rust
//! Operation layer shared by the CLI, `maj mcp`, and the desktop app.
//! One function per verb: request in, serde-serializable outcome out.
//! Heads render outcomes; they never re-implement operations.
pub mod app;
pub mod error;
pub mod iso8601;
pub mod state_dir;
pub mod volume_identity;
```

`crates/services/src/error.rs`:

```rust
//! Service-level error type. Carries operation + input + suggested fix so
//! every head (CLI exit message, MCP tool error, GUI dialog) can render the
//! same remedy without re-deriving it.
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("no catalog at {root} — run `maj catalog init` first")]
    NoCatalog { root: PathBuf },
    /// Escape hatch while extraction is in flight: wraps the anyhow chains
    /// the cmd_* bodies already produce. Individual verbs migrate to typed
    /// variants only when a head needs to match on them (YAGNI).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Move the four modules**

`git mv` each file, then in the moved files change every `pub(crate)` to `pub` and every `crate::` path to the new crate-local path. `app.rs`'s `use crate::...` becomes `use crate::iso8601...` etc. (they moved together). Add `# Errors` doc sections where clippy's `missing_errors_doc` fires on newly-`pub` functions — state what the existing behavior is, do not change it.

In `crates/cli`: delete the four `mod` declarations from `main.rs`, add `use majestical_services::{app, iso8601, state_dir, volume_identity};` shims where needed, or update call sites to the full path — prefer updating call sites (`rg -l 'crate::(app|state_dir|iso8601|volume_identity)' crates/cli/src` lists them; mechanical replace `crate::app::` → `majestical_services::app::` etc.).

`FsApp::open`/`init` and `App::{log,events,projection,projection_of,emit}` keep their exact signatures. `warn_skipped_corrupt_lines` and `physical_now_ms` become `pub`.

- [ ] **Step 4: Full suite green**

Run: `cargo test --workspace`
Expected: PASS — zero behavior change; this is a pure move.

- [ ] **Step 5: Byte-identical spot check**

```bash
cargo build -p majestical-cli
export MAJ_CATALOG=$(mktemp -d)/cat MAJ_MACHINE_ID=m1
/tmp/maj-ref catalog init && rm -rf "$MAJ_CATALOG"  # ref binary behavior...
target/debug/maj catalog init                        # ...matches new binary
```

Both print `initialized catalog at <path>`. (The systematic diff harness arrives in Task 2; this is the smoke check that the wiring holds.)

- [ ] **Step 6: Lint and commit**

```bash
just check
git add crates/services crates/cli Cargo.toml Cargo.lock
git commit -m "refactor: extract shared CLI foundation into crates/services"
```

---

### Task 2: Extract search + the parity harness

**Files:**
- Create: `crates/services/src/search.rs` (compute moved from cli), `crates/cli/tests/services_parity.rs`
- Move: `crates/cli/src/query.rs` → `crates/services/src/query.rs` (make `pub`)
- Modify: `crates/cli/src/search.rs` (shrinks to rendering), `crates/services/src/lib.rs` (`pub mod query; pub mod search;`)

The current `crates/cli/src/search.rs` (1610 lines) interleaves compute and printing. The seam: everything that produces the ranked result rows + notices moves to services; the `json!`/table printing stays in the CLI, fed from the outcome.

- [ ] **Step 1: Write the failing outcome-shape test**

In `crates/services/src/search.rs` (new file, test first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_outcome_carries_rows_and_notices() {
        // Fixture: init a catalog, scan a tempdir with one file, search "*".
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app =
            crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("clip.txt"), b"hello").expect("write");
        crate::scan::scan(&mut app, &src, Some("vol1".into())).expect("scan");
        let out = search(
            &mut app,
            &root,
            &SearchRequest { query: Some("clip".into()), limit: 50, saved: None, save: None },
        )
        .expect("search");
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "clip.txt");
        assert!(out.results[0].asset.starts_with("xxh3:"));
    }
}
```

(`crate::scan::scan` arrives in Task 5; until then substitute the direct
`app.emit(vec![Op::VolumeSeen{..}, Op::AssetSeen{..}])` fixture — copy the op
construction from `commands.rs::cmd_scan`'s body. Write it that way now; Task 5
does NOT need to revisit this test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p majestical-services search_outcome -- --nocapture`
Expected: FAIL — module/types don't exist.

- [ ] **Step 3: Define the contract and move the compute**

In `crates/services/src/search.rs`:

```rust
//! Search compute: query parse → layered retrieval → ranked rows + notices.
//! Moved verbatim from crates/cli/src/search.rs; the CLI keeps rendering.
use serde::Serialize;

pub struct SearchRequest {
    pub query: Option<String>,
    pub limit: usize,
    /// Run a previously saved search by name.
    pub saved: Option<String>,
    /// Save the query under this name (and run it).
    pub save: Option<String>,
}

#[derive(Serialize)]
pub struct SearchHit {
    pub asset: String,
    pub name: String,
    pub size: u64,
    pub volumes: Vec<VolumeRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Serialize)]
pub struct VolumeRef {
    pub id: String,
    pub label: String,
    pub online: bool,
}

#[derive(Serialize)]
pub struct SearchOutcome {
    pub count: usize,
    pub results: Vec<SearchHit>,
    /// Embedding coverage: (embedded, eligible), when semantic search ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_coverage: Option<(u64, u64)>,
    /// Text-layer degradation notices, verbatim strings the CLI prints today.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub text_coverage: Vec<TextCoverageNotice>,
}
```

**Field-fitting rule:** the struct fields above are the plan's best reading of
`search.rs:1071-1111`'s `json!` payloads. While moving the code, fit the
structs to what the compute actually produces (e.g. `TextCoverageNotice`'s
exact fields come from the notice construction around `search.rs:1103`) —
never the reverse. The CLI rendering keeps building its `json!` payloads
exactly as today from these fields, so any mismatch shows up as a parity
diff in Step 5, not silent drift.

Move mechanics:

1. `git mv crates/cli/src/query.rs crates/services/src/query.rs`; `pub(crate)` → `pub`.
2. In cli `search.rs`, identify `cmd_search`'s body up to the first `if args.json` branch — that's the compute (query parse, saved-search resolve/store, sqlite + vector retrieval, ranking, notice assembly). Move it into `pub fn search(app: &mut FsApp, catalog_dir: &Path, req: &SearchRequest) -> Result<SearchOutcome, ServiceError>` in the services crate, together with the private helpers it drags along (the compiler is the checklist — keep moving helpers until it builds). Saved-search list/rm compute moves as `pub fn searches_list(...) -> Result<Vec<SavedSearch>, ServiceError>` / `pub fn searches_rm(app: &mut FsApp, name: &str) -> Result<(), ServiceError>` (`SavedSearch { pub name: String, pub query: String }`, `Serialize`).
3. cli `search.rs` keeps `SearchArgs`, `cmd_search`, `cmd_searches` — each now builds the request, calls services, renders. The `json!` blocks are UNCHANGED except their inputs now come from the outcome struct fields.

- [ ] **Step 4: Services + cli tests green**

Run: `cargo test -p majestical-services -p majestical-cli`
Expected: PASS, including the existing search smoke suite (unchanged output).

- [ ] **Step 5: Parity harness**

The fixture helpers go in `crates/cli/tests/common/mod.rs` (shared with Task 6's `mcp_smoke.rs` via `mod common;` — integration test files are separate crates, so a sibling module is the sharing mechanism, same as cucumber suites' `Fixture` idiom):

```rust
//! crates/cli/tests/common/mod.rs
use assert_cmd::Command;
use std::path::Path;

pub fn fixture_catalog(dir: &Path) -> std::path::PathBuf {
    let root = dir.join("cat");
    let run = |args: &[&str]| {
        Command::cargo_bin("maj")
            .expect("bin")
            .env("MAJ_CATALOG", &root)
            .env("MAJ_MACHINE_ID", "m1")
            .args(args)
            .assert()
            .success();
    };
    run(&["catalog", "init"]);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("a.txt"), b"alpha").expect("write");
    std::fs::write(src.join("b.txt"), b"beta").expect("write");
    run(&["scan", src.to_str().expect("utf8"), "--volume", "vol1"]);
    run(&["tag", "add", &asset_id_of(&root, "a.txt"), "demo"]);
    root
}

/// Finds an asset id via `search --json` — keeps the fixture independent of
/// hash literals.
pub fn asset_id_of(root: &Path, name: &str) -> String {
    let out = Command::cargo_bin("maj")
        .expect("bin")
        .env("MAJ_CATALOG", root)
        .env("MAJ_MACHINE_ID", "m1")
        .args(["search", name, "--json"])
        .output()
        .expect("run");
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("json");
    v["results"][0]["asset"].as_str().expect("asset").to_string()
}
```

The harness itself:

```rust
//! crates/cli/tests/services_parity.rs — byte-identical CLI output through
//! the services extraction, proven by diffing this build's stdout against
//! the pre-extraction reference binary (/tmp/maj-ref, built at each PR
//! chunk's start). Skips (with a loud message) when the reference is
//! absent — CI rebuilds it in the job.
mod common;

use assert_cmd::Command; // both arms below are assert_cmd::Command
use std::path::Path;

fn diff_against_ref(root: &Path, args: &[&str]) {
    let reference = Path::new("/tmp/maj-ref");
    if !reference.is_file() {
        eprintln!("SKIP parity({args:?}): /tmp/maj-ref missing — build it first");
        return;
    }
    let run = |bin: &str| {
        let mut c = if bin == "ref" {
            Command::new(reference)
        } else {
            Command::cargo_bin("maj").expect("bin")
        };
        c.env("MAJ_CATALOG", root).env("MAJ_MACHINE_ID", "m1").args(args);
        c.output().expect("run")
    };
    let (new, old) = (run("new"), run("ref"));
    assert_eq!(
        (String::from_utf8_lossy(&new.stdout), new.status.code()),
        (String::from_utf8_lossy(&old.stdout), old.status.code()),
        "stdout/exit diverged for {args:?}"
    );
}

#[test]
fn search_output_is_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    for args in [
        ["search", "a.txt", "--json"].as_slice(),
        ["search", "a.txt"].as_slice(),
        ["search", "tag:demo", "--json"].as_slice(),
        ["search", "nomatch", "--json"].as_slice(),
        ["searches", "list", "--json"].as_slice(),
    ] {
        diff_against_ref(&root, args);
    }
}
```

Run: `cargo test -p majestical-cli --test services_parity`
Expected: PASS (with `/tmp/maj-ref` present from Task 1 Step 1).

- [ ] **Step 6: Lint and commit**

```bash
just check
git add crates/services crates/cli
git commit -m "refactor: extract search compute into services with parity harness"
```

---

### Task 3: Extract the remaining read verbs

**Files:**
- Create: `crates/services/src/{volumes.rs,meta.rs,para.rs,tags.rs,describer.rs,sync.rs,index.rs}` (read halves), `crates/services/src/catalog.rs` (`open_catalog` moved here)
- Modify: cli `commands.rs`, `tags_cmd.rs`, `describer_cmd.rs`, `sync_cmd.rs`, `index_cmd.rs` (shrink to rendering), `crates/services/src/lib.rs`
- Test: extend `crates/cli/tests/services_parity.rs`

Same recipe as Task 2 for each verb. Per verb: outcome struct (fields fitted
from the existing `json!` payload, field-fitting rule applies), `pub fn` in
services holding the moved compute, CLI renders unchanged. TDD each verb with
one services unit test on a fixture catalog (Task 2 Step 1's fixture pattern),
then the parity diff.

- [ ] **Step 1: `volumes_list`** — move `commands.rs::cmd_volumes_list`'s compute (the `db.volumes()` + counts + `suspect_ceiling` block, `commands.rs:269-283`) to `services::volumes::volumes_list(app, catalog_dir) -> Result<VolumesOutcome, ServiceError>`:

```rust
#[derive(serde::Serialize)]
pub struct VolumeRow {
    pub id: String,
    pub label: String,
    pub last_seen_ms: u64,
    pub online: bool,
    pub asset_count: u64,
    pub clock_suspect: bool,
}
#[derive(serde::Serialize)]
pub struct VolumesOutcome { pub volumes: Vec<VolumeRow> }
```

`volume_is_online` moves to `services::volumes` (`pub`). CLI keeps `iso8601_ms` formatting + table/`json!` printing. Failing test → move → parity rows: `["volumes", "list", "--json"]`, `["volumes", "list"]`.

- [ ] **Step 2: `meta_get`** — move `print_meta_get`'s lookup half: `services::meta::meta_get(app, asset, field) -> Result<MetaOutcome, ServiceError>` where `MetaOutcome { pub fields: Vec<(String, String)> }` (single-field request returns 0-or-1 entries). CLI keeps both print styles. Parity: `["meta", "get", <asset>, "--json"]`.

- [ ] **Step 3: `para_list`** — move `cmd_para_list` compute: `services::para::para_list(app, catalog_dir) -> Result<ParaOutcome, ServiceError>`, `ParaNodeRow { pub id, pub kind, pub name, pub archived }` (types per `db.para_nodes()` row). Also move (unchanged, now `pub`): `parse_kind`, `resolve_para_node` — they're shared compute the mutating task needs. Parity: `["para", "list", "--json"]`.

- [ ] **Step 4: `tags_suggestions`** — move `tags_cmd.rs::cmd_suggestions` compute to `services::tags::suggestions(app, catalog_dir) -> Result<SuggestionsOutcome, ServiceError>` (fit the row struct to the existing output — the suggestions list with asset/tag/source fields as printed today). Parity: `["tags", "suggestions"]`.

- [ ] **Step 5: `describer_show`** — `describer_cmd.rs::cmd_show` reads state-dir config; move the read to `services::describer::show(catalog_dir) -> Result<Option<DescriberConfigView>, ServiceError>` with the key redacted in the view type (redaction is compute, not rendering — MCP must never see the raw key either):

```rust
#[derive(serde::Serialize)]
pub struct DescriberConfigView {
    pub backend: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Always `"<redacted>"` when a key is configured; never the key.
    pub api_key: Option<String>,
}
```

Parity: `["describer", "show"]`.

- [ ] **Step 6: `sync_status` + `sync_location_list`** — move `sync_cmd.rs::cmd_status`'s walk (the planned-transfer computation both directions) to `services::sync::status(catalog_dir) -> Result<SyncStatusOutcome, ServiceError>`; fit row structs to the existing per-location JSON (location name, reachability, per-machine segment counts, per-class blob counts, `in sync` collapse happens at render). Move `cmd_location_list`'s config read to `services::sync::locations_list(catalog_dir) -> Result<Vec<LocationRow>, ServiceError>`. Parity: `["sync", "status", "--json"]`, `["sync", "location", "list", "--json"]` (fixture adds one location first).

- [ ] **Step 7: `index_status`** — move `index_cmd.rs::cmd_index_status` compute to `services::index::status(app, catalog_dir) -> Result<IndexStatusOutcome, ServiceError>` (fit per-kind queue rows). Parity: `["index", "status", "--json"]`.

- [ ] **Step 8: Full suite + lint + commit**

```bash
cargo test --workspace && just check
git add crates/services crates/cli
git commit -m "refactor: extract read-verb compute into services"
```

---

### Task 4: `get_asset` — the one genuinely new operation

**Files:**
- Modify: `crates/services/src/catalog.rs` (add `get_asset`), `crates/services/src/lib.rs`
- Test: services unit tests (in-module)

No CLI verb changes here — `get_asset` exists for MCP (spec tool table) and the
GUI inspector. It assembles what the projection + sqlite already know.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn get_asset_assembles_instances_tags_para_meta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
    let asset = AssetId("xxh3:0123456789abcdef0123456789abcdef".to_string());
    let node = ulid::Ulid::generate().to_string();
    app.emit(vec![
        Op::VolumeSeen { volume: "vol1".into(), label: "vol1".into() },
        Op::AssetSeen {
            asset: asset.clone(),
            volume: "vol1".into(),
            path: "clips/a.mov".into(),
            size: 5,
            mtime_ms: 1000,
        },
        Op::TagAdd { asset: asset.clone(), tag: "demo".into() },
        Op::FieldSet {
            asset: asset.clone(),
            field: "shot".into(),
            value: "sunset".into(),
        },
        Op::ParaNodeCreate {
            node: node.clone(),
            kind: ParaKind::Project,
            name: "client-x".into(),
        },
        Op::AssetParaSet { asset: asset.clone(), node },
    ])
    .expect("emit");
    let out = get_asset(&mut app, &root, &asset.0)
        .expect("get_asset")
        .expect("known asset");
    assert_eq!(out.instances.len(), 1);
    assert_eq!(out.tags, vec!["demo"]);
    assert_eq!(out.para.as_deref(), Some("project/client-x"));
    assert_eq!(out.fields[0], ("shot".to_string(), "sunset".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p majestical-services get_asset -- --nocapture`
Expected: FAIL — function doesn't exist.

- [ ] **Step 3: Implement**

```rust
#[derive(serde::Serialize)]
pub struct AssetInstance {
    pub volume: String,
    pub volume_label: String,
    pub online: bool,
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(serde::Serialize)]
pub struct AssetDetail {
    pub asset: String,
    pub instances: Vec<AssetInstance>,
    pub tags: Vec<String>,
    /// `<kind>/<name>` of the assigned PARA node, if any.
    pub para: Option<String>,
    pub fields: Vec<(String, String)>,
    /// Latest verification per volume: (volume, outcome, hashdate_ms).
    pub verifications: Vec<(String, String, u64)>,
    pub has_thumb: bool,
}

/// Returns `Ok(None)` for an unknown asset — "not found" is a value here
/// (MCP tools and the GUI both need to render it), not an error.
pub fn get_asset(
    app: &mut crate::app::FsApp,
    catalog_dir: &Path,
    asset_id: &str,
) -> Result<Option<AssetDetail>, ServiceError> { /* assemble from
    app.projection() (instances/tags/fields/para via the accessors used by
    cmd_tag/print_meta_get/resolve_para_node) + volumes::volume_is_online
    per instance + BlobStore::new(catalog_dir) thumb existence via
    majestical_index::blob::{BlobStore, Derivation, asset_hex}. */ }
```

The body is written against accessors that already exist (`projection.has_instances`, `projection.fields`, `projection.tag_add_ids`' sibling readers, `projection.para_node`); if a needed reader is missing on `Projection`, add it to `crates/core` with its own unit test (extending the hexagon's read surface is in-scope; new `Op` variants are NOT).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p majestical-services get_asset`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/services crates/core
git commit -m "feat: add get_asset service operation"
```

---

### Task 5: Extract the mutating verbs

**Files:**
- Create: `crates/services/src/{scan.rs,verify.rs,ingest.rs,inbox.rs}`; extend `catalog.rs`, `meta.rs`, `para.rs`, `tags.rs`, `describer.rs`, `sync.rs`, `index.rs`
- Modify: cli `commands.rs`, `sync_cmd.rs`, `inbox_cmd.rs`, `index_cmd.rs`, `tags_cmd.rs`, `describer_cmd.rs` (shrink), `main.rs`
- Test: extend `services_parity.rs`; services unit tests per verb

Recipe identical to Tasks 2-3; the seam per verb is "everything before the
print". The dry-run modes that exist today (`ingest --dry-run`,
`para archive --dry-run`) move WITH the compute — outcome structs carry a
`planned` vs `executed` shape so Task 8's MCP confirm gate reuses them without
new plumbing. Per-verb inventory (source function → service fn → outcome):

| Source (cli) | Service fn | Outcome |
| --- | --- | --- |
| `cmd_catalog_init` | `catalog::init(root, machine, author)` | `()` (path printed by CLI) |
| `cmd_scan` | `scan::scan(app, dir, volume)` | `ScanOutcome { pub assets: usize, pub volume_id: String }` |
| `cmd_tag` add/rm | `tags::tag_add` / `tags::tag_rm` | `()` — refusals stay `ServiceError` |
| `cmd_meta` set | `meta::meta_set(app, asset, field, value)` | `()` |
| `cmd_para_add/rename/archive` | `para::{add,rename,archive}` | `add` → `NodeId(String)`; `archive` → `ArchiveOutcome { pub moves: Vec<ArchiveMove>, pub executed: bool }`, `ArchiveMove { pub from: PathBuf, pub to: PathBuf, pub status: MoveStatus }`, `MoveStatus { Moved, AlreadyArchived, Planned }` |
| `cmd_verify` | `verify::verify_dir_op(dir)` | `VerifyReport { pub verified: usize, pub altered: Vec<String>, pub missing: Vec<String>, pub new_files: Vec<String>, pub generation: u64 }` — name avoids clashing with `core::event::VerifyOutcome`; the pass/fail *policy* (nonzero exit when altered/missing) STAYS in the CLI/heads |
| `cmd_ingest` + `run_ingest` | `ingest::plan(app, req)` + `ingest::execute(app, catalog_dir, req)` | plan → the existing `plan::IngestPlan` (+ rendered subdir); execute → existing `IngestRun` (moves to services; `IngestReport` enum + printing STAY in cli — `execute` takes a `on_progress: &mut dyn FnMut(&str)` no-op default replacing the direct eprintln for the resume line, keeping stderr byte-identical when the CLI passes its printer through) |
| `cmd_location_add/rm` | `sync::location_add` / `sync::location_rm` | `()` |
| `cmd_push` / `cmd_pull` | `sync::push(catalog_dir, req)` / `sync::pull(catalog_dir, machine, author, req)` | fit `PushOutcome`/`PullOutcome` row structs to the existing per-location JSON rows (outcome/skipped/failed, pull's applied-events + remedy notice flag) |
| `cmd_inbox_process` | `inbox::process(app, catalog_dir, req)` | fit `InboxOutcome` to the existing per-contribution report rows |
| `cmd_index_run` | `index::run(app, catalog_dir, req)` | fit `IndexRunOutcome` to the existing summary JSON; `--watch` loop STAYS in the CLI (the service exposes one pass; the CLI/MCP callers decide about looping) |
| `cmd_set/cmd_test` (describer) | `describer::set` / `describer::test` | `set` → `()`; `test` → `DescriberProbe { pub connected: bool, pub model_present: bool, pub vision: bool, pub detail: String }` (fit to current output) |
| `cmd_model_fetch` | `index::model_fetch(verify, only)` | fit to current summary |

Exit-code polarity note (spec §Architecture): `sync::push`/`pull` and
`inbox::process` outcomes each expose `pub fn overall_failed(&self) -> bool`
implementing the phase-6 policy (all-locations-failed OR any per-file failure;
operator-fixable faults). The CLI maps it to exit codes exactly as today; MCP
maps it in Task 8. The policy lives on the outcome — written once.

- [ ] **Step 1: Simple event-emitting verbs** (catalog_init, scan, tag, meta_set, para add/rename) — failing services test each (fixture + assert emitted projection state), move, parity rows: `["tag", "add", <asset>, "x"]`, `["para", "add", "project", "p1"]`, `["scan", <dir>, "--volume", "v"]`, `["meta", "set", <asset>, "f", "v"]`. Commit: `refactor: extract simple mutating verbs into services`.

- [ ] **Step 2: para archive + verify** — services tests cover: archive dry-run returns `Planned` moves without touching disk; archive converges on re-run (`AlreadyArchived`); verify counts on an intact + a tampered destination (fixture: run a real tiny ingest via the existing engine, then flip a byte). Parity rows: `["verify", <dir>, "--json"]`, `["para", "archive", "project/p1", "--dry-run"]`. Commit: `refactor: extract verify and para archive into services`.

- [ ] **Step 3: ingest** — this is the phase-6 `run_ingest` moving crates. The move: `ExecuteIngest`, `IngestRun`, `run_ingest`, and its private helpers (`build_dest_specs` … `manifest_ops`, `commands.rs:696-1107`) go to `services::ingest`; `IngestReport` + `print_ingest_*` stay in cli. `inbox_cmd.rs`'s call sites update. Existing ingest + inbox smoke suites are the net; parity rows: `["ingest", <src>, "--dest", <d>, "--para", "project/p1", "--json"]` and `--dry-run` both modes. Commit: `refactor: move run_ingest pipeline into services`.

- [ ] **Step 4: sync push/pull + locations + inbox process + index run + describer set/test + model fetch** — same recipe each; sync/inbox smoke suites (~60 tests) are the net. THE ordering pin from phase 6 (pull applies events even when blob failures occurred) must keep its direct `catalog.db` assertion green — do not touch that test's expectations. Parity rows: `["sync", "push", "--json"]`, `["sync", "pull", "--json"]`, `["inbox", "process", <inbox>, "--dest", <d>, "--json"]`, `["index", "run", "--limit", "0", "--json"]`. Commit: `refactor: extract remaining mutating verbs into services`.

- [ ] **Step 5: Full suite + lint**

```bash
cargo test --workspace && just check
```

Expected: PASS. `crates/cli/src/commands.rs` is now rendering + arg structs; if any `cmd_*` still contains compute, it's a missed seam — move it before closing the task.

---

### Task 6: `maj mcp` scaffold + read tools

**Files:**
- Create: `crates/cli/src/mcp_cmd.rs`, `crates/cli/tests/mcp_smoke.rs`
- Modify: `crates/cli/src/main.rs` (Mcp subcommand), `crates/cli/Cargo.toml` (rmcp, tokio, schemars)

- [ ] **Step 1: Verify current rmcp API + versions**

Look up current stable `rmcp`, `tokio`, `schemars` versions (context7:
`/modelcontextprotocol/rust-sdk`, or crates.io). Pin exact versions in the
workspace `Cargo.toml`. The code below is written against rmcp's
`#[tool_router]`/`#[tool]` + `ServerHandler` + `serve(stdio())` surface as of
the 0.x line — adapt names to the current release; **the smoke tests in Step 2
define the contract, not this sketch.** tokio features: `rt-multi-thread`,
`macros`, `io-std` only.

- [ ] **Step 2: Write the failing smoke test (protocol-level, no client dep)**

MCP stdio transport is newline-delimited JSON-RPC 2.0 — the test speaks it raw:

```rust
//! crates/cli/tests/mcp_smoke.rs
mod common; // fixture_catalog + asset_id_of from Task 2

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl Mcp {
    fn spawn(catalog: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_maj"))
            .env("MAJ_CATALOG", catalog)
            .env("MAJ_MACHINE_ID", "m1")
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn maj mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut s = Self { child, stdin, stdout, next_id: 0 };
        let init = s.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp_smoke", "version": "0"}
            }),
        );
        assert!(init["result"]["serverInfo"]["name"].is_string());
        s.notify("notifications/initialized", serde_json::json!({}));
        s
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": self.next_id,
            "method": method, "params": params
        });
        writeln!(self.stdin, "{msg}").expect("write");
        let mut line = String::new();
        loop {
            line.clear();
            self.stdout.read_line(&mut line).expect("read");
            let v: serde_json::Value = serde_json::from_str(&line).expect("json");
            if v["id"] == serde_json::json!(self.next_id) {
                return v;
            } // skip server notifications
        }
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("write");
    }

    fn call_tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The full-parity tool roster. This IS the spec's parity rule as a test:
/// a verb added to the CLI without a tool shows up here as a diff.
#[test]
fn tool_list_matches_roster() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root);
    let listed = mcp.request("tools/list", serde_json::json!({}));
    let mut names: Vec<String> = listed["result"]["tools"]
        .as_array().expect("tools")
        .iter().map(|t| t["name"].as_str().expect("name").to_string())
        .collect();
    names.sort();
    let expected = [
        "add_sync_location", "catalog_init", "get_asset", "get_describer",
        "index_run", "index_status", "ingest_source", "inbox_process",
        "list_saved_searches", "list_sync_locations", "list_volumes",
        "move_para", "rm_saved_search", "rm_sync_location",
        "run_saved_search", "scan_volume", "search_assets", "set_describer",
        "set_metadata", "suggest_tags_review", "sync_pull", "sync_push",
        "sync_status", "tag_assets", "test_describer", "verify_volume",
    ];
    assert_eq!(names, expected, "tool roster drifted from CLI parity");
}

#[test]
fn search_assets_rows_match_cli_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root);
    let res = mcp.call_tool("search_assets", serde_json::json!({"query": "a.txt"}));
    let content = res["result"]["structuredContent"].clone();
    assert_eq!(content["count"], 1);
    assert_eq!(content["results"][0]["name"], "a.txt");
    assert!(content["results"][0]["asset"].as_str().expect("id").starts_with("xxh3:"));
}

#[test]
fn get_asset_unknown_is_a_value_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let mut mcp = Mcp::spawn(&root);
    let res = mcp.call_tool("get_asset", serde_json::json!({"asset": "xxh3:0000"}));
    assert_eq!(res["result"]["isError"], serde_json::json!(false));
    assert_eq!(res["result"]["structuredContent"]["found"], false);
}
```

(Mutating-tool names appear in the roster now — Task 8 implements them;
until then they are registered with a handler returning an MCP tool error
`"not yet implemented"` so the roster test is stable from day one. The
Task 8 tests replace that behavior.)

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p majestical-cli --test mcp_smoke -- --nocapture`
Expected: FAIL — no `mcp` subcommand.

- [ ] **Step 4: Implement scaffold + read tools**

`main.rs`: add `Mcp` to `enum Cmd` (`/// Serve the catalog to MCP clients over stdio.`); dispatch:

```rust
Cmd::Mcp => mcp_cmd::serve(&cli.catalog, &cli.machine_id, &author)?,
```

`mcp_cmd.rs` shape (adapt to verified rmcp API):

```rust
//! `maj mcp`: stdio MCP server. Thin tool wrappers over majestical_services
//! — no operation logic lives here. Mutating tools gate on `confirm`
//! (Task 8); read tools call straight through. Every tool returns the
//! service outcome as structured content.
use majestical_services::{app::FsApp, ...};

pub(crate) fn serve(catalog: &Path, machine_id: &str, author: &str) -> anyhow::Result<()> {
    // Catalog existence is checked per tool call, not at startup — the
    // server must come up (and list tools) against a not-yet-initialized
    // catalog so agents can call catalog_init through it.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let server = MajServer::new(catalog, machine_id, author);
        rmcp::serve_server(server, rmcp::transport::stdio()).await?.waiting().await?;
        Ok(())
    })
}

struct MajServer { catalog: PathBuf, machine_id: String, author: String }

// Per tool: a schemars-derived params struct mirroring the service request
// struct, e.g.:
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SearchAssetsParams {
    /// Same query language as `maj search`: bare terms match names,
    /// key:value tokens filter (tag: vol: para: kind: online: before:
    /// after:), '-' negates.
    query: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    saved: Option<String>,
}
fn default_limit() -> usize { 50 }

// Tool body: open FsApp, call services, wrap outcome:
//   let mut app = FsApp::open(&self.catalog, &self.machine_id, &self.author)
//       .map_err(tool_error)?;   // NoCatalog surfaces the remedy string
//   let out = majestical_services::search::search(&mut app, &self.catalog, &req)
//       .map_err(tool_error)?;
//   Ok(structured(out))          // serde_json::to_value → structuredContent
```

Read tools in this task: `search_assets`, `get_asset` (returns
`{"found": false}` for unknown — mirrors `Ok(None)`), `list_volumes`,
`list_saved_searches`, `run_saved_search`, `sync_status`, `index_status`,
`list_sync_locations`, `get_describer`, `suggest_tags_review` (read half:
lists pending suggestions). Every open goes through `FsApp::open` per call —
no cached handles (stateless beats clever state; a concurrent CLI append is
always picked up).

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p majestical-cli --test mcp_smoke`
Expected: PASS all three.

- [ ] **Step 6: Lint and commit**

```bash
just check
git add crates/cli Cargo.toml Cargo.lock
git commit -m "feat: add maj mcp stdio server with read tools"
```

---

### Task 7: MCP resources — thumbnails + keyframe manifests

**Files:**
- Modify: `crates/cli/src/mcp_cmd.rs`
- Test: extend `crates/cli/tests/mcp_smoke.rs`

Spec: `majestical://thumb/{asset_id}` and keyframe resources. As-built
deviation to record at closing: keyframe *images* are not stored as blobs
(only embeddings + the timestamp manifest, `blob.rs::Derivation`), so the
keyframe resource serves the manifest JSON; on-demand frame extraction goes
to the watchlist with this plan's attribution.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn thumb_resource_serves_webp_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    // Plant a thumb blob directly (BlobStore::write_atomic path layout):
    // blobs/<first2>/<resthex>/thumb-320.webp
    let asset = common::asset_id_of(&root, "a.txt");
    let hex = asset.strip_prefix("xxh3:").expect("hex");
    let blob_dir = root.join("blobs").join(&hex[..2]).join(&hex[2..]);
    std::fs::create_dir_all(&blob_dir).expect("mkdir");
    std::fs::write(blob_dir.join("thumb-320.webp"), b"RIFFfakewebp").expect("write");

    let mut mcp = Mcp::spawn(&root);
    let res = mcp.request(
        "resources/read",
        serde_json::json!({"uri": format!("majestical://thumb/{asset}")}),
    );
    let blob = res["result"]["contents"][0]["blob"].as_str().expect("b64");
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob).expect("decode");
    assert_eq!(bytes, b"RIFFfakewebp");
    assert_eq!(res["result"]["contents"][0]["mimeType"], "image/webp");
}

#[test]
fn missing_thumb_is_a_clean_resource_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, "a.txt");
    let mut mcp = Mcp::spawn(&root);
    let res = mcp.request(
        "resources/read",
        serde_json::json!({"uri": format!("majestical://thumb/{asset}")}),
    );
    let msg = res["error"]["message"].as_str().expect("err");
    assert!(msg.contains("maj index run"), "error must name the remedy: {msg}");
}
```

(Confirm the exact `<first2>/<resthex>` split against
`blob.rs::blob_paths_are_derivation_keyed` — the test fixture must use
`BlobStore::path_for` if the layout helper is public, which it is:
`BlobStore::new(&root).path_for(hex, &Derivation::Thumb)`. Prefer that over
hand-building the path.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p majestical-cli --test mcp_smoke thumb -- --nocapture`
Expected: FAIL — resources/read unhandled.

- [ ] **Step 3: Implement**

Resource handlers in `mcp_cmd.rs`:

- `majestical://thumb/{asset_id}` → `BlobStore::new(catalog).path_for(asset_hex(id), &Derivation::Thumb)`; serve bytes base64 as `image/webp`. Missing file → resource error: `"no thumbnail for <id> — run `maj index run --kinds thumbs`"`.
- `majestical://keyframes/{asset_id}` → the keyframe manifest blob (`Derivation::KeyframeManifest { model_tag }` for the current encoder's tag — resolve the tag the same way `index_cmd` does); serve decompressed JSON as `application/json`, same remedy-naming error when absent.
- `resources/list` returns the two URI templates (MCP `resources/templates/list` if the current spec revision splits them — follow the rmcp API).

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p majestical-cli --test mcp_smoke`
Expected: PASS.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/cli
git commit -m "feat: serve thumbnail and keyframe-manifest MCP resources"
```

---

### Task 8: Mutating tools + the confirm gate

**Files:**
- Modify: `crates/cli/src/mcp_cmd.rs`
- Test: extend `crates/cli/tests/mcp_smoke.rs`

Every mutating tool takes `confirm: bool` (schemars-documented: "false (the
default) returns a dry-run description of what would happen; true executes").
The dry-run halves come from Task 5's outcomes: `ingest::plan` for
`ingest_source`, `para::archive`'s `Planned` moves for `move_para` archives,
and for verbs with no natural plan (tag_assets, set_metadata, catalog_init,
set_describer, add/rm_sync_location, scan_volume, sync_push/pull,
inbox_process, index_run, verify_volume) the dry-run returns a structured
`{"would": ...}` description built from the request + current state (e.g.
tag_assets dry-run reports the asset's current tags and the requested
add/remove; sync_push dry-run returns `sync::status`'s planned-transfer rows
for the target locations — real state walked fresh, never a guess).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn tag_assets_defaults_to_dry_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, "b.txt");
    let mut mcp = Mcp::spawn(&root);
    let res = mcp.call_tool(
        "tag_assets",
        serde_json::json!({"asset": asset, "op": "add", "tag": "kf"}),
    );
    let c = &res["result"]["structuredContent"];
    assert_eq!(c["executed"], false);
    // ...and the catalog is untouched — verified through a real search:
    let hits = mcp.call_tool("search_assets", serde_json::json!({"query": "tag:kf"}));
    assert_eq!(hits["result"]["structuredContent"]["count"], 0);
}

#[test]
fn tag_assets_confirm_executes_and_is_visible_to_cli() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let asset = common::asset_id_of(&root, "b.txt");
    let mut mcp = Mcp::spawn(&root);
    let res = mcp.call_tool(
        "tag_assets",
        serde_json::json!({"asset": asset, "op": "add", "tag": "kf", "confirm": true}),
    );
    assert_eq!(res["result"]["structuredContent"]["executed"], true);
    drop(mcp);
    // Cross-process visibility: the CLI sees the tag (same event log).
    let out = Command::new(env!("CARGO_BIN_EXE_maj"))
        .env("MAJ_CATALOG", &root).env("MAJ_MACHINE_ID", "m2")
        .args(["search", "tag:kf", "--json"]).output().expect("run");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["count"], 1);
}

#[test]
fn ingest_source_dry_run_returns_plan_and_copies_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&dest).expect("mkdir");
    let mut mcp = Mcp::spawn(&root);
    mcp.call_tool(
        "move_para",
        serde_json::json!({"op": "add", "kind": "project", "name": "p1", "confirm": true}),
    );
    let res = mcp.call_tool("ingest_source", serde_json::json!({
        "source": dir.path().join("src").to_str().expect("utf8"),
        "dest": [dest.to_str().expect("utf8")],
        "para": "project/p1"
    }));
    let c = &res["result"]["structuredContent"];
    assert_eq!(c["executed"], false);
    assert_eq!(c["plan"]["files"].as_array().expect("files").len(), 2);
    assert!(std::fs::read_dir(&dest).expect("dir").next().is_none(),
        "dry-run must not copy");
}

/// Phase-6 polarity through MCP: per-location rows always come back in the
/// structured outcome; `isError` follows `overall_failed()` — but partial
/// progress is attached either way, never discarded.
#[test]
fn sync_push_partial_failure_keeps_rows_and_maps_polarity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = common::fixture_catalog(dir.path());
    let (good, bad) = (dir.path().join("loc-good"), dir.path().join("loc-bad"));
    let mut mcp = Mcp::spawn(&root);
    for (name, path) in [("good", &good), ("bad", &bad)] {
        let res = mcp.call_tool("add_sync_location", serde_json::json!({
            "name": name, "path": path.to_str().expect("utf8"), "confirm": true
        }));
        assert_eq!(res["result"]["isError"], serde_json::json!(false));
    }
    // Break "bad" the same way sync_smoke.rs's unreachable-location tests
    // do (reuse its permissions guard verbatim — it already skips when
    // running as root, where chmod 000 doesn't deny).
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let res = mcp.call_tool("sync_push", serde_json::json!({"confirm": true}));
    let c = &res["result"]["structuredContent"];
    let rows = c["locations"].as_array().expect("rows");
    assert_eq!(rows.len(), 2, "both locations reported: {c}");
    assert!(rows.iter().any(|r| r["name"] == "good" && r["failed"] == serde_json::json!(false)));
    assert!(rows.iter().any(|r| r["name"] == "bad" && r["failed"] == serde_json::json!(true)));
    assert_eq!(c["overall_failed"], true);
    assert_eq!(res["result"]["isError"], serde_json::json!(true),
        "operator-fixable partial failure maps to isError with rows attached");
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755))
        .expect("chmod back");
}
```

(Field names `locations`/`failed` follow Task 5's `PushOutcome` — if the
fitted struct named them differently, align the TEST to the struct, and the
struct to the existing CLI JSON rows, in that order.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p majestical-cli --test mcp_smoke tag_assets -- --nocapture`
Expected: FAIL — tools return "not yet implemented".

- [ ] **Step 3: Implement**

One generic gate, used by every mutating tool:

```rust
/// Wraps a mutating tool: `confirm: false` returns `dry` (executed=false),
/// `true` runs `exec` (executed=true). Both arms serialize the same outcome
/// type family so agents can diff plan vs result.
fn confirm_gate<T: serde::Serialize>(
    confirm: bool,
    dry: impl FnOnce() -> anyhow::Result<T>,
    exec: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<serde_json::Value> {
    let (executed, outcome) = if confirm { (true, exec()?) } else { (false, dry()?) };
    let mut v = serde_json::to_value(outcome)?;
    v.as_object_mut()
        .expect("outcomes serialize as objects")
        .insert("executed".into(), serde_json::json!(executed));
    Ok(v)
}
```

Mutating tools and their service calls (all from Task 5): `tag_assets`
(`op: add|rm|confirm_suggestion|reject_suggestion` — the tags/tags_cmd
family), `set_metadata`, `move_para` (`op: add|rename|archive|assign` —
assign = ingest-style `AssetParaSet` is NOT a current CLI verb, so it is NOT
a tool; ops are exactly add/rename/archive), `scan_volume`, `verify_volume`,
`ingest_source`, `catalog_init` (uses `FsApp::init`; refuses when the catalog
exists), `sync_push`, `sync_pull`, `add_sync_location`, `rm_sync_location`,
`inbox_process`, `index_run` (no watch mode — single pass), `set_describer`,
`test_describer` (mutating classification per spec: it probes an external
backend — confirm-gated, dry-run reports the config it would probe),
`rm_saved_search`.

isError mapping: `ServiceError` → MCP tool error with the error's display
chain (remedy included). Outcomes with `overall_failed() == true` →
`isError: true` WITH the full structured outcome still attached (partial
progress reported, never discarded).

- [ ] **Step 4: Run the full MCP suite**

Run: `cargo test -p majestical-cli --test mcp_smoke`
Expected: PASS — roster test now exercises real handlers too.

- [ ] **Step 5: Lint and commit**

```bash
just check
git add crates/cli
git commit -m "feat: add mutating MCP tools behind confirm gate"
```

---

### Task 9: Closing — parity assertion, docs, mutants, watchlist

**Files:**
- Modify: `crates/cli/tests/mcp_smoke.rs`, `docs/superpowers/plans/2026-07-29-phase2-watchlist.md`, spec as-built section, `README`/site docs snippet for MCP client config
- Create: none

- [ ] **Step 1: Wire-format + op-variant assertion** — re-run the phase-6 check: `git diff main -- crates/core/src/event.rs` must be empty of `Op` variant changes (get_asset read accessors on `Projection` are fine). Extend `sample_ops()`'s doc note: phase 7 added zero variants.

- [ ] **Step 2: MCP client config doc** — add to the README (or docs site page) the two-line client config:

```json
{ "mcpServers": { "majestical": {
  "command": "maj", "args": ["mcp"],
  "env": { "MAJ_CATALOG": "/path/to/catalog", "MAJ_MACHINE_ID": "studio-1" }
} } }
```

- [ ] **Step 3: cargo-mutants on the new seams** — run per-file, foreground, split scopes (phase-6 lesson: never park a subagent waiting on a background mutants run):

```bash
cargo mutants --package majestical-services --file crates/services/src/search.rs --timeout 300
cargo mutants --package majestical-services --file crates/services/src/catalog.rs --timeout 300
cargo mutants --package majestical-cli --file crates/cli/src/mcp_cmd.rs --timeout 300
```

Triage survivors into the watchlist's "cargo-mutants triage (phase 7A)" section with dispositions.

- [ ] **Step 4: Watchlist + spec as-built** — record: keyframe images not stored → manifest resource + on-demand extraction deferred; `PortError` opacity / projection re-scans if they surfaced during MCP work (address inline if they blocked anything, per spec); MCP progress notifications for long tools (deferred per spec); any rmcp API adaptations worth noting for Plan B/C.

- [ ] **Step 5: Full CI + commit**

```bash
just ci
git add -u docs crates
git commit -m "test: phase 7A closing - parity assertion and mutants triage"
```

---

## Self-review checklist (run before handing off)

- Spec chunks 1-5 all have tasks: services skeleton+search (T1-2), read verbs (T3), get_asset (T4), mutating verbs (T5), MCP read (T6), resources (T7), MCP mutating+confirm (T8), closing (T9). ✓
- Every extraction task pins byte-identical CLI output via the reference-binary harness. ✓
- Tool roster in Task 6's test == spec's table + parity-rule additions (`set_metadata`, `scan_volume`, `rm_saved_search`, `suggest_tags_review`, location tools). Model-fetch intentionally has NO tool: it's a machine-local cache op needing no catalog; add it only if an agent workflow demands it (record in watchlist).
- `Date`-free, placeholder-free; rmcp API adaptation is explicitly bounded by protocol-level tests that don't depend on the SDK.
