# Majestical Phase 3 — Ingest engine + ASC MHL + PARA

Date: 2026-07-29
Status: Approved section-by-section in design session; pending written-spec review.
Parent spec: `2026-07-28-majestical-design.md` (§2 data model, §3 ingest, §7 testing).
Handoff: `docs/superpowers/HANDOFF-phase3.md`.

## Scope decisions (from design session)

1. **Full PARA model** — ParaNode CRDT with create/rename/archive, asset
   assignment, and the `maj para` command family. Not the minimal-node or
   plain-path variants.
2. **Multi-destination ingest now** — N destinations per run, each independently
   verified with its own ASC MHL history.
3. **Watchlist absorption: mandated + adjacent** — non-UTF-8 byte preservation,
   commands-module extraction, root-volume-lumping revisit, cargo-mutants run,
   plus the CatalogStore port catch-up. Segment rotation, incremental SQLite
   apply, local-state/sync-root split, and meta-get clock flagging stay deferred.
4. **Engine: file-parallel workers** — bounded worker pool, each file handled
   sequentially end to end. The spec §3 staged pipeline remains a documented
   later option if profiling demands it.

Out of scope this phase (per parent spec build order): auto-ingest rules, cloud
inbox / contribution manifests, cascading transfers, nested MHL histories /
flatten / collections, thumbnails, FTS5.

## Architecture

New crate `crates/ingest`: planner, copy engine, transfer journal, ASC MHL
reader/writer. Depends on `core` ports only; the CLI injects real adapters.
Events remain truth; SQLite stays disposable.

### New core ops (additive; wire format extended, never changed)

- `ParaNodeCreate { node_id, kind: Project|Area|Resource|Archive, name }` —
  nodes are an add-only set; name and lifecycle state are HLC-LWW fields.
- `ParaNodeRename { node_id, name }` — LWW on the node.
- `ParaNodeArchive { node_id }` — LWW lifecycle state. The disk move is an
  engine action; the event records the outcome. Rename graph preserved.
- `AssetParaSet { asset_hash, node_id }` — asset→PARA assignment, HLC-LWW
  scalar (same semantics as the existing `FieldSet`).
- `VerificationRecorded { asset_hash, volume_id, path, algo, value,
  outcome: Original|Verified|Failed, hashdate }` — the hash-history model from
  parent spec §2.
- `ManifestRecorded { volume_id, mhl_path, generation, roothash }` — manifest
  identity stored in the catalog so tampering with `ascmhl/` on disk is
  detectable.

Every new op extends the proptest generator (order-independence obligation) and
gets golden wire-format tests.

### PARA is logical; volumes materialize it

A ParaNode is a catalog entity (`Projects/client-x`). Each destination root
materializes the node as a real directory. Multi-destination ingest is "the
same ParaNode materialized on N volumes": one `AssetParaSet` per asset, N
`FileInstance`s. The CRDT model stays volume-agnostic; the copy engine is a
pure fan-out.

### Projection / store

New catalog-sqlite tables: `para_nodes`, `asset_para`, `file_instances` (with
hash history), `manifests`. Watchlist fix folded in: `volumes()` and
`volume_asset_counts()` move onto the `CatalogStore` trait together with the
new queries, so the port stops lagging the inherent surface.

## Ingest engine

Flow (parent spec §3): plan → copy+hash → verify → manifest → catalog → place
into PARA.

### Plan stage

Walk the source (any folder or mounted volume/card). Per file: stat, then
dedupe with a **size prefilter** — only when the size matches a known asset's
size is the source pre-hashed to confirm a content-hash match. Dedupe decision
per run: `skip` / `copy` (copy anyway) / `link` (copy-and-link). `--dry-run`
prints the full plan (files, dedupe decisions, destination paths) and exits.

### Copy stage — file-parallel workers

K workers (default: physical core count, capped; `--jobs` overrides). Per file:

1. Stream the source once in large chunks, updating two hashers in one pass —
   xxHash64 (ASC MHL interchange) and xxh3-128 (catalog asset identity,
   matching `scan`) — while fanning each chunk out to every destination's
   write handle.
2. Destinations write to a temp name (`.maj-partial-<ulid>`), then fsync.
3. Read-back verify: re-read each destination independently, re-hash, compare
   to the source hash.
