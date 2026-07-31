//! `maj index run`/`maj index status`: the derived-data queue, worked from
//! the catalog projection against the on-disk blob store. main.rs owns the
//! clap definitions; this module owns behavior, following `search.rs`'s
//! precedent of keeping non-trivial verbs out of `commands.rs`.
use crate::app::FsApp;
use crate::commands::open_catalog;
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::projection::Projection;
use majestical_index::blob::{BlobStore, Derivation};
use majestical_index::encoder::{Encoder, EncoderOptions};
use majestical_index::vector_store::{VectorRow, VectorStore};
use majestical_index::work::{self, AssetSource, Capabilities, KindStatus, WorkKind, WorkPlan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const VALID_KINDS: &[&str] = &["thumbs", "embeddings", "keyframes"];

/// Args for `maj index run`, bundled to keep `cmd_index_run`'s own signature
/// within the house 5-positional-parameter limit.
pub(crate) struct IndexRunArgs {
    pub(crate) watch: bool,
    pub(crate) threads: Option<usize>,
    pub(crate) limit: Option<usize>,
    pub(crate) kinds: Option<Vec<String>>,
    pub(crate) json: bool,
}

/// What this machine can currently produce: the encoder model if it's been
/// fetched into the cache (see `maj model fetch`) and present at every
/// file's exact size, and whether `ffmpeg`/`ffprobe` are on `PATH`.
fn capabilities() -> Capabilities {
    let model_tag = majestical_index::model::model_dir()
        .ok()
        .filter(|dir| majestical_index::model::model_present(dir))
        .map(|_| majestical_index::model::MODEL_TAG.to_string());
    Capabilities {
        model_tag,
        ffmpeg: majestical_index::video::ffmpeg_available(),
    }
}

/// Validates `--kinds`, defaulting to every kind when omitted.
fn parse_kinds(kinds: Option<&[String]>) -> Result<BTreeSet<String>> {
    let Some(kinds) = kinds else {
        return Ok(VALID_KINDS.iter().map(|s| (*s).to_string()).collect());
    };
    for kind in kinds {
        anyhow::ensure!(
            VALID_KINDS.contains(&kind.as_str()),
            "unknown --kinds value '{kind}' — one of: {}",
            VALID_KINDS.join(", ")
        );
    }
    Ok(kinds.iter().cloned().collect())
}

/// Builds one [`AssetSource`] per catalog asset that has at least one
/// recorded instance: kind from the first instance's path (all instances of
/// one asset share content, so any instance's extension classifies it), and
/// an absolute path to the first instance whose volume is currently mounted
/// and whose bytes are actually present on disk — mounted-but-stale rows
/// (see the phase 4 watchlist entry on pre-phase-4 scan-relative paths)
/// degrade to offline rather than erroring.
fn gather_sources(projection: &Projection) -> Vec<AssetSource> {
    let mounted = volume_identity::mounted_volumes();
    projection
        .assets()
        .filter_map(|(asset, state)| {
            let (_, first_path) = state.instances.keys().next()?;
            let kind = media_kind(first_path);
            let abs_path = state.instances.keys().find_map(|(volume, path)| {
                let mount = mounted.get(volume)?;
                let candidate = mount.join(path);
                candidate.is_file().then_some(candidate)
            });
            Some(AssetSource {
                asset: asset.0.clone(),
                kind,
                abs_path,
            })
        })
        .collect()
}

fn default_index_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(4)
}

/// Decodes a source frame for a thumbnail: the image itself for
/// `MediaKind::Image`, or a frame one-tenth of the way into a video for
/// `MediaKind::Video` (early enough to usually avoid a black open/fade-in,
/// late enough to usually avoid a title card).
fn decode_thumb_source(path: &Path) -> Result<image::RgbImage> {
    if media_kind(&path.to_string_lossy()) == MediaKind::Video {
        let info = majestical_index::video::probe(path)?;
        Ok(majestical_index::video::extract_frame(
            path,
            info.duration_ms / 10,
        )?)
    } else {
        Ok(majestical_index::thumbs::decode_image(path)?)
    }
}

fn decode_and_write_thumb(blobs: &BlobStore, item: &work::WorkItem) -> Result<()> {
    let rgb = decode_thumb_source(&item.abs_path)?;
    let webp = majestical_index::thumbs::thumbnail_webp(&rgb)?;
    let path = blobs.path_for(&item.asset_hex, &Derivation::Thumb);
    blobs.write_atomic(&path, &webp)?;
    Ok(())
}

/// One pass's thumbnail-kind result: `written` new thumbnails and per-item
/// `failed` (path, reason) — mirrors [`EmbedOutcome`]/[`KeyframeOutcome`]'s
/// shape so every kind's executor returns one outcome value instead of a
/// bare tuple.
struct ThumbOutcome {
    written: u64,
    failed: Vec<(PathBuf, String)>,
}

