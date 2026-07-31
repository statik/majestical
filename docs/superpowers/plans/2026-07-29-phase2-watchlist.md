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
- **A kill during legacy `runs/` journal migration between the copy and the
  rename leaves an inert `<name>.jsonl.partial` that nothing ever revisits.**
  `migrate_legacy_journals` (`crates/cli/src/state_dir.rs:113-117`) copies
  into `<name>.jsonl.partial`, then renames onto the final path; no code
  scans the state dir's `runs/` for a leftover partial from an earlier
  killed run (PR1 quality review).
- **Symlinked `.jsonl` journals are silently skipped by legacy migration** —
  as a side effect, not a stated policy. The file-type check
  (`crates/cli/src/state_dir.rs:95`, `is_file()`) doesn't follow symlinks, so
  a symlinked journal is excluded from migration; narrower is safer for now,
  but nothing documents it as deliberate (PR1 spec review).
- **`cli_smoke`'s tempdir/catalog/state preamble repeats verbatim 16+
  times** ahead of `maj(&catalog, &state)`/`maj_as(...)` calls in
  `crates/cli/tests/cli_smoke.rs`; a fixture struct would collapse it (PR1).
- **`MAJ_STATE_DIR=""` resolves relative to cwd.** `std::env::var_os`
  returns `Some("")` for an empty-string override, so `state_base()`
  (`crates/cli/src/state_dir.rs:16-19`) returns an empty `PathBuf` instead of
  falling through to `dirs::data_dir()`, and the catalog dir ends up
  relative to the process's cwd (PR1).
- **sync's two read paths diverge in walk and UTF-8 handling.**
  `read_all_reporting` and `read_since_reporting`
  (`crates/sync/src/lib.rs:152-169`, `:283-301`) duplicate the same segment
  walk; `read_all_reporting` uses `fs::read_to_string` and fails the *whole
  segment* on one bad byte, while `read_since_reporting`'s line-by-line path
  degrades one line at a time via `on_bad_line` — contradicting
  `read_all_reporting`'s own doc comment, which claims the same per-line
  tolerance. Separately, `LogError::Io` is hand-built via `map_err` at 13
  call sites; a `LogError::io(path, source)` constructor would remove the
  repetition (PR2 quality).
- **The para-node incomplete-node guard lives in the shared insert helper,
  so no direct test pins it.** `insert_one_para_node`'s
  `let (Some(kind), Some(name)) = (...) else { return Ok(()) }`
  (`crates/catalog-sqlite/src/apply.rs:349-351`) is reached by both
  `rebuild` and `apply_touched`, but nothing directly asserts that a
  rename-before-create node is skipped rather than inserted with placeholder
  values (PR2 spec).
- **`open_synced`'s "no writes" fast path is an unpinned equivalent
  mutant.** The `events.is_empty()` early return
  (`crates/catalog-sqlite/src/apply.rs:55-57`) skips `apply_touched`
  entirely; every test asserting `ApplyMode::Incremental { applied: 0 }`
  would pass identically if this fell through to a no-op `apply_touched`
  call instead (PR2).
- **`apply_touched` does an O(assets)/O(volumes) `.find()` per touched
  entity** (`crates/catalog-sqlite/src/apply.rs:137,143`) where
  `para_node`/`manifests` already have keyed O(1) lookups
  (`crates/core/src/projection.rs:489,517`); worth adding keyed
  `asset()`/`volume()` getters to `Projection` if this shows up as a
  hotspot (PR2 quality).
- **`incremental.rs`'s two snapshot-fallback tests share a ~12-line
  preamble** (`crates/catalog-sqlite/tests/incremental.rs:216-232`,
  `:297-313`); a shared helper is due next time this file is touched (PR2).
- **`media_kind`'s extension lists omit several common formats** — video
  misses `mpg`/`mpeg`/`3gp`/`wmv`/`insv`; image misses
  `jxl`/`pef`/`iiq`/`3fr` (`crates/core/src/media_kind.rs:12-18`).
  `MediaKind::ALL` only enumerates the three kind variants, not extensions,
  so there's no single list to extend today — a one-place extension table
  is the fix (PR3 quality).
