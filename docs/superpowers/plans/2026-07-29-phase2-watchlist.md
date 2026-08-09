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

## Phase 7A deferrals

Recorded during the phase 7A PR chain (#64-#69) and this closing task. Items
marked "(phase 7 spec)" come from that spec's own Deferred list; the rest
were found during execution.

- **Keyframe-image extraction is deferred** — keyframe *images* are never
  stored as blobs, only the detected-timestamp manifest is
  (`majestical_index::blob::Derivation::KeyframeManifest`). The
  `majestical://keyframes/{asset_id}` MCP resource
  (`crates/cli/src/mcp_cmd/resources.rs`) serves that manifest, not an
  image; on-demand frame extraction from the source video whenever an
  agent wants to actually SEE a keyframe is unbuilt (Task 7, restated at
  closing; also recorded in the spec's as-built section).
- **`maj sync location list` and its MCP tool disagree on a missing
  catalog.** `services::sync::locations_list`
  (`crates/services/src/sync.rs:383-393`) reads `sync.toml` from the state
  dir and needs no catalog at all — `locations_list_of_an_unconfigured_
  catalog_is_empty` (`:838-843`) proves it returns an empty list against a
  bare, never-`catalog init`'d directory. The CLI (`cmd_location_list`,
  `crates/cli/src/sync_cmd.rs:450-451`) calls it directly and succeeds. The
  MCP tool (`list_sync_locations`, `crates/cli/src/mcp_cmd/read_tools.rs:
  196-206`) calls `self.ensure_catalog()` first and returns the `maj
  catalog init` remedy error instead — same verb, same underlying service
  call, different behavior against the same input. Named `ensure_catalog`'s
  own doc comment (`crates/cli/src/mcp_cmd/mod.rs:108-119`), added during
  Task 6's implementation; the plan document itself doesn't flag it in
  prose. Unresolved at closing.
- **CLOSED IN PHASE 7B (#75): Enum-shaped string parameters have no schemars
  enum, losing schema-level validation/documentation.** All four fields now
  take a `schemars`-derived enum, so the allowed values appear in the tool's
  JSON schema and a typo'd value is rejected by the MCP layer before any
  handler runs. Two findings worth carrying: `rmcp` rejects a bad enum value
  as a protocol-level error (not a tool result), and the pre-existing schema
  snapshot in `mcp_smoke.rs` did NOT pin these fields at all — the snapshot
  correction plus a tripwire asserting each enum's values landed with the
  change, because without them the derive could be dropped again invisibly.
  Original finding follows.

  Four `write_tools.rs` fields
  are each a closed set of string literals, documented only in a doc
  comment and hand-parsed at call time with a `match`/`bail!` that errors
  on anything else: `TagAssetsArgs::op` (`:145`, one of
  `add`/`rm`/`confirm_suggestion`/`reject_suggestion`, parsed by
  `parse_tag_op`, `:173-187`), `MoveParaArgs::op` (`:284`, one of
  `add`/`rename`/`archive`), `IngestSourceArgs::dedupe` (`:486`, one of
  `skip`/`copy`, default via `default_dedupe`, `:466-468`), and
  `SetDescriberArgs::backend` (`:822`, one of
  `ollama`/`lm-studio`/`open-router`, parsed by `parse_backend`,
  `:124-135`). A real `schemars`-derived enum on each would surface the
  allowed values in the tool's JSON schema itself, catching a typo'd value
  at the client before the call round-trips instead of after.
- **CLOSED IN PHASE 7B (#75): Dry-run previews over-promise on an unknown
  asset id.** Both `set_metadata`'s and `tag_assets`' dry-run branches now
  validate the asset exists before describing a write, so a preview and a
  `confirm: true` call on the same unknown id agree. Two carve-outs stayed,
  deliberately, and are the honest scope of the fix:

  - `tag_assets`' `reject_suggestion` op is unguarded on BOTH preview and
    execute. Rejecting a suggestion for an asset the catalog never knew is a
    no-op rather than an error, and making the preview stricter than the
    execute path would be the same divergence in the other direction.
  - `tag_assets`' `rm` op still over-promises in one narrower case: an asset
    the catalog DOES know, carrying a tag it does not have. The existence
    check passes, the preview says "would remove", and the real `tag_rm`
    then fails on its empty `tag_add_ids` lookup. Closing that would mean
    the preview reimplementing `tag_rm`'s own guard.

  Original finding follows.

  `set_metadata`'s dry-run branch (`crates/cli/src/mcp_cmd/write_tools.rs:254-273`) calls
  `meta::meta_get`, which does not validate the asset exists
  (`crates/services/src/meta.rs:85-100` has no `ensure_asset_known` call —
  unlike `meta_set_impl`, `:52-62`, which does) — an unknown asset id
  silently reports `current_value: null` and a "would set..." message as
  if the write will succeed. `tag_assets`'s dry-run branch
  (`crates/cli/src/mcp_cmd/write_tools.rs:198-220`) has the identical gap:
  it reads `projection.tags(...)` directly (empty for an unknown id, no
  existence check), while the real `tag_add` (`crates/services/src/
  tags.rs:31-39`) calls `ensure_asset_known` and `tag_rm` (`:52-64`)
  requires a non-empty `tag_add_ids` lookup — an unknown asset has none,
  so it's rejected too, just via a different guard. Both tools' dry runs
  describe an action as
  achievable when `confirm: true` on the same unknown id would actually
  fail.
- **`scan_volume`'s dry-run count silently drops walk errors.** The preview
  branch (`crates/cli/src/mcp_cmd/write_tools.rs:400-404`) counts files via
  `walkdir::WalkDir::new(&args.dir).into_iter().filter_map(Result::ok)` —
  every entry that errors (permission denied, a broken symlink, ...) is
  dropped from `would_scan_files` with no indication any occurred, so the
  previewed count can undercount what a real `confirm: true` scan would
  actually attempt.
- **The `"ascmhl"` directory name is a literal repeated in ~8 places, not a
  shared const.** `majestical_ingest::mhl` defines `const ASCMHL_DIR: &str
  = "ascmhl"` (`crates/ingest/src/mhl.rs:139`) but keeps it private, so
  `verify_volume`'s dry-run history check
  (`crates/cli/src/mcp_cmd/write_tools.rs:436`,
  `args.dir.join("ascmhl").is_dir()`) and several test fixtures
  (`crates/ingest/tests/conformance.rs:33`, `crates/cli/tests/
  cli_smoke.rs:1086,1162,1250`, `crates/cli/tests/inbox_smoke.rs:146,209`)
  each hand-write the same string. Exporting the const (or a small public
  helper) would remove the risk of the literal drifting from the real
  directory name in exactly one of these call sites.
- **CLOSED IN PHASE 7B (#71, #72): MCP clients never see stderr
  diagnostics.** Every one of the 28 stderr sites now writes to a
  thread-local notices sink (`crates/services/src/notices.rs`) instead of
  `eprintln!`, and each verb's outcome struct carries a `notices: Vec<String>`
  field that the sink drains into. All three heads read the same field: the
  CLI prints it to stderr as before (so no user-visible change), `maj mcp`
  folds it into the structured result via `with_notices`, and the GUI renders
  it above each surface. The `#[expect(clippy::print_stderr)]` blocks in
  `crates/services` are gone with the `eprintln!`s they covered. What the
  fix does NOT cover is recorded under "Phase 7B deferrals" below — chiefly
  the `Err` path (a service call that fails drops the notices its sink was
  holding) and per-site `with_notices` foldings that no test pins. Original
  finding follows.

  `crates/services` inherits
  the workspace's `print_stderr = "deny"` (unlike `crates/cli`, which
  allows it crate-wide since CLI diagnostics are the product), so every
  stderr notice moved verbatim from the pre-extraction CLI carries its own
  local `#[expect(clippy::print_stderr, reason = "...not yet a rendered
  outcome")]` (first at `crates/services/src/app.rs:35-54`'s
  `warn_skipped_corrupt_lines`, which itself names this as later work). 28
  such sites exist across
  `crates/services/src/{app,state_dir,tags,search,inbox,index/run,
  index/heal,index/mod}.rs` (app.rs 2, state_dir.rs 2, tags.rs 1,
  search.rs 6, inbox.rs 4, index/run.rs 5, index/heal.rs 6, index/mod.rs
  2), and most are reachable from an MCP
  tool: `warn_skipped_corrupt_lines`/the HLC clock-clamp warning
  (`app.rs`) from nearly every tool that opens the catalog; legacy-catalog
  migration notices (`state_dir.rs`) from the same path; the
  unreadable-suggestions notice (`tags.rs`) from `suggest_tags_review`; six
  semantic-miss notices (`search.rs`) from `search_assets`/
  `run_saved_search`; vector-store/caption/video-caption notices
  (`index/run.rs`) and unreadable-blob notices (`index/heal.rs`) from
  `index_run`; the describer-model-tag and failure-report notices
  (`index/mod.rs`) from `index_status`/`index_run`; and marker/quiescence/
  per-row failure notices (`inbox.rs`) from `inbox_process`. `maj mcp`'s
  tools route every outcome through `structured_ok`/`tool_error`, never
  reading stderr, so an agent driving any of these paths never sees these
  notices at all today. Migrating each to a field on its outcome struct
  (rather than a bare `eprintln!`) before the GUI head lands — which faces
  the identical stderr-is-invisible problem — is the fix.
- **`IngestRun.outcome`'s name collides in spirit with the service layer's
  own `*Outcome` convention.** Every other service verb returns a struct
  named `<Verb>Outcome` (`SearchOutcome`, `MetaOutcome`, `LocationsOutcome`,
  `ScanOutcome`, ...) — `run_ingest` breaks the pattern by returning
  `IngestRun` (`crates/services/src/ingest.rs:168-172`), whose own
  `outcome` field holds `majestical_ingest::engine::Outcome`, a completely
  different, lower-level, per-file-copy type from a different crate. A
  reader skimming for "the ingest outcome struct" finds two different
  things named `Outcome` in two different crates, neither of them called
  `IngestOutcome`. Renaming is a call-site-touching change across
  `crates/cli`/`crates/services`; not done this phase given the volume of
  extraction work already in flight.
- **CLOSED IN-CHUNK: 8 of 16 mutating MCP tools had no functional test in
  `mcp_smoke.rs` beyond the roster/schema checks** (found during the
  write_tools.rs cargo-mutants triage below): `add_sync_location`,
  `rm_sync_location`, `scan_volume`, `set_describer`, `set_metadata`,
  `sync_pull`, `test_describer`, and `inbox_process` were each named only
  inside the `EXPECTED_TOOLS`/`MUTATING_TOOLS` constant arrays
  (`crates/cli/tests/mcp_smoke.rs`) — never called via `mcp.call_tool(...)`
  to assert an actual dry-run or executed response. The other 8
  (`tag_assets`, `ingest_source`, `sync_push`, `catalog_init`,
  `verify_volume`, `index_run`, `move_para`'s `archive` op, `rm_saved_
  search`) already had a dedicated test. This was why each of the 8
  untested functions' own `if !args.confirm { ... }` dry-run guard
  survived a "delete `!`" mutation (e.g. `scan_volume_result`, `:397`)
  alongside the whole-body `Ok(Default::default())` replacement:
  `confirm_gate`/`inject_executed` (`crates/cli/src/mcp_cmd/
  write_tools.rs:47-91`) always injects `"executed": confirm` from the
  REQUEST value, not from which branch actually ran, so an inverted
  guard's response still carries a self-consistent but wrong `executed`
  flag — a symptom worth naming for anyone touching this module's tests
  again, distinct from the untested-function root cause itself.

  Added one dry-run-then-confirm test per tool (the `move_para_archive_
  dry_run_plans_then_confirm_moves`/`rm_saved_search_dry_run_then_
  confirm_removes_it` shape), each verifying the confirmed effect through
  a read tool or the CLI: `add_sync_location`/`rm_sync_location` via
  `list_sync_locations`; `scan_volume` via `search_assets` finding the
  scanned file; `set_metadata` via `get_asset`'s `fields`; `set_describer`
  via `get_describer`; `sync_pull` via a real two-machine push-then-pull
  (`search_assets` finds the pulled asset); `inbox_process` via a minimal
  valid contribution (fixture shape copied from `inbox_smoke.rs`'s
  `write_contribution`) actually landing at `dest` with an ASC MHL
  history and moving to `.processed/`; `test_describer` against an
  unreachable backend (`http://127.0.0.1:1`, matching `describer_smoke.
  rs`'s own CLI-level test) — confirmed the ACTUAL semantics rather than
  assuming: `describer_config::test`'s probe error propagates through
  `?`, so `confirm_gate`'s `Err` arm renders it exactly like a read
  tool's error (plain `isError: true` text naming the URL), never a
  structured probe payload.

  Two of the eight new tests each close one but not both variants of a
  match-guard mutant (`MajServer::inbox_process`'s `:999:33` and
  `MajServer::sync_pull`'s `:1127:33`, both `Ok(json) if failed => ...`):
  a successful pass proves the guard doesn't spuriously report `isError`
  when nothing failed (the `true`-replacement variant, confirmed by hand
  — reverted after confirming), but neither test drives an actual
  failure, so the `false`-replacement variant (the guard never firing
  even on a real failure) remains open for both — the same residual
  `sync_push_partial_failure_keeps_rows_and_maps_polarity` already closes
  on the push side. Every other listed survivor in the 8 tools' functions
  was confirmed by hand for at least one representative
  (`scan_volume_result`'s whole-body replacement; `MajServer::
  inbox_process`'s match guard) before committing.

### cargo-mutants triage (phase 7A)

Three scoped runs (`cargo mutants --package <pkg> --file <path> --timeout
300`; this repo's version, 27.1.0). The two `crates/services` runs used the
plan's sketch verbatim, default full-package test command; the
`crates/cli` run needed narrowing to `-- --test mcp_smoke` (see that run's
own note below — the crate's full default suite measured roughly 100s per
mutant against 88 mutants, over two hours projected). Every count below is
the run's own final tally; several representative survivors were confirmed
by hand (mutate the real source, run the specific test that should catch
it, confirm failure, revert) rather than assumed.

```bash
cargo mutants --package majestical-services --timeout 300 \
  --file crates/services/src/search.rs
cargo mutants --package majestical-services --timeout 300 \
  --file crates/services/src/catalog.rs
cargo mutants --package majestical-cli --timeout 300 \
  --file crates/cli/src/mcp_cmd/write_tools.rs -- --test mcp_smoke
```

**`crates/services/src/search.rs`**: 128 mutants tested in 21m, 24 caught,
52 unviable, **52 missed — all triaged, none needing a new test in this
crate**:

- **39 of the 52 are covered by `crates/cli`'s own suite, not this crate's.**
  `search.rs` was extracted from `crates/cli/src/search.rs` this same
  phase (PR1, #65); `cargo mutants -p majestical-services` only runs this
  crate's OWN `#[cfg(test)]` module, never `crates/cli`'s integration
  suites that already drive every one of these code paths through the real
  `maj` binary — the same "covered by a sibling crate's own suite"
  category phase 4 (`model.rs::fetch`) and phase 6 (`EventLog`
  trait-forwarding) both documented. Confirmed by hand for one
  representative: `searches_rm_impl` reduced to a bare `Ok(())` no-op
  still passes every test in this crate, but fails two `crates/cli/tests/
  cli_smoke.rs` tests (`running_and_managing_saved_searches`,
  `saved_searches_sync_between_machines`) — reverted after confirming.
  The remaining 38 are the same category by direct inspection, not
  independently hand-verified this pass: `searches_list` (1, exercised by
  the same `cli_smoke.rs` tests), `resolve_filters`/`resolve_filter` (3,
  exercised by `cli_smoke.rs`'s `key:value` filter tests),
  `text_coverage_notices`/`source_remedy` (9, `search_text_smoke.rs`'s
  `coverage_notice_names_uncovered_transcripts` and neighbors assert the
  exact stdout remedy text these functions produce),
  `SemanticMiss::note`/`TextSemanticMiss::note`/`open_semantic_index`/
  `open_text_semantic_index`/`embed_query`/`embed_text_query`/
  `semantic_candidates`/`text_semantic_candidates` (20, `crates/cli/tests/
  index_smoke.rs`'s `MAJ_MODEL_DIR`-gated tests assert the exact
  "semantic index is empty"/"semantic index unreadable" stderr text these
  functions produce), and `selected_text_sources`/`eligible_assets`/
  `text_fts_search` (5, exercised end-to-end by `search_text_smoke.rs`'s
  `maj search <term> --json` calls).
- **13 remain open — pure ranking arithmetic with no confirmed coverage
  even at the `crates/cli` level, in the spirit of phase 4's
  "aggregate-covered arithmetic" bucket**: `run_search`'s `!=`->`==` guard
  (1, `:198`), `TermSearchOutput`'s `ranked` field deletion (1, `:223`),
  `term_search`'s `>>`->`<<` (1, `:406`), `fuse_ranked_n`'s two `!`
  deletions and one `&&`->`||` (3, `:556,558,567`), and `rrf_merge`'s
  scoring arithmetic (7, `:732-733` — the `+=`->`*=` mutant in particular
  would zero every fresh score, collapsing fusion ranking to alphabetical
  tie-break order). No test asserts exact multi-source result ORDER (only
  membership/coverage), so a subtly wrong fusion score could survive
  end-to-end too. Not chased this pass; a future task adding an
  order-sensitive fusion test would need to cover both crates' suites,
  since `crates/services` itself has no test of its own for this
  arithmetic either.

**`crates/services/src/catalog.rs`**: 27 mutants tested in 9m, 20 caught, 5
unviable, **2 missed — both the same single line, both closed by the same
existing `crates/cli` test**: `open_catalog`'s `skipped += 1` corrupt-line
counter (`:30`, `+=`->`-=` and `+=`->`*=`) has no test in this crate
(`warn_skipped_corrupt_lines` is a display-only diagnostic here), but
`crates/cli/tests/cli_smoke.rs`'s `corrupt_log_line_is_skipped_and_
reported_on_stderr` asserts the literal stderr text `"warning: skipped 1
corrupt event log line(s)"` — confirmed by hand: mutating `+=` to `*=`
(count stays 0 regardless of skips) fails that exact test with the real
stderr showing no corrupt-line warning at all; reverted after confirming.
Same sibling-crate-coverage category as `search.rs` above, and for the
same reason — `catalog.rs` is also "moved verbatim from `crates/cli/src/
commands.rs`" per its own module doc.

**`crates/cli/src/mcp_cmd/write_tools.rs`**: 88 mutants tested in 42m
(rescoped mid-run from the default full-crate test command, which measured
~100s/mutant against 88 mutants — over two hours projected — to `-- --test
mcp_smoke`, the one suite that actually exercises these tools; the killed
first attempt's one real result, `parse_only`'s `Ok(None)` mutant missed at
96s, is not counted below, only the clean rescoped run is), 33 caught, 4
unviable, **51 missed**:

- **7 closed with new unit tests added this pass** (`parse_only` 1,
  `non_empty_tags` 3, `parse_index_kinds` 3): these three are pure
  request-parsing helpers with no test anywhere touching more than one
  input value each. A new `#[cfg(test)] mod tests` at the end of
  `write_tools.rs` tests each directly (cheaper and more precise than a
  full `maj mcp` stdio round trip for pure logic) — confirmed by hand for
  `parse_only` (mutated to unconditional `Ok(None)`, watched the new test
  fail with `left: None, right: Some(Segments)`, reverted); the other two
  mutated similarly by inspection, not independently re-run.
- **41 were the systemic gap this pass's other watchlist item names (above,
  now marked CLOSED IN-CHUNK): 29 closed by the 8 new `mcp_smoke.rs`
  tests, 12 remain open.** Closed: `set_metadata_result` (2),
  `scan_volume_result` (2), `add_sync_location_result` (3),
  `rm_sync_location_result` (3), `sync_transfer_dry_run` (2, including
  its `==`->`!=` location filter — exercised via `sync_pull`'s new test
  passing an explicit `location` argument), `inbox_dry_run` (1),
  `set_describer_result` (2), `test_describer_result` (2), and 8 of the
  corresponding `MajServer::*` `#[tool]` wrapper survivors
  (`add_sync_location`, 3 of `inbox_process`'s 4, `rm_sync_location`,
  `scan_volume`, `set_describer`, `set_metadata`, 3 of `sync_pull`'s 4,
  `test_describer`). Still open (12): `move_para_add` (7) and
  `move_para_rename` (2) — `move_para`'s `add`/`rename` ops were never
  part of this fix (only its already-tested `archive` op was in scope);
  `MajServer::inbox_process`'s and `MajServer::sync_pull`'s remaining
  match-guard `false`-variant mutant each (a real failure case isn't
  exercised by either new test — see the watchlist item's own note); and
  `MajServer::sync_push`'s one match-guard `true`-variant survivor, which
  predates this fix and belongs to `sync_push`'s own already-existing
  test (`sync_push_partial_failure_keeps_rows_and_maps_polarity` only
  ever drives a FAILING push, so it can't discriminate a guard that fires
  unconditionally from one that fires correctly — a distinct, narrower
  gap than the one this pass closed, watchlisted here rather than in a
  new bullet since it's one mutant on one already-tested tool).
- **2 are `verify_volume_result`'s own narrower gap, not the systemic
  one**: the tool DOES have a dedicated test
  (`verify_volume_on_a_tampered_dir_is_iserror_with_the_report_attached`),
  and its dry-run guard (`:435`) is correctly caught — but the "failed"
  determination (`crates/cli/src/mcp_cmd/write_tools.rs:451`, `let failed
  = !report.altered.is_empty() || !report.missing.is_empty()`) only ever
  gets exercised with an ALTERED file, never a MISSING one, so the second
  `!` (`:451:56`) and its neighboring match-guard mutant (`:453`) survive.
  Not chased this pass; a second fixture (delete a listed file rather than
  tamper with it) would close both.
- **1 display-only, not chased**: `default_ingest_template`'s literal
  default string (`:463`, mutated to `"xyzzy"`) — no test asserts the
  exact default template text, only that omitting `template` doesn't
  error; cosmetic, matching the phase-4 "display/diagnostic-only" category.

## Phase 7B deferrals

Recorded during the phase 7B PR chain (#71, #72, #75, #76, #77, #80, #81,
#82, and PR7) and this closing task. Each item names where it came from.

- **The sqlite sync-offset suppresses repeat corrupt-log notices**
  (pre-existing, surfaced by Task 1's notices work). The projection records
  how far it has read; a second call over the same log resumes past the
  corrupt lines and so emits no notice for them. A user who runs the same
  verb twice sees the warning once. Correct as caching, wrong as reporting —
  the damage is still there on the second call.
- **`maj searches list`'s shared-tty interleaving is inverted** relative to
  the other verbs (Task 1 design decision, accepted). Notices drain to
  stderr after the listing reaches stdout rather than before it. On a shared
  terminal the warning therefore appears below the rows it qualifies. No
  parity test can see this: each stream is captured separately, so
  cross-stream order is invisible to the harness that would otherwise pin it.
- **The order-parity test cannot exercise the `NoModel` arms** (Task 2). The
  semantic-miss notices only arise on a machine with a describer model
  installed; on a model-less machine — which is what CI is — those match
  arms are never entered, so the test proves ordering for every other notice
  source and stays silent about those.
- **The skip-if-empty notices contract is wire-pinned on only two structs**
  (Task 3). `notices` is `#[serde(skip_serializing_if = "Vec::is_empty")]`
  on every outcome struct, but only `AssetDetail` and `SearchOutcome` have a
  test asserting the field is absent — not null, not `[]` — from the
  serialized JSON when there is nothing to report. The rest rely on the
  attribute being copied correctly.
- **A failing service call loses the notices its sink was holding** (Task 3,
  narrow but real). Notices are drained on the `Ok` path; an `Err` returns
  without draining, so a call that collected warnings and then failed
  reports the failure alone. The 7C fix shape is a notices payload on
  `ServiceError` itself. `sync::pull_impl` gains the most from it: its sink
  holds the buffer `apply_pulled_events` folded, and that is exactly what is
  dropped at `PullApplyFailure` — the moment a user most wants to know what
  else went wrong.
- **`with_notices`' per-site foldings are largely unpinned** (Task 3,
  confirmed by this phase's mutants runs). The helper itself has tests; the
  ~20 call sites that each decide which outcome to fold notices into do not,
  so a site folding the wrong thing — or not folding at all — is caught only
  by review.
- **CLI and MCP disagree about where a failure report's notices appear**
  (Task 3). `index_cmd` drains at end-of-command; the MCP path folds into the
  outcome. Same warnings, different position relative to the failure report
  they describe.
- **`move_para_archive` serializes without notices on either arm** (Task 3).
  Both the dry-run and the executed response are built by hand rather than
  through the outcome struct, so notices collected during the call are not
  in either.
- **`get_asset` carries notices at two JSON depths** (Task 3). They are
  pinned as a contract, so this is a documented shape rather than a bug, but
  it leaves a real client-layer ergonomics question for the GUI. It also has
  a genuine divergence inside it: the GUI's `get_asset` command returns
  `Ok(None)` for an unknown asset and drops the notices with it, where MCP's
  `found: false` response folds them in.
- **`MoveParaArgs.kind` and sync's `only` filter are still free strings**
  (Task 4). They are the next two `schemars`-enum candidates, left out of
  #75 only because the four fields it did close were the ones with a
  hand-written `match`/`bail!` behind them.
- **The services graph is macOS-only, so 3-OS Rust CI is impossible**
  (discovered by PR #77's first matrix run). `crates/index` depends on
  `objc2`, Vision and PDFKit unconditionally, and everything downstream of it
  inherits that — the Rust steps in the CI matrix therefore run on macOS
  alone. The frontend gates (`pnpm check`/`lint`/`test`/`build`) stay 3-OS
  and are genuinely cross-platform. Porting means target-gating those
  dependencies and supplying non-Apple OCR and PDF fallbacks; it is a phase
  of its own, not a cleanup.
- **The TypeScript wire layer is unpinned against Rust** (Task 7). Nothing
  cross-checks `api.ts`'s field names or its camelCase argument names against
  the Rust structs and `#[tauri::command]` signatures they mirror — a renamed
  serde field breaks the GUI at runtime and no test anywhere fails. Fix
  shape: a Rust-serialized fixture parsed under the TS types inside vitest.
- **The Lance scoped-thread rule is review-enforced on both async heads**
  (Task 7). `run_off_tokio_runtime` must wrap any call that may open a Lance
  store; omitting it panics only on a machine that has a model installed and
  an index built, which no fixture has. No test can catch the omission, and
  this phase's mutants run confirmed the tooling cannot either — see the
  triage section below.
- **The `__eh_frame` linker note now also appears on the GUI binary**
  (Task 7). Dev-profile only, cosmetic, unchanged in character from the
  headless workspace's long-standing one.
- **A keyframe manifest whose timestamps are all zero renders nothing**
  (Task 9). Unreachable today because `over_half_failed` withholds the
  manifest entirely in that case. If that guard ever changes, the strip needs
  an explicit zero state rather than an empty one.
- **`clock_suspect`'s explanation lives only in `title=`** (Task 9). Hover
  text is unreachable by keyboard and unreliable for screen readers; the
  marker itself is announced, its reason is not.
- **`about.hbs` emits duplicate HTML ids** (Task 10). One `h2` per crate
  version — 155 of them for 11 licenses — sharing ids by license. Invalid
  HTML in a generated artifact nobody navigates by anchor.
- **`x86_64-apple-darwin` release artifacts are blocked by `ort`** (the
  `v0.1.0-rc2` dry run; user decision 2026-08-05). `ort-sys` 2.0.0-rc.13's
  distribution table lists `aarch64-apple-darwin` as its only macOS target,
  so an Intel build stops in the build script with `no prebuilt binaries
  available for target x86_64-apple-darwin`. This has been true since phase 5
  introduced `ort`; the release workflow predated that and had gone unrun
  against Intel since, so the spec's two-architecture commitment was
  unbuildable for two phases without anything saying so. The release now
  targets Apple silicon alone. Three routes back, none taken: build ONNX
  Runtime from source in CI (slow, and a second toolchain to maintain),
  vendor a prebuilt via `ORT_LIB_LOCATION` (someone must produce and host
  it), or make the semantic stack optional at build time so an Intel binary
  can ship without it (the only one that also helps anyone else, and the
  largest). Revisit if `ort` restores the target.
- **`tauri-action`'s draft creation races** (Task 10; narrowed 2026-08-05).
  Two concurrent matrix jobs list-then-create and then rewrite
  `latest.json`, so a lost race yields two drafts each holding half the
  artifacts. Dropping to one desktop target removed the concurrency, so the
  race cannot fire as the workflow stands — this is dormant, not fixed.
  Restoring a second target brings it back, and the fix then is to create the
  release in a job of its own ahead of the matrix.
- **`zizmor`'s auditor persona reports two informationals** (Task 10):
  secrets-outside-env (the signing secrets should live in a GitHub
  Environment) and the `rust-toolchain` pin. Neither is a finding at the
  default persona. Both are 7C hardening.
- **Cross-binary `tauri_parity` loud-skips in CI** (Task 7, restated at
  closing). Without `MAJ_BIN` the test that compares the GUI's rows against
  `maj search --json` announces a skip and passes. Only `just gui-test`
  builds a `maj` first, so the parity claim is proved locally and not in CI.
- **The GUI's sync commands rebuild the projection per call on the dispatch
  thread** (Task 7). Only the two search commands go through `blocking`.
  Escalate the others if the UI is ever seen to hitch.
- **Keyframe-image extraction is still deferred** (carried unchanged from
  phase 7A — see that section's first item). Nothing in 7B touched it: the
  GUI's inspector renders the manifest's timestamps as timecodes, which is
  the same manifest the MCP resource serves, and still not an image.
- **arrow 59 is blocked on lancedb** (dependabot triage, 2026-08-09).
  Dependabot PRs #74 and #79 bumped `arrow-array`/`arrow-schema` to 59.1.0
  against lancedb 0.33.0's arrow-58 API and failed to compile
  (`RecordBatch: Scannable` unsatisfied) — exactly what the Cargo.toml
  lockstep comment warns against. Both PRs closed; `dependabot.yml` now
  ignores both crates in both cargo ecosystems. When lancedb releases
  against arrow 59, bump all three together and drop the ignores.
- **TypeScript 7 is not a drop-in** (dependabot triage, 2026-08-09). PR #78
  failed on all three GUI builds: svelte-check requires TS6 and TS7
  installed side by side via an npm alias plus a `--tsgo` flag. PR closed;
  `dependabot.yml` now ignores typescript majors. Migrate deliberately when
  the toolchain supports TS7 standalone, then drop the ignore.
- **The GUI cargo ecosystem's dependabot entry sidestepped the root's
  ignores** (dependabot triage, 2026-08-09). The src-tauri workspace
  path-includes the root crates, so its dependabot entry edits the root
  Cargo.toml — PR #79 re-proposed the rusqlite 0.40 bump the root entry
  already ignores. The two ignore lists are now mirrored and must stay so.

### cargo-mutants triage (phase 7B)

Five scoped runs (this repo's version, 27.1.0), each foreground and one at a
time. The two GUI runs used `--in-place` so they build against the existing
target directory: cargo-mutants otherwise copies the tree to a scratch dir
and rebuilds the Tauri graph cold, which put the baseline alone at several
times the whole run's mutant budget. `--in-place` restores the sources when
it finishes; `git status` was checked clean after each.

```bash
cargo mutants --package majestical-services --timeout 300   --file crates/services/src/notices.rs
cargo mutants --package majestical-services --timeout 300   --file crates/services/src/runtime.rs
cargo mutants --package majestical-services --timeout 300   --file crates/services/src/index/blobs.rs
cargo mutants --in-place --manifest-path apps/desktop/src-tauri/Cargo.toml   --timeout 300 --file src/commands.rs
cargo mutants --in-place --manifest-path apps/desktop/src-tauri/Cargo.toml   --timeout 300 --file src/thumb_protocol.rs
```

**`crates/services/src/notices.rs`**: 4 mutants tested in 5m, **4 caught, 0
missed**. The sink's own tests cover it completely.

**`crates/services/src/runtime.rs`**: 1 mutant tested in 5m, **1 unviable, 0
viable**. Worth stating plainly, because it settles a pre-registered
expectation: the Lance rule cannot be checked by this tool at all. The only
mutation cargo-mutants generates for this file replaces
`run_off_tokio_runtime`'s body with `Ok(Default::default())`, which does not
compile — `T: Send` carries no `Default` bound. The mutation that WOULD
matter, dropping the `std::thread::scope` and calling `f` inline, is not in
the tool's catalogue. The rule stays review-enforced, as recorded above.

**`crates/services/src/index/blobs.rs`**: 9 mutants, **8 missed on the first
run, all 8 closed with new tests** (re-run: 8 caught, 1 unviable). The file
was extracted this phase and had no `#[cfg(test)]` module of its own; both
its callers live in other packages (`maj mcp`'s `majestical://` resources and
the desktop app's `thumb://` protocol), so a `--package majestical-services`
run exercised none of it. The survivors were `Kind::noun` and
`Kind::kinds_flag`'s return strings (4), `read`'s whole body (3), and the
`NotFound` match arm (1) — between them, every word of the remedy text and
the difference between "not derived yet" and "the read failed". Four tests
now pin the round trip, the malformed-id rejection, both kinds' exact remedy
strings, and a non-absence read failure reported as one.

**`apps/desktop/src-tauri/src/commands.rs`**: 28 mutants, **4 missed on the
first run, 2 closed with new tests, 2 accepted** (re-run: 6 caught, 2 missed,
20 unviable). The 20 unviable are the `#[tauri::command]` wrappers whose
return types have no `Default`.

- *Closed:* `machine_identity` mutated to `"xyzzy"` survived — nothing
  asserted the app stamps its events with the hostname, which is what makes a
  `maj` user on the same machine a converging peer rather than a second one.
  `selected_catalog` mutated to `None` survived — `tests/commands.rs` drives
  the `*_impl` layer with a `CatalogCfg` in hand and read the state lock by
  hand where it did check publication. Two unit tests in `commands.rs` (where
  `hostname` and `AppState` are both in scope) close both.
- *Accepted:* `restore_persisted_catalog -> Ok(())` and `get_asset ->
  Ok(None)`. Both take `AppHandle`/`State`, which cannot be constructed
  without a running Tauri app; there is no mock app anywhere in this suite,
  which is precisely why every command has an `_impl` the tests drive
  directly. Confirmed by hand rather than assumed: with
  `restore_persisted_catalog`'s body replaced by `Ok(())`, the entire GUI
  suite — 2 lib tests, 23 `commands.rs` tests, and 2 `tauri_parity` tests
  with `MAJ_BIN` set so the cross-binary comparison genuinely ran — still
  passes. Reverted after confirming. `get_asset`'s impl is pinned by
  `get_asset_returns_detail_for_known_and_none_for_unknown`; what survives is
  the one delegation line. Reaching either would mean enabling `tauri`'s
  `test` feature and building a mock app — a 7C call, recorded here.

**`apps/desktop/src-tauri/src/thumb_protocol.rs`**: 51 mutants tested in 5m,
**16 caught, 32 unviable, 3 missed — all 3 accepted**, and all three are the
same function: `respond(app: &AppHandle, uri)`, replaced with an empty/one-byte
`Response`. Same root cause as the two accepted above — `respond` exists only
to pull the selected catalog out of the `AppHandle` and hand it to `handle`,
which is the seam the tests drive and which is fully caught. Every route,
status and body of the protocol is covered through `handle`.

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
