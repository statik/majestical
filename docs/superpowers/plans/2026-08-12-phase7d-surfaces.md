# Phase 7D — Browse / Ingest / Organize Surfaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the three remaining GUI surfaces (Browse, Ingest, Organize) with the four backend seams they need — keyframe-image extraction, browse read verbs, the `TagRenamed` CRDT op + organize verbs, and the ingest progress seam — each with CLI + MCP parity.

**Architecture:** Three surface verticals in seven chunk PRs. Each vertical lands services verbs first (TDD against fixture catalogs), then CLI/MCP parity, then the Tauri commands with wire fixtures pinned on both sides, then the Svelte surface. Spec: `docs/superpowers/specs/2026-08-12-phase7d-surfaces-design.md` — the committed mockups under `docs/superpowers/specs/mockups/2026-08-12-phase7d/` are normative for the GUI.

**Tech Stack:** Rust workspace (`crates/*`), Tauri 2 + Svelte 5 + Vite (`apps/desktop`), rusqlite, ffmpeg via subprocess, vitest, cargo test.

---

## Standing mandates (verbatim from the handoff — every task inherits these)

1. Implementers stage ONLY their files; never `git add -A`. `trash`, never `rm -rf`.
2. NO Claude-Session trailers in commit messages. Do NOT use the `submitting-changes` skill.
3. Zero warnings on BOTH `cargo clippy --all-targets --all-features -- -D warnings` legs (macOS dev machine and, mentally, the ubuntu leg: anything cfg-gated needs `#[expect]` with a reason where a lint fires on one leg only).
4. `cargo-mutants` runs FOREGROUND, one at a time — no `run_in_background`, no monitors, no sleep-polling.
5. Verbs in `crates/services` never print (`print_stdout`/`print_stderr` denied); diagnostics go to the notices sink; mutating verbs whose sink is local attach it on `Err` via `Notices::attach_on_err` (`crates/services/src/notices.rs:55`).
6. Polarity doctrine: per-item failures are rows in successful outcomes; only operator-fixable/total failures are hard errors, always with partial progress attached.
7. Every new Tauri command: one-liner over a `*_impl` taking `CatalogCfg`, plus a wire fixture on BOTH sides (`src-tauri/tests/wire_fixtures.rs` + `src/lib/fixtures.test.ts`) in the same PR.
8. Two-asset rule for every new counter: tests drive two assets per bucket and `assert_eq!` exact counts.
9. `git fetch origin` before reviewing/rebasing (SSH-less remote goes stale). Push via `git -c credential.helper='!gh auth git-credential' push https://github.com/statik/majestical.git <branch>`.

## File structure (created/modified across the phase)

```
crates/index/src/blob.rs             # + Derivation::KeyframeImage / KeyframeImagesComplete
crates/index/src/keyframe_images.rs  # NEW: extract_keyframe_webp
crates/index/src/work.rs             # + WorkKind::KeyframeImages, plan_keyframe_images, WorkPlan.keyframe_images
crates/services/src/index/run.rs     # + run arm extracting keyframe images
crates/services/src/index/mod.rs     # + kind name, status lines
crates/services/src/browse.rs        # NEW: browse_tree, browse_list
crates/core/src/event.rs             # + Op::TagRenamed
crates/core/src/projection.rs        # + tag alias map, Touched::Tag, resolution
crates/catalog-sqlite/src/apply.rs   # + Touched::Tag arm
crates/services/src/tags.rs          # + tags_list, tag_rename, tag_merge, tags_assign
crates/services/src/para.rs          # + para_file
crates/ingest/src/engine.rs          # + ProgressEvent, progress+cancel in run()
crates/services/src/ingest.rs        # + progress/cancel on ExecuteIngest path, ingest_unfinished
crates/cli/src/main.rs               # + browse/tags/para/ingest-unfinished verbs
crates/cli/src/commands.rs           # + their renderers
crates/cli/src/mcp_cmd/…             # + tools + keyframe image resource
apps/desktop/src-tauri/src/commands.rs   # + one command per new verb + ingest job state
apps/desktop/src-tauri/src/lib.rs        # + handler registrations (thumb:// route grows)
apps/desktop/src-tauri/src/thumb_protocol.rs  # + keyframe image route
apps/desktop/src/lib/api.ts          # + interfaces + wrappers
apps/desktop/src/lib/BrowseView.svelte    # NEW (+ .test.ts)
apps/desktop/src/lib/Filmstrip.svelte     # NEW (+ .test.ts)
apps/desktop/src/lib/OrganizeView.svelte  # NEW (+ .test.ts)
apps/desktop/src/lib/SelectionBar.svelte  # NEW (+ .test.ts)
apps/desktop/src/lib/IngestView.svelte    # NEW (+ .test.ts)
apps/desktop/src/App.svelte          # sidebar order Search, Browse, Ingest, Organize, Volumes
```

Branch naming: `phase7d-pr1` … `phase7d-pr7`, each branched from fresh `main` after the previous merge. The spec branch `phase7d-spec` merges as part of PR 1.

---

## PR Chunk 1 — keyframe images: derivation, planner pass, runner, both heads' serving

### Task 1: `Derivation::KeyframeImage` + `KeyframeImagesComplete` blob paths

**Files:**
- Modify: `crates/index/src/blob.rs` (enum at :18, `path_for` at :150, tests at bottom)