4. Only on match is the file renamed into place. Rename-after-verify means a
   crash never leaves an unverified file at a final path; failures stay
   quarantined under the temp name by construction.

0-byte detection and an end-of-run missing-file sweep, per parent spec.

### Transfer journal

JSONL checkpoint per run in the catalog's local state area; one record per file
state transition (planned → copied → verified → placed / failed).
`maj ingest --resume <run-id>` — and a bare re-run that detects an incomplete
journal — resumes at file granularity: placed files skip; partials are deleted
and redone.

### Non-UTF-8 paths (mandated)

The engine carries `OsString`/bytes end to end — copy, journal, rename are
byte-exact. ASC MHL XML cannot represent non-UTF-8 names, so a file with a
non-UTF-8 name is a per-file hard error (counted, reported; the run continues),
never a silent lossy rename. macOS-local filesystems enforce UTF-8; this bites
only on foreign NAS/SMB sources.

### Volume identity (mandated revisit)

Destination rows record the actual mount's identity via the existing
`volume_identity` machinery; the root-volume-lumping fallback applies only when
diskutil yields nothing.

## ASC MHL

Own implementation in `crates/ingest/src/mhl/` — no mature Rust crate existed
at design time; re-verify at execution. Create + verify only this phase.

- Generation-numbered `ascmhl/NNNN_<name>_<date>.mhl` XML plus the chain file.
- Each destination gets its own independent history.
- `maj verify <dir>` re-reads files against the latest generation and appends a
  new generation recording the outcome.
- Conformance in CI, both directions: our output must pass the Python
  reference `ascmhl verify`; manifests created by the Python tool must verify
  with ours. Python package pinned in CI.

## CLI surface

Phase opens with the mandated main.rs extraction: clap definitions + dispatch
stay in `main.rs`; `cmd_*` handlers move to a `commands/` module before any new
command lands.

- `maj para add <kind> <name>` · `maj para list [--json]` ·
  `maj para rename <node> <name>` · `maj para archive <node> [--dry-run]` —
  archive performs the disk move where materialized and emits the event;
  `--dry-run` shows the move first.
- `maj ingest <source> --dest <root>... --para <node>
  [--template "{date}/{source-label}"] [--dedupe skip|copy|link] [--jobs N]
  [--dry-run] [--resume <run-id>] [--json]` — repeatable `--dest` is the
  multi-destination surface; the template controls layout inside the
  materialized node.
- `maj verify <dir> [--json]` — read-only against the `ascmhl/` history;
  appends a generation with the outcome.

## Error handling

Typed `thiserror` errors carrying operation + path + suggested fix.
Verification failure marks the file failed in both manifest and catalog
(`VerificationRecorded` with `Failed`); the partial stays quarantined; the
summary offers the re-copy command. A destination failing mid-run does not
abort the others — per-destination outcomes are independent, matching the
independent MHL histories.

## Testing

- Cucumber acceptance at the hexagon boundary with real temp-dir filesystems
  (the sync-crate pattern): happy-path ingest, each dedupe mode,
  resume-after-kill, verification failure via a fault-injecting writer adapter
  (flips a byte between write and read-back), non-UTF-8 rejection,
  multi-destination partial failure.
- proptest: routing-template rendering; journal replay (any prefix of journal
  records resumes to a consistent state); CRDT generator extended over the new
  ops.
- ASC MHL conformance CI job (pinned Python `ascmhl`, both directions).
- cargo-mutants on verification + CRDT modules as the phase's closing task;
  surviving mutants triaged onto the watchlist.

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. main.rs → commands module extraction + CatalogStore port catch-up (pure
   refactors, no behavior change).
2. ParaNode + AssetParaSet + verification/manifest ops in core: events,
   projection, proptest, golden wire tests.
3. `maj para` command family.
4. Ingest planner: walk, size-prefilter dedupe, PARA routing template.
5. Copy engine: workers, dual-hash fan-out, read-back verify, quarantine,
   journal + resume.
6. ASC MHL create/verify + conformance CI + `maj verify`.
7. `maj ingest` end-to-end wiring + acceptance scenarios + cargo-mutants run.

Each task runs the mandated loop: fresh implementer subagent → adversarial
spec-compliance reviewer → code-quality reviewer → fix rounds until APPROVED.
