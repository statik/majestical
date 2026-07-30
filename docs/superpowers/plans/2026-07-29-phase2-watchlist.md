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
- **Workspace→cli lint-table drift** — the CLI hand-copies the clippy table (Cargo
  can't merge them); workspace lint changes won't propagate automatically.
- **Case-insensitive search is ASCII-only** until FTS lands (documented in
  catalog-sqlite).
- **Site copy phantom features** — resolved for the CLI transcript (real commands
  as of the photo-hero redesign); revisit remaining aspirational prose when the
  AI phase ships.
- **`meta get` shows poisoned LWW winners unflagged** — a far-future peer clock's
  FieldSet wins the display forever; needs the clock-suspect analog that
  `volumes list` has (phase 2 final review).
- **`volume_is_online` is /Volumes-only, and internal-disk scans all map to the
  "root" volume row** — the auto-detect fallback itself now has real identity
  (`volume_identity::resolve`, used by both `scan` and `ingest`'s destinations);
  the online-heuristic's `/Volumes`-only check is the part still open.
- **PortError double-display / `on_bad_line` file-flavored naming** — accepted
  house-style minors; rename when the seam is next touched.

## Phase 3 deferrals

- **`para_nodes()` is inherent-only on `SqliteCatalog`** — the port-lag
  pattern PR #23 resolved recurred in PR #24; add the trait method when a
  second adapter or trait-generic caller needs it (phase 3 final review).
- **`maj verify` appends generations without emitting `ManifestRecorded`** —
  every verify makes the catalog's newest manifest record stale by one
  generation, so the future chain-tamper-check consumer must treat "disk
  generation > catalog generation" as normal; consider catalog emission from
  `maj verify` when that consumer lands (phase 3 final review).

Recorded during Task 7 (`maj ingest` end to end) and its reviews.

- **`--dedupe link` hard-link mode is not exposed on the CLI.** The planner's
  `DedupeMode::Link` exists and the engine already copies bytes for it exactly
  like `CopyAnyway` (see `engine.rs`'s `partition_plan`), but wiring a real
  hard-link fast path needs a per-destination existing-instance lookup (which
  volume, which path, already there?) that isn't built yet. `maj ingest
  --dedupe` only maps to `skip`/`copy` this phase (Task 7).
- **Per-destination failure attribution in the engine's `Outcome` is missing,
  so `maj ingest` does not emit `Failed` `VerificationRecorded` events.** A
  failed file's `FailedFile.reason` joins every destination's failure string
  into one (`copy_one`/`verify_and_place` in `engine.rs`); there is no
  structured per-destination result to attribute a Failed record to a single
  volume without guessing. Emitting one against every destination would
  incorrectly mark healthy destinations failed too, so `cmd_ingest` emits
  nothing for failures this phase rather than a confidently wrong record
  (Task 7).
- **MHL generations written by `maj ingest` cover only that run's placed
  files, not the destination's full tree.** Built directly from
  `Outcome.placed` per the engine review's guidance (re-hashing the whole
  destination on every ingest is the wrong default) — a reused destination
  root's pre-existing, unrelated content is not recorded until the next `maj
  verify`, which correctly reports it as new. A related, narrower gap: on
  `--resume` where every remaining file was already placed by an earlier,
  crashed invocation of the same run, this invocation's `Outcome.placed` is
  empty (those files are `skipped_resumed`, never re-added to `placed`), so no
  generation is written for them at all — `cmd_ingest` skips the write
  entirely in that case rather than writing a hollow one (see the next item)
  (Task 7).
- **A dedupe-only or fully-resumed ingest run (nothing newly placed) must not
  write a new, empty MHL generation.** `mhl::write_generation` always writes
  exactly the `HashList` it's given rather than merging with the previous
  generation (unlike `verify_dir`'s diff-and-merge), so writing one from an
  empty `Outcome.placed` would make the destination's latest generation
  forget every file a prior run genuinely placed and verified there — the
  next `maj verify` would then report all of them as "new" instead of
  verified. `cmd_ingest`'s `write_ingest_generations` skips the write when
  `Outcome.placed` is empty (found and fixed during Task 7's own manual
  testing, not merely deferred).
- **Journal abort-path automated test still missing**, and **`Journal::append`'s
  `fsync` is a known-untestable mutant** — carried forward from the Task 5
  reviews; `chmod`-based fault injection doesn't work against an already-open
  file descriptor, so exercising the abort path needs a `Journal` trait seam
  that doesn't exist yet.
- **`verify_dir` doesn't cross-check the chain file's `c4` values against the
  generation files' actual bytes** — carried forward from the Task 6 review.
  The catalog's `ManifestRecorded` events are the intended tamper-evidence
  layer for a synced catalog; wiring an explicit check when those events are
  consumed (not yet built) is the natural place for this.
- **`cargo mutants` survivors needing a fault-injection seam that doesn't
  exist yet** (Task 7's `cargo mutants --package majestical-ingest` run, 200
  mutants: 136 caught, 34 missed, 4 timeouts treated as caught — see below —
  26 unviable):
  - **fsync/flush durability is unobservable without a real crash
    simulation** — `FileSink::flush`, `<impl Sink for FileSink>::finish`, and
    `finish_open_sinks` (`engine.rs`) can each be replaced with a no-op and
    every test still passes, for the same reason `Journal::append`'s fsync
    mutant survives: a `std::fs::File` write is visible to an in-process
    read-back the instant the syscall returns, with or without a following
    `flush`/`fsync` — the durability fsync buys is only meaningful across an
    actual crash, which no current test simulates.
  - **`LockDiagnostics::record`/`drain`** (`engine.rs`) can each be replaced
    with a no-op/empty-vec and every test still passes: recording a note
    only happens when a worker thread's mutex lock is recovered from
    poisoning, and no test deliberately panics a worker mid-critical-section
    to trigger that. Needs a fault-injection seam (e.g. an injectable panic
    point), not just a new assertion.
  - **`local_hostname`** (`mhl.rs`) can be replaced with an empty or wrong
    string and every test still passes — it's documented as informational
    only (the oracle never validates it), and the only existing check
    (`write_then_read_round_trip`'s hostname round trip) is self-consistent
    regardless of what the function actually returns. Not chased: the
    function is inherently machine-dependent, so a meaningful assertion
    would just duplicate the same `hostname::get()` call the function
    itself makes.
  - **`c4_hash`'s `|` at `(carry << 8) | u32::from(*byte)`** surviving a
    `|` -> `^` mutation is an equivalent mutant, not a gap: `carry` is always
    `< 58` (bits 8+), and `*byte` is a full byte (bits 0-7), so the two
    operands never share a set bit — `|` and `^` are provably identical for
    every input this loop ever produces. No test can discriminate them
    because no input exists that would.
  - **Four mutants timed out rather than being reported missed** (deleting
    the `Event::Eof` arm in `read_generation`/`read_chain`'s parse loops;
    `!=` -> `==` and `/` -> `%` in `c4_hash`'s digit-reduction loop) — each
    turns a loop that's supposed to terminate into one that spins forever,
    which is a louder failure than a normal assertion mismatch (any CI run
    would hang, not silently pass), so these are treated as effectively
    caught rather than chased.
- **Symlinks are silently skipped by the planner** (a `walkdir` default) —
  carried forward from the Task 4 review; a policy decision (follow, copy as
  link, or reject) is still pending.
- **Junk files (`.DS_Store`, `._*` AppleDouble sidecars) are planned as
  ordinary copies by the planner**, while `mhl::hash_dir` skips dotfiles —
  carried forward from the Task 4 review; ingest will happily copy and
  catalog `.DS_Store`, which is probably not wanted, but no filtering policy
  has been decided.
- **`maj verify` still requires `--catalog`/`--machine-id`** despite using
  neither — carried forward from the Task 6 review; they're required,
  non-`Option` top-level clap args shared by every subcommand.
- **Rust tests run in CI on macOS only**; the raw-byte non-UTF-8 integration
  tests in `plan.rs` stay dormant there (APFS refuses the write) — carried
  forward from the Task 4/5 reviews.
- **`cli_smoke`'s `assert_is_ulid` is laxer than its name** (checks length and
  alphabet, not a valid ULID timestamp field) — carried forward from the
  Task 3 review.
- **Stale `.maj-partial-*` quarantine files are never garbage-collected** —
  carried forward from the Task 5 review; a failed or corrupted destination
  keeps its partial forever unless a human deletes it.
- **`--resume` re-renders `{date}` at resume time, not at the original run's
  time** — carried forward from the Task 7 quality review. `cmd_ingest`
  recomputes `subdir` (including `{date}`) fresh on every invocation; a run
  interrupted before midnight and resumed after it targets a new dated
  subdir, so the resumed invocation re-copies everything there and
  yesterday's already-placed copies are orphaned under the old subdir (no
  data loss, but resume stops being a resume for an overnight interruption).
  The durable fix is to persist the originally rendered subdir in the
  journal — `Record::RunStarted` already carries `source`/`dests` and could
  carry `subdir` too — and have a `--resume` load it from there instead of
  re-rendering.

## Phase 4 deferrals

- **Pre-phase-4 instance rows keep scan-dir-relative paths** — `scan` and
  `ingest` now store paths relative to the volume's actual mount root
  (auto-detected volumes only; an explicit `--volume` override still stores
  scan-dir-relative paths, since the override id has no real mount to
  re-base against). Instance rows written before this change are stale: the
  indexer (`maj index run`/`status`, `crates/index/src/work.rs`) can't
  re-resolve their bytes and reports them offline rather than erroring. A
  rescan of the same volume overwrites the stale row (HLC-LWW on
  `(volume, path)`) with a fresh, correctly-rooted one, so this self-heals
  without any migration step — but until that rescan happens, those rows
  sit in the queue as permanently offline.

## Done in phase 3

- **Non-UTF-8 path handling** — the planner rejects a non-UTF-8-named source
  file per-file (not fatally) with both a reason and a lossy fallback path,
  and the copy engine streams bytes exactly (no text conversion at any point
  in the copy/verify path) (PR #26; task-1 through task-6, `crates/ingest`).
- **Extract `cmd_*` handlers into a commands module** (PR #23).
- **CatalogStore port lags the inherent surface** — resolved for the volume
  queries alongside the commands-module extraction (PR #23). NOTE the lag
  pattern has since recurred: `para_nodes()` (PR #24) is inherent-only on
  `SqliteCatalog` with no trait counterpart; see Phase 3 deferrals.
- **`volume_is_online`/internal-disk root-volume lumping revisited for
  ingest** — every `maj ingest` destination root now gets its own real
  volume identity via `volume_identity::resolve` (diskutil `VolumeUUID` on
  macOS, documented mount-label fallback elsewhere), the same mechanism
  `scan` already used for its source, rather than every destination being
  lumped under one row (Task 7, this branch).
- **`cargo-mutants` run against `majestical-ingest` and `majestical-core`**
  (spec §7; previously not yet run). `majestical-ingest`: 200 mutants, 136
  caught, 34 missed, 4 timeouts, 26 unviable before triage. Six missed
  mutants were genuine test gaps (not display/diagnostic-only) and got
  discriminating tests added in this branch:
  - `engine.rs` `copy_one`'s prehash-mismatch check (`!=` -> `==`): no
    existing test ever populated `PlannedFile.prehash` on a `Copy` decision
    with a value that actually matches the copied bytes, so the whole
    branch was dead code as far as the suite was concerned
    (`a_correctly_predicted_prehash_does_not_block_the_copy`).
  - `engine.rs` `stream_to_sinks`'s size accumulator (`+=` -> `*=`): no test
    asserted `PlacedFile.size` at all (extended
    `copies_verifies_and_places_to_every_destination`).
  - `journal.rs` `Journal::load`'s `NotFound` guard (mutated to always
    match): no test exercised a load failure that *isn't* "file doesn't
    exist yet" (`load_propagates_a_non_missing_error_instead_of_folding_to_empty`).
  - `mhl.rs` `verify_dir`'s `Verified` action tag (deleted): no test ever
    checked an *unchanged* file's action after a `verify_dir` pass — the one
    scenario test only altered/deleted/added files
    (`unchanged_file_is_tagged_verified_in_the_new_generation`).
  - `mhl.rs` `read_generation`'s `Event::Empty` arm (deleted) and its
    `in_creatorinfo`/`in_hash`/`in_directoryhash` context guards (14
    mutants): our own writer never emits a self-closing tag, a `<tool>`/
    `<creationdate>`/`<hostname>` outside `<creatorinfo>`, or a
    `directoryhash`, so no round-trip test ever proved the reader's guards
    actually gate on context rather than just agreeing with the happy path
    (`a_self_closed_hash_element_is_rejected_as_malformed`,
    `reader_context_guards_ignore_lookalikes_outside_their_scope`). The
    first version of the guard test placed its directoryhash decoy *before*
    the real file's `path`/`xxh64`, so the real values simply overwrote the
    decoy afterward regardless of whether the guard worked — a re-run of
    `cargo mutants --file crates/ingest/src/mhl.rs` after adding it still
    showed those particular mutants missed. Reordering the fixture (real
    values first, decoy nested after, including a nested `<hash>` inside
    `<directoryhash>` matching the guard's own shape) killed 12 of the 14; a
    second scoped run still missed the `<xxh64>` start tag's own guard
    (which only sets `cur_action`/`cur_hashdate`, not the hash text captured
    later in `handle_text`) because the decoy's `action`/`hashdate`
    attributes matched the real entry's values exactly, and the test never
    asserted those fields anyway — giving the decoy nothing observable to
    corrupt. Using different decoy attribute values and asserting
    `entry.action`/`entry.hashdate` (confirmed by hand-applying that exact
    mutation locally and watching the test fail, then reverting) closed the
    last two.
  - `template.rs` `render`'s separator check (`||` -> `&&`): the existing
    traversal test's value happened to also trip the segment-emptiness
    check, so it never discriminated the value check on its own
    (`a_backslash_only_value_is_rejected_even_without_a_forward_slash`).

  The remaining missed/timeout mutants are display-only, fsync-durability-only,
  lock-poisoning-only, or a provably equivalent mutation — triaged above under
  "`cargo mutants` survivors needing a fault-injection seam that doesn't exist
  yet" rather than chased.

  `majestical-core`: 95 mutants, 64 caught, 17 missed, 14 unviable, 0
  timeouts before triage. All 17 missed mutants were genuine test gaps (this
  crate has no display/diagnostic-only surface) and got discriminating tests
  added in this branch:
  - `clock.rs`'s `MAX_DRIFT_MS` constant expression (`24 * 60 * 60 * 1000`,
    4 mutants across `*`/`+`/`/`): every existing test uses the constant
    symbolically, so any arithmetic mutation of its definition was
    self-consistent with them regardless of the actual value — pinned with
    a literal `assert_eq!(MAX_DRIFT_MS, 86_400_000)`.
  - `clock.rs`'s `HlcClock::observe` (7 mutants across the clamp-advance
    guard and the equal-wall tiebreak): `observe_far_future_is_clamped`
    only asserted `next.wall_ms <= ceiling`, a bound loose enough to pass
    even if the clamp never advanced `last_wall` at all; no test covered
    equal-or-lower-counter-at-equal-wall (`AlreadyCurrent`) or an
    older-wall-but-higher-counter remote (must still be `AlreadyCurrent`,
    since wall time is the primary key) — three new tests close all seven.
  - `event.rs`'s `ParaKind::dir_name` (2 mutants): never unit-tested
    directly in core (only exercised indirectly through the CLI's
    materialized-directory tests, which `cargo mutants -p majestical-core`
    doesn't run) — added a direct one.
  - `projection.rs`'s `ParaNodeState::archived` (1 mutant, hardcoded
    `true`): every existing test only checks `archived()` *after* an
    archive op; none checked a freshly created node defaults to `false`.
  - `projection.rs`'s `assets`/`para_nodes`/`all_manifests` accessors (3
    mutants, each replaceable with an empty iterator): never called
    anywhere in this crate (only by downstream crates), so each needed a
    direct test.

- **Discretionary polish flagged by the Task 7 quality review, not applied**
  (none affect behavior): `commands.rs`'s `(PathBuf, String, String)`
  destination-volume tuples could be a named struct instead of a positional
  one; `cmd_ingest` could take a single timestamp at the top and derive both
  `hashdate` and `hashdate_ms` from it rather than computing them where
  they're first needed; `acceptance.rs`'s per-scenario setup has some
  duplication a shared step could absorb; `cli_smoke`'s `read_events` hardcodes
  the `0001.jsonl` segment name instead of globbing `events/*/*.jsonl`.

## Done in phase 2

- **EventLog / CatalogStore port traits in core** (PR #14).
- **HLC `observe()` max-drift bound** with clamp warnings and acceptance-level
  assertion (PRs #14, #18).
- **Author identity configuration** — `--author` / `MAJ_AUTHOR` (PR #16).
- **Asset-existence validation on `tag add`** (and `meta set`) (PR #16).
- **`catalog init` is load-bearing** — commands error on uninitialized roots
  (PR #16).
- **`Op::FieldSet` CLI surface** — `maj meta set/get` (PR #16).
