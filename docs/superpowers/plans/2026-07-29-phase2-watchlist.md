# Phase 2 watch list (now the phase 3 backlog)

Deferred items recorded during Phase 1/2 execution and their final reviews. The
phase 3 planning session should triage the open items against the spec's build
order. Items marked "(Done in phase 2)" are resolved and listed for the record.

## Open

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
unviable, before triage. Every count below is recounted directly against the
two runs' `missed.txt` (grouped by function), not estimated.

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

A fifth item, found rather than triaged for: `thumbs.rs:95` (`+` -> `*` in
`scaled_dimension`) doesn't miss, it **times out** (20s) — the mutated
numerator can blow up `resize_rgb`'s target dimensions into an enormous
allocation/resize rather than a fast wrong answer. Same reasoning as phase
3's `Event::Eof`-deletion timeouts: a hang is a louder CI failure than a
silently-wrong assertion, so this is treated as effectively caught, not
chased. (Not counted in the 139 missed — cargo-mutants reports it
separately as 1 timeout.)

**`majestical-catalog-sqlite` (18 missed, all genuine — closed):**

- **Port-lag, again** (`crates/catalog-sqlite/src/lib.rs:95-114`, 16
  mutants: `search_names_ranked` 1, `asset_summaries` 1, `volumes` 9,
  `volume_asset_counts` 5 — the 9 and 5 are cargo-mutants trying several
  hardcoded dummy tuples per method, not 9/5 independent bugs):
  `CatalogStore`'s `search_names_ranked`/`asset_summaries`/`volumes`/
  `volume_asset_counts` are each a one-line delegation to the inherent
  `SqliteCatalog` method of the same name — the same pattern flagged in
  phase 3 ("`para_nodes()` is inherent-only on `SqliteCatalog`"). Nothing in
  the workspace ever called these four through `&dyn CatalogStore` (only
  through the inherent methods directly), so a mutant replacing a
  delegation body with a hardcoded `Ok(vec![...])` survived unconditionally
  regardless of which dummy values cargo-mutants tried. Closed by extending
  the trait-object test: `catalog_store_trait_object_exposes_every_read_
  query` (`crates/catalog-sqlite/src/lib.rs`) seeds a volume and a named
  asset, then calls all four through `&mut dyn CatalogStore` and asserts on
  the real rows — since the real seeded values (`"v1"`/`"Card A"`) never
  match any of cargo-mutants' dummy candidates (`""`/`"xyzzy"`), the same
  one test discriminates every dummy-value variant it tried.
- **`debug_dump` is content-blind in every existing test** (`apply.rs:424`,
  2 mutants): every other test comparing two dumps only checks *equality*
  between them (`assert_eq!(dump, fresh.debug_dump())`), which a mutant
  returning a hardcoded constant string still passes trivially, since both
  sides call the same mutated function. Closed by
  `debug_dump_reflects_real_row_content_not_a_constant` (`apply.rs`), which
  seeds one asset and one tag and asserts the dump contains their actual
  rows.

16 + 2 = 18: every missed mutant in this crate, closed by these two tests —
confirmed by hand: applied each of `search_names_ranked`/`asset_summaries`/
`volumes`/`volume_asset_counts` -> `Ok(vec![])` and `debug_dump` ->
`Ok(String::new())` / `Ok("xyzzy".into())` one at a time, watched
`catalog_store_trait_object_exposes_every_read_query`/
`debug_dump_reflects_real_row_content_not_a_constant` fail, reverted.

**`majestical-index` genuine gaps — closed with new tests:**

- **Planner counting, `work.rs`** (12 mutants: `plan_thumb` 1,
  `plan_image_embed` 4, `plan_keyframes` 7 — each `+=` counter has both a
  `-=` and a `*=` variant, which is why the counts are double what a first
  read of the line numbers suggests): the existing planner tests
  (`plans_missing_thumbs_and_counts_statuses`, `existing_blobs_count_done_
  and_raw_images_are_unsupported`) only asserted *counts*, never which asset
  produced them — a mutant that inverts `plan_thumb`'s `kind == Video`
  ffmpeg gate to `!=` swaps which of a video and an image asset lands in
  `needs_ffmpeg` vs. `pending`, but the two counts stay `1`/`1` either way.
  Separately, `plan_image_embed`/`plan_keyframes`'s `done`/`offline`/
  `needs_ffmpeg` counters (as opposed to `pending`, which was already
  covered) had no test touching them at all, so an `+=` -> `*=`/`-=`
  mutation (which leaves a counter starting at its default `0` stuck at `0`,
  or wraps to `u64::MAX`) went unnoticed. Three new tests:
  `plan_thumb_ffmpeg_gate_applies_only_to_the_video_kind` (asserts *which*
  asset lands in `items`, not just a count),
  `plan_image_embed_counts_done_and_offline_assets`, and
  `plan_keyframes_counts_done_offline_and_needs_ffmpeg` (seed a done blob
  and an offline/no-ffmpeg asset, assert each counter directly) — all 12
  mutants confirmed closed by hand, both the `-=` and `*=` variant of each.