/// Works every thumbnail item with `jobs` parallel workers sharing one
/// atomic cursor into `items` — a plain work-stealing pool without pulling in
/// a thread-pool dependency for one queue.
fn run_thumb_items(blobs: &BlobStore, items: &[work::WorkItem], jobs: usize) -> ThumbOutcome {
    let next = AtomicUsize::new(0);
    let written = AtomicU64::new(0);
    let failed: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    match decode_and_write_thumb(blobs, item) {
                        Ok(()) => {
                            written.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(err) => {
                            let mut guard = failed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            guard.push((item.abs_path.clone(), err.to_string()));
                        }
                    }
                }
            });
        }
    });
    let failed = failed
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ThumbOutcome {
        written: written.load(Ordering::Relaxed),
        failed,
    }
}

fn workkind_name(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Thumb => "thumbs",
        WorkKind::ImageEmbed => "embeddings",
        WorkKind::Keyframes => "keyframes",
    }
}

/// Builds the plan for one pass: gathers sources fresh from the projection
/// (so `--watch` sees newly scanned assets), diffs against `blobs`, then
/// narrows `items` to `kinds`. Deliberately does not apply `--limit` here —
/// that happens after `run_once` splits items by kind, so `--limit` bounds
/// each kind's own per-pass budget independently rather than one kind
/// starving another.
fn build_plan(projection: &Projection, blobs: &BlobStore, kinds: &BTreeSet<String>) -> WorkPlan {
    let sources = gather_sources(projection);
    let caps = capabilities();
    let mut plan = work::plan_work(&sources, blobs, &caps);
    plan.items
        .retain(|item| kinds.contains(workkind_name(item.kind)));
    plan
}

/// Where a pass's per-machine derived state lives: the Lance vector store
/// and the `CoreML` compiled-graph cache. Bundled to keep [`run_embed_items`]
/// within the house 5-positional-parameter limit.
struct EmbedPaths {
    lance_dir: PathBuf,
    coreml_cache_dir: PathBuf,
}

/// One pass's embedding-kind result: `written` new embeddings (encoder ran),
/// `loaded` vectors pulled in from blobs the local Lance store didn't have
/// yet (the blob↔Lance diff — a teammate's synced vectors, or a lance dir
/// just rebuilt after corruption), and per-item `failed` (path, reason).
struct EmbedOutcome {
    written: u64,
    loaded: u64,
    failed: Vec<(PathBuf, String)>,
}

/// One pass's keyframe-kind result: `videos_done` videos whose manifest got
/// written this pass (the planner's done marker — see [`run_keyframe_items`]),
/// `keyframes_written` individual frames actually extracted and embedded,
/// `keyframes_failed` individual timestamps that failed extract/embed
/// (whether or not their video ended up over the half-failed threshold — see
/// [`over_half_failed`]), and per-video `failed` (path, reason — for a video
/// over that threshold, the reason includes the first per-timestamp failure).
#[derive(Default)]
struct KeyframeOutcome {
    videos_done: u64,
    keyframes_written: u64,
    keyframes_failed: u64,
    failed: Vec<(PathBuf, String)>,
}

/// Bundles `run_once`'s per-pass options (everything but `app`/`catalog_dir`)
/// to keep its own signature within the house 5-positional-parameter limit.
struct RunOnceArgs<'a> {
    kinds: &'a BTreeSet<String>,
    limit: Option<usize>,
    threads: Option<usize>,
    json: bool,
}

/// One `index run` pass: builds the plan, works every thumbnail, embedding,
/// and keyframe item, and prints the result.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced, or if the Lance
/// vector store can't be opened even after one corruption-recovery retry.
fn run_once(app: &FsApp, catalog_dir: &Path, args: &RunOnceArgs<'_>) -> Result<()> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let plan = build_plan(&projection, &blobs, args.kinds);
    let (thumb_items, embed_items, keyframe_items) = split_and_cap_items(plan.items, args.limit);

    let jobs = args.threads.unwrap_or_else(default_index_jobs);
    let thumb_outcome = run_thumb_items(&blobs, &thumb_items, jobs);

    let embed_paths = EmbedPaths {
        lance_dir: state_dir.join("lance"),
        coreml_cache_dir: state_dir.join("coreml-cache"),
    };
    let embed_outcome = run_embed_items(&embed_paths, &blobs, &embed_items)?;
    let keyframe_outcome = run_keyframe_items(&embed_paths, &blobs, &keyframe_items)?;

    print_run_result(&thumb_outcome, &embed_outcome, &keyframe_outcome, args.json);
    Ok(())
}

/// Splits `items` by kind, then caps each kind independently at `limit` —
/// every kind has an executor now, so `--limit` bounds each one's own
/// per-pass budget rather than one kind starving another.
fn split_and_cap_items(
    items: Vec<work::WorkItem>,
    limit: Option<usize>,
) -> (
    Vec<work::WorkItem>,
    Vec<work::WorkItem>,
    Vec<work::WorkItem>,
) {
    let mut thumbs = Vec::new();
    let mut embeds = Vec::new();
    let mut keyframes = Vec::new();
    for item in items {
        match item.kind {
            WorkKind::Thumb => thumbs.push(item),
            WorkKind::ImageEmbed => embeds.push(item),
            WorkKind::Keyframes => keyframes.push(item),
        }
    }
    if let Some(limit) = limit {
        thumbs.truncate(limit);
        embeds.truncate(limit);
        keyframes.truncate(limit);
    }
    (thumbs, embeds, keyframes)
}