- **The LWW HLC-event-builder test helper (`ev()`) is duplicated
  near-verbatim across three crates** — `crates/core/src/projection.rs:554`,
  `crates/catalog-sqlite/tests/incremental.rs:13`,
  `crates/sync/src/lib.rs:371` — worth a shared test-support helper (PR3).
- **A snapshot's duplicate `(volume,path)` instance entries resolve
  last-wins via `BTreeMap::collect()`**
  (`crates/core/src/projection.rs:85-93`) — a known, harmless property
  since only this program ever writes the data, but undocumented; worth a
  one-line comment (PR3).
- **Two definitions of "online" coexist.** `volume_is_online`
  (`crates/cli/src/commands.rs:255-267`, `/Volumes`-label heuristic, used by
  `volumes list`) and `mounted_volumes`
  (`crates/cli/src/volume_identity.rs:44-57`, resolved mount ids, used by
  `search`/`index run`) answer the same question two different ways;
  consolidate onto `mounted_volumes` and remove `volume_is_online` (PR3
  spec review).
- **Filter-only search ordering is asset-hash-string order**
  (`BTreeSet<AssetId>` iteration in `crates/cli/src/search.rs:117`) —
  arbitrary to users; revisit once ranking covers the no-bare-terms case
  (PR3).
- **`search para:<node>` has no CLI/acceptance coverage** — only
  `Filter::Para`'s unit test (`crates/catalog-sqlite/src/query.rs:594`)
  exercises it; no cucumber scenario or `cli_smoke` test runs it through the
  CLI (PR3).
- **`print_search_results_json` builds an unreachable placeholder
  `AssetSummary`** (`crates/cli/src/search.rs:572-579`) for an `unwrap_or`
  fallback that can never trigger, since `asset_summaries` always returns
  exactly one row per requested id (PR3).
- **The truncation notice fires at exactly-`limit` matches even when
  nothing was cut off** (`crates/cli/src/search.rs:644-649`) — a deliberate
  "probably more exist" heuristic (comment at `:641-643`), but the boundary
  case is imprecise (PR3).
- **`saved_searches` is written by both `rebuild` and `apply_touched` but
  read by no production code** — the CLI reads/writes saved searches
  straight off the event-log `Projection` (`crates/cli/src/search.rs:61`,
  `:668`), never through SQL. Deliberate (future GUI/MCP surfaces, and
  `debug_dump` equivalence checking), but in tension with the
  no-speculative-features default (PR4).
- **`search --saved old --save new` (a one-command rename) is impossible**
  — `--save`/`--saved` are mutually exclusive via clap `conflicts_with`
  (`crates/cli/src/main.rs:45,48`); defensible, not a bug (PR4).
- **Lance panics on a corrupted manifest** (a verified Lance 9.0.0
  subtract-overflow during manifest parsing) — `catch_corruption`
  (`crates/index/src/vector_store.rs:295`, doc at `:271-286`) wraps dataset
  open+probe in `catch_unwind` to recover. An upstream issue candidate; the
  repo's own comments don't cite an exact Lance source line, so don't take
  one on faith (PR7).
- **The release profile has no `overflow-checks`** (`Cargo.toml:81-83`) —
  the same manifest corruption that panics in debug just wraps silently in
  release (PR7).
- **A Lance IVF-PQ index is deferred until roughly 100k vectors** — noted in
  the phase 4 plan, not yet needed; the vector store is brute-force scan
  only today, with no index-type code path at all (PR7).
- **Semantic-layer end-to-end coverage needs the real encoder model;
  CLI-level tests pin behavior with fake-size model files and blob-loaded
  vectors instead** (`crates/cli/tests/index_smoke.rs:250-292`) (PR7).
- **The `__eh_frame section too large` linker warning from Lance's debug
  build graph has no clean fix** — the candidate `-no_compact_unwind` flag
  is recorded `DO NOT USE` in `Cargo.toml:46-58`: it makes real panics abort
  instead of unwind, which would break `catch_corruption`'s recovery
  outright (PR7).