- **Video timing/parsing pure functions, `video.rs`** (33 mutants, all
  closed: `seconds_to_ms` 4, `frame_timestamp_ms` 6, `format_timestamp` 6,
  `chunk_frames` 14, `parse_probe_json` 1, `binary_runs` 2): none of these
  had a direct unit test — they were only exercised indirectly (or not at
  all outside the gated ffmpeg suites) even though every one is pure and
  needs no subprocess. Seven new tests call each directly:
  `seconds_to_ms_rounds_to_the_nearest_millisecond`,
  `frame_timestamp_ms_scales_index_by_the_analysis_frame_rate`,
  `format_timestamp_splits_whole_seconds_and_millis`,
  `chunk_frames_slices_ts_and_bytes_per_frame_without_overlap` (a two-frame
  buffer with a distinct fill byte per frame, so a slicing-offset bug shows
  up as wrong bytes, not just a wrong length),
  `chunk_frames_rejects_a_length_not_a_multiple_of_the_frame_size`,
  `parse_probe_json_finds_the_video_stream_and_ignores_others` (an audio
  stream listed before the video one, so matching the wrong `codec_type`
  is visible), and `binary_runs_reports_true_only_for_an_actually_runnable_
  binary` (`true` vs. a nonexistent binary name — no PATH manipulation
  needed). All 33 mutants across these six functions confirmed closed —
  32 by hand, one mutation at a time; the 33rd (`chunk_frames`'s
  `173:20` `frame_bytes == 0` -> `!= 0`) was misjudged "equivalent" in an
  earlier pass of this triage (checked by hand against only one of the two
  `chunk_frames` tests, the one that expects an error either way, so it
  couldn't discriminate); the scoped rerun below caught it correctly
  against the full suite — with the OR's first clause unconditionally
  `true`, `chunk_frames` errors on *every* input, including the valid
  two-frame buffer `chunk_frames_slices_ts_and_bytes_per_frame_without_
  overlap` expects to succeed, so that test's own `.expect("chunk")` fails.
  Re-confirmed directly afterward: `cargo test -p majestical-index --lib`
  with the mutation hand-applied fails exactly that one test.
- **Scene-detection boundary conditions, `video.rs`** (5 mutants,
  intentionally targeted: `detect_scenes` 1, `raw_candidate_cuts` 2,
  `neighborhood_average`'s two whole-function constant mutants —
  `-> 0.0`/`-> 1.0`): `detect_scenes`'s `frames.len() < 2` guard had no test
  at exactly 2 frames (the boundary) —
  `exactly_two_frames_is_enough_to_run_detection` pins that 2 real,
  differently-colored frames still produce a cut. `raw_candidate_cuts`'s
  `score < MIN_CONTENT` gate had no test at the exact floor value — `min_
  content_gate_is_exclusive_a_score_at_the_floor_still_cuts` uses two
  1-pixel grayscale frames engineered so `content_score` lands on exactly
  `15.0` (`|145-100| / 3 == 15.0`), surrounded by flat frames giving a
  near-zero neighborhood average (so the ratio check can't mask the gate
  itself), and asserts the cut still fires. `neighborhood_average`'s
  divide-by-`count` guard (`if count == 0 { return 0.0; }`) can be replaced
  wholesale with `return 0.0;` (or `1.0`) and every *existing* test still
  passed before this branch, because no clip had a stretch of above-floor
  motion with a genuinely flat neighborhood —
  `sustained_above_threshold_motion_does_not_fire_a_cut_on_its_own` builds
  a 40-frame grayscale zigzag with a constant ±50 step (so every adjacent
  pair scores identically ~16.67, just above `MIN_CONTENT`, with the
  neighborhood average settling at the same ~16.67 and the ratio near 1.0,
  safely under `RATIO_THRESHOLD`), and asserts it falls back to the
  10-sample uniform default rather than firing a cut on every frame (a
  hardcoded `0.0`/`1.0` neighborhood average turns every above-floor pair's
  ratio into `score / 0.0 == +inf` or a fixed small ratio that still
  clears the threshold, firing on every frame instead).

  This last test (plus `exactly_two_frames_is_enough_to_run_detection` and
  `min_content_gate_is_exclusive_a_score_at_the_floor_still_cuts`, both new
  this round) turned out to **incidentally** close far more than the 5
  mutants they targeted — the scoped rerun below shows 6 more of
  `neighborhood_average`'s 9 mutants (`293:17` ×2, `297:24`, `297:15` ×2,
  `298:17` ×2, `299:19` ×2, `302:14`, `310:9` ×2 — several of these weren't
  even in the original missed count, meaning some were already caught by
  pre-existing tests before this triage started) and 5 of `content_score`'s
  7 mutants now pass, without either function ever being tested directly.
  Not claimed as deliberate coverage — genuinely incidental, and left
  exactly that way rather than retrofitted with an explanation.
- **NCHW plane-offset math, `preprocess.rs`** (3 of `preprocess_rgb`'s 4
  mutants; the 4th is equivalent, see below): the existing normalization
  tests only ever checked pixel index `i == 0`, where `plane + i` and
  `plane - i` are the same index, and used uniform-color images, where
  `2 * plane + i` landing at the wrong offset is invisible because every
  pixel writes the same value anyway. `plane_offsets_are_correct_for_a_
  pixel_past_the_first` uses two adjacent pixels with distinct per-channel
  colors and checks pixel 1's R/G/B all land at their real NCHW offsets.
- **Color-space conversion, `video.rs`** (`rgb_to_hsv_u8`, 22 mutants — the
  single largest survivor cluster in the crate; 11 closed, 11 remain open):
  never unit-tested directly; only exercised indirectly through full
  `detect_scenes` runs on synthetic solid-color frames, which never pinned
  the function's own branch structure or arithmetic against known values.
  `rgb_to_hsv_u8_matches_known_color_conversions` checks six colors (pure
  red/green/blue — one per `hue_deg` branch — plus white/black/mid-grey for
  the zero-delta and zero-saturation guards) against hand-derived expected
  `(hue, sat, val)` triples on the function's actual 0-255 packed scale.
  TDD'd against 4 mutants by hand before the scoped rerun: `430:21`
  (`-` -> `+` in `delta = max - min`), `434:32` (`<=` -> `>` in the
  zero-delta guard), and `441:22` (`<=` -> `>` in the zero-saturation
  guard) all fail as expected; `435:27` (`/` -> `%` in the red branch's
  `(gf - bf) / delta`) survives even this test, because pure red has
  `gf == bf == 0` on both sides of that branch, so `0 / delta` and `0 %
  delta` agree — genuinely not discriminated by a primary-color fixture,
  not a test bug. Rescoped `cargo mutants --package majestical-index --file
  crates/index/src/video.rs` after adding this test (221 mutants, 25m: 185
  caught, 30 missed, 6 unviable): 11 of the 22 `rgb_to_hsv_u8` mutants now
  pass — every branch-selection and zero-guard mutant the six colors
  exercise, plus a few arithmetic variants beyond the 4 hand-checked above.
  The remaining 11 (`435:27` both variants, `435:21` both, `437:27` both,
  `437:21` one, `439:27` both, `439:21` one, `444:15` one) are all in the
  per-channel `hue_deg`/`sat` arithmetic where the six fixture colors
  (three pure primaries plus achromatic white/black/grey) don't force a
  distinguishing value — e.g. `435:27` above. Closing them needs
  non-primary, non-achromatic colors (an off-hue color where every channel
  differs); not added this pass, listed under "aggregate-covered
  arithmetic" below.