/// Resolves the encoder model dir only if it's actually present at every
/// file's exact size — mirrors `capabilities()`'s check, kept separate
/// since that function returns a `model_tag` string, not a usable path.
fn model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir().ok()?;
    majestical_index::model::model_present(&dir).then_some(dir)
}

/// Opens `dir` and runs a cheap probe scan (`existing_keys`), catching both
/// an ordinary `Err` and an unwinding panic — see
/// [`majestical_index::vector_store::catch_corruption`]'s doc comment for
/// why a panic is even possible here (lance's own manifest reader panics on
/// a garbage `.manifest` rather than erroring). The probe matters on its
/// own: some corruption (a missing/truncated data file) doesn't surface at
/// `open` time at all, only once something reads past the manifest — the
/// probe forces that discovery here, instead of failing later deep inside
/// real embedding work.
fn open_and_probe(dir: &Path) -> Result<VectorStore, String> {
    let owned = dir.to_path_buf();
    majestical_index::vector_store::catch_corruption(move || {
        let store = VectorStore::open(&owned)?;
        store.existing_keys(majestical_index::model::MODEL_TAG)?;
        Ok(store)
    })
}

/// Removes a corrupt lance path, whichever shape it takes on disk: a plain
/// file (if the lance path itself got clobbered by one — `write_atomic`-style
/// corruption elsewhere in this codebase never does this, but nothing stops
/// a stray tool or a bad sync from replacing a directory with a file) or a
/// directory (the normal shape). `remove_dir_all` errors with
/// `NotADirectory` on a plain file rather than removing it, so the file case
/// needs its own arm — otherwise a corrupt file at the lance path recovers
/// from neither `open` nor removal, and every future run keeps hitting the
/// same broken path forever.
fn remove_lance_state(dir: &Path) -> Result<()> {
    let result = if dir.is_file() {
        std::fs::remove_file(dir)
    } else {
        std::fs::remove_dir_all(dir)
    };
    match result {
        Ok(()) => Ok(()),
        Err(_) if !dir.exists() => Ok(()),
        Err(source) => Err(source)
            .with_context(|| format!("removing corrupt lance state at {}", dir.display())),
    }
}

/// Opens the Lance vector store at `dir`, applying the corruption-recovery
/// policy: a Lance dataset has no journal to replay, so any failure to open
/// AND probe it almost always means an interrupted write left it corrupt —
/// and the dataset is disposable, rebuildable entirely from blobs via the
/// blob↔Lance diff. On failure: log a note (with the underlying reason),
/// remove the corrupt path, and retry once. Removal failing propagates as a
/// real error — a corrupt store that also can't be removed must not
/// silently retry against the same still-broken path forever.
///
/// # Errors
/// Returns an error if the corrupt path can't be removed, or if the store
/// still can't be opened and probed after that removal.
fn open_or_rebuild(dir: &Path) -> Result<VectorStore> {
    match open_and_probe(dir) {
        Ok(store) => return Ok(store),
        Err(reason) => eprintln!(
            "note: lance vector store at {} is unreadable ({reason}) — removing and \
             rebuilding from blobs",
            dir.display()
        ),
    }
    remove_lance_state(dir)?;
    open_and_probe(dir)
        .map_err(|reason| anyhow::anyhow!("rebuilding lance store at {}: {reason}", dir.display()))
}

fn embed_one(blobs: &BlobStore, encoder: &mut Encoder, item: &work::WorkItem) -> Result<VectorRow> {
    let rgb = majestical_index::thumbs::decode_image(&item.abs_path)?;
    let vector = encoder.embed_image(&rgb)?;
    let model_tag = majestical_index::model::MODEL_TAG;
    let path = blobs.path_for(&item.asset_hex, &Derivation::ImageEmbedding { model_tag });
    blobs.write_vector(&path, &vector)?;
    Ok(VectorRow {
        asset_hex: item.asset_hex.clone(),
        kind: "image".to_string(),
        ts_ms: -1,
        model_tag: model_tag.to_string(),
        vector,
    })
}