- **Vector-column corruption produces silently wrong similarity scores, not
  errors** — `catch_corruption`'s probe reads every column except `vector`
  (doc at `crates/index/src/vector_store.rs:280-286`), so corrupted vector
  bytes pass undetected. Documented as a known, untested gap; a
  checksum-on-read or periodic blob↔Lance verification pass is the future
  fix (PR7).
- **`run_embed_items`'s blob↔Lance diff runs every pass regardless of
  `--kinds`** (`crates/cli/src/index_cmd.rs:455-465`) — deliberate:
  `--kinds` bounds embedding *work*, not this diff's cheap
  self-heal/teammate-sync safety net (PR7).
- **`analysis_frames` buffers the entire decoded analysis stream in
  memory** (`crates/index/src/video.rs:7-11,154`) — roughly 600MB/hour of
  footage; stream frame-by-frame before testing against real (not
  synthetic) footage (PR8).
- **ffmpeg/ffprobe subprocess calls have no timeout**
  (`crates/index/src/video.rs:77,156,218`) — a stalled or disconnecting
  volume stalls the whole `index run` pass (PR8).
- **Hue is computed on a 0-255 scale, not the reference 0-179**
  (`crates/index/src/video.rs:411-424,455-457`) — roughly 1.42x the
  reference weighting, intentional and validated by e2e tests, but worth
  knowing when tuning scene-detection thresholds against outside
  references (PR8).
- **Keyframe manifest-written-last crash recovery is documented but
  untested** — the doc comment (`crates/cli/src/index_cmd.rs:657-661`)
  explains that a crash mid-video leaves no manifest so the next pass
  re-plans, but no test kills the process mid-write to confirm it (PR8).
- **The same synthetic 3-segment ffmpeg clip generator is duplicated under
  different names in two test crates** — `generate_test_clip`
  (`crates/index/tests/video_e2e.rs:13`) and `generate_three_segment_clip`
  (`crates/cli/tests/index_smoke.rs:540`), acknowledged in a comment but
  not shared (PR8).
- **Keyframe manifests only record succeeded embeddings — a keyframe that
  fails permanently drops out rather than retrying**, by design
  (`crates/cli/src/index_cmd.rs:663-675`); the manifest's `detected` count
  alongside `succeeded_timestamps` makes the gap auditable, not hidden
  (PR8).

### cargo-mutants triage (phase 4)

`majestical-index` (spec §7, index/thumbnail/encoder/video code): 451 mutants
tested, 285 caught, 139 missed, 26 unviable, 1 timeout, before triage.
`majestical-catalog-sqlite`: 109 mutants tested, 78 caught, 18 missed, 13
unviable, before triage.

**Structural note.** The gated suites (`encoder_conformance`, `encoder_gated`,
`video_e2e`, and the model/ffmpeg-gated tests in `crates/cli/tests/
index_smoke.rs`) are all `#[ignore]`d, so mutants in `encoder.rs`, `model.rs`'s
fetch internals, `thumbs.rs`'s HEIC decode, and `video.rs`'s ffmpeg subprocess
wrappers show as "missed" against `cargo mutants`'s default-suite run even
where a gated test genuinely catches them. Spot-verified four by hand (apply
the mutant, run the relevant suite, confirm failure, revert):

- `video.rs:90` deleting `!` in `probe`'s status check (treats ffprobe
  failure as success and vice versa) -> `cargo test -p majestical-index
  --test video_e2e -- --ignored` fails
  (`probe_frames_and_scene_detection_agree_on_a_real_clip` panics decoding a
  bogus `VideoInfo`).
- `encoder.rs:90` `embed_image` -> `Ok(vec![])` -> `MAJ_MODEL_DIR=.model-cache
  MAJ_GOLDEN=target/encoder-golden.json cargo test -p majestical-index --test
  encoder_conformance -- --ignored` fails (`cpu_embeddings_match_reference`:
  cosine collapses to ~0).
