# Phase 2 watch list (now the phase 3 backlog)

Deferred items recorded during Phase 1/2 execution and their final reviews. The
phase 3 planning session should triage the open items against the spec's build
order. Items marked "(Done in phase 2)" are resolved and listed for the record.

## Open

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
- **Workspace→cli lint-table drift** — the CLI hand-copies the clippy table (Cargo
  can't merge them); workspace lint changes won't propagate automatically.
- **Case-insensitive search is ASCII-only** until FTS lands (documented in
  catalog-sqlite).
- **Extract `cmd_*` handlers into a commands module** — main.rs is at ~600 lines
  after phase 2; when the next command lands, leave main.rs as clap definitions +
  dispatch (phase 2 Task 4-5 quality review).
- **Site copy phantom features** — resolved for the CLI transcript (real commands
  as of the photo-hero redesign); revisit remaining aspirational prose when the
  AI phase ships.
- **CatalogStore port lags the inherent surface** — `volumes()` and
  `volume_asset_counts()` exist only on `SqliteCatalog`, so a second adapter or
  trait-generic CLI can't serve `volumes list` (phase 2 final review).
- **`meta get` shows poisoned LWW winners unflagged** — a far-future peer clock's
  FieldSet wins the display forever; needs the clock-suspect analog that
  `volumes list` has (phase 2 final review).
- **`volume_is_online` is /Volumes-only, and internal-disk scans all map to the
  "root" volume row** — documented fallback; revisit when ingest lands (phase 2
  final review).
- **PortError double-display / `on_bad_line` file-flavored naming** — accepted
  house-style minors; rename when the seam is next touched.

## Done in phase 2

- **EventLog / CatalogStore port traits in core** (PR #14).
- **HLC `observe()` max-drift bound** with clamp warnings and acceptance-level
  assertion (PRs #14, #18).
- **Author identity configuration** — `--author` / `MAJ_AUTHOR` (PR #16).
- **Asset-existence validation on `tag add`** (and `meta set`) (PR #16).
- **`catalog init` is load-bearing** — commands error on uninitialized roots
  (PR #16).
- **`Op::FieldSet` CLI surface** — `maj meta set/get` (PR #16).