/// Encodes and stores every item in `items`. Single-threaded: `Session::run`
/// needs `&mut self`, and `CoreML`'s Apple Neural Engine execution serializes
/// inference across threads anyway, so a worker pool here would add
/// complexity with no throughput gain.
fn embed_and_store(
    model_dir: &Path,
    coreml_cache_dir: &Path,
    blobs: &BlobStore,
    store: &VectorStore,
    items: &[&work::WorkItem],
) -> Result<(u64, Vec<(PathBuf, String)>)> {
    let mut encoder = Encoder::load(
        model_dir,
        &EncoderOptions {
            coreml: true,
            coreml_cache: Some(coreml_cache_dir.to_path_buf()),
        },
    )?;
    let mut written = 0u64;
    let mut failed = Vec::new();
    let mut batch = Vec::new();
    for item in items {
        match embed_one(blobs, &mut encoder, item) {
            Ok(row) => {
                written += 1;
                batch.push(row);
                if batch.len() >= 64 {
                    store.add(std::mem::take(&mut batch))?;
                }
            }
            Err(err) => failed.push((item.abs_path.clone(), err.to_string())),
        }
    }
    if !batch.is_empty() {
        store.add(batch)?;
    }
    Ok((written, failed))
}

/// The blob↔Lance diff: adds every vector blob for `MODEL_TAG` the local
/// Lance store doesn't have yet. Runs every pass regardless of whether any
/// encoding happened this pass — this is what indexes a teammate's synced
/// vectors, and what repopulates a lance dir just rebuilt after corruption,
/// with zero re-inference.
fn load_missing_vectors_from_blobs(store: &VectorStore, blobs: &BlobStore) -> Result<u64> {
    let model_tag = majestical_index::model::MODEL_TAG;
    let existing = store.existing_keys(model_tag)?;
    let mut loaded = 0u64;
    let mut batch = Vec::new();
    for blob_ref in blobs.iter_vectors(model_tag)? {
        let key = (
            blob_ref.asset_hex.clone(),
            blob_ref.kind.clone(),
            blob_ref.ts_ms,
        );
        if existing.contains(&key) {
            continue;
        }
        let vector = blobs.read_vector(&blob_ref.path)?;
        batch.push(VectorRow {
            asset_hex: blob_ref.asset_hex,
            kind: blob_ref.kind,
            ts_ms: blob_ref.ts_ms,
            model_tag: model_tag.to_string(),
            vector,
        });
        loaded += 1;
        if batch.len() >= 256 {
            store.add(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        store.add(batch)?;
    }
    Ok(loaded)
}

/// Works every `ImageEmbed` item in `items` (encoding only if the model is
/// present), then always performs the blob↔Lance diff — see
/// [`load_missing_vectors_from_blobs`]. The diff runs every pass regardless
/// of `--kinds` — even `--kinds thumbs`, which leaves `items` with zero
/// `ImageEmbed` entries — since `--kinds` bounds embedding *work*, not the
/// cheap self-heal/teammate-sync safety net this diff provides.
///
/// # Errors
/// Returns an error if the vector store can't be opened, or if a batch add
/// or the blob↔Lance diff fails.
fn run_embed_items(
    paths: &EmbedPaths,
    blobs: &BlobStore,
    items: &[work::WorkItem],
) -> Result<EmbedOutcome> {
    let embed_items: Vec<&work::WorkItem> = items
        .iter()
        .filter(|i| i.kind == WorkKind::ImageEmbed)
        .collect();
    let store = open_or_rebuild(&paths.lance_dir)?;

    let (written, failed) = if embed_items.is_empty() {
        (0, Vec::new())
    } else if let Some(model_dir) = model_dir_if_present() {
        embed_and_store(
            &model_dir,
            &paths.coreml_cache_dir,
            blobs,
            &store,
            &embed_items,
        )?
    } else {
        (0, Vec::new())
    };

    let loaded = load_missing_vectors_from_blobs(&store, blobs)?;
    Ok(EmbedOutcome {
        written,
        loaded,
        failed,
    })
}

/// Keyframe timestamps stay far below `i64::MAX` milliseconds for any real
/// video; saturating instead of a checked cast keeps this infallible without
/// it ever mattering in practice.
fn ts_ms_i64(ts_ms: u64) -> i64 {
    i64::try_from(ts_ms).unwrap_or(i64::MAX)
}

/// What happened when working one detected keyframe timestamp.
enum TimestampOutcome {
    /// A blob already existed for this timestamp. Its vector is already in
    /// Lance too: `run_embed_items`'s blob↔Lance diff (`load_missing_vectors_from_blobs`)
    /// runs unconditionally, every pass, before this executor ever gets a
    /// look — so a keyframe blob written by an earlier pass, or synced in
    /// from a teammate, is already indexed by the time this check runs.
    AlreadyComplete,
    /// Freshly extracted, embedded, and written.
    Written(VectorRow),
}

/// Works one detected keyframe timestamp: skip if a blob already exists for
/// it, else extract + embed + write a fresh blob.
///
/// # Errors
/// Returns a human-readable reason if extracting the frame, running the
/// encoder, or writing the new blob fails.
fn keyframe_at_timestamp(
    blobs: &BlobStore,
    encoder: &mut Encoder,
    item: &work::WorkItem,
    ts_ms: u64,
) -> Result<TimestampOutcome, String> {
    let model_tag = majestical_index::model::MODEL_TAG;
    let path = blobs.path_for(
        &item.asset_hex,
        &Derivation::KeyframeEmbedding {
            model_tag,
            timestamp_ms: ts_ms,
        },
    );
    if path.is_file() {
        return Ok(TimestampOutcome::AlreadyComplete);
    }
    let frame =
        majestical_index::video::extract_frame(&item.abs_path, ts_ms).map_err(|e| e.to_string())?;
    let vector = encoder.embed_image(&frame).map_err(|e| e.to_string())?;
    blobs
        .write_vector(&path, &vector)
        .map_err(|e| e.to_string())?;
    Ok(TimestampOutcome::Written(VectorRow {
        asset_hex: item.asset_hex.clone(),
        kind: "keyframe".to_string(),
        ts_ms: ts_ms_i64(ts_ms),
        model_tag: model_tag.to_string(),
        vector,
    }))
}

/// One video's keyframe work: the timestamps that ended up complete (already
/// indexed or freshly written — this is what the manifest ends up listing),
/// the new `VectorRow`s to batch into Lance, how many frames were freshly
/// embedded, how many timestamps failed outright, and — when at least one
/// did — the first failure's reason (so a video-level failure message can
/// say *why*, not just *how many*).
struct VideoKeyframeResult {
    rows: Vec<VectorRow>,
    succeeded_timestamps: Vec<u64>,
    keyframes_written: u64,
    keyframe_failures: usize,
    total_keyframes: usize,
    first_failure_reason: Option<String>,
}

/// Detects `item`'s scenes and works every resulting timestamp.
///
/// # Errors
/// Returns a human-readable reason if probing or decoding analysis frames
/// fails — a video-level failure distinct from a single timestamp's
/// extract/embed failure, which this collects into `keyframe_failures`
/// (keeping only the first reason, in `first_failure_reason`) instead of
/// aborting the whole video.
fn process_video_keyframes(
    blobs: &BlobStore,
    encoder: &mut Encoder,
    item: &work::WorkItem,
) -> Result<VideoKeyframeResult, String> {
    let info = majestical_index::video::probe(&item.abs_path).map_err(|e| e.to_string())?;
    let frames =
        majestical_index::video::analysis_frames(&item.abs_path).map_err(|e| e.to_string())?;
    let timestamps = majestical_index::video::detect_scenes(&frames, 2000, info.duration_ms);

    let mut rows = Vec::new();
    let mut succeeded_timestamps = Vec::new();
    let mut written = 0u64;
    let mut failures = 0usize;
    let mut first_failure_reason = None;
    for &ts_ms in &timestamps {
        match keyframe_at_timestamp(blobs, encoder, item, ts_ms) {
            Ok(TimestampOutcome::AlreadyComplete) => succeeded_timestamps.push(ts_ms),
            Ok(TimestampOutcome::Written(row)) => {
                succeeded_timestamps.push(ts_ms);
                written += 1;
                rows.push(row);
            }
            Err(reason) => {
                failures += 1;
                first_failure_reason.get_or_insert(reason);
            }
        }
    }
    Ok(VideoKeyframeResult {
        rows,
        succeeded_timestamps,
        keyframes_written: written,
        keyframe_failures: failures,
        total_keyframes: timestamps.len(),
        first_failure_reason,
    })
}

/// True once more than half of a video's detected keyframes failed to
/// extract/embed — the threshold at which [`run_keyframe_items`] gives up on
/// the video for this pass rather than marking it done with a majority of
/// its keyframes missing. A video with zero detected keyframes is never over
/// this threshold (nothing failed).
fn over_half_failed(failures: usize, total: usize) -> bool {
    total > 0 && failures * 2 > total
}

/// Builds the video-level failure message for a video [`over_half_failed`]
/// gave up on: the path (nothing else in this message otherwise carries it —
/// see the "no path prefix" note on [`print_run_result`]'s stderr loop), the
/// failure/total counts, and the first per-timestamp failure's reason, so
/// the message says why at least one keyframe failed, not just how many.
fn over_half_failed_message(
    path: &Path,
    failures: usize,
    total: usize,
    first_reason: &str,
) -> String {
    format!(
        "{}: {failures}/{total} keyframes failed to extract/embed (over half) — first \
         failure: {first_reason} — video skipped, will retry",
        path.display()
    )
}

/// `detected` is the video's full scene-detected keyframe count, even when
/// `timestamps` (the succeeded subset — see [`run_keyframe_items`]) is
/// shorter: without it, a video that finished under the over-half-failed
/// threshold with a few permanently-missing keyframes would look, from the
/// manifest alone, exactly like one that succeeded at every timestamp.
fn keyframes_manifest_json(model_tag: &str, detected: usize, timestamps: &[u64]) -> Vec<u8> {
    serde_json::json!({ "model_tag": model_tag, "detected": detected, "timestamps": timestamps })
        .to_string()
        .into_bytes()
}

/// Works every `Keyframes` item in `items`: per video, detects scenes, then
/// works each timestamp (extract+embed, or skip a blob that already exists —
/// see [`keyframe_at_timestamp`]). The manifest blob is written LAST, only
/// after every timestamp has been attempted: its existence is
/// `plan_keyframes`'s (in `work.rs`) done marker, so a crash mid-video
/// simply re-plans the whole video next pass, while the per-timestamp blob
/// check in `keyframe_at_timestamp` skips whatever already finished.
///
/// A video whose probe or analysis-frame decode fails outright is a
/// video-level failure. Otherwise, per [`over_half_failed`]: if more than
/// half a video's keyframes fail to extract/embed, the whole video is
/// treated as failed for this pass and its manifest is withheld — the
/// timestamps that did succeed stay as orphaned blobs, picked back up when
/// the video is retried, rather than being permanently hidden behind a
/// manifest that would stop the video from ever being revisited. A video at
/// or under that threshold gets a manifest listing only the timestamps that
/// actually succeeded; a few permanently-missing keyframes are the accepted
/// cost of not retrying an entire video forever over one flaky frame. The
/// manifest's `detected` field (see [`keyframes_manifest_json`]) keeps that
/// gap auditable — the full scene-detected count survives even when
/// `timestamps` is the smaller succeeded-only subset.
///
/// # Errors
/// Returns an error if the vector store can't be opened/rebuilt, the encoder
/// fails to load, or a Lance batch add or manifest write fails.
fn run_keyframe_items(
    paths: &EmbedPaths,
    blobs: &BlobStore,
    items: &[work::WorkItem],
) -> Result<KeyframeOutcome> {
    if items.is_empty() {
        return Ok(KeyframeOutcome::default());
    }
    let Some(model_dir) = model_dir_if_present() else {
        return Ok(KeyframeOutcome::default());
    };

    let store = open_or_rebuild(&paths.lance_dir)?;
    let model_tag = majestical_index::model::MODEL_TAG;
    // Loaded separately from `run_embed_items`'s encoder rather than shared
    // across both: each is only loaded at all when its own kind has pending
    // items, and a second `CoreML` session load (a second or so, cached
    // graph) is cheap next to keeping the two executors independently
    // testable and not threading a shared `&mut Encoder` between them.
    let mut encoder = Encoder::load(
        &model_dir,
        &EncoderOptions {
            coreml: true,
            coreml_cache: Some(paths.coreml_cache_dir.clone()),
        },
    )?;

    let mut outcome = KeyframeOutcome::default();
    let mut batch = Vec::new();
    for item in items {
        match process_video_keyframes(blobs, &mut encoder, item) {
            Ok(result) => {
                outcome.keyframes_written += result.keyframes_written;
                outcome.keyframes_failed +=
                    u64::try_from(result.keyframe_failures).unwrap_or(u64::MAX);
                batch.extend(result.rows);
                if batch.len() >= 64 {
                    store.add(std::mem::take(&mut batch))?;
                }
                if over_half_failed(result.keyframe_failures, result.total_keyframes) {
                    let first_reason = result
                        .first_failure_reason
                        .as_deref()
                        .unwrap_or("<no reason recorded>");
                    outcome.failed.push((
                        item.abs_path.clone(),
                        over_half_failed_message(
                            &item.abs_path,
                            result.keyframe_failures,
                            result.total_keyframes,
                            first_reason,
                        ),
                    ));
                    continue;
                }
                let manifest_path =
                    blobs.path_for(&item.asset_hex, &Derivation::KeyframeManifest { model_tag });
                let manifest = keyframes_manifest_json(
                    model_tag,
                    result.total_keyframes,
                    &result.succeeded_timestamps,
                );
                blobs.write_atomic(&manifest_path, &manifest)?;
                outcome.videos_done += 1;
            }
            Err(reason) => outcome.failed.push((item.abs_path.clone(), reason)),
        }
    }
    if !batch.is_empty() {
        store.add(batch)?;
    }
    Ok(outcome)
}

fn failed_json(failed: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
        .collect()
}

fn print_run_result(
    thumbs: &ThumbOutcome,
    embed: &EmbedOutcome,
    keyframes: &KeyframeOutcome,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbnails": { "written": thumbs.written, "failed": failed_json(&thumbs.failed) },
                "embeddings": {
                    "written": embed.written,
                    "loaded_from_blobs": embed.loaded,
                    "failed": failed_json(&embed.failed),
                },
                "keyframes": {
                    "videos_done": keyframes.videos_done,
                    "keyframes_written": keyframes.keyframes_written,
                    "keyframes_failed": keyframes.keyframes_failed,
                    "failed": failed_json(&keyframes.failed),
                },
            })
        );
    } else {
        println!(
            "thumbnails: {} written, {} failed",
            thumbs.written,
            thumbs.failed.len()
        );
        println!(
            "embeddings: {} written, {} loaded from blobs, {} failed",
            embed.written,
            embed.loaded,
            embed.failed.len()
        );
        println!(
            "keyframes: {} videos, {} frames embedded, {} frame failures, {} videos failed",
            keyframes.videos_done,
            keyframes.keyframes_written,
            keyframes.keyframes_failed,
            keyframes.failed.len()
        );
    }
    // No path prefix here: every `IndexError` display already embeds the
    // path it failed on (the structured path is still available in the
    // `--json` branch above, for callers that want it out-of-band).
    for (_, err) in thumbs
        .failed
        .iter()
        .chain(&embed.failed)
        .chain(&keyframes.failed)
    {
        eprintln!("failed: {err}");
    }
}