- `thumbs.rs:18` `decode_image` -> `Ok(Default::default())` -> same
  conformance test fails (cosine collapses to ~0.78, an empty/default image
  instead of the real fixture).
- `model.rs:210` `fetch` -> `Ok(())` (a no-op that skips every file) is
  **not** caught by any `majestical-index` gated test, but **is** caught by
  `majestical-cli`'s own default (non-`#[ignore]`d) suite:
  `model_fetch_reports_already_present_without_network` asserts stdout
  contains "already present" 3 times, which requires the per-file progress
  loop to actually run. This is real coverage that simply lives outside the
  package boundary `cargo mutants -p majestical-index` scans — a distinct
  "covered by a sibling crate's own suite" case, neither a gated-suite gap
  nor a genuine one.

**`majestical-catalog-sqlite` (18 missed, all genuine — closed):**

- **Port-lag, again** (`crates/catalog-sqlite/src/lib.rs:95-114`, 11
  mutants): `CatalogStore`'s `search_names_ranked`/`asset_summaries`/
  `volumes`/`volume_asset_counts` are each a one-line delegation to the
  inherent `SqliteCatalog` method of the same name — the same pattern
  flagged in phase 3 ("`para_nodes()` is inherent-only on `SqliteCatalog`").
  Nothing in the workspace ever called these four through `&dyn
  CatalogStore` (only through the inherent methods directly), so a mutant
  replacing a delegation body with a hardcoded `Ok(vec![...])` survived
  unconditionally. Closed by extending the trait-object test:
  `catalog_store_trait_object_exposes_every_read_query`
  (`crates/catalog-sqlite/src/lib.rs`) seeds a volume and a named asset,
  then calls all four through `&mut dyn CatalogStore` and asserts on the
  real rows.
- **`debug_dump` is content-blind in every existing test** (`apply.rs:424`,
  2 mutants): every other test comparing two dumps only checks *equality*
  between them (`assert_eq!(dump, fresh.debug_dump())`), which a mutant
  returning a hardcoded constant string still passes trivially, since both
  sides call the same mutated function. Closed by
  `debug_dump_reflects_real_row_content_not_a_constant` (`apply.rs`), which
  seeds one asset and one tag and asserts the dump contains their actual
  rows.

All 18 mutants (the delegation return-value variants and the two
`debug_dump` constants) are closed by these two tests — confirmed by hand:
applied each of `search_names_ranked`/`asset_summaries`/`volumes`/
`volume_asset_counts` -> `Ok(vec![])` and `debug_dump` -> `Ok(String::new())`
/ `Ok("xyzzy".into())` one at a time, watched
`catalog_store_trait_object_exposes_every_read_query`/
`debug_dump_reflects_real_row_content_not_a_constant` fail, reverted.

**`majestical-index` genuine gaps — closed with new tests:**

- **Planner counting, `work.rs`** (6 mutants across `plan_thumb`/
  `plan_image_embed`/`plan_keyframes`): the existing planner tests
  (`plans_missing_thumbs_and_counts_statuses`, `existing_blobs_count_done_
  and_raw_images_are_unsupported`) only asserted *counts*, never which asset
  produced them — a mutant that inverts `plan_thumb`'s `kind == Video`
  ffmpeg gate to `!=` swaps which of a video and an image asset lands in
  `needs_ffmpeg` vs. `pending`, but the two counts stay `1`/`1` either way.
  Separately, `plan_image_embed`/`plan_keyframes`'s `done`/`offline`/
  `needs_ffmpeg` counters (as opposed to `pending`, which was already
  covered) had no test touching them at all, so an `+=` -> `*=` mutation
  (which leaves a counter starting at its default `0` stuck at `0` forever)
  went unnoticed. Three new tests: `plan_thumb_ffmpeg_gate_applies_only_to_
  the_video_kind` (asserts *which* asset lands in `items`, not just a
  count), `plan_image_embed_counts_done_and_offline_assets`, and
  `plan_keyframes_counts_done_offline_and_needs_ffmpeg` (seed a done blob
  and an offline/no-ffmpeg asset, assert each counter directly).
