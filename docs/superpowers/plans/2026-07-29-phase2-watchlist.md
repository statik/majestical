# Phase 2 watch list

Deferred items recorded during Phase 1 execution and its final review. The Phase 2
planning session should triage these against the spec's build order.

- **EventLog / CatalogStore port traits in core** before adapters multiply — only
  `Clock` exists as a trait today; the CLI couples to the concrete types (spec §1).
- **HLC `observe()` max-drift bound** before ingesting remote logs — one bad peer
  clock must not permanently poison local ordering (also noted in the phase 1 plan).
- **Segment rotation** must keep zero-padded `NNNN` names — lexicographic sort
  constraint documented at `crates/sync/src/lib.rs` next to `segments.sort()`.
- **Incremental SQLite apply** — full projection rebuild per search won't scale
  (documented in catalog-sqlite's module doc).
- **Local-state vs sync-root layout split** — `catalog.db` currently lives beside
  `events/`; spec §5's sync root holds only `events/` and `blobs/`. Decide before
  sync locations become Dropbox folders (db churn).
- **Non-UTF-8 path handling** — scan does lossy conversion (documented in the CLI);
  the ingest phase must preserve exact bytes end to end.
- **cargo-mutants on the CRDT and verification modules** — spec §7 calls for it;
  not yet run.
- **Author identity configuration** — CLI sets `author = machine_id`; the event
  model documents author as a human identity distinct from the machine.
- **Asset-existence validation on `tag add`** — tagging an unknown id currently
  creates a phantom asset.
- **`catalog init` should be load-bearing** — other commands auto-create the tree,
  so a typo'd `MAJ_CATALOG` silently births an empty catalog.
- **Workspace→cli lint-table drift** — the CLI hand-copies the clippy table (Cargo
  can't merge them); workspace lint changes won't propagate automatically.
- **Case-insensitive search is ASCII-only** until FTS lands (documented in
  catalog-sqlite).
- **`Op::FieldSet` has no CLI surface** — implemented and property-tested in core;
  expose ratings/titles when the organize surface lands.