/// True when `--kinds` was passed explicitly and names `keyframes` — the one
/// case where a missing ffmpeg is a hard error rather than a degrade: an
/// unqualified `index run` silently reports `needs_ffmpeg` in its kind
/// status, but asking for keyframes by name and getting nothing back, with
/// no explanation, is a worse experience than failing loudly.
fn explicitly_requested_keyframes(kinds: Option<&[String]>) -> bool {
    kinds.is_some_and(|kinds| kinds.iter().any(|k| k == "keyframes"))
}

/// Works the derivation queue once, or repeatedly (`--watch`, a 5s poll
/// loop) so newly scanned assets get picked up without a manual re-run.
///
/// # Errors
/// Returns an error if `--kinds` names an unknown kind, if `--kinds`
/// explicitly names `keyframes` while ffmpeg is absent, or the catalog can't
/// be opened/synced.
pub(crate) fn cmd_index_run(app: &FsApp, catalog_dir: &Path, args: &IndexRunArgs) -> Result<()> {
    let kinds = parse_kinds(args.kinds.as_deref())?;
    if explicitly_requested_keyframes(args.kinds.as_deref())
        && !majestical_index::video::ffmpeg_available()
    {
        anyhow::bail!("--kinds keyframes requires ffmpeg/ffprobe on PATH (brew install ffmpeg)");
    }
    let run_once_args = RunOnceArgs {
        kinds: &kinds,
        limit: args.limit,
        threads: args.threads,
        json: args.json,
    };
    loop {
        run_once(app, catalog_dir, &run_once_args)?;
        if !args.watch {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Ok(())
}

/// Prints one line per derivation kind: `done`, `pending`, `offline`,
/// `unsupported`, `needs_ffmpeg` (need ffmpeg), `needs_model` (need model).
fn print_kind_status(name: &str, status: &KindStatus) {
    println!(
        "{name}: {} done, {} pending, {} offline, {} unsupported, {} need ffmpeg, {} need model",
        status.done,
        status.pending,
        status.offline,
        status.unsupported,
        status.needs_ffmpeg,
        status.needs_model,
    );
}

fn kind_status_json(status: &KindStatus) -> serde_json::Value {
    serde_json::json!({
        "done": status.done,
        "pending": status.pending,
        "offline": status.offline,
        "unsupported": status.unsupported,
        "needs_ffmpeg": status.needs_ffmpeg,
        "needs_model": status.needs_model,
    })
}

/// Reports the queue's current state per derivation kind without doing any
/// work — a diff against the blob store, same as `run`, just not executed.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced.
pub(crate) fn cmd_index_status(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    let plan = build_plan(&projection, &blobs, &kinds);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbs": kind_status_json(&plan.thumbs),
                "embeddings": kind_status_json(&plan.embeddings),
                "keyframes": kind_status_json(&plan.keyframes),
            })
        );
    } else {
        print_kind_status("thumbs", &plan.thumbs);
        print_kind_status("embeddings", &plan.embeddings);
        print_kind_status("keyframes", &plan.keyframes);
    }
    Ok(())
}