- **Video timing/parsing pure functions, `video.rs`** (23 mutants across
  `seconds_to_ms`, `frame_timestamp_ms`, `format_timestamp`, `chunk_frames`,
  `parse_probe_json`, `binary_runs`): none of these had a direct unit test —
  they were only exercised indirectly (or not at all outside the gated
  ffmpeg suites) even though every one is pure and needs no subprocess.
  Six new tests call each directly: `seconds_to_ms_rounds_to_the_nearest_
  millisecond`, `frame_timestamp_ms_scales_index_by_the_analysis_frame_rate`,
  `format_timestamp_splits_whole_seconds_and_millis`,
  `chunk_frames_slices_ts_and_bytes_per_frame_without_overlap` (a two-frame
  buffer with a distinct fill byte per frame, so a slicing-offset bug shows
  up as wrong bytes, not just a wrong length),
  `chunk_frames_rejects_a_length_not_a_multiple_of_the_frame_size`,
  `parse_probe_json_finds_the_video_stream_and_ignores_others` (an audio
  stream listed before the video one, so matching the wrong `codec_type`
  is visible), and `binary_runs_reports_true_only_for_an_actually_runnable_
  binary` (`true` vs. a nonexistent binary name — no PATH manipulation
  needed).
- **Scene-detection internals, `video.rs`** (4 mutants): `detect_scenes`'s
  `frames.len() < 2` guard had no test at exactly 2 frames (the boundary) —
  `exactly_two_frames_is_enough_to_run_detection` pins that 2 real,
  differently-colored frames still produce a cut. `raw_candidate_cuts`'s
  `score < MIN_CONTENT` gate had no test at the exact floor value — `min_
  content_gate_is_exclusive_a_score_at_the_floor_still_cuts` uses two
  1-pixel grayscale frames engineered so `content_score` lands on exactly
  `15.0` (`|145-100| / 3 == 15.0`), surrounded by flat frames giving a
  near-zero neighborhood average (so the ratio check can't mask the gate
  itself), and asserts the cut still fires. `neighborhood_average`'s
  divide-by-`count` guard (`if count == 0 { return 0.0; }`) can be replaced
  wholesale with `return 0.0;` and every *existing* test still passes,
  because no clip had a stretch of above-floor motion with a genuinely flat
  neighborhood — `sustained_above_threshold_motion_does_not_fire_a_cut_on_
  its_own` builds a 40-frame grayscale zigzag with a constant ±50 step (so
  every adjacent pair scores identically ~16.67, just above `MIN_CONTENT`,
  with the neighborhood average settling at the same ~16.67 and the ratio
  near 1.0, safely under `RATIO_THRESHOLD`), and asserts it falls back to
  the 10-sample uniform default rather than firing a cut on every frame (a
  hardcoded-`0.0` neighborhood average turns every above-floor pair's ratio
  into `score / 0.0 == +inf`, clearing the threshold unconditionally).
- **NCHW plane-offset math, `preprocess.rs`** (3 mutants): the existing
  normalization tests only ever checked pixel index `i == 0`, where `plane +
  i` and `plane - i` are the same index, and used uniform-color images,
  where `2 * plane + i` landing at the wrong offset is invisible because
  every pixel writes the same value anyway. `plane_offsets_are_correct_for_
  a_pixel_past_the_first` uses two adjacent pixels with distinct per-channel
  colors and checks pixel 1's R/G/B all land at their real NCHW offsets.

**Equivalent / dead-branch-under-current-constants (not chased):**

- `preprocess.rs:19` `(rgb.width(), rgb.height()) == (EDGE, EDGE)` mutated
  to `!=` — proven equivalent, not just suspected: `resize_rgb`'s
  antialiased-bilinear-via-convolution resize to the *same* dimensions is
  byte-identical to not resizing at all, confirmed empirically for a full
  256×256 gradient (not just a solid color, which can't tell a real resize
  apart from a skip). Pinned permanently by `resize_to_matching_dimensions_
  is_the_identity_even_for_a_non_uniform_image`, so a future resize
  algorithm change that breaks this invariant fails that test directly
  rather than being caught by chasing the mutant.