**Aggregate-covered arithmetic — open, not chased (22 mutants total,
recounted against the scoped rerun's actual `missed.txt`, not estimated):**

Several of these functions' end-to-end behavior is pinned by the
scene-detection and video-timing tests above, but no test asserts anything
narrow enough to discriminate every individual arithmetic operator inside
them — some, as noted above, got caught anyway as an incidental side
effect; these did not:

- `neighborhood_average` (1 of its 9 mutants remains open — 8 close above,
  see the incidental-closure note): `293:39` (`+` -> `*` in the window's
  `(i + NEIGHBORHOOD_WINDOW + 1)` upper bound — multiplying by 1 is a
  silent identity, shrinking the window's right edge by one frame without
  producing an out-of-range or obviously-wrong result).
- `content_score` (2 of its 7 mutants remain open — 5 close above,
  incidentally): `391:20` (`*` -> `/` in the per-pixel index `px * 3`) and
  `408:11` (`/` -> `*` in the final `total / denom`).
- `enforce_min_scene_length` (4 mutants, unaffected by anything added this
  round): the short-scene boundary checks (`334:47`, `334:43`, `335:57`,
  `335:44`).
- `thin_to_cap` (3 mutants, unaffected): the even-spacing index math
  (`380:38`, `380:30` ×2).
- `uniform_fallback` (1 mutant, unaffected): the sample-spacing
  multiplication (`368:35`).
- `rgb_to_hsv_u8`'s remaining 11 mutants (see above): `435:27` ×2, `435:21`
  ×2, `437:27` ×2, `437:21`, `439:27` ×2, `439:21`, `444:15`.

**Equivalent (not chased, 1 mutant — down from 2 claimed in an earlier
pass of this triage; see the `chunk_frames` correction above):**

- `preprocess.rs:19` `(rgb.width(), rgb.height()) == (EDGE, EDGE)` mutated
  to `!=` — proven equivalent, not just suspected: `resize_rgb`'s
  antialiased-bilinear-via-convolution resize to the *same* dimensions is
  byte-identical to not resizing at all, confirmed empirically for a full
  256×256 gradient (not just a solid color, which can't tell a real resize
  apart from a skip), and re-confirmed against the crate's *full* test
  suite (`cargo test -p majestical-index --lib`, all 59 tests), not just
  the `preprocess` module in isolation — the same mistake that produced the
  `chunk_frames` misjudgment above. Pinned permanently by
  `resize_to_matching_dimensions_is_the_identity_even_for_a_non_uniform_
  image`, so a future resize algorithm change that breaks this invariant
  fails that test directly rather than being caught by chasing the mutant.

**Display/diagnostic-only (not chased, 2 mutants):**

- `model.rs:215` `file.bytes / 1_000_000` (mutated to `%`/`*`) only feeds
  the `"{name} ({mb} MB): {status}"` progress string passed to the CLI's
  callback — cosmetic, no behavioral effect. (The rest of `fetch`'s 3
  missed mutants is the `Ok(())` no-op covered by the CLI's own suite, see
  the structural note above.)

**Not chased, deferred (1 mutant):**

- `model.rs::model_dir` (1 mutant) — pure path logic, but testing it needs
  careful `MAJ_MODEL_DIR` env-var scoping in an in-process test (risk of
  cross-test races, since env vars are process-global and this crate's
  tests run multi-threaded); not attempted this pass.

**Gated-coverage (spot-verified above, not chased further), by file
(37 mutants):**

- `encoder.rs` (25 mutants: `embed_image` 4, `embed_text` 4, `token_ids` 5,
  `pooled` 12) — needs the real SigLIP2 ONNX model; covered by
  `encoder_conformance`/`encoder_gated --ignored`.
- `video.rs` ffmpeg subprocess wrappers (8 mutants: `probe` 1,
  `analysis_frames` 2, `extract_frame` 2, `ffmpeg_available` 3) — need a
  real ffmpeg binary and a real clip; covered by `video_e2e --ignored`.
  (`binary_runs`'s own 2 mutants are *not* in this bucket — they're pure
  and closed above; `ffmpeg_available`'s 3 remain gated because testing its
  true/false cases without relying on the host's real ffmpeg install would
  need PATH manipulation, not attempted this pass.)
- `thumbs.rs` `decode_image`/`decode_via_sips` (4 mutants) — **found, not
  just assumed**: no HEIC fixture exists anywhere in this repository, so
  `decode_via_sips` (the macOS `sips`-shellout HEIC decoder) is untested by
  *any* suite, gated or not — `decode_image` is only exercised via
  `encoder_conformance`'s PNG fixtures, which never take the HEIC branch.
  Since `sips` is real and present on this (and presumably every CI) macOS
  box, a small real HEIC fixture and a direct test of `decode_via_sips` is
  buildable without any gating at all — left for a follow-up rather than
  this pass, given the phase-4 mutants budget was spent on higher-value
  planner/scene-detection/catalog gaps.

Reconciliation, by file (matches missed.txt's totals exactly, and
`video.rs`'s split is the *measured* scoped-rerun result, not an estimate):
`majestical-catalog-sqlite`: 18, all closed (16 port-lag + 2 `debug_dump`).
`majestical-index`: 139 = `work.rs` 12 (closed) + `video.rs` 90 (60 closed:
33 pure fns + 5 scene-detection, both intentional, + 11 `rgb_to_hsv_u8`,
intentional, + 11 incidental — 6 more of `neighborhood_average`'s 9 and 5
of `content_score`'s 7, neither ever tested directly; 22 open:
aggregate-covered arithmetic including `rgb_to_hsv_u8`'s remaining 11; 8
gated: `probe`/`analysis_frames`/`extract_frame`/`ffmpeg_available`) +
`preprocess.rs` 4 (3 closed + 1 equivalent) + `model.rs` 4 (2 display-only +
1 `fetch`'s `Ok(())` no-op, covered elsewhere by the CLI's own suite + 1
`model_dir`, deferred) + `thumbs.rs` 4 (gated, no fixture) + `encoder.rs` 25
(gated). Plus the 1 `thumbs.rs::scaled_dimension` timeout, accounted for
separately (not in the 139).

## Phase 5 deferrals

Recorded during the phase 5 PR chain (#43-#53) and its reviews. Items marked
"(phase 5 spec)" come from that spec's own deferred list; the rest were found
during execution.

- **Hosted multimodal embeddings (the OpenRouter quality tier) are not
  built.** OpenRouter participates only as a describer backend
  (`crates/describe/src/client.rs`'s `BackendKind::OpenRouter`); a hosted
  embedding tier needs a parallel vector space plus cross-space fusion.
  `Derivation`'s model tags (`crates/index/src/blob.rs:18-86`) already key
  every blob by model, so nothing forecloses it (phase 5 spec).
- **Tag-suggestion rejections are per-machine and never synced.**
  `tag-rejections.jsonl` lives in the state dir, deliberately outside both
  the event log and the disposable SQLite so projection rebuilds can't
  resurrect a rejected suggestion (`crates/cli/src/tags_cmd.rs`). A teammate
  rejecting a suggestion does not stop it reappearing on your machine
  (phase 5 spec).
- **API keys live in `describer.toml` at mode 0600, not the macOS
  Keychain** — `crates/describe/src/config.rs` writes the file with
  restricted permissions and `MAJ_OPENROUTER_KEY` lets the file stay keyless,
  but the key is still plaintext on disk when set (phase 5 spec).
- **PSD/Sketch/AI native parsing is not implemented.** `.ai` files that are
  PDF-compatible open through `crates/index/src/pdf.rs` for free; genuinely
  layered formats classify as `MediaKind::Other` and derive nothing
  (phase 5 spec).
- **Caption, OCR, and PDF text are FTS-only — only transcripts get
  vectors.** `text_fts` (`crates/catalog-sqlite/src/schema.rs:81`) indexes
  all four sources; the 384-d Lance text table
  (`crates/index/src/vector_store.rs:543`) holds transcript chunks alone, so
  a paraphrase query can only reach spoken words, never on-screen or
  captioned text (phase 5 spec).
- **No diarization, translation, or language forcing** in transcription —
  `crates/index/src/transcribe.rs` runs whisper with language auto-detect
  and no speaker attribution (phase 5 spec).
- **`maj describer test` exits 0 even for a provably unusable config** —
  a missing model or `capabilities.vision: false` prints prose but returns
  `Ok(())` (`crates/cli/src/describer_cmd.rs:61`), so a script can't gate on
  it. Nonzero exit for provably-unusable (as opposed to merely unreachable)
  configuration is the fix (PR1 quality review).
- **No dialect test covers a non-2xx backend response.** The fixtures replay
  200s; a 401 or 500 from `crates/describe/src/client.rs` has no test
  asserting the error names the URL it called (PR1 spec review).
- **The caption `PortError` for malformed tag output carries a snippet and
  context but not the backend URL** — enough to see what came back, not
  enough to see which of several configured machines sent it
  (`crates/describe/src/client.rs`, PR1 spec review).
- **Cosmetic describer-CLI nits, not applied**: `cmd_set` clones three
  fields where by-value would do, and `std::path::PathBuf` is written
  fully-qualified rather than imported (`crates/cli/src/describer_cmd.rs`) —
  fold in whenever that file is next touched (PR1 quality review).
- **`text_encoder_conformance.rs` has neither a cosine upper-bound check nor
  measured-floor reporting**, both of which its SigLIP sibling
  (`crates/index/tests/encoder_conformance.rs`) has — so a suspiciously
  perfect 1.0 (an oracle accidentally comparing a vector to itself) would
  pass, and a slow drift toward the floor is invisible until it crosses
  (PR2 quality review, discretionary).
- **`fetch_spec` (the function) and `FetchSpec` (the struct) collide by
  name** in `crates/index/src/model.rs:175`; and **`TextEncoder` has no
  `Debug` derive** (`crates/index/src/text_encoder.rs:18`), so it can't be
  embedded in any struct that wants one (PR2 quality review,
  discretionary).
- **The pre-existing ffmpeg/ffprobe calls still have no timeout.**
  `run_with_timeout` (`crates/index/src/video.rs:259`) landed this phase but
  is wired only into `extract_audio_pcm` (`:325`), the new audio path;
  `probe` (`:88`), `analysis_frames` (`:161`), and `extract_frame` (`:223`)
  still call bare `.output()`. Narrows, but does not close, the phase 4
  item (PR3).
- **`run_with_timeout`'s `try_wait` error path leaks the child and the
  reader thread.** The `?` at `crates/index/src/video.rs:280` returns
  without `kill()`ing the child or joining the reader — the timeout path
  right below it does both. Also still missing: the one-line comment
  documenting `extract_audio_pcm`'s `chunks_exact(4)` alignment invariant
  (`:337`) (PR3 quality review).
- **`whisper_gated`'s `say` fixture produced a silent aiff once on CI** and
  the test was hardened rather than diagnosed: it now prefers `MAJ_AUDIO`
  when the recipe set it, and otherwise retries `say` once after an
  `is_silent` check (`crates/index/tests/whisper_gated.rs:20-66`). Why
  `say` occasionally emits silence on a CI runner is still unexplained; the
  retry masks it (PR5 CI run, hardened in PR6).
- **The locked-PDF branch is written but untested.** `open_document`
  rejects password-protected PDFs via `isLocked`
  (`crates/index/src/pdf.rs:42-63`); no fixture exercises it because `qpdf`
  (the tool that would generate an encrypted fixture) isn't installed on
  this machine. Add the fixture to `crates/index/tests/pdf_golden.rs` when
  it is (PR6).
- **`maj tags suggestions` takes no query argument** — the phase 5 spec
  wrote `maj tags suggestions [query]`, and the optional filter was not
  built (`crates/cli/src/main.rs:327`, `crates/cli/src/tags_cmd.rs`).
  Suggestions are listed whole; filtering is the caller's job (PR8 spec
  review, deferred explicitly in PR9).
- **`maj tags reject` does no asset validation** — an id that names no
  asset appends to `tag-rejections.jsonl` all the same
  (`crates/cli/src/tags_cmd.rs`). Matches the plan; recorded as an
  observation, not a defect, since the file is a per-machine suppression
  list rather than catalog state (PR8 spec review).
- **A mid-run describer outage records one skip row per remaining item, not
  one aggregate count.** `run_caption_items`
  (`crates/cli/src/index_cmd.rs:1521-1524`) attributes
  `DESCRIBER_SKIPPED_REASON` to every item after the first failure, where
  the plan said report the skipped count — defensible (each row is
  independently re-plannable and the count is derivable), but it makes a
  large outage noisy in the run report (PR8, as-built).
- **`PortError` conflates a per-item malformed response with a whole-backend
  outage.** The caption loop can only see "this call failed", so one asset
  whose response won't parse aborts the remaining caption work exactly like
  a dead backend does (`crates/cli/src/index_cmd.rs:1483-1524`).
  Distinguishing them is a port-level change (a retryable-vs-fatal
  discriminant on `DescribeError`); left until it actually bites (PR8).
- **`search_text_ranked` treats `Some(<empty set>)` of sources as "no
  sources at all"** (`crates/catalog-sqlite/src/query.rs:113-130`) —
  documented at the call site rather than made unrepresentable in the type
  (PR4).
- **The image `VectorStore`'s empty-add short-circuit is unpinned.**
  `VectorStore::add`'s `rows.is_empty()` guard
  (`crates/index/src/vector_store.rs:131-132`) is only checked by row count
  (`:826`), which a deleted guard still passes — Lance versions every write
  that reaches the table, including an empty one. The text store's
  equivalent test pins `table.version()` across the call
  (`:1150-1157`); the image store's does not. A pre-existing gap, found
  while testing the text store (PR4).
- **`eligible_assets` walks the whole projection once per text source.**
  `crates/cli/src/search.rs:494` is called four times (once per
  `TEXT_SOURCE_INFO` entry, `:465-490`) and `eligible_asset_count`
  (`:439`) walks it a fifth time, each classifying the same first-instance
  basename. One pass building all five populations is the fix if coverage
  notices ever show up in a search profile (PR9).
- **`--json` emits the `-1` locator sentinel verbatim.**
  `crates/cli/src/search.rs:1087` writes `locator` straight through for
  every text hit, while the human renderer (`:1013-1020`) correctly prints
  nothing for `-1` (captions and still-image OCR, which have no timestamp
  or page). A JSON consumer therefore sees a magic number where the field
  should be absent (PR9).
- **`search.rs`'s test module is still named `semantic_tests`**
  (`crates/cli/src/search.rs:1218`) though it now covers FTS text hits,
  N-way fusion, locator rendering, and coverage notices as well as the
  semantic path (PR9).
- **The phase 5 plan's Task 14 verification command lacks `--lib`** — the
  acceptance harness rejects a test-name filter without it; fix if the plan
  document is ever edited again (PR6 quality review).

### cargo-mutants triage (phase 5)

`majestical-describe` (spec's local-LLM describer client/config, all new in
phase 5): 47 mutants tested, 32 caught, 10 unviable, 5 missed, before triage.
`majestical-index`, scoped to `chunk.rs` only (see "not run" below): 7
mutants tested, 6 caught, 1 unviable, 0 missed. Every count below is
recounted directly against each run's own output, not estimated.

**Invocations used:**

```
cargo mutants -p majestical-describe --timeout 120 -j 4
cargo mutants -p majestical-index -f crates/index/src/chunk.rs --timeout 180 -j 4
```

**Not run (model/framework-bound; triage deferred):** `crates/index/src/
ocr.rs`, `crates/index/src/pdf.rs`, `crates/index/src/text_encoder.rs`,
`crates/index/src/transcribe.rs`. `majestical-index`'s build graph pulls in
`lance`/`ort`/`whisper-rs`, so every per-mutant incremental rebuild against
this crate is expensive: the `chunk.rs`-only scoped run alone took a 278s
baseline build plus a further ~12 minutes wall-clock for just 7 mutants (pure
logic, no model dependency). A `-p majestical-index` run additionally
scoped to these four files (884 lines together, each doing real work against
an external model or framework — Whisper transcription, OCR, PDF text
extraction, the MiniLM ONNX text encoder) would run for hours against this
task's foreground/interactive budget, well past what two prior attempts (a
full-crate run and a five-file run) already showed timing out or getting
killed mid-build. This is the same structural shape as the phase-4 triage's
note that gated/`#[ignore]`d conformance suites (encoder, whisper, ffmpeg)
show mutants in their own files as "missed" against cargo-mutants's default
run even where the gated suite genuinely catches them — except here the cost
is in the rebuild itself, not just suite selection, so even a single-file
scoped run per remaining file needs a dedicated longer-timeout session
(ideally a background/CI job) rather than this task's foreground window.

**`majestical-describe` genuine gaps — closed with new tests:**

- **`tags_prompt` (`crates/describe/src/client.rs:16-28`, 2 mutants: replace
  body with `String::new()` / `"xyzzy".into()`):** every existing test that
  reaches this function goes through `suggest_tags` and an httpmock server
  that matches requests only on method/path — never the prompt text in the
  request body (unlike the caption path's `ollama_caption_sends_base64_
  data_url_no_auth`, which does inspect the body via `is_true`) — so
  collapsing the whole prompt to an empty or constant string went unnoticed,
  even though it would silently drop the vocab list from every real
  request. Closed by two new tests that call the function directly:
  `tags_prompt_lists_the_vocab_and_the_json_reply_shape` (asserts both vocab
  tags and the JSON reply shape appear in the text) and `tags_prompt_names_
  empty_vocab_explicitly` (asserts the `"(none yet)"` branch).
- **`BackendKind::as_str` (`crates/describe/src/config.rs:26-33`, 2 mutants:
  `""` / `"xyzzy"`):** its only caller is `describer_cmd.rs`'s `println!` for
  `maj describer status`, which no test exercises. The sibling
  `default_base_url` already had `default_base_urls_per_backend` pinning
  every variant; `as_str` had no equivalent. Closed by `as_str_per_backend`
  (`config.rs`), mirroring that existing test one-for-one.
- **`DescriberConfig::load`'s `NotFound` guard (`crates/describe/src/
  config.rs:76`, 1 mutant: widen the guard to unconditional `true`):** the
  only existing test on the `Err`-turned-`Ok(None)` branch
  (`load_missing_file_is_none`) triggers a genuine `NotFound`, so a mutant
  that treats *every* io error as "file absent" still returns `Ok(None)`
  there — indistinguishable from correct behavior. Closed by `load_non_
  not_found_io_error_is_a_read_error` (`config.rs`), which points `load` at
  a directory (a different io error kind on every platform this runs on)
  and asserts the result surfaces as `ConfigError::Read`, not swallowed into
  `None`.

5 mutants (2 + 2 + 1) closed by four new tests — confirmed by rerunning
`cargo mutants -p majestical-describe --timeout 120 -j 4` after the fix:
47 mutants tested, 37 caught, 10 unviable, **0 missed**.

**`majestical-index` (`chunk.rs`) — no gaps found:** 7 mutants tested, 6
caught, 1 unviable, 0 missed. `chunk.rs`'s existing property test
(`crates/index/src/chunk.rs`'s `proptest!` block, lines 169+, fuzzing
segment durations and word counts) together with its direct unit tests
already discriminates every mutant cargo-mutants tried in this scope; no
action needed.

## Phase 6 deferrals

Recorded during the phase 6 PR chain (#55-#62) and its reviews. Items marked
"(phase 6 spec)" come from that spec's own Deferred list; the rest were found
during execution and fed into this closing task via a scratchpad note.

- **`SyncTransport` port is not built** — arrives with the first
  non-filesystem transport (self-hosted server / iOS app integration point)
  (phase 6 spec).
- **Divergence detection within one machine's segments is not built** —
  equal-length, different-bytes segments (a reused machine-id after a
  reinstall) go undetected (phase 6 spec).
- **The share-sheet Shortcut that generates `contribution.json` on-device is
  not built** — `maj inbox process` validates and ingests a manifest, but
  nothing produces one yet outside hand-authoring (phase 6 spec).
- **A resident inbox watcher (FSEvents) is not built** — `maj inbox process`
  is a one-shot pass; a GUI-phase concern (phase 6 spec).
- **Auto-import of pulled blobs into Lance/`text_fts` as part of `maj sync
  pull` is deliberately not built** — left to `maj index run` for
  composability; revisit if the two-step trips real users (phase 6 spec).
- **Permanently truncated segment tail is invisible to both readers** —
  deferred indefinitely, no diagnostic. The doc states this honestly; a `maj
  doctor`-style residue check (compare cursor offsets to segment lengths,
  report unparsed tail bytes) is the real fix (Task 1 code-quality review).
- **`read_all_reporting` hot-path cost** — an extra per-segment `fs::metadata`
  stat, a discarded cursor `Vec`, and two `String` clones per segment for an
  always-empty map lookup. Negligible at one segment per machine; revisit
  once 4 MiB rotation grows segment counts (`App::events()` runs on ~every
  CLI command, twice on emit) (Task 1 code-quality review).
- **`FileEventLog::read_all` is public API with no non-test callers** —
  production goes through the `EventLog` trait (confirmed again by this
  closing task's mutation triage: the trait impl's forwarding methods are
  exercised only through `crates/cli`, never through this inherent method).
  Remove or demote when the seam is next touched (Task 1 code-quality
  review).
- **`TransferError::io` should mirror, not generalize** — a reviewer note for
  Task 4: don't build a generic error-constructor abstraction over
  `LogError`/`TransferError` (already the plan's shape; noted so nobody
  "improves" it) (Task 1 code-quality review).
- **`count_landed_events`'s doc should note the read-back cost on push** — a
  delta re-read over the wire; the full segment on first push; bounded by the
  4 MiB rotation (Task 4 quality re-review).
- **A segment whose copy lands but read-back fails records a failure row and
  is not counted in `segments_copied`** — self-clears next run; worth a
  clarifying comment or reorder (Task 4 quality re-review).
- **`transfer.rs`'s module-doc skip list should mention broken symlinks
  explicitly**, and **`is_effectively_file`'s doc says "one symlink hop" but
  actually follows the whole chain** — both cosmetic, fold in when the file
  is next touched (Task 4 quality re-review).
- **NFC/NFD Unicode normalization in `inbox_manifest`'s unlisted-file
  comparison** — iOS exports emit NFD; APFS resolves lookups, but the raw
  string-equality comparison reports false "unlisted" strays for an
  NFD-manifest/NFC-disk name mismatch. Deferred (report-only noise); revisit
  when the share-sheet Shortcut lands and real NFD manifests exist.
  Documented in `inbox_cmd`'s module doc (Task 9 spec review).
- **Windows-authored `contribution.json` manifests are not handled** —
  `C:\evil` parses as a relative `Normal` path component on macOS, so the
  traversal guard is Unix-scoped. Fine until a Windows contributor exists
  (Task 9 spec review).
- **`crates/core` and `crates/ingest` cucumber mains lack
  `fail_on_skipped()`** — an unmatched step in those suites reports as
  skipped and passes. Bring all four cucumber mains to the CLI suites' shape
  (`fail_on_skipped` + `run_and_exit`). Cheap follow-up, next time either
  crate's tests are touched (Task 12 quality review).
- **Stuck operator-fault contributions re-hash every pass** — a contribution
  parked on a typo'd `para_target` re-reads every listed byte per cron tick
  (the hash gate runs after routing now, so this is only the still
  uploading→ready→bad-target sequence; bounded by operator action). Revisit
  if real inboxes hit it (Task 12 quality review).

### cargo-mutants triage (phase 6)

Five scoped runs, each `--in-place` with the test command narrowed to
`--bin maj --test sync_smoke --test inbox_smoke --test inbox_acceptance` (the
default full-package `cargo test` reruns the CLI's whole suite — every
gated/model-bound integration binary included — per mutant, which does not
finish in a practical session window; the three phase-6 suites are the ones
that actually exercise these files). Every count below is the final,
post-fix run.

```bash
cargo mutants -p majestical-sync --in-place
cargo mutants -p majestical-cli --in-place -f crates/cli/src/sync_cmd.rs \
  -- --bin maj --test sync_smoke --test inbox_smoke --test inbox_acceptance
cargo mutants -p majestical-cli --in-place -f crates/cli/src/inbox_cmd.rs \
  -- --bin maj --test sync_smoke --test inbox_smoke --test inbox_acceptance
cargo mutants -p majestical-cli --in-place -f crates/cli/src/inbox_manifest.rs \
  -- --bin maj --test sync_smoke --test inbox_smoke --test inbox_acceptance
cargo mutants -p majestical-ingest --in-place -f crates/ingest/src/plan.rs
```

**`majestical-sync`** (own crate, `transfer.rs` + `lib.rs`): 112 mutants
tested, 93 caught, 15 unviable, **4 missed, all triaged, none chased with a
new test**:

- `<impl EventLog for FileEventLog>::append`/`read_all_reporting`/
  `read_since_reporting` replaced with a no-op/empty return (3 mutants,
  `lib.rs:362,369,378`) — **covered by a sibling crate's own suite**, the
  same category phase 4's triage documented for `model.rs::fetch`.
  `App<L: EventLog>` (`crates/cli/src/app.rs`) calls these three methods
  through the trait bound on every CLI command that reads or appends events,
  but `cargo mutants -p majestical-sync` only runs this crate's own tests,
  which call the *inherent* methods of the same name directly and never
  construct a `&dyn EventLog`/generic `EventLog` bound. Confirmed by hand
  (not just assumed): applying each mutation and running
  `cargo test -p majestical-cli --test cli_smoke` fails 1, 17, and 19 tests
  respectively.
- `sweep_stale_temps`'s `age_ms > STALE_TEMP_MS` boundary (`transfer.rs:423`,
  `>` → `>=`) — a millisecond-exact boundary test would need a real
  `filetime`-crate mtime write (the existing `backdate` test helper shells
  out to `touch -t`, which only has minute resolution); not worth a new
  dependency for one boundary. Timing-precision, not chased.

**`crates/cli/src/sync_cmd.rs`**: 73 mutants tested, 69 caught, 4 unviable,
**0 missed** (12 closed). Fixes: `SyncConfig::load`'s `NotFound`-guard test
(permission-denied config file); `check_exit_policy`/`summarize_pull`/
`BlobCounts::from_blobs` had no direct unit test at all (each is pure logic,
now tested inline); `cmd_location_rm`/`cmd_location_list` had **zero**
CLI-level coverage (only their inner `remove_location`/config-read logic was
unit-tested) — closed with `location_list_and_rm_reflect_the_real_config`
(`crates/cli/tests/sync_smoke.rs`), a real gap this task fixed rather than
worked around.

**`crates/cli/src/inbox_manifest.rs`**: 30 mutants tested, 28 caught, 2
unviable, **0 missed** (2 closed). `load_manifest`'s `NotFound` guard got a
permission-denied test. `check_files`' *existing* permission-denied test
(`a_permission_denied_listed_file_is_a_hard_error_not_waiting_forever`) had a
latent bug this task found and fixed: it used `check_files`'s own return
value to decide whether the OS enforced `chmod 000`, so a mutant that folds
every io error into "waiting" also returns `Ok`, which looks identical to
"this environment doesn't enforce mode 000" from the outside — the test
would vacuously skip past exactly the mutant it existed to catch. Confirmed
by hand: applying the mutation still passed the un-fixed test on this
machine, even though a raw `chmod 000` + `stat` proves this environment does
enforce the permission. Fixed with a new, deterministic test
(`a_listed_path_through_a_non_directory_component_is_a_hard_error`) that
doesn't depend on permission enforcement at all: a manifest entry whose path
runs *through* a plain file (`plain-file/IMG_1.HEIC`) makes `symlink_metadata`
fail with `NotADirectory`/`Other`, never `NotFound`, and — unlike a
chmod'd directory — doesn't also break `collect_unlisted`'s separate walk,
so it isolates the guard completely. Confirmed to fail exactly as expected
with the mutation hand-applied, then reverted.

**`crates/cli/src/inbox_cmd.rs`**: 116 mutants tested, 98 caught, 17
unviable, 1 timeout, **0 missed** (24 closed, largest cluster this phase):

- `QUIESCENCE_MS`'s `5 * 60 * 1000` and `format_window`'s `< 1000` boundary
  (6 mutants) — untested pure functions; closed with direct literal-pin and
  boundary tests.
- `resolve_contribution_node`'s archived-node detection (`st.archived() &&
  st.kind() == Some(kind) && st.name() == Some(name)`, 2 mutants, both
  `&&` → `||`) — the existing `an_archived_para_target_names_unarchive_not_add`
  test only ever archives the exact target node, so every sub-condition is
  true for that one node either way and can't tell `&&` from `||` apart.
  Closed by `an_archived_node_of_a_different_kind_is_not_mistaken_for_the_target`
  (archives a node with the target's name but a DIFFERENT kind — real code
  correctly falls through to "does not exist yet"; either single-operator
  mutation wrongly reports "exists but is archived"). Confirmed by hand
  against both operators individually.
- `processed_target`'s `suffix += 1` (2 mutants) — no test exercised a
  *second* collision (only ever 0 or 1 existing `.processed/<name>`).
  `+=`→`-=` closed by a direct 3-collision test; `+=`→`*=` (suffix frozen at
  its start value forever) **times out** rather than being reported missed —
  the mutated loop spins forever re-testing the same already-existing target
  — the same "a hang is louder than a silently wrong assertion" reasoning
  phase 3's `Event::Eof`-deletion timeouts used; not chased further, and the
  new test's own timeout confirms it genuinely reaches this code path.
- `print_report_json`/`print_report_text`'s `skipped_duplicates > 0`
  boundaries (12 mutants across both `Ingested` and `PartlyIngested` arms, in
  both text and JSON rendering) — no test exercised the zero-vs-nonzero
  boundary in either output mode for either outcome shape. Closed with four
  paired zero/nonzero e2e tests (`crates/cli/tests/inbox_smoke.rs`):
  `json_output_is_a_single_parseable_document`/
  `json_output_includes_skipped_duplicates_only_when_nonzero` (JSON,
  `Ingested`), `a_partial_batch_with_a_duplicate_reports_both_counts`/the
  negative assertion added to
  `a_bad_loose_file_does_not_wedge_a_good_loose_file_in_the_same_group` (text,
  `PartlyIngested`), and
  `json_output_includes_skipped_duplicates_on_a_partial_row`/
  `json_output_omits_skipped_duplicates_on_a_partial_row_with_none` (JSON,
  `PartlyIngested`) — the last pair needed a real duplicate-plus-failure
  batch in the same triage pass, not just a duplicate alone.

**`crates/ingest/src/plan.rs`** (the `plan_source_filtered` inbox-triage
filter): 13 mutants tested, 12 caught, 1 unviable, **0 missed** — the
task's own TDD tests (`a_filtered_out_directorys_contents_are_absent_from_
the_plan`, `a_filtered_out_directory_is_never_entered_even_when_unreadable`)
already discriminated everything; no gaps found, no action needed.

## Done in phase 6

- **Segment rotation** (was Open, phase 2): landed with zero-padded `NNNN`
  segment names as designed — `ROTATE_BYTES`/`active_segment` in
  `crates/sync/src/lib.rs`, next to `list_segments`'s numeric-sort
  assumption (phase 6 Task 2).
- **Sync's two read paths diverging in walk and UTF-8 handling** (was a
  phase 4 deferral): `read_all_reporting` is now literally
  `read_since_reporting` called with empty cursors and the cursors
  discarded, so the two can no longer disagree — plus the `LogError::io`
  constructor removing the 13-call-site `map_err` repetition the same
  deferral named (`crates/sync/src/lib.rs`, phase 6 Task 1).

## Done in phase 5

Items this phase closed from earlier watchlists.

- **`media_kind`'s missing extensions and the one-place extension table** —
  both closed. `crates/core/src/media_kind.rs:16-70` is now a single
  `EXTENSIONS: &[(&str, MediaKind)]` table, and it carries the formats the
  phase 4 review named: `mpg`/`mpeg`/`3gp`/`wmv`/`insv` for video and
  `jxl`/`pef`/`iiq`/`3fr` for image, plus the new `Audio` and `Pdf` kinds
  PR #50 needed (was: "**`media_kind`'s extension lists omit several common
  formats**", phase 4 deferrals).
- **The ffmpeg-subprocess timeout gap, for the new audio call site only.**
  `run_with_timeout` (`crates/index/src/video.rs:259`) with a
  duration-scaled budget (`audio_timeout`, `:304`) covers
  `extract_audio_pcm`, so a stalled volume can no longer hang transcription.
  `probe`, `analysis_frames`, and `extract_frame` still call bare
  `.output()` — the phase 4 item stays open for those three (see phase 5
  deferrals).
- **The gated video-caption e2e deferred from PR 8** — closed by
  `video_captions_describe_real_keyframes`
  (`crates/cli/tests/phase5_e2e.rs:218`), which drives real scene detection
  and real keyframe re-extraction through the CLI against a mock
  OpenAI-compatible backend, rather than PR 8's hand-planted
  `KeyframeManifest`/`Captions` blobs.

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
