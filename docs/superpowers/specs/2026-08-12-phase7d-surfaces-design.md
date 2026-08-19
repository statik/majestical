# Majestical Phase 7D — Browse / Ingest / Organize surfaces

Written 2026-08-12 from the 7D design session (brainstorming with visual
mockup review, per the 7C handoff's mandate). Parent spec §6 names the five
GUI surfaces; 7B shipped Search + Volumes + Inspector on Layout C; this
phase ships the remaining three, each with the backend seam it needs and
CLI + MCP parity for every new capability.

Approved mockups (committed alongside, self-contained HTML — open in any
browser): `mockups/2026-08-12-phase7d/browse.html`, `organize.html`,
`ingest.html`. The drawn states are part of this spec: where prose and
mockup disagree, the mockup wins.

## Scope decisions (from design session)

- **All three surfaces in one phase**, sequenced as surface verticals:
  Browse wave, then Organize wave, then Ingest wave. Each wave lands its
  service verbs + CLI/MCP parity + GUI surface before the next starts.
- **Browse is catalog-only.** No Kyno-style browse-without-import; offline
  drives browse identically to online ones (the NeoFinder strength).
  Layout: variant A — a persistent volumes/folders tree pane; any selected
  node shows its **entire subtree flattened** into one grid ("Flatten
  subfolders" on by default; off shows direct children only).
- **Keyframe-image extraction is built this phase** (closes the standing
  watchlist deferral): real frame images in the blob store, feeding the
  Browse hover-scrub filmstrip, the Inspector, and the MCP keyframes
  resource that today serves only timestamp manifests.
- **Ingest is a single job with live progress** — plan → run → done, with
  crash-recovery resume. The multi-job queue is explicitly deferred.
- **Organize manages structure AND assignment**: PARA tree
  (add/rename/archive with dry-run preview) and tag manager (rename/merge)
  on one two-column surface; bulk assignment (tag N assets, file N assets
  to a PARA node) happens via a selection toolbar in the Browse and Search
  grids, where the assets are.
- **Tag rename/merge are first-class CRDT ops** (user decision over
  composing per-asset ops): one new `Op` variant with alias-map projection
  semantics, extending `sample_ops()` and the proptest generator per the
  standing invariant.
- The menu-bar indicator + indexing throttle stays deferred; it is
  independent of these surfaces.
- Sidebar order for the phase: Search, Browse, Ingest, Organize, Volumes.
  No dead buttons at any point: each surface's button appears in the PR
  that ships the surface.

## Architecture

### Wave 1 — Browse + keyframe images

**Keyframe-image extraction** (`crates/index`): a new derivation —
`KeyframeImage` — extracted per manifest timestamp once an asset has a
`KeyframeManifest`. ffmpeg seeks to each timestamp and writes a
thumbnail-scale JPEG (longest edge 320px, same scale class as thumbnails)
into the blob store, keyed like every other derivation. The planner gains
a pass gated on the manifest's existence and the source volume being
online (offline sources are counted, not errored, like every other pass);
`index status` names the new derivation's counts. ffmpeg is already a
baseline requirement on every platform — no Apple seam, no
`PlatformUnavailable` sibling needed.

**MCP**: `majestical://keyframes/{asset_id}` keeps serving the manifest,
now with a blob reference per extracted frame;
`majestical://keyframes/{asset_id}/{index}` serves the frame image itself.
An agent can finally SEE a keyframe — the same images the GUI scrubs.

**Service verbs** (`crates/services/src/browse.rs`, new):

- `browse_tree(app, catalog_dir) -> BrowseTreeOutcome` — every volume
  (id, label, online state) with its folder hierarchy derived from
  cataloged instance paths, each node carrying a recursive asset count.
  Computed from the projection/SQLite the same way volumes/search read
  today; read-only, no sink.
- `browse_list(app, catalog_dir, req) -> BrowseListOutcome` — request
  carries volume, path prefix, `flatten: bool`, sort key (captured date ↓
  default, name, size), optional kind filter, and a `limit`/`offset` pair.
  Outcome rows reuse the search result row shape (thumbnail hash, name,
  kind, size, volumes, timestamped-video flag) plus the outcome's total
  count and any degradation notices (offline volumes named, verbatim).

**GUI**: `BrowseView.svelte` per the mockup — tree pane (fourth grid
column; collapses to a narrow strip when the inspector is open and the
window is narrower than 1100px), breadcrumb, toolbar chips (flatten toggle,
sort, kind), count line + notices, and the existing card grid. Hover on a
video card shows the filmstrip: mouse-x maps to the keyframe index, the
frame renders via `thumb://` (which learns to serve keyframe blobs), a
timecode chip shows the manifest timestamp; click pins that frame in the
Inspector. Selection in wave 1 is single-click driving the Inspector,
exactly like Search; ⌘-click/shift-click multi-select lands with the
selection toolbar in wave 2 (PR 4), in both Browse and Search at once.
The Inspector is unchanged except its keyframe strip becomes images.

### Wave 2 — Organize + tag ops + assignment

**The CRDT op** (`crates/core`): one new variant,
`Op::TagRenamed { from: String, to: String }` — merge is a rename whose
target already has assets. Projection keeps a tag-alias map: an asset's
effective tags resolve every add through the alias chain with a
visited-set cycle guard; two renames of the same `from` resolve LWW by
HLC (the standing rule for scalar conflicts). Convergence is
order-independent: a concurrent `TagAdded { tag: from }` lands as `to` no
matter which event arrives first. `sample_ops()` and the proptest op
generator are extended in the same commit that adds the variant, and the
CRDT proptests (commutativity, associativity, idempotence) must cover
alias chains and two-rename cycles.

**Service verbs** (`crates/services/src/tags.rs` + `para.rs`):

- `tags_list -> TagsListOutcome` — every live tag with asset count and
  last-used timestamp (renamed-away tags resolve to their target and
  disappear from the list).
- `tag_rename(from, to)` / `tag_merge(from, into)` — both emit
  `TagRenamed`; merge additionally validates the target exists.
- `tags_assign(assets, tags)` — bulk add: one `TagAdded` per (asset, tag);
  outcome reports per-asset results as rows (unknown asset = row, not
  abort).
- `para_file(assets, node)` — one `asset_para_set` per asset (the op
  exists; only ingest emits it today). Catalog metadata only — no files
  move.

**CLI/MCP parity**: `maj tags list`, `maj tag rename <from> <to>`,
`maj tag merge <from> <into>`, `maj para file <node> <asset>...`; MCP
tools `list_tags` (read, no confirm) and `rename_tag`, `merge_tags`,
`tag_assets`, `file_assets` (mutating: `confirm` defaulting to dry-run,
previews describing real read state — e.g. merge names the real asset
count it will rewrite).

**GUI**: `OrganizeView.svelte` per the mockup — left column PARA tree
grouped under the four kinds with counts, node detail card
(Rename / Archive…), "+ New node"; right column tag list with counts,
filter box, client-side near-duplicate hint (≈), tag detail card
(Rename / Merge into…). Archive… opens the dry-run preview modal backed by
the existing archive verb's `dry_run` (the modal lists the real moves read
from disk, then one explicit confirm — archive is the one Organize action
that moves files). The selection toolbar (`SelectionBar.svelte`) mounts in
Browse and Search: N selected, "Tag…" (picker: existing tags + create),
"File to node…" (PARA picker), Clear; both actions report per-asset
results and surface failures as rows.

### Wave 3 — Ingest + the progress seam

**The progress seam** (`crates/ingest` engine + `crates/services`):
`run_ingest` gains a `progress: &mut dyn FnMut(ProgressEvent)` alongside
the existing `notice` callback — a signature change, applied to all
callers (replace, don't deprecate). `ProgressEvent` carries: run started
(run id, totals from the plan), file copy started / bytes advanced / file
verified / file placed / file failed (each with destination attribution),
and per-destination running tallies. Cancellation: an `Arc<AtomicBool>`
token the engine checks between files — "Stop after current file"
semantics, which the journal already makes safe (a stopped run is a
resumable run). The CLI passes a no-op progress callback this phase
(rendering a progress line is a deferred nicety, recorded on the
watchlist); MCP progress notifications stay on the watchlist as before.

- `ingest_unfinished(catalog_dir) -> UnfinishedRunsOutcome` (new read
  verb): resumable runs from the transfer journal (run id, files placed /
  planned, source, destinations) — feeds the resume banner and gives
  agents `maj ingest --resume` discovery (`maj ingest unfinished`, MCP
  `list_unfinished_ingests`).

**GUI** (`IngestView.svelte` per the mockup, three states):

- *Setup*: native folder pickers for source and destinations, PARA node
  picker, subfolder template field; the plan panel runs the existing plan
  verb and shows counts/bytes/duplicates/rejects + notices verbatim. Start
  is disabled until source + ≥1 destination + node are set and the plan is
  current — any edit stales the plan back to "Plan again".
- *Running*: the Tauri command spawns the run on a worker thread
  (honoring the `run_off_tokio_runtime` rule — read its doc comment)
  and forwards `ProgressEvent`s as Tauri events keyed by run id; the
  surface renders the overall bar, now-copying rows, per-destination
  tallies (a failure reddens a counter, the run continues), and the run id
  with its resume affordance from the first second — the same id
  `maj ingest --resume` takes, so a GUI run can be finished from the CLI
  and vice versa. Leaving the surface does not cancel the run; the sidebar
  shows a running marker on the Ingest entry.
- *Done / resume*: completion card states exactly what exists — placed
  counts from real rows, per-destination MHL generations, failures listed
  with reasons and quarantine state, "Re-copy failed…" re-planning just
  the failed set. On surface load, `ingest_unfinished` populates the
  resume banner.

### Shared shell changes

`App.svelte` gains the three surfaces in sidebar order Search, Browse,
Ingest, Organize, Volumes; the Inspector is fed by Browse as well as
Search (selection clears when leaving a surface, as today, except a
running ingest keeps its state — the run lives in the Tauri backend, not
the component). Every new Tauri command stays a one-liner over a tested
`*_impl` taking `CatalogCfg`, and every new outcome struct gets wire
fixtures on both sides in the same PR (the #92 mechanism) — a command
without fixtures is unpinned and does not merge.

## Error handling

Phase-6 polarity doctrine, unchanged, applied to the new verbs: per-item
failures are rows inside successful outcomes (a file that fails to copy, an
unknown asset in a bulk assign, a keyframe whose extraction fails); only
operator-fixable or total failures become hard errors, always with partial
progress attached. New mutating verbs whose notices sink is local to the
call attach it on `Err` via `Notices::attach_on_err` (the 7C carrier); the
browse/tags/unfinished read verbs surface degradation as outcome notices
(offline volumes named with counts, never silently thinner results).
Keyframe extraction failures are ordinary per-item derivation failures —
counted, named in `index status`, never a panic. The ingest surface never
claims completion beyond what the outcome rows prove.

## Testing

TDD throughout (RED → GREEN per task, as every phase). Specifics this
phase pins:

- **CRDT**: `sample_ops()` + proptest generator extended with `TagRenamed`
  in the same commit; property tests cover alias chains, merge-into-
  existing, concurrent rename LWW, and rename cycles (a→b ∥ b→a) —
  convergence and the cycle guard are proptest properties, not examples.
- **Counters**: every new planner/outcome counter follows the two-asset
  rule — two assets per bucket, exact `assert_eq!` per counter (the 7C
  mutants lesson, applied at authoring time).
- **Progress seam**: engine tests assert the event sequence for a
  multi-file multi-destination run including a mid-run failure and a
  cancellation between files (events up to the stop, journal resumable
  after).
- **Wire**: one Rust fixture test + one TS assignment per new outcome
  struct (browse tree/list, tags list, unfinished runs, progress event
  payload, assignment outcomes), regenerated via `MAJ_UPDATE_FIXTURES=1`.
- **Parity**: `services_parity.rs` and `tauri_parity.rs` gain rows for the
  new verbs; MCP dry-run previews are asserted against real fixture state.
- **GUI**: vitest component tests per new component (tree, toolbar chips,
  selection bar, modal, ingest states driven by synthetic progress
  events); `commands.rs` impls tested against real fixture catalogs.
- **Phase close**: scoped cargo-mutants runs over the new modules —
  FOREGROUND, one at a time, per the standing mandate — with triage
  recorded on the watchlist.
- The manual `tauri dev` smoke is repeated by hand if `lib.rs` plugin
  registration or `tauri.conf.json` changes (the ingest event channel is
  plain Tauri events — no new plugin expected).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **PR 1 (Wave 1)**: keyframe-image extraction — derivation, planner
   pass, `index status` counts, MCP resource images, `thumb://` serving
   keyframe blobs; Inspector strip becomes images.
2. **PR 2 (Wave 1)**: `browse_tree`/`browse_list` verbs + CLI (`maj
   browse tree|list --json`) + MCP tools + wire fixtures + the Browse
   surface with filmstrip hover-scrub.
3. **PR 3 (Wave 2)**: `Op::TagRenamed` + projection alias map + proptests;
   `tags_list`/`tag_rename`/`tag_merge`/`tags_assign`/`para_file` verbs +
   CLI/MCP parity.
4. **PR 4 (Wave 2)**: the Organize surface + archive dry-run modal + the
   selection toolbar in Browse and Search.
5. **PR 5 (Wave 3)**: the progress seam (`ProgressEvent`, cancellation
   token, engine tests), `ingest_unfinished` + CLI/MCP parity.
6. **PR 6 (Wave 3)**: the Ingest surface — three states, Tauri event
   forwarding, resume banner.
7. **Closing PR**: cargo-mutants triage, watchlist updates, spec as-built
   section, `HANDOFF-phase7E.md`.

## Deferred (watchlist items with this spec's attribution)

- The ingest queue (multiple pending jobs, reordering, persistence).
- CLI ingest progress rendering (the seam exists; the CLI passes a no-op).
- MCP long-running-tool progress notifications (carried from phase 7).
- Menu-bar indicator with indexing throttle (carried from phase 7).
- PARA-count click-through from Organize to a `para:`-filtered Search.
- Grid virtualization for very large flattened subtrees (browse_list
  paginates; the GUI loads incrementally — a windowed DOM is the deferred
  part).
- Hover-scrub frame prefetch tuning (first paint uses whatever blobs
  exist; no speculative extraction on hover).

## As-built (phase 7D)

What shipped, where it differs from the design above. Written as what IS,
not as a change log. Six PRs squash-merged after green CI on both matrix
legs, plus this closing one.

**PR #97 — keyframe images** (26 files, +4828/-142). `Derivation::
KeyframeImage { model_tag, timestamp_ms }` and `KeyframeImagesComplete`
join the blob layout; `extract_keyframe_webp` pulls one frame at
thumb scale; `plan_keyframe_images` is a planner pass of its own with its
own `index status` line and `--kinds keyframe-images` name; the MCP
resource returns image blocks, the `thumb://` protocol routes keyframe
blobs, and the Inspector strip renders them. The phase-7A watchlist
deferral this closes is marked closed there.

**PR #98 — Browse** (35 files, +4387/-71). `services::browse` with
`browse_tree` (every volume's folder tree, recursive per-folder asset
counts, offline volumes named in one collapsed notice) and `browse_list`
(scope, `kind:` filter, three sorts, limit/offset), rendered through
`search.rs`'s own `SearchHit` row so the GUI grid is shared. `maj browse
tree|list`, two MCP read tools, four wire fixtures, and `BrowseView` +
`Filmstrip` with hover-scrub over the keyframe images PR #97 made real.

**PR #99 — the tag CRDT op** (20 files, +3551/-104). `Op::TagRenamed` is
the phase's one new op variant — `sample_ops()` enumerates it and the
proptest generator produces it, per the standing absence assertion. The
projection folds renames into an alias map with a resolve rule (chains
collapse to their final target; the verbs refuse to create a cycle at
emit time, and the read path's bounded walk terminates on any cycle a
concurrent merge constructs anyway). On top of it: `tags_list`, `tag_rename`, `tag_merge`,
`tags_assign`, `para_file`, each at all three heads.

**PR #100 — Organize** (39 files, +4747/-59). Two columns (PARA tree,
tag vocabulary), the archive dry-run modal, and `SelectionBar` in Browse
and Search so a grid selection can be tagged or filed in one call.

**PR #101 — the progress seam** (15 files, +1700/-43). `ProgressEvent`
(seven variants) and a cancel flag reach the engine as one `RunControl`
parameter; `ingest_unfinished` reads this machine's run journals and
lists resume candidates newest-first.

**PR #102 — the Ingest surface** (33 files, +5182/-32). Three states
(setup board, live run, completion card) plus the resume banner, with the
run owned by the backend and rejoined on mount. Manual smoke passed
2026-08-19 and is recorded on the PR — the standing rule that a GUI
surface is not done until a human has driven it.

**Closing PR** — parity rows for the phase's verbs in both harnesses, the
scheduled deletion of Task 4's keyframe-images normalization, four scoped
`cargo-mutants` runs with every survivor triaged, and this section.

### Deviations from the design above

**`Touched::Tag` carries no payload, and a tag rename rewrites the whole
table** (PR #99, plan Tasks 10-11). The plan's Task 11 recipe was a
targeted refresh of the affected rows. A rename can move every asset's
effective tags at once, so the invalidation payload would have been "all
of them" in the general case; `Touched::Tag` triggers `rebuild_tags` —
one whole-table rewrite — instead, which is simpler and, at catalog
sizes this phase targets, faster than computing the affected set.

**The bulk-assign verb is `assign_tags`, not `tag_assets`** (plan's own
AMENDED note, Task 12). `tag_assets` already names the single-asset
op-dispatch tool (`add`/`rm`/`confirm_suggestion`/`reject_suggestion`), a
different parameter shape entirely.

**The engine takes one `RunControl`, not two flat parameters** (PR #101,
plan's AMENDED note on Task 17). `run()` already took six arguments;
`progress` and `cancel` as separate parameters would trip
`clippy::too_many_arguments` on a public API, landable only behind an
`#[expect]`. Bundled instead, following the `RunEngineArgs` precedent one
layer up.

**`plan_ingest` takes no destinations** (PR #102). The planner needs the
source and the dedupe mode; destinations are a run-time concern, and
asking the preview for them made the setup board demand a destination
before it could show the user what was on the card.

**The journal gained a `RunStarted` record, and the snapshot version went
to 8** (PRs #101, #102). Nothing else in a journal knows how big a run
set out to be — `FilePlanned` only covers files a worker reached, so a
run cancelled with entries still queued looked complete. `RunStarted`
records the run id, source, destinations and intended file count; the
fold keeps the FIRST one, so a resume's own record does not overwrite the
original run's identity. `SNAPSHOT_VERSION` is 8
(`crates/catalog-sqlite/src/lib.rs:75`): the tag-alias projection changes
what a snapshot means, and a stale snapshot is rebuilt rather than
migrated.

**Mounted roots come from a new `list_mounted_roots` command, not from
the archive dry run** (PR #100, plan's AMENDED note on Task 15). Deriving
them from the dry run is circular — the dry run takes roots as its input
— and nothing in the catalog records where a node was materialized. The
new read command answers from `volume_identity::mounted_volumes()`, so
the modal previews against what is mounted right now.

**Resume re-asks for the PARA node** (PR #102). `UnfinishedRun` carries
run id, counts, source and destinations, all from `RunStarted` — not the
node the run was filing into. One-click resume needs that field on the
journal record first; it is on the watchlist.

**`migrate_legacy` runs on read paths too** (PR #101). Resolving a
catalog's state dir migrates pre-phase-4 derived files out of the sync
root, and `ingest_unfinished` resolves the state dir like every other
verb — so a read verb can perform a one-time move. That is deliberate
(the migration must happen before anything reads `runs/`), and it is why
`catalog_paths` takes a notices sink: the move announces itself.

**The GUI captures a run id by parsing a notice line** (PR #102). The
engine reports its run id in a notice of the form `run <id> …`;
`run_id_from_notice` (`apps/desktop/src-tauri/src/ingest.rs:270`) strips
that prefix. This makes the notice's leading token a wire contract, not
just prose — a test pins it, and the format must not be reworded without
updating both.

**`run_stopped` is not the outcome** (PR #102). The progress stream's
terminal event says the run thread finished; what it finished WITH comes
from a separate `ingest_state` poll that hands out the finished
`Outcome`. Treating the event as the result was the bug the review
caught: a cancelled run and a completed one both emit it.

### Review-loop shape

The plan carries five in-place AMENDED notes (Tasks 10/11, 12, 15, 17,
and the Task 11 reduction), each written where the task lives rather than
appended as errata. That convention worked: an implementer reading the
task read the correction first, and the closing agent could reconstruct
every deviation from the plan alone. Reviewer mutation probes — applying
a hand-written mutant and watching the test still pass — caught two
generator-lucky tests during the phase; the same technique found four
more survivors in this PR's `cargo-mutants` triage.