- `video.rs:173:20` `frame_bytes == 0` mutated to `!= 0` — dead code under
  the current module constants: `frame_bytes` is always
  `ANALYSIS_W * ANALYSIS_H * 3` (43,200), never `0`, so no input to
  `chunk_frames` can ever make this branch's outcome differ between `==`
  and `!=`. Confirmed by hand (the mutant survives even against
  `chunk_frames_rejects_a_length_not_a_multiple_of_the_frame_size`, whose
  actual failure comes entirely from the `is_multiple_of` half of the `||`
  — separately confirmed live via the `||` -> `&&` and `delete !` mutants,
  both of which *are* caught). Only a change to `ANALYSIS_W`/`ANALYSIS_H`
  could ever make this branch reachable.

**Display/diagnostic-only (not chased):**

- `model.rs:215` `file.bytes / 1_000_000` (mutated to `%`/`*`) only feeds
  the `"{name} ({mb} MB): {status}"` progress string passed to the CLI's
  callback — cosmetic, no behavioral effect.

**Gated-coverage (not chased beyond the spot-verifications above), by file:**

- `encoder.rs` (17 mutants: `embed_image`/`embed_text`/`token_ids`/`pooled`)
  — needs the real SigLIP2 ONNX model; covered by `encoder_conformance`/
  `encoder_gated --ignored`.
- `model.rs` (`model_dir` 1, `fetch` 1) — `model_dir` is pure path logic
  gated only by needing careful `MAJ_MODEL_DIR` env-var scoping in an
  in-process test (risk of cross-test races since env vars are
  process-global); `fetch` is covered by the CLI's own default suite (see
  the structural note above), not actually gated.
- `video.rs` ffmpeg subprocess wrappers (`ffmpeg_available`, `probe`,
  `analysis_frames`, `extract_frame` — 12 mutants): need a real ffmpeg
  binary and a real clip; covered by `video_e2e --ignored`
  (`ffmpeg_available`'s own mutants specifically would need PATH
  manipulation to test the true-and-false cases without relying on the
  host's real ffmpeg install, which wasn't attempted this pass).
- `thumbs.rs` `decode_via_sips`/`decode_image`'s HEIC branch (5 mutants) —
  **found, not just assumed**: no HEIC fixture exists anywhere in this
  repository, so `decode_via_sips` (the macOS `sips`-shellout HEIC decoder)
  is untested by *any* suite, gated or not — `decode_image` is only
  exercised via `encoder_conformance`'s PNG fixtures, which never take the
  HEIC branch. Since `sips` is real and present on this (and presumably
  every CI) macOS box, a small real HEIC fixture and a direct test of
  `decode_via_sips` is buildable without any gating at all — left for a
  follow-up rather than this pass, given the phase-4 mutants budget was
  spent on higher-value planner/scene-detection/catalog gaps.

## Done in phase 4

- **`para_nodes()` remaining inherent-only on `SqliteCatalog`** carries
  forward unresolved from phase 3 (see "Phase 3 deferrals" above); not
  touched this phase.
- **The catalog-sqlite submodule split** (`schema.rs`/`apply.rs`/`query.rs`)
  landed as PR4's first commit, closing the module-size concern before the
  incremental-apply and query-language work compounded it.
- **`asset_summaries` re-preparing a statement per id** is resolved:
  `first_instance_name`/`instance_volumes`/`query_asset_tags`/
  `query_asset_para` (`crates/catalog-sqlite/src/query.rs:182,193,208,220`)
  all use `prepare_cached`, landed as part of PR4's refactor.
- **The equivalence proptest generator's `TagRemove`-always-empty gap is
  closed** by a direct shrink test,
  `incremental_apply_removes_a_row_when_a_later_event_untags_it`
  (`crates/catalog-sqlite/tests/incremental.rs:365-371`).

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