- [ ] **Step 1: Write the failing test** (in `blob.rs`'s existing `#[cfg(test)] mod tests`)

```rust
#[test]
fn keyframe_image_paths_are_model_scoped_and_per_timestamp() {
    let store = BlobStore::new(std::path::Path::new("/tmp/blobs"));
    let hex = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let a = store.path_for(hex, &Derivation::KeyframeImage { model_tag: "m1", timestamp_ms: 1500 });
    let b = store.path_for(hex, &Derivation::KeyframeImage { model_tag: "m1", timestamp_ms: 2500 });
    assert_ne!(a, b, "one image per timestamp");
    assert!(a.ends_with(format!("m1/kf-img-{THUMB_EDGE}-1500.webp")), "got {}", a.display());
    let done = store.path_for(hex, &Derivation::KeyframeImagesComplete { model_tag: "m1" });
    assert!(done.ends_with("m1/keyframe-images-complete.json"), "got {}", done.display());
}
```

(If `BlobStore::new` is named differently, use the constructor the neighboring tests in this file use — copy their setup line.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p majestical-index --lib keyframe_image_paths -- --nocapture`
Expected: compile error — no `KeyframeImage` variant.

- [ ] **Step 3: Add the variants and `path_for` arms**

In the `Derivation` enum, after `KeyframeManifest`:

```rust
    /// One extracted keyframe image (thumb-scale WebP) at a manifest
    /// timestamp. Scoped to the manifest's model tag: the timestamps are
    /// the manifest's, so the images live and die with it.
    KeyframeImage {
        model_tag: &'a str,
        timestamp_ms: u64,
    },
    /// Marker written once EVERY timestamp in the manifest has its
    /// `KeyframeImage` blob (mirrors [`Derivation::OcrComplete`]). Images
    /// without this marker mean an interrupted run: the item re-plans and
    /// existing images make the retry cheap.
    KeyframeImagesComplete {
        model_tag: &'a str,
    },
```

In `path_for`, after the `KeyframeManifest` arm:

```rust
    Derivation::KeyframeImage { model_tag, timestamp_ms } => {
        dir.join(model_tag).join(format!("kf-img-{THUMB_EDGE}-{timestamp_ms}.webp"))
    }
    Derivation::KeyframeImagesComplete { model_tag } => {
        dir.join(model_tag).join("keyframe-images-complete.json")
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p majestical-index --lib keyframe_image_paths`
Expected: PASS. Then `cargo clippy -p majestical-index --all-targets -- -D warnings` — fix anything.

- [ ] **Step 5: Commit**

```bash
git add crates/index/src/blob.rs
git commit -m "feat: KeyframeImage + KeyframeImagesComplete derivations"
```

### Task 2: `extract_keyframe_webp` — one frame, thumb-scale WebP

**Files:**
- Create: `crates/index/src/keyframe_images.rs`
- Modify: `crates/index/src/lib.rs` (add `pub mod keyframe_images;` in alphabetical order)

- [ ] **Step 1: Write the failing test.** This is a composition of two proven pieces (`video::extract_frame` at `video.rs:217`, `thumbs::thumbnail_webp` at `thumbs.rs:93`), so the unit test uses a real tiny video only where the suite already does — find the existing fixture-video helper used by `video.rs`'s tests (`rg -n "fn test_video|fixture" crates/index/src/video.rs`) and reuse it. If those tests gate on ffmpeg presence, gate the same way.

```rust
// in keyframe_images.rs
#[cfg(test)]
mod tests {
    // generate_test_clip: same synthesis as
    // crates/index/tests/video_e2e.rs::generate_test_clip, but at 640x360
    // (double THUMB_EDGE) so thumbnail_webp's resize branch is actually
    // exercised instead of a same-size pass-through — three 3s lavfi color
    // segments (red, green, blue) concatenated, at 25fps.

    #[test]
    fn extracted_frame_is_webp_at_thumb_scale() {
        if !crate::video::ffmpeg_available() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let video_path = dir.path().join("clip.mp4");
        generate_test_clip(&video_path); // 640x360, red/green/blue, 3s each

        // 4500ms lands mid-way through the green segment (video_e2e.rs
        // extracts at the same timestamp for the same reason) — a `ts_ms`
        // silently swapped for a constant would decode the red segment
        // instead, which the color-dominance assertion below catches.
        let bytes = super::extract_keyframe_webp(&video_path, 4500).expect("frame at 4500ms");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let img = image::load_from_memory(&bytes).expect("decode");
        // Exact dims, not just "under the cap" (the convention thumbs.rs's
        // own test uses) — 640x360 source at THUMB_EDGE (320) longest-edge
        // scale must land on exactly (320, 180).
        assert_eq!((img.width(), img.height()), (320, 180));

        let rgb = img.to_rgb8();
        let center = rgb.get_pixel(rgb.width() / 2, rgb.height() / 2);
        let (r, g, b) = (u16::from(center[0]), u16::from(center[1]), u16::from(center[2]));
        assert!(
            g > r + 50 && g > b + 50,
            "expected the 4500ms frame's center pixel to be green-dominant, got {center:?}"
        );
    }
}
```

(`generate_test_clip` mirrors `video_e2e.rs`'s synthesis exactly, scaled to 640x360 — copy that synthesis rather than reusing a smaller fixture, since the resize-branch and color-dominance assertions above both depend on it.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index --lib extracted_frame_is_webp`
Expected: compile error — module/function missing.

- [ ] **Step 3: Implement**

```rust
//! Keyframe-image extraction: one thumb-scale WebP per manifest timestamp.
//! Pure composition — ffmpeg frame decode (`crate::video::extract_frame`)
//! into the thumbnail encoder (`crate::thumbs::thumbnail_webp`).

use std::path::Path;

use crate::error::IndexError;

/// Extracts the frame at `ts_ms` and encodes it at thumbnail scale.
///
/// # Errors
/// Returns [`IndexError::Video`] if ffmpeg fails or produces no frame, or
/// [`IndexError::Resize`] if downscaling fails — ordinary per-item failures.
pub fn extract_keyframe_webp(path: &Path, ts_ms: u64) -> Result<Vec<u8>, IndexError> {
    let frame = crate::video::extract_frame(path, ts_ms)?;
    crate::thumbs::thumbnail_webp(&frame)
}
```

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p majestical-index --lib keyframe_images && cargo clippy -p majestical-index --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/index/src/keyframe_images.rs crates/index/src/lib.rs
git commit -m "feat: extract_keyframe_webp composes frame decode + thumb encode"
```

### Task 3: planner pass `plan_keyframe_images`

**Files:**
- Modify: `crates/index/src/work.rs` — `WorkKind` (:32), `WorkPlan` (:118), `plan_work` (:183), new `plan_keyframe_images` next to `plan_ocr_keyframes` (:530), tests

The pass mirrors `plan_ocr_keyframes` (`work.rs:530-573`) exactly, with these substitutions: completion marker `Derivation::KeyframeImagesComplete { model_tag }` (the MANIFEST's model tag, not a separate one), counts land in a new `WorkPlan::keyframe_images: KindStatus`, the item kind is `WorkKind::KeyframeImages`, and there is no `AVAILABLE` platform gate (ffmpeg is the only capability, all platforms).

- [ ] **Step 1: Write the failing tests** (two-asset rule — copy the arrange helpers from `plan_keyframes_counts_done_offline_and_needs_ffmpeg` at `work.rs:1093`, which already builds manifest blobs):

```rust
#[test]
fn plan_keyframe_images_counts_every_bucket_with_two_assets_each() {
    // Arrange, using the same helpers as the ocr-keyframes planner tests:
    // - two video assets WITH manifest + KeyframeImagesComplete marker -> done
    // - two WITH manifest, online, ffmpeg on                          -> pending (+2 items)
    // - two WITH manifest but abs_path None                           -> offline
    // - two WITH manifest, ffmpeg off                                 -> needs_ffmpeg
    // - two WITHOUT any manifest                                      -> not counted anywhere
    // Assert exact counts on plan.keyframe_images: done == 2,
    // pending == 2, offline == 2, needs_ffmpeg == 2, and exactly two
    // WorkKind::KeyframeImages items whose asset ids match the pending pair.
}
```

Write the body for real — every bucket driven by two assets, every counter asserted with `assert_eq!`, mirroring the arrange code of the OCR-keyframes test one screen up. Also add:

```rust
#[test]
fn plan_keyframe_images_requires_a_manifest_not_a_model() {
    // caps.model_tag = Some(...) is required only to LOCATE the manifest.
    // An asset with a manifest and no other capability gates plans pending
    // even when whisper/text_model/describer are all absent.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p majestical-index --lib plan_keyframe_images`
Expected: compile errors (no variant, no field).

- [ ] **Step 3: Implement** — add `KeyframeImages` to `WorkKind` (doc comment: `/// A video whose keyframe manifest is ready -> per-timestamp image blobs.`), add `pub keyframe_images: KindStatus` to `WorkPlan`, write the pass:

```rust
/// KEYFRAME IMAGES (`MediaKind::Video` only): manifest present -> diff the
/// completion marker. No platform gate — ffmpeg is the one capability.
fn plan_keyframe_images(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let Some(model_tag) = &caps.model_tag else {
        return;
    };
    let manifest_path = blobs.path_for(hex, &Derivation::KeyframeManifest { model_tag });
    if !manifest_path.is_file() {
        return;
    }
    let done_path = blobs.path_for(hex, &Derivation::KeyframeImagesComplete { model_tag });
    if done_path.is_file() {
        plan.keyframe_images.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.keyframe_images.offline += 1;
        return;
    };
    if !caps.ffmpeg {
        plan.keyframe_images.needs_ffmpeg += 1;
        return;
    }
    plan.keyframe_images.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::KeyframeImages,
    });
}
```

Call it from `plan_work` as its own pass, ordered directly AFTER the keyframes pass (images depend on the manifest the keyframes pass produces) and before transcribe. Update `plan_work`'s doc comment: "Ten passes" becomes "Eleven passes" and the priority list gains "keyframe images" after keyframes.

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p majestical-index --lib plan_ && cargo clippy -p majestical-index --all-targets -- -D warnings`
Expected: all planner tests PASS (existing ones must not regress), no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/index/src/work.rs
git commit -m "feat: planner pass for keyframe-image extraction"
```

### Task 4: runner arm + `index status` naming

**Files:**
- Modify: `crates/services/src/index/run.rs` (split at :350, run arms nearby)
- Modify: `crates/services/src/index/mod.rs` (kind name map at :129, status rendering)
- Test: extend the existing `index run`/`index status` service tests (find them: `rg -n "keyframes" crates/services/src/index/run.rs | head`) — they build real manifests against temp catalogs.

- [ ] **Step 1: Write the failing service test**: an asset with a keyframe manifest of two timestamps; `index run` produces two `kf-img-*-*.webp` blobs + the completion marker; a second `plan_work` counts it done. A manifest with one extractable and one unreadable timestamp (point the asset at a truncated video after manifest creation, the trick the failure-record tests use) records a per-item failure row, writes NO completion marker, and the item re-plans.

- [ ] **Step 2: Run to verify failure** — `cargo test -p majestical-services --lib keyframe_image` — fails: no runner arm.

- [ ] **Step 3: Implement the runner arm.** In `run.rs`'s split (`:350`), route `WorkKind::KeyframeImages` to its own bucket; the execution function reads the manifest JSON exactly the way the OCR-keyframes runner does (same file, search `KeyframeManifest` read), loops timestamps, skips images whose blob already exists (`path_for(...).is_file()` — resume cheapness), calls `majestical_index::keyframe_images::extract_keyframe_webp`, writes via the blob store's tempfile-then-rename writer (the same helper every other runner arm uses), and writes `KeyframeImagesComplete` (JSON body `{"count": N}`) ONLY when every timestamp has a blob. Per-timestamp failures become the same failure-record rows the OCR arm produces. In `mod.rs:129`'s kind-name map add `WorkKind::KeyframeImages => "keyframe-images"`, and give the status output a line for the new `KindStatus` following the existing per-kind lines (same struct, same rendering helper).

- [ ] **Step 4: Run** `cargo test -p majestical-services --lib index && cargo clippy -p majestical-services --all-targets -- -D warnings` — PASS, no warnings. Also run `cargo test -p majestical-cli` (the CLI status snapshot tests may pin status text — update them deliberately if they fail, they now name a real new derivation).

- [ ] **Step 5: Commit**

```bash
git add crates/services/src/index/run.rs crates/services/src/index/mod.rs
git commit -m "feat: index run extracts keyframe images; status names them"
```

### Task 5: serve the images — MCP resource + `thumb://` route + Inspector strip

**Files:**
- Modify: `crates/cli/src/mcp_cmd/resources.rs` (the `majestical://keyframes/{asset_id}` handler)
- Modify: `apps/desktop/src-tauri/src/thumb_protocol.rs`
- Modify: `apps/desktop/src/lib/thumb.ts`, `apps/desktop/src/lib/Inspector.svelte` (+ its test)

- [ ] **Step 1 (MCP): failing test.** In the MCP resource tests (same file or `crates/cli/tests/` — find with `rg -n "keyframes" crates/cli/src/mcp_cmd/resources.rs`): a catalog with a manifest and two image blobs lists/reads `majestical://keyframes/{asset}/0` and `/1` as `image/webp` blobs with the right bytes; index `2` and a malformed index return the resource-not-found error, not a panic; the bare manifest URI still serves the JSON and now includes `"images": [<one "majestical://keyframes/{asset}/{index}" URI per timestamp whose image blob exists>]` — the per-frame blob references the spec promises, so an agent can tell exactly what is servable before fetching.

- [ ] **Step 2: Run to verify failure**, then **Step 3: implement** the sub-path route in `resources.rs`, reusing `blob::asset_hex` for id validation (never join an unvalidated id into a path — the module already does this for thumbs) and `path_for(KeyframeImage { model_tag, timestamp_ms })` where `timestamp_ms` comes from the manifest's `timestamps[index]`.

- [ ] **Step 4 (GUI): failing vitest.** `thumb.ts` gains `keyframeImageUrl(assetId, index)` returning `convertFileSrc(`keyframe/${assetId}/${index}`, "thumb")`; `Inspector.test.ts` gains a case: an asset whose manifest fetch resolves with 2 timestamps renders 2 `<img>` elements with those URLs (today it renders timecode chips — the strip becomes images with the timecode as `title`/caption). Then implement: `thumb_protocol.rs` gains the `keyframe/{asset_id}/{index}` route mirroring its existing two routes (404 when the blob is absent; the manifest is read to map index → timestamp); `Inspector.svelte`'s `.strip` items become `<img class="kf" src={keyframeImageUrl(...)} alt={timecode} title={timecode}>` with the existing timecode text as the alt.

- [ ] **Step 5: Run everything for the chunk**

Run: `cargo test -p majestical-cli && (cd apps/desktop && pnpm vitest run && pnpm check)`
Expected: PASS across the board. (`pnpm check` = svelte-check/tsc, per `apps/desktop/package.json` — use the script name that exists there.)

- [ ] **Step 6: Commit, open PR 1**

```bash
git add crates/cli/src/mcp_cmd/resources.rs apps/desktop/src-tauri/src/thumb_protocol.rs apps/desktop/src/lib/thumb.ts apps/desktop/src/lib/Inspector.svelte apps/desktop/src/lib/Inspector.test.ts
git commit -m "feat: keyframe images served to MCP agents and the Inspector"
```

PR 1 = the `phase7d-spec` branch's docs commit + Tasks 1-5. Title: `feat: keyframe-image extraction, served at every head`. Body describes what is in the diff only. Squash-merge when CI is green (user mandate: merge when green, no per-PR ask).

---

## PR Chunk 2 — browse verbs + the Browse surface

### Task 6: `services::browse` — `browse_tree` and `browse_list`

**Files:**
- Create: `crates/services/src/browse.rs`
- Modify: `crates/services/src/lib.rs` (add `pub mod browse;`)

The verbs read the projection/SQLite the way `volumes.rs` and `search.rs` do — open with those two files side by side; `browse.rs` should feel like their sibling. Folder paths come from `AssetState::instances` keys (`(volume, path)` — `projection.rs:32`); a folder is every `/`-separated prefix of an instance path.

**Wire shapes (outcome structs — serde snake_case as-is, notices absent-when-empty like every outcome):**

```rust
#[derive(Debug, serde::Serialize)]
pub struct BrowseFolder {
    /// `/`-separated path relative to the volume root; "" is the root.
    pub path: String,
    /// Direct child folder names, sorted.
    pub children: Vec<String>,
    /// Assets in this folder's entire subtree (the Drilldown count).
    pub recursive_count: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct BrowseVolume {
    pub id: String,
    pub label: String,
    pub online: bool,
    pub folders: Vec<BrowseFolder>, // flat, sorted by path; GUI nests by path
}

#[derive(Debug, serde::Serialize)]
pub struct BrowseTreeOutcome {
    pub volumes: Vec<BrowseVolume>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrowseRequest {
    pub volume: String,
    /// "" for the volume root.
    pub path: String,
    pub flatten: bool,
    /// "captured" (default, newest first), "name", or "size".
    pub sort: Option<String>,
    /// Filter to one MediaKind name (the strings `media_kind` already parses).
    pub kind: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct BrowseListOutcome {
    /// Total matching assets before limit/offset.
    pub count: u64,
    /// Distinct folders contributing to `count` (the "across N folders" line).
    pub folder_count: u64,
    pub results: Vec<crate::search::SearchHit>, // reuse: score = 0.0, known = true
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}
```

Reusing `SearchHit` keeps the GUI grid identical to Search's. If constructing a `SearchHit` requires fields that make no sense here, populate them the way `search.rs:281` does for unknown assets — but `known: true` and real summaries; `score: 0.0` is documented as "browse has no ranking".

- [ ] **Step 1: Write failing tests** in `browse.rs`'s test module against a temp catalog (copy the fixture-catalog arrange used by `volumes.rs` tests): three assets on volume V at `A/x.mov`, `A/B/y.jpg`, `C/z.pdf`, one asset on offline volume W.
  - `browse_tree`: V has folders `""` (children `[A, C]`, recursive 3), `A` (children `[B]`, recursive 2), `A/B` (recursive 1), `C` (recursive 1); W present with `online: false` and its folders intact (offline browses identically).
  - `browse_list` V, path `A`, flatten true → count 2, folder_count 2; flatten false → count 1 (only `x.mov` directly in `A`).
  - sort `name` orders by name; `kind: Some("image")` filters to `y.jpg`.
  - limit/offset: limit 1 offset 1 returns the second row, count stays 2.
  - unknown volume → `Err` naming the volume and suggesting `maj volumes list` (operator-fixable, hard error).

- [ ] **Step 2: Run to verify failure** — `cargo test -p majestical-services --lib browse` — compile errors.

- [ ] **Step 3: Implement.** `browse_tree`: walk every asset's instances, bucket by volume, split paths on `/`, accumulate `BTreeMap<String, (BTreeSet<String>, u64)>` per volume (folder → children + recursive count: every prefix of every path gets +1), then flatten sorted. Volume label/online from the same source `volumes.rs` uses. `browse_list`: filter instances by volume + path prefix (`flatten` ? prefix match : exact parent match), dedupe by asset id (an asset with two instances under the scope appears once), sort, then fetch summaries through the same summary helper `search.rs:251` uses. Both verbs read-only: collect notices (offline volume named when the request's volume is offline) into the outcome, no sink gymnastics.

- [ ] **Step 4: Run** `cargo test -p majestical-services --lib browse && cargo clippy -p majestical-services --all-targets -- -D warnings` — PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/services/src/browse.rs crates/services/src/lib.rs
git commit -m "feat: browse_tree and browse_list service verbs"
```

### Task 7: CLI + MCP parity for browse

**Files:**
- Modify: `crates/cli/src/main.rs` (new `Browse` subcommand with `Tree`/`List` variants — mirror `ParaCmd` at :187), `crates/cli/src/commands.rs` (renderers), `crates/cli/src/mcp_cmd/` (two read tools, no `confirm`)
- Test: `crates/cli/tests/` — follow the existing per-verb e2e pattern (`rg -l "volumes list" crates/cli/tests/`)

- [ ] **Step 1: failing CLI e2e test**: `maj browse tree --json` on a fixture catalog prints the `BrowseTreeOutcome` JSON; `maj browse list --volume V --path A --json` prints `BrowseListOutcome`; non-`--json` renders a human table (folder, count) — assert on a stable line, not the whole table.
- [ ] **Step 2: run, verify failure.**
- [ ] **Step 3: implement** — clap variants `Tree { json: bool }` and `List { volume: String, path: Option<String>, no_flatten: bool, sort: Option<String>, kind: Option<String>, limit: Option<usize>, offset: Option<usize>, json: bool }` (default flatten ON per spec, so the flag is `--no-flatten`); renderers print the outcome, notices to stderr the way every read verb does.
- [ ] **Step 4: MCP tools** `browse_tree` and `browse_assets` in `mcp_cmd` — read tools, no `confirm` param, result = outcome struct as structured content (the established `tool_ok` path). Extend the MCP tool-list test that pins tool names/schemas (find: `rg -n "search_assets" crates/cli/src/mcp_cmd/ -l`).
- [ ] **Step 5: run all** `cargo test -p majestical-cli && cargo clippy -p majestical-cli --all-targets -- -D warnings` — PASS.
- [ ] **Step 6: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/src/mcp_cmd/ crates/cli/tests/
git commit -m "feat: maj browse tree|list + MCP browse tools"
```

### Task 8: Tauri commands + wire fixtures for browse

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands.rs`, `apps/desktop/src-tauri/src/lib.rs` (register), `apps/desktop/src-tauri/tests/commands.rs`, `apps/desktop/src-tauri/tests/wire_fixtures.rs`
- Modify: `apps/desktop/src/lib/api.ts`, `apps/desktop/src/lib/fixtures.test.ts` (+ generated `src/lib/fixtures/browse_tree.json`, `browse_list.json`)

- [ ] **Step 1: failing Rust test** in `tests/commands.rs`: `browse_tree_impl(cfg)` and `browse_list_impl(cfg, req)` against the fixture catalog return the same shapes Task 6's tests pinned. Then implement both impls + `#[tauri::command]` one-liners (`browse_tree`, `browse_list` — list takes the request fields as camelCase args, builds `BrowseRequest` with `DEFAULT_LIMIT` when omitted), register in `lib.rs`'s `generate_handler!`.
- [ ] **Step 2: wire fixtures.** Add builders in `wire_fixtures.rs` for fully-populated `BrowseTreeOutcome` and `BrowseListOutcome` (every Option `Some`, every Vec non-empty — the file's convention), run `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` to generate the JSONs, then add the two interfaces to `api.ts`:

```ts
/** `majestical_services::browse::BrowseFolder` */
export interface BrowseFolder {
  path: string;
  children: string[];
  recursive_count: number;
}
/** `majestical_services::browse::BrowseVolume` */
export interface BrowseVolume {
  id: string;
  label: string;
  online: boolean;
  folders: BrowseFolder[];
}
/** `majestical_services::browse::BrowseTreeOutcome` */
export interface BrowseTreeOutcome {
  volumes: BrowseVolume[];
  notices?: string[];
}
/** `majestical_services::browse::BrowseListOutcome` */
export interface BrowseListOutcome {
  count: number;
  folder_count: number;
  results: SearchHit[];
  notices?: string[];
}
```

and the wrappers:

```ts
  browseTree: () => invoke<BrowseTreeOutcome>("browse_tree"),
  browseList: (req: {
    volume: string;
    path: string;
    flatten: boolean;
    sort?: string;
    kind?: string;
    offset?: number;
  }) => invoke<BrowseListOutcome>("browse_list", req),
```

then assign both fixtures in `fixtures.test.ts` following its existing per-fixture blocks.
- [ ] **Step 3: run** `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml && (cd apps/desktop && pnpm vitest run && pnpm check)` — PASS.
- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src-tauri/src/commands.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/tests/ apps/desktop/src/lib/api.ts apps/desktop/src/lib/fixtures.test.ts apps/desktop/src/lib/fixtures/
git commit -m "feat: browse Tauri commands with pinned wire fixtures"
```

### Task 9: `BrowseView.svelte` + `Filmstrip.svelte` + sidebar entry

**Files:**
- Create: `apps/desktop/src/lib/BrowseView.svelte`, `BrowseView.test.ts`, `Filmstrip.svelte`, `Filmstrip.test.ts`
- Modify: `apps/desktop/src/App.svelte` (+ `App.test.ts`), `apps/desktop/src/app.css`

Normative mockup: `docs/superpowers/specs/mockups/2026-08-12-phase7d/browse.html`, variant A. Reuse existing CSS classes (`.grid`, `.card`, `.chip`, `.notice`, `.count`) — new classes prefixed `browse-` (tree pane: `.browse-tree`), per app.css's naming note at :112.

- [ ] **Step 1: failing component tests** (mock `api` via `test-support.ts`, same as `SearchView.test.ts`):
  - renders one tree section per volume with the offline badge on offline volumes;
  - selecting a tree node calls `api.browseList` with that volume/path and `flatten: true`;
  - the flatten chip toggles and re-queries with `flatten: false`;
  - count line renders `"{count} items across {folder_count} folders"` plus notices verbatim below it;
  - clicking a card fires `onselect` with the asset id (Inspector wiring, same contract as SearchView);
  - "Load more" appears when `results.length < count`, requests `offset: results.length`, and appends.
  - `Filmstrip.test.ts`: given 4 timestamps, pointer position at 60% of the width shows image index 2 (`floor(0.6 * 4)` clamped), with `keyframeImageUrl(asset, 2)` as the img src and the timecode rendered; no manifest → renders nothing (static thumb stays).
- [ ] **Step 2: run `pnpm vitest run` — FAIL.**
- [ ] **Step 3: implement.** BrowseView: `browseTree` on mount; tree pane renders volumes → nested folders (nest the flat `folders` client-side by path segments); state: `{volume, path, flatten, sort, kind, offset}`; grid maps `results` exactly like SearchView's cards; video cards (`timestamp_ms`/`source === "transcript"` is NOT the marker — use the same "is video" signal SearchView uses for its timecode chips, or `kind` if the hit carries it; whichever exists, keep it consistent with SearchView) wrap their thumb in `<Filmstrip>`. Filmstrip: fetches the manifest lazily on first pointerenter (`fetchKeyframes` from `thumb.ts`), tracks pointermove x → index, swaps the img src; pointerleave restores the thumb. Tree collapse: when `selected !== null` (inspector open) and `window.innerWidth < 1100`, the tree pane gets class `browse-tree-collapsed` (width 36px, labels hidden) — a plain CSS class + one `matchMedia` listener; App passes nothing new.
  App.svelte: `type Surface = "search" | "browse" | "ingest" | "organize" | "volumes"` — but ONLY add `"browse"` and its button in this PR (no dead buttons; ingest/organize buttons land with their surfaces). Browse participates in selection exactly like Search (`onselect` → `selected`).
- [ ] **Step 4: run** `(cd apps/desktop && pnpm vitest run && pnpm check && pnpm lint)` — PASS, zero warnings.
- [ ] **Step 5: Commit; open PR 2** (`feat: the Browse surface — tree, drilldown grid, filmstrip`).

```bash
git add apps/desktop/src/lib/BrowseView.svelte apps/desktop/src/lib/BrowseView.test.ts apps/desktop/src/lib/Filmstrip.svelte apps/desktop/src/lib/Filmstrip.test.ts apps/desktop/src/App.svelte apps/desktop/src/App.test.ts apps/desktop/src/app.css
git commit -m "feat: Browse surface with folder tree and hover-scrub filmstrip"
```

---

## PR Chunk 3 — `Op::TagRenamed` + organize verbs + parity

### Task 10: the CRDT op and alias-map projection

**Files:**
- Modify: `crates/core/src/event.rs` (Op enum :58 + wire-format test :204)
- Modify: `crates/core/src/projection.rs` (state, apply :242, `tags()` :435, `Touched` :20, `sample_ops` :601, proptest generator)

**Semantics (normative, from the spec):** `Op::TagRenamed { from, to }`. Projection holds `tag_aliases: BTreeMap<String, (Hlc, String)>` — LWW per `from`. An asset's effective tags (`tags()`) resolve each live raw tag through the alias chain with a visited set (cycle guard: stop when a tag repeats; the LAST tag before the repeat is the result — deterministic because the map is deterministic). Merge is a rename whose `to` already exists. `Touched::Tag(from)` is the new touched variant.

- [ ] **Step 1: failing unit tests** in `projection.rs`:

```rust
#[test]
fn tag_renamed_resolves_existing_and_future_adds() {
    // add "goldenhour" to asset A; apply TagRenamed goldenhour->golden-hour;
    // tags(A) == {"golden-hour"}. Then TagAdd "goldenhour" to asset B AFTER
    // the rename: tags(B) == {"golden-hour"} too (aliases resolve at read).
}

#[test]
fn tag_renamed_is_order_independent() {
    // Apply [add, rename] forward and reversed into two projections;
    // assert equal tags() on every asset (the standing order-independence
    // harness in this file has a helper — reuse it).
}

#[test]
fn concurrent_renames_of_one_tag_resolve_lww() {
    // Two TagRenamed{from:"t"} events with different HLCs; higher wins;
    // apply in both orders, same result.
}

#[test]
fn rename_cycles_terminate_deterministically() {
    // TagRenamed a->b and TagRenamed b->a (distinct HLCs). tags() of an
    // asset tagged "a" terminates (visited-set) and both application
    // orders agree on the result.
}
```

Extend `sample_ops_facts()` (:607) with a `TagRenamed` entry (`Touched::Tag("old".into())`) — the absence-assertion test at :758 will fail until every adapter handles it, which is the point. Extend the proptest op generator (find: `rg -n "prop_oneof|Just\(Op::" crates/core/src/projection.rs`) with a `TagRenamed` arm drawing from the same small tag alphabet the TagAdd generator uses (small alphabet = real collisions = real cycle coverage).

- [ ] **Step 2: run** `cargo test -p majestical-core` — compile failures, then red.
- [ ] **Step 3: implement**: enum variant (doc: `/// HLC-LWW tag rename; merge = rename onto an existing tag. Aliases resolve at read time, chained, cycle-guarded.`); wire-format line in the stability test at :204 (`{"type":"tag_renamed","from":"goldenhour","to":"golden-hour"}`); `tag_aliases` field + apply arm (`Self::lww` on the entry, `Touched::Tag(from.clone())`); resolution inside `tags()`:

```rust
fn resolve_alias<'a>(&'a self, tag: &'a str) -> &'a str {
    let mut seen = std::collections::BTreeSet::new();
    let mut current = tag;
    while seen.insert(current) {
        match self.tag_aliases.get(current) {
            Some((_, to)) => current = to,
            None => break,
        }
    }
    current
}
```

`tags()` maps live raw tags through `resolve_alias` into the returned `BTreeSet` (dedupe by construction). Add an accessor `pub fn tag_alias_target(&self, tag: &str) -> Option<&str>` (services will need it for merge validation) and `pub fn live_raw_tags_matching(&self, resolved: &str) -> …` ONLY if Task 11's SQLite arm turns out to need it — do not add speculatively.
- [ ] **Step 4: run** `cargo test -p majestical-core && cargo clippy -p majestical-core --all-targets -- -D warnings` — everything green, including the proptests (give them a real run: `PROPTEST_CASES=2048 cargo test -p majestical-core --lib proptest`, using this repo's actual proptest test names).
- [ ] **Step 5: Commit**

```bash
git add crates/core/src/event.rs crates/core/src/projection.rs
git commit -m "feat: Op::TagRenamed with LWW alias-map projection"
```

### Task 11: `Touched::Tag` in catalog-sqlite

**Files:**
- Modify: `crates/catalog-sqlite/src/apply.rs` (match at :120, tests at bottom)

- [ ] **Step 1: failing test** (mirror the rename test at `apply.rs:908`): seed two assets tagged "old" and one tagged "other" through the normal event path; apply a `TagRenamed old->new` event + `apply_touched` with `Touched::Tag("old")`; query the tags table: both assets now row "new", "other" untouched; a search by `tag:new` (the query.rs path) finds both.
- [ ] **Step 2: run — red** (non-exhaustive match on `Touched`).
- [ ] **Step 3: implement** the arm: `SELECT DISTINCT asset FROM tags WHERE tag = ?1` (the alias source), then for each asset delete+reinsert its tag rows from `projection.tags(&asset)` — the same refresh the `Touched::Asset` arm does for tags (reuse its helper; extract one if it is inline). Also handle the alias-target side: if a rename lands BEFORE any add of `from` reaches this machine, the arm finds zero rows and does nothing — correct, because those later adds arrive as `Touched::Asset` events and refresh through `tags()`, which resolves. State that in the arm's comment.
- [ ] **Step 4: run** `cargo test -p majestical-catalog-sqlite && cargo clippy -p majestical-catalog-sqlite --all-targets -- -D warnings`.
- [ ] **Step 5: Commit**

```bash
git add crates/catalog-sqlite/src/apply.rs
git commit -m "feat: Touched::Tag refreshes renamed tags in SQLite"
```

### Task 12: organize service verbs

**Files:**
- Modify: `crates/services/src/tags.rs` (+ `tags_list`, `tag_rename`, `tag_merge`, `tags_assign`)
- Modify: `crates/services/src/para.rs` (+ `para_file`)

**Wire shapes:**

```rust
#[derive(Debug, serde::Serialize)]
pub struct TagRow {
    pub tag: String,
    pub count: u64,
    /// HLC wall-time of the newest surviving add, ms.
    pub last_used_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct TagsListOutcome {
    pub tags: Vec<TagRow>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// tags_assign / para_file both report per-asset rows, never abort on one.
#[derive(Debug, serde::Serialize)]
pub struct AssignFailure {
    pub asset: String,
    pub reason: String,
}

#[derive(Debug, serde::Serialize)]
pub struct AssignOutcome {
    pub applied: u64,
    pub failed: Vec<AssignFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// tag_rename / tag_merge: what one rename event did.
#[derive(Debug, serde::Serialize)]
pub struct TagRenameOutcome {
    pub from: String,
    pub to: String,
    /// Assets whose effective tags changed (count at emit time).
    pub rewritten: u64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}
```

- [ ] **Step 1: failing tests** in each module's test mod (fixture catalogs, same arrange style as the existing `tag_add` tests):
  - `tags_list`: three tags with 2/1/1 uses → rows sorted by tag, exact counts; after a `tag_rename`, the old name is gone and the target's count is the union.
  - `tag_rename("goldenhour","golden-hour")`: emits one `TagRenamed` event (assert via the event log the way `tags.rs` tests assert `TagAdd`), outcome `rewritten == 2` when two assets carried it; renaming a nonexistent tag → `Err` naming the tag (operator-fixable).
  - `tag_merge("a","b")`: same op; merging into a nonexistent target → `Err` telling the caller to use `tag rename` instead; merge where `from == into` → `Err`.
  - `tags_assign(assets, tags)`: 2 known assets × 2 tags → `applied == 4`, event log has 4 `TagAdd`s; 1 unknown asset id → a `failed` row naming it, the known ones still applied.
  - `para_file(assets, node)`: emits one `AssetParaSet` per known asset; unknown node → `Err` (resolve via `resolve_para_node`, `para.rs:50`); unknown asset → `failed` row.
  - Every mutating verb here has a local sink: wrap returns with `Notices::attach_on_err` exactly as the four sync verbs do (`sync.rs:379` pattern).
- [ ] **Step 2: run — red.** `cargo test -p majestical-services --lib tags && cargo test -p majestical-services --lib para`
- [ ] **Step 3: implement.** `rewritten` = count of assets whose `tags()` contains the resolved `from` at emit time (read projection before emitting). `last_used_ms` comes from the newest surviving add-event HLC (the projection keeps add-event ids — resolve their HLC wall ms through the same accessor the suggestion machinery uses; if none exists, add `pub fn newest_tag_add_ms(&self, …)` to the projection with its own small test).
- [ ] **Step 4: run + clippy — green, zero warnings.**
- [ ] **Step 5: Commit**

```bash
git add crates/services/src/tags.rs crates/services/src/para.rs
git commit -m "feat: tags_list/rename/merge/assign and para_file verbs"
```

### Task 13: CLI + MCP parity for organize verbs

**Files:**
- Modify: `crates/cli/src/main.rs`, `crates/cli/src/commands.rs`, `crates/cli/src/mcp_cmd/`
- Test: CLI e2e + the MCP tool-list pin + MCP dry-run preview tests

- [ ] **Step 1: failing e2e tests**: `maj tags list --json`; `maj tag rename <from> <to>`; `maj tag merge <from> <into>`; `maj para file <node> <asset>...` (multiple assets). Destructive-verb doctrine: rename/merge/file mutate the catalog log — follow whatever the existing `maj tag add` does (it executes directly; these do too — catalog events are cheap and revertible-by-rename, NOT `--dry-run` gated; only archive keeps its dry-run because it moves files).
- [ ] **Step 2-3: implement** clap + renderers.
- [ ] **Step 4: MCP tools**: `list_tags` (read), `rename_tag`, `merge_tags`, `tag_assets`, `file_assets` (mutating: `confirm` defaulting false → dry-run preview). Each preview reads REAL state: `rename_tag` preview says `would rewrite N assets from 'x' to 'y'` with N from the projection; `merge_tags` additionally names the target's current count; `tag_assets`/`file_assets` previews name how many of the requested assets exist (and list the unknown ids). Tests assert preview text against fixture state AND that `confirm: false` leaves the event log untouched (byte-identical log dir).
- [ ] **Step 5: run** `cargo test -p majestical-cli && cargo clippy -p majestical-cli --all-targets -- -D warnings`.
- [ ] **Step 6: Commit; open PR 3** (`feat: tag rename/merge as CRDT events + assignment verbs, all heads`).

```bash
git add crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/src/mcp_cmd/ crates/cli/tests/
git commit -m "feat: CLI and MCP parity for tag rename/merge and filing"
```

---

## PR Chunk 4 — the Organize surface + selection toolbar

### Task 14: Tauri commands + fixtures for organize verbs

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands.rs`, `lib.rs`, `tests/commands.rs`, `tests/wire_fixtures.rs`
- Modify: `apps/desktop/src/lib/api.ts`, `fixtures.test.ts` (+ generated fixtures `tags_list.json`, `assign.json`, `tag_rename.json`, `para_list.json`, `archive.json`)

Commands: `list_tags`, `rename_tag` (from, to), `merge_tags` (from, into), `assign_tags` (assetIds, tags), `file_assets` (assetIds, node), plus the two the Organize surface needs that already exist as verbs but have no command yet: `list_para` (over `para::para_list`) and `archive_node` (over `para::archive`, taking `dryRun: bool` and root paths — the modal calls it twice: dry-run for the preview, then executing). Also `add_para_node` (over `para::add`) and `rename_para_node` (over `para::rename`).

- [ ] **Step 1: failing impl tests** in `tests/commands.rs` for every impl (fixture catalog; assert the same outcomes Task 12's service tests pinned; `archive_node_impl` with `dry_run: true` returns the move list WITHOUT moving — assert the source dir still exists).
- [ ] **Step 2: implement** impls + one-liner commands + registrations. All are synchronous commands (projection reads + event appends — prompt returns); none touch Lance, so no runtime gymnastics.
- [ ] **Step 3: wire fixtures both sides.** New `api.ts` interfaces mirror Task 12's structs (`TagRow`, `TagsListOutcome`, `AssignFailure`, `AssignOutcome`, `TagRenameOutcome`) plus `ParaNodeRow`/`ParaOutcome` (`para.rs:347,356`) and `ArchiveMove`/`ArchiveOutcome` (`para.rs:137,162`) — snake_case field-for-field; wrappers:

```ts
  listTags: () => invoke<TagsListOutcome>("list_tags"),
  renameTag: (from: string, to: string) =>
    invoke<TagRenameOutcome>("rename_tag", { from, to }),
  mergeTags: (from: string, into: string) =>
    invoke<TagRenameOutcome>("merge_tags", { from, into }),
  assignTags: (assetIds: string[], tags: string[]) =>
    invoke<AssignOutcome>("assign_tags", { assetIds, tags }),
  fileAssets: (assetIds: string[], node: string) =>
    invoke<AssignOutcome>("file_assets", { assetIds, node }),
  listPara: () => invoke<ParaOutcome>("list_para"),
  addParaNode: (kind: string, name: string) =>
    invoke<string>("add_para_node", { kind, name }),
  renameParaNode: (node: string, name: string) =>
    invoke<void>("rename_para_node", { node, name }),
  archiveNode: (node: string, roots: string[], dryRun: boolean) =>
    invoke<ArchiveOutcome>("archive_node", { node, roots, dryRun }),
```

- [ ] **Step 4: run** `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml && (cd apps/desktop && pnpm vitest run && pnpm check)`.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/commands.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/tests/ apps/desktop/src/lib/api.ts apps/desktop/src/lib/fixtures.test.ts apps/desktop/src/lib/fixtures/
git commit -m "feat: organize Tauri commands with pinned wire fixtures"
```

### Task 15: `OrganizeView.svelte` — two columns + archive modal

**Files:**
- Create: `apps/desktop/src/lib/OrganizeView.svelte`, `OrganizeView.test.ts`
- Modify: `apps/desktop/src/App.svelte` (+ test), `app.css` (classes prefixed `org-`)

Normative mockup: `mockups/2026-08-12-phase7d/organize.html`.

- [ ] **Step 1: failing component tests**:
  - PARA column groups nodes under the four kind headings with counts; selecting fills the detail card; "+ New node" prompts kind+name and calls `addParaNode`; Rename calls `renameParaNode`.
  - Archive… first calls `archiveNode(node, roots, true)` and renders each returned move as a modal row; Confirm calls with `dryRun: false`; Cancel calls nothing further. (Roots: the detail card lists the node's materialized roots — derive from the archive dry-run's own outcome; the modal flow is open → dry-run → show → confirm.)
  - Tags column renders `listTags` rows with counts; filter box narrows client-side; near-duplicate hint: two tags whose lowercased, `-`/`_`/space-stripped forms match get the `≈ other` marker (pure function `nearDuplicates(tags: TagRow[]): Map<string, string>` exported for testing, with its own unit test: `golden-hour`/`goldenhour` pair up, `drone`/`b-roll` don't).
  - Rename/Merge in the tag detail card call `renameTag`/`mergeTags` and re-fetch the list; the outcome's `rewritten` count renders as "Rewrote N assets".
  - Per-asset failures from any `AssignOutcome` render as rows, never swallowed.
- [ ] **Step 2: red run,** then **Step 3: implement** per the mockup (two `.org-col` columns inside the surface, `.mk` styles translated to real app classes).
- [ ] **Step 4:** App.svelte adds the `organize` surface + button (now Search, Browse, Organize, Volumes — Ingest's button still absent until PR 6). Run `(cd apps/desktop && pnpm vitest run && pnpm check && pnpm lint)`.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/OrganizeView.svelte apps/desktop/src/lib/OrganizeView.test.ts apps/desktop/src/App.svelte apps/desktop/src/App.test.ts apps/desktop/src/app.css
git commit -m "feat: Organize surface — PARA tree, tag manager, archive preview"
```

### Task 16: `SelectionBar.svelte` in Browse and Search

**Files:**
- Create: `apps/desktop/src/lib/SelectionBar.svelte`, `SelectionBar.test.ts`
- Modify: `apps/desktop/src/lib/BrowseView.svelte` (+ test), `SearchView.svelte` (+ test), `App.svelte` (selection model), `app.css`

Multi-select lives in the surfaces: ⌘-click (metaKey or ctrlKey) toggles an asset in a `selectedSet`; shift-click extends a contiguous range from the last plain click; plain click keeps today's single-select + Inspector behavior. The bar floats bottom-center (mockup: `organize.html`, third frame) when `selectedSet.size >= 2`.

- [ ] **Step 1: failing tests**: bar hidden at 0/1 selected, shown at 2+ with the exact count; Tag… opens the picker (existing tags from `listTags` + free-text create) and calls `assignTags([...selected], chosen)`; File to node… lists nodes from `listPara` and calls `fileAssets`; outcome failures render in the bar's result line ("Tagged 3 assets · 1 failed: <id> — <reason>"); Clear empties the set. ⌘-click/shift-click semantics tested in `BrowseView.test.ts` and `SearchView.test.ts` (range selection over the rendered card order).
- [ ] **Step 2: red,** **Step 3: implement** (one shared component; the two surfaces own their `selectedSet` and pass it down — no global store; App is untouched except passing nothing: the bar renders inside each surface).
- [ ] **Step 4: run the full GUI suite** `(cd apps/desktop && pnpm vitest run && pnpm check && pnpm lint)`.
- [ ] **Step 5: Commit; open PR 4** (`feat: Organize surface + bulk assignment from the grids`).

```bash
git add apps/desktop/src/lib/SelectionBar.svelte apps/desktop/src/lib/SelectionBar.test.ts apps/desktop/src/lib/BrowseView.svelte apps/desktop/src/lib/BrowseView.test.ts apps/desktop/src/lib/SearchView.svelte apps/desktop/src/lib/SearchView.test.ts apps/desktop/src/app.css
git commit -m "feat: multi-select with Tag/File-to-node selection bar"
```

---

## PR Chunk 5 — the ingest progress seam

### Task 17: `ProgressEvent` + cancellation in the engine

**Files:**
- Modify: `crates/ingest/src/engine.rs` (`run` at :121, `WorkerContext` at :318, `run_workers` at :334, `copy_one` at :437)
- Modify: `crates/ingest/src/lib.rs` (re-export)

**The seam (normative):**

```rust
/// One observable moment in a run. Workers emit from their own threads:
/// the callback must be `Sync` (wrap a channel sender or a Mutex).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Once, before any copying: totals from the plan partition.
    RunStarted { files_total: u64, bytes_total: u64 },
    FileStarted { rel: String, size: u64 },
    /// Cumulative bytes for `rel` across all destinations, throttled to
    /// one event per copy-buffer chunk.
    BytesCopied { rel: String, bytes_done: u64 },
    FileVerified { rel: String, dest_root: String },
    FilePlaced { rel: String },
    FileFailed { rel: String, reason: String },
    /// After the queue drains or cancellation: why the loop ended.
    RunStopped { cancelled: bool },
}

/// Checked between files by every worker. Cancellation is cooperative and
/// file-granular: the in-flight file finishes (journal stays consistent),
/// remaining queue entries are left unplaced — resumable by run id.
pub type CancelFlag = std::sync::atomic::AtomicBool;
```

`run()` gains two parameters: `progress: &(dyn Fn(ProgressEvent) + Sync)` and `cancel: &CancelFlag`. Replace, don't deprecate: update every existing caller (services `run_ingest_engine`, engine unit tests) with a no-op `&|_| {}` and a fresh `AtomicBool::new(false)` in the same commit.

- [ ] **Step 1: failing engine tests** (in `engine.rs`'s test mod, which already has fake sinks and plans):
  - a 3-file, 2-destination run emits: one `RunStarted { files_total: 3, .. }`; per file `FileStarted` → ≥1 `BytesCopied` (monotonic per rel) → 2 `FileVerified` (one per dest_root) → `FilePlaced`; one `RunStopped { cancelled: false }`. Collect via a `Mutex<Vec<ProgressEvent>>` closure; assert per-file ordering (filter by rel) rather than global interleaving (workers race).
  - a run whose second file's sink fails emits `FileFailed` with the reason, and the other files still place.
  - cancellation: set the flag from inside the FIRST `FilePlaced` emission; with `jobs: 1` and 3 files, exactly 1 places, `RunStopped { cancelled: true }`, the journal contains the placed file (resume works: a second `run` with the placed rel in `resume` places the remaining 2).
- [ ] **Step 2: red run** `cargo test -p majestical-ingest --lib progress`.
- [ ] **Step 3: implement**: `WorkerContext` gains `progress: &'a (dyn Fn(ProgressEvent) + Sync)` and `cancel: &'a CancelFlag`; the worker loop checks `cancel.load(Ordering::Relaxed)` before popping the queue; `copy_one` emits FileStarted before opening sinks, BytesCopied inside the copy loop (per buffer chunk — the loop already reads fixed-size chunks), FileVerified after each destination's read-back, FilePlaced/FileFailed at its exits. `run()` emits RunStarted after `partition_plan` (totals = queue length + byte sum) and RunStopped after `run_workers`.
- [ ] **Step 4: run** `cargo test -p majestical-ingest && cargo clippy -p majestical-ingest --all-targets -- -D warnings` — the whole crate, existing tests updated for the new signature.
- [ ] **Step 5: Commit**

```bash
git add crates/ingest/src/engine.rs crates/ingest/src/lib.rs
git commit -m "feat: progress events and cooperative cancellation in the ingest engine"
```

### Task 18: services plumbing + `ingest_unfinished`

**Files:**
- Modify: `crates/services/src/ingest.rs` (`ExecuteIngest` :186, `run_ingest` :243, `run_ingest_engine`)
- Modify: `crates/cli/src/commands.rs` (CLI passes no-ops), `crates/services/src/inbox.rs` (no-ops)

- [ ] **Step 1: failing service test**: `run_ingest` with a collecting progress closure over a 2-file fixture plan yields the engine's event sequence (RunStarted..RunStopped); `ingest_unfinished` over a journal dir holding one complete and one incomplete run returns exactly the incomplete one with its placed/planned counts and source/destination strings (read from the journal's own records — inspect `crates/ingest/src/journal.rs` for what a run records; expose whatever accessor is missing there WITH its own unit test).

**Wire shape:**

```rust
#[derive(Debug, serde::Serialize)]
pub struct UnfinishedRun {
    pub run_id: String,
    pub placed: u64,
    pub planned: u64,
    pub source: String,
    pub destinations: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct UnfinishedRunsOutcome {
    pub runs: Vec<UnfinishedRun>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}
```

- [ ] **Step 2: red,** **Step 3: implement**: `ExecuteIngest` gains `pub progress: &'a (dyn Fn(majestical_ingest::engine::ProgressEvent) + Sync)` and `pub cancel: &'a majestical_ingest::engine::CancelFlag`; `run_ingest_impl` threads them to the engine. CLI (`maj ingest`, `maj inbox process`) passes `&|_| {}` + a fresh never-set flag — behavior unchanged (the deferred CLI progress line stays deferred). `ingest_unfinished(catalog_dir)` walks the journal directory; "complete" = every planned rel checkpointed placed (or however the journal marks terminal state — pin it to what `journal.rs` actually records, with the accessor test).
- [ ] **Step 4: run** `cargo test -p majestical-services --lib ingest && cargo test -p majestical-cli && cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 5: CLI + MCP read parity**: `maj ingest unfinished --json` (new IngestCmd-adjacent variant on the existing clap tree) and MCP `list_unfinished_ingests` (read tool, no confirm) — e2e tests per the Task 7 pattern.
- [ ] **Step 6: Commit; open PR 5** (`feat: ingest progress seam + unfinished-run discovery`).

```bash
git add crates/services/src/ingest.rs crates/services/src/inbox.rs crates/cli/src/main.rs crates/cli/src/commands.rs crates/cli/src/mcp_cmd/ crates/cli/tests/
git commit -m "feat: progress/cancel through run_ingest; ingest_unfinished verb"
```

---

## PR Chunk 6 — the Ingest surface

### Task 19: ingest Tauri commands — plan, start, cancel, unfinished

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands.rs`, `lib.rs`, `tests/commands.rs`, `tests/wire_fixtures.rs`
- Modify: `apps/desktop/src/lib/api.ts`, `fixtures.test.ts` (+ fixtures `ingest_plan.json`, `ingest_run.json`, `unfinished_runs.json`, `ingest_progress.json`)

**Architecture (normative).** The run lives in the Tauri backend, not the webview:

```rust
/// Managed state for the one in-flight ingest (spec: single job).
pub struct IngestJob {
    pub run_id: String,
    pub cancel: std::sync::Arc<majestical_ingest::engine::CancelFlag>,
    /// Some(outcome) once the worker thread finishes; the surface polls
    /// `ingest_state` on mount to survive a webview reload mid-run.
    pub finished: std::sync::Arc<std::sync::Mutex<Option<IngestRunWire>>>,
}
pub struct IngestState(pub std::sync::RwLock<Option<IngestJob>>);
```

Commands:
- `plan_ingest(source, dests, para, subdir)` → the existing plan verb's `IngestPlanOutcome` (`services/ingest.rs:123`) — pure read, synchronous.
- `start_ingest(source, dests, para, subdir, resume)` → `String` (run id). Refuses (`CommandError`) if a job is already running (single-job spec). Spawns `std::thread::spawn` (a plain OS thread — the engine is synchronous and must not sit on the tokio blocking pool for a multi-hour copy; `run_off_tokio_runtime`'s doc comment rule is about Lance, but the same "own thread" reasoning applies and the comment in the code must say so). The thread's progress closure forwards every `ProgressEvent` as a Tauri event `ingest-progress` with payload `{ run_id, event }` via `AppHandle::emit`; completion stores the outcome in `finished` and emits `ingest-progress` one final time (the engine's own `RunStopped` already signals it).
- `cancel_ingest()` → sets the flag; idempotent, no error when nothing runs.
- `ingest_state()` → `{ running: Option<String>, finished: Option<IngestRunWire> }` — what the surface needs after a reload.
- `list_unfinished_ingests()` → `UnfinishedRunsOutcome`.

`IngestRunWire` = `majestical_services::ingest::IngestRun` serialized as-is (it already derives Serialize — the MCP head ships it today).

- [ ] **Step 1: failing impl tests** (`tests/commands.rs`, real temp source/dest dirs): `plan_ingest_impl` returns counts matching an arranged source; `start_ingest_impl` + a poll loop on `finished` places files and the collected forwarded events contain RunStarted/FilePlaced/RunStopped (impls take a `&dyn Fn(…)` emitter parameter so tests collect without an AppHandle — the command wrapper passes the real Tauri emitter closure; this keeps the impl testable per the standing seam rule); `start` while running → error mentioning the live run id; `cancel_ingest_impl` between files stops the run resumable.
- [ ] **Step 2: red,** **Step 3: implement.** Wire fixtures for `IngestPlanOutcome`, `IngestRun`, `UnfinishedRunsOutcome`, and one fully-populated `ProgressEvent` per variant (serialize a `Vec<ProgressEvent>` fixture — the TS side types the event union). `api.ts`:

```ts
/** `majestical_ingest::engine::ProgressEvent` — serde tag "type". */
export type ProgressEvent =
  | { type: "run_started"; files_total: number; bytes_total: number }
  | { type: "file_started"; rel: string; size: number }
  | { type: "bytes_copied"; rel: string; bytes_done: number }
  | { type: "file_verified"; rel: string; dest_root: string }
  | { type: "file_placed"; rel: string }
  | { type: "file_failed"; rel: string; reason: string }
  | { type: "run_stopped"; cancelled: boolean };
```

plus `IngestPlanOutcome`/`IngestRun`/`UnfinishedRunsOutcome`/`IngestState` interfaces mirrored field-for-field from the Rust, and wrappers (`planIngest`, `startIngest`, `cancelIngest`, `ingestState`, `listUnfinishedIngests`).
- [ ] **Step 4: run** `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml && (cd apps/desktop && pnpm vitest run && pnpm check)`.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/commands.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/tests/ apps/desktop/src/lib/api.ts apps/desktop/src/lib/fixtures.test.ts apps/desktop/src/lib/fixtures/
git commit -m "feat: ingest job commands with progress event forwarding"
```

### Task 20: `IngestView.svelte` — three states

**Files:**
- Create: `apps/desktop/src/lib/IngestView.svelte`, `IngestView.test.ts`
- Modify: `apps/desktop/src/App.svelte` (+ test — sidebar gains Ingest, completing the order Search, Browse, Ingest, Organize, Volumes; a running job shows the ● marker on the Ingest button and the surface stays mounted while hidden, or state lives outside the component — the Tauri backend holds it, so the component may unmount freely), `app.css` (`ingest-` classes)

Normative mockup: `mockups/2026-08-12-phase7d/ingest.html`. Source/destination pickers use `tauri-plugin-dialog` (already registered, `lib.rs:24`) via `@tauri-apps/plugin-dialog`'s `open({ directory: true })`.

- [ ] **Step 1: failing component tests** (mock `api` + a fake event emitter for `listen("ingest-progress", …)` in `test-support.ts`):
  - Setup: Start disabled until source + ≥1 dest + node set AND a current plan; editing any field after planning disables Start and shows "Plan again"; the plan panel renders counts/bytes/duplicates/rejects + notices verbatim.
  - Running: synthetic event sequence drives the bar (`bytes_done/bytes_total`), the files/bytes counters, per-destination tallies (a `file_failed` reddens its row's counter via class `bad`, run keeps rendering); the run id renders from `startIngest`'s return; Stop calls `cancelIngest`.
  - Done: after `run_stopped`, `ingestState` fetch renders the completion card from `IngestRun` — placed/failed exact counts, MHL line, failures listed with reasons; "Re-copy failed…" calls `planIngest` with the same source/dests (the new plan naturally re-plans failures — placed files dedupe-skip).
  - Resume banner: `listUnfinishedIngests` returning one run renders the banner with placed/planned; Resume calls `startIngest` with `resume: run_id`.
  - Reload-mid-run: mounting with `ingestState` reporting `running` renders the running state and re-subscribes.
- [ ] **Step 2: red,** **Step 3: implement** per the mockup's three frames.
- [ ] **Step 4: run** `(cd apps/desktop && pnpm vitest run && pnpm check && pnpm lint)`.
- [ ] **Step 5: MANUAL GATE (user-run):** PR 6 adds no plugin and doesn't touch `tauri.conf.json`, so the standing smoke rule doesn't trigger — but this PR ships the first long-running GUI operation, so ask the user to run `just gui-dev` once and click through one tiny real ingest before merge. Record the result in the PR body.
- [ ] **Step 6: Commit; open PR 6** (`feat: the Ingest surface — plan, live progress, resume`).

```bash
git add apps/desktop/src/lib/IngestView.svelte apps/desktop/src/lib/IngestView.test.ts apps/desktop/src/App.svelte apps/desktop/src/App.test.ts apps/desktop/src/app.css apps/desktop/src/lib/test-support.ts
git commit -m "feat: Ingest surface — plan, run with live progress, resume banner"
```

---

## PR Chunk 7 — phase close

### Task 21: parity harness rows + mutants + docs

**Files:**
- Modify: `crates/cli/tests/services_parity.rs`, `apps/desktop/src-tauri/tests/tauri_parity.rs` (rows for browse/tags/para-file/unfinished verbs)
- Modify: `docs/superpowers/plans/2026-07-29-phase2-watchlist.md` (close the keyframe-image deferral with PR 1's number; add "Phase 7D deferrals": ingest queue, CLI progress rendering, MCP progress notifications (carried), menu-bar indicator (carried), PARA-count click-through, grid virtualization, hover-scrub prefetch tuning; add "cargo-mutants triage (phase 7D)" section)
- Modify: `docs/superpowers/specs/2026-08-12-phase7d-surfaces-design.md` (As-built section)
- Create: `docs/superpowers/HANDOFF-phase7E.md` (supersedes 7D handoff, same skeleton: state, architecture pointers for the new seams, backlog pointer, 7E recommendation, process conventions carried verbatim — including the FOREGROUND mutants mandate in position 10)

- [ ] **Step 1: parity rows** — add the new verbs to both harnesses following their existing row pattern; run them.
- [ ] **Step 2: mutants, FOREGROUND, one at a time** (each command completes before the next starts, no background):

```bash
cargo mutants --package majestical-core --file crates/core/src/projection.rs -- --lib
cargo mutants --package majestical-services --file crates/services/src/browse.rs --file crates/services/src/tags.rs -- --lib
cargo mutants --package majestical-ingest --file crates/ingest/src/engine.rs -- --lib
cargo mutants --package majestical-index --file crates/index/src/work.rs -- --lib
```

Triage every survivor against the `--lib`-scoping caveat (a survivor may be killed by tests outside the scope — check before chasing); genuine gaps get tests in this PR; each disposition recorded in the watchlist's 7D triage section.
- [ ] **Step 3: docs** — as-built, watchlist, handoff.
- [ ] **Step 4: full local CI** `just ci` (the recipe CI runs) green.
- [ ] **Step 5: Commit; open PR 7** (`ci/docs: phase 7D close — parity rows, mutants triage, handoff`).

```bash
git add crates/cli/tests/services_parity.rs apps/desktop/src-tauri/tests/tauri_parity.rs docs/superpowers/plans/2026-07-29-phase2-watchlist.md docs/superpowers/specs/2026-08-12-phase7d-surfaces-design.md docs/superpowers/HANDOFF-phase7E.md
git commit -m "docs: phase 7D close — parity rows, mutants triage, 7E handoff"
```

---

## Verification (end-to-end, per chunk)

Every PR before merge: `just ci` locally green; `git fetch origin` + rebase on fresh `main`; CI green on the PR (both matrix legs — remember the 7C lesson: a cfg-flavored clippy lint can fire only on ubuntu); squash-merge; delete the branch.

Cross-cutting invariants to spot-check at the end of each wave:
1. `maj mcp` tool list: every new mutating tool has `confirm`, every read tool has none, every preview text names real state (run the preview tests, then one manual `tools/call` against a fixture catalog).
2. Wire drift: `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures` produces NO diff on a clean tree (fixtures current), and `pnpm check` passes (TS side agrees).
3. `sample_ops()` — phase 7D adds exactly ONE op variant (`TagRenamed`); the absence-assertion test enumerates it; no other variant appeared.
4. Notices: force one failure per new mutating verb (read-only FS, unknown ids) and confirm the notices arrive at all three heads (CLI stderr, MCP leading text blocks, `CommandError.notices`).