/// Downloads the pinned encoder model into the shared cache
/// (`MAJ_MODEL_DIR`, or the platform data dir — see
/// [`majestical_index::model::model_dir`]), verifying every file's sha256
/// before it's installed.
///
/// # Errors
/// Returns an error if the cache directory can't be resolved, or if any
/// file fails to download or verify.
pub(crate) fn cmd_model_fetch(verify: bool) -> Result<()> {
    let dir = majestical_index::model::model_dir()?;
    println!("model cache: {}", dir.display());
    majestical_index::model::fetch(&dir, verify, &mut |line| println!("{line}"))?;
    println!("model '{}' ready", majestical_index::model::MODEL_TAG);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kinds_defaults_to_every_kind() {
        let kinds = parse_kinds(None).expect("default kinds");
        assert_eq!(kinds.len(), 3);
        assert!(kinds.contains("thumbs"));
        assert!(kinds.contains("embeddings"));
        assert!(kinds.contains("keyframes"));
    }

    #[test]
    fn parse_kinds_rejects_an_unknown_value() {
        let err = parse_kinds(Some(&["thumbs".to_string(), "bogus".to_string()]))
            .expect_err("must reject unknown kind");
        assert!(err.to_string().contains("bogus"));
        assert!(err.to_string().contains("thumbs"));
    }

    #[test]
    fn over_half_failed_cases() {
        assert!(
            !over_half_failed(0, 0),
            "zero detected keyframes never counts as over half failed"
        );
        assert!(
            !over_half_failed(1, 2),
            "exactly half (1/2) is not OVER half"
        );
        assert!(over_half_failed(2, 3), "2/3 (66%) is over half");
    }

    #[test]
    fn over_half_failed_message_includes_path_counts_and_first_reason() {
        let message = over_half_failed_message(
            Path::new("/media/clip.mov"),
            3,
            5,
            "ffmpeg failed: no such filter",
        );
        assert!(
            message.contains("/media/clip.mov"),
            "message must name the video: {message}"
        );
        assert!(
            message.contains("3/5"),
            "message must carry the failure/total counts: {message}"
        );
        assert!(
            message.contains("ffmpeg failed: no such filter"),
            "message must carry the first per-timestamp failure reason: {message}"
        );
    }

    /// Opens (creating) a lance store at `lance_dir` and seeds one vector —
    /// the "healthy store to be corrupted" starting point every recovery
    /// test below needs.
    fn seed_populated_lance_store(lance_dir: &Path) {
        let store = VectorStore::open(lance_dir).expect("open for seeding");
        store
            .add(vec![majestical_index::vector_store::VectorRow {
                asset_hex: "aa11".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: vec![0.1f32; majestical_index::vector_store::DIM],
            }])
            .expect("seed vector");
    }

    /// Corruption recipe 1: overwrites every manifest file with garbage
    /// bytes. This is what makes lance's own manifest reader panic (an
    /// unchecked subtraction; verified by hand against real `lance` 9.0.0),
    /// not merely error — the reason `open_or_rebuild` needs `catch_unwind`
    /// at all, not just a `Result` match.
    fn corrupt_all_manifests(lance_dir: &Path) {
        let versions_dir = lance_dir.join("vectors.lance/_versions");
        for entry in std::fs::read_dir(&versions_dir)
            .expect("read _versions dir")
            .flatten()
        {
            if entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "manifest")
            {
                std::fs::write(entry.path(), b"GARBAGE-NOT-A-REAL-MANIFEST")
                    .expect("corrupt manifest");
            }
        }
    }

    /// Corruption recipe 2: truncates a data file. `open` alone doesn't
    /// notice (the manifest is intact) — only a read that reaches into the
    /// truncated file's bytes fails, which is exactly why `open_and_probe`
    /// runs a probe scan rather than just `VectorStore::open`.
    fn truncate_a_data_file(lance_dir: &Path) {
        let data_dir = lance_dir.join("vectors.lance/data");
        let entry = std::fs::read_dir(&data_dir)
            .expect("read data dir")
            .flatten()
            .next()
            .expect("at least one data file");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(entry.path())
            .expect("open data file for truncation");
        file.set_len(10).expect("truncate data file");
    }

    /// Recovery from a garbage manifest: `VectorStore::open` panics on this
    /// (see `corrupt_all_manifests`'s doc comment) rather than erroring, so
    /// this is the case that pins `open_or_rebuild` needing `catch_unwind`
    /// and not just error handling — a plain `Result`-based retry (the
    /// pre-fix version of this function) would have let this panic escape
    /// and abort the whole `index run`.
    #[test]
    fn open_or_rebuild_recovers_from_a_garbage_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        seed_populated_lance_store(&lance_dir);
        corrupt_all_manifests(&lance_dir);

        let store = open_or_rebuild(&lance_dir).expect("must recover, not panic or error");
        assert_eq!(
            store.existing_keys("m1").expect("keys").len(),
            0,
            "rebuilt store starts empty — the corrupt generation is gone, not recovered"
        );
        // A second call against the now-healthy store must be clean — no
        // note, no further rebuild.
        open_or_rebuild(&lance_dir).expect("second open is clean");
    }

    /// Recovery from a truncated data file: `open` alone succeeds (the
    /// manifest is fine), so this pins that the probe scan inside
    /// `open_and_probe` is what actually catches this corruption, not just
    /// `VectorStore::open`.
    #[test]
    fn open_or_rebuild_recovers_from_a_truncated_data_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        seed_populated_lance_store(&lance_dir);
        truncate_a_data_file(&lance_dir);

        let store = open_or_rebuild(&lance_dir).expect("must recover");
        assert_eq!(store.existing_keys("m1").expect("keys").len(), 0);
    }

    /// Recovery when the lance path itself is a plain file, not a
    /// directory: `remove_dir_all` errors with `NotADirectory` on a file, so
    /// this pins that `remove_lance_state` has a file-specific removal arm —
    /// without it, the corrupt path can never be cleared and every future
    /// run hits the same broken path forever.
    #[test]
    fn open_or_rebuild_recovers_when_the_lance_path_is_a_plain_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_path = dir.path().join("lance");
        std::fs::write(&lance_path, b"not a directory").expect("seed a plain file");

        let store = open_or_rebuild(&lance_path).expect("must recover");
        store
            .add(vec![majestical_index::vector_store::VectorRow {
                asset_hex: "bb22".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: vec![0.1f32; majestical_index::vector_store::DIM],
            }])
            .expect("recovered store is usable");
    }
}
