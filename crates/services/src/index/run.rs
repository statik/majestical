//! `maj index run`'s compute: builds the derivation plan, works every kind's
//! queued items (thumbnails, embeddings, keyframes, transcripts,
//! transcript-embeddings, OCR, PDF text, captions), and heals `text_fts` from
//! blobs. Moved from `crates/cli/src/index_cmd.rs`. The `--watch` loop, the
//! failure-report bookkeeping (`failure_report_json`/`merge_failure_report`/
//! `write_failure_report`), and all rendering (`print_run_result`/
//! `run_result_json`) stay in the CLI — this module hands back one
//! [`IndexRunOutcome`] per pass and never prints.
use crate::app::FsApp;
use crate::catalog::open_catalog;
use crate::describer_config::load_config;
use crate::error::ServiceError;
use crate::index::heal::heal_text_fts;
use anyhow::{Context, Result};
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::ports::{Describer, TagSubject};
use majestical_core::projection::Projection;
use majestical_describe::HttpDescriber;
use majestical_index::blob::{BlobStore, Derivation};
use majestical_index::chunk::chunk_segments;
use majestical_index::encoder::{Encoder, EncoderOptions};
use majestical_index::model::MINILM;
use majestical_index::ocr::OCR_MODEL_TAG;
use majestical_index::pdf::PDF_MODEL_TAG;
use majestical_index::text_encoder::TextEncoder;
use majestical_index::transcribe::{Transcriber, WHISPER_MODEL_TAG};
use majestical_index::vector_store::{TextChunkRow, TextVectorStore, VectorRow, VectorStore};
use majestical_index::work::{self, WorkKind};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// zstd level for JSON derivation blobs (transcripts, OCR, PDF text) —
/// matches the level `BlobStore::write_vector` uses for vector blobs.
const BLOB_ZSTD_LEVEL: i32 = 3;

/// One pass's request: `--kinds` already validated/defaulted by the CLI (see
/// `crates/cli/src/index_cmd.rs::parse_kinds`) into a plain set, `--limit`
/// and `--threads` passed straight through, and the describer API key the
/// CLI read from the environment (`MAJ_OPENROUTER_KEY`) — the same
/// CLI-reads-env-and-passes-in convention `describer_config::test` already
/// uses, since `majestical_services` never touches the environment itself.
/// `--watch` and `--json` are CLI-only concerns and have no place here.
pub struct IndexRunReq {
    pub kinds: BTreeSet<String>,
    pub limit: Option<usize>,
    pub threads: Option<usize>,
    pub api_key: Option<String>,
}

fn default_index_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(4)
}

/// Edge length for PDF page-1 renders feeding the thumbnail/embedding
/// pipeline — comfortably above both the thumbnail edge and the encoder's
/// input resolution, so one render serves both consumers.
const PDF_RENDER_EDGE: u32 = 1024;

/// Decodes a source frame for a thumbnail, routed by media kind: the image
/// itself for stills, a frame one-tenth of the way into a video (early
/// enough to usually avoid a black open/fade-in, late enough to usually
/// avoid a title card), or a page-1 render for a PDF.
fn decode_thumb_source(path: &Path) -> Result<image::RgbImage> {
    match media_kind(&path.to_string_lossy()) {
        MediaKind::Video => {
            let info = majestical_index::video::probe(path)?;
            Ok(majestical_index::video::extract_frame(
                path,
                info.duration_ms / 10,
            )?)
        }
        MediaKind::Pdf => Ok(majestical_index::pdf::render_first_page(
            path,
            PDF_RENDER_EDGE,
        )?),
        MediaKind::Image | MediaKind::Audio | MediaKind::Other => {
            Ok(majestical_index::thumbs::decode_image(path)?)
        }
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
#[derive(serde::Serialize)]
pub struct ThumbOutcome {
    pub written: u64,
    pub failed: Vec<(PathBuf, String)>,
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
#[derive(serde::Serialize)]
pub struct EmbedOutcome {
    pub written: u64,
    pub loaded: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// One pass's keyframe-kind result: `videos_done` videos whose manifest got
/// written this pass (the planner's done marker — see [`run_keyframe_items`]),
/// `keyframes_written` individual frames actually extracted and embedded,
/// `keyframes_failed` individual timestamps that failed extract/embed
/// (whether or not their video ended up over the half-failed threshold — see
/// [`over_half_failed`]), and per-video `failed` (path, reason — for a video
/// over that threshold, the reason includes the first per-timestamp failure).
#[derive(Default, serde::Serialize)]
pub struct KeyframeOutcome {
    pub videos_done: u64,
    pub keyframes_written: u64,
    pub keyframes_failed: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// One `index run` pass: builds the plan, works every kind's items, and
/// heals `text_fts` from blobs.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced, a model that
/// passed its presence check fails to load, the Lance vector store can't be
/// opened even after one corruption-recovery retry, or `text_fts` healing
/// fails.
pub fn run(
    app: &FsApp,
    catalog_dir: &Path,
    req: &IndexRunReq,
) -> Result<IndexRunOutcome, ServiceError> {
    run_impl(app, catalog_dir, req).map_err(ServiceError::from)
}

fn run_impl(app: &FsApp, catalog_dir: &Path, req: &IndexRunReq) -> Result<IndexRunOutcome> {
    let (mut db, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let caps = crate::index::capabilities(catalog_dir);
    let plan = crate::index::build_plan(&projection, &blobs, &req.kinds, &caps);
    let items = split_and_cap_items(plan.items, req.limit);

    let jobs = req.threads.unwrap_or_else(default_index_jobs);
    let embed_paths = EmbedPaths {
        lance_dir: state_dir.join("lance"),
        coreml_cache_dir: state_dir.join("coreml-cache"),
    };
    let caption_env = CaptionEnv {
        catalog_root: catalog_dir,
        vocab: tag_vocabulary(&projection),
        api_key: req.api_key.clone(),
    };
    let outcome = run_all_kinds(&embed_paths, &blobs, &items, jobs, &caption_env)?;

    heal_text_fts(&mut db, &blobs)?;
    Ok(outcome)
}

/// The caption runner's per-pass inputs beyond blobs/items: where the
/// describer config lives, the catalog's current tag vocabulary, and the
/// describer API key (read from the environment by the CLI, passed in here
/// — see [`IndexRunReq`]'s doc). Bundled to keep [`run_all_kinds`] within the
/// house 5-positional-parameter limit.
struct CaptionEnv<'a> {
    catalog_root: &'a Path,
    vocab: Vec<String>,
    api_key: Option<String>,
}

/// The catalog's full folksonomy — the union of every asset's tags, sorted
/// — sent to the describer so it prefers existing tags over inventing new
/// spellings of the same concept.
fn tag_vocabulary(projection: &Projection) -> Vec<String> {
    let mut vocab = BTreeSet::new();
    for (asset, _) in projection.assets() {
        vocab.extend(projection.tags(asset));
    }
    vocab.into_iter().collect()
}

/// Executes every kind's items in priority order. Kinds whose capability is
/// missing (a runner's own model-presence re-check) or whose item list is
/// empty are cheap no-ops.
fn run_all_kinds(
    paths: &EmbedPaths,
    blobs: &BlobStore,
    items: &KindItems,
    jobs: usize,
    caption_env: &CaptionEnv<'_>,
) -> Result<IndexRunOutcome> {
    Ok(IndexRunOutcome {
        thumbs: run_thumb_items(blobs, &items.thumbs, jobs),
        embed: run_embed_items(paths, blobs, &items.embeds)?,
        keyframes: run_keyframe_items(paths, blobs, &items.keyframes)?,
        transcribe: run_transcribe_items(blobs, &items.transcribes)?,
        transcript_embed: run_transcript_embed_items(paths, blobs, &items.transcript_embeds)?,
        ocr: run_ocr_items(blobs, &items.ocr_images, &items.ocr_keyframes),
        pdf: run_pdf_text_items(blobs, &items.pdfs),
        captions: run_caption_items(blobs, &items.captions, caption_env),
    })
}

/// Every kind's items for one pass, split so each executor gets exactly its
/// own queue.
#[derive(Default)]
struct KindItems {
    thumbs: Vec<work::WorkItem>,
    embeds: Vec<work::WorkItem>,
    keyframes: Vec<work::WorkItem>,
    transcribes: Vec<work::WorkItem>,
    transcript_embeds: Vec<work::WorkItem>,
    ocr_images: Vec<work::WorkItem>,
    ocr_keyframes: Vec<work::WorkItem>,
    pdfs: Vec<work::WorkItem>,
    captions: Vec<work::WorkItem>,
}

impl KindItems {
    fn cap_each(&mut self, limit: usize) {
        self.thumbs.truncate(limit);
        self.embeds.truncate(limit);
        self.keyframes.truncate(limit);
        self.transcribes.truncate(limit);
        self.transcript_embeds.truncate(limit);
        self.ocr_images.truncate(limit);
        self.ocr_keyframes.truncate(limit);
        self.pdfs.truncate(limit);
        self.captions.truncate(limit);
    }
}

/// Splits `items` by kind, then caps each kind independently at `limit` —
/// every kind has its own executor, so `--limit` bounds each one's own
/// per-pass budget rather than one kind starving another.
fn split_and_cap_items(items: Vec<work::WorkItem>, limit: Option<usize>) -> KindItems {
    let mut split = KindItems::default();
    for item in items {
        match item.kind {
            WorkKind::Thumb => split.thumbs.push(item),
            WorkKind::ImageEmbed => split.embeds.push(item),
            WorkKind::Keyframes => split.keyframes.push(item),
            WorkKind::Transcribe => split.transcribes.push(item),
            WorkKind::TranscriptEmbed => split.transcript_embeds.push(item),
            WorkKind::OcrImage => split.ocr_images.push(item),
            WorkKind::OcrKeyframes => split.ocr_keyframes.push(item),
            WorkKind::PdfText => split.pdfs.push(item),
            WorkKind::Caption => split.captions.push(item),
        }
    }
    if let Some(limit) = limit {
        split.cap_each(limit);
    }
    split
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
        Err(reason) => {
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!(
                    "note: lance vector store at {} is unreadable ({reason}) — removing and \
                     rebuilding from blobs",
                    dir.display()
                );
            }
        }
    }
    remove_lance_state(dir)?;
    open_and_probe(dir)
        .map_err(|reason| anyhow::anyhow!("rebuilding lance store at {}: {reason}", dir.display()))
}

/// Decodes an `ImageEmbed` item's pixels: a page-1 render for a PDF (the
/// same route `decode_thumb_source` takes, so PDFs flow through the
/// existing embedding kind), the decoded image otherwise.
fn decode_embed_source(path: &Path) -> Result<image::RgbImage> {
    if media_kind(&path.to_string_lossy()) == MediaKind::Pdf {
        Ok(majestical_index::pdf::render_first_page(
            path,
            PDF_RENDER_EDGE,
        )?)
    } else {
        Ok(majestical_index::thumbs::decode_image(path)?)
    }
}

fn embed_one(blobs: &BlobStore, encoder: &mut Encoder, item: &work::WorkItem) -> Result<VectorRow> {
    let rgb = decode_embed_source(&item.abs_path)?;
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
    } else if let Some(model_dir) = crate::index::model_dir_if_present() {
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
pub(crate) fn ts_ms_i64(ts_ms: u64) -> i64 {
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
/// gave up on: the path (nothing else in this message otherwise carries it),
/// the failure/total counts, and the first per-timestamp failure's reason,
/// so the message says why at least one keyframe failed, not just how many.
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

/// Parses a keyframe manifest written by [`keyframes_manifest_json`] back
/// into `(model_tag, detected, timestamps)` — the reader the keyframe-OCR
/// runner uses to diff a video's detected timestamps against its OCR blobs.
///
/// # Errors
/// Returns an error naming the missing/mistyped field on malformed bytes.
fn keyframes_manifest_read(bytes: &[u8]) -> Result<(String, usize, Vec<u64>)> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("parsing keyframe manifest json")?;
    let model_tag = value["model_tag"]
        .as_str()
        .context("keyframe manifest missing string field 'model_tag'")?
        .to_string();
    let detected = value["detected"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .context("keyframe manifest missing integer field 'detected'")?;
    let timestamps = value["timestamps"]
        .as_array()
        .context("keyframe manifest missing array field 'timestamps'")?
        .iter()
        .map(|v| {
            v.as_u64()
                .context("keyframe manifest timestamp is not a non-negative integer")
        })
        .collect::<Result<Vec<u64>>>()?;
    Ok((model_tag, detected, timestamps))
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
    let Some(model_dir) = crate::index::model_dir_if_present() else {
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

/// One pass's transcribe-kind result: `written` new transcript blobs and
/// per-item `failed` (path, reason).
#[derive(Default, serde::Serialize)]
pub struct TranscribeOutcome {
    pub written: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// Timeout-sizing fallback for sources ffprobe can't report a duration for
/// — audio-only containers have no video stream, which `video::probe`
/// requires. One hour keeps `audio_timeout` generous without letting a hung
/// ffmpeg block a pass for the better part of a day.
const FALLBACK_AUDIO_DURATION_MS: u64 = 60 * 60 * 1000;

fn transcribe_one(
    blobs: &BlobStore,
    transcriber: &Transcriber,
    item: &work::WorkItem,
) -> Result<()> {
    let duration_ms = majestical_index::video::probe(&item.abs_path)
        .map_or(FALLBACK_AUDIO_DURATION_MS, |info| info.duration_ms);
    let pcm = majestical_index::video::extract_audio_pcm(&item.abs_path, duration_ms)?;
    let transcript = transcriber.transcribe(&pcm)?;
    let json = transcript.to_json()?;
    let bytes = zstd::encode_all(&json[..], BLOB_ZSTD_LEVEL)
        .with_context(|| format!("compressing transcript for {}", item.abs_path.display()))?;
    let path = blobs.path_for(
        &item.asset_hex,
        &Derivation::Transcript {
            model_tag: WHISPER_MODEL_TAG,
        },
    );
    blobs.write_atomic(&path, &bytes)?;
    Ok(())
}

/// Works every `Transcribe` item serially: the whisper context is loaded
/// ONCE before the loop (model-bound kinds don't get a worker pool — one
/// Metal-backed inference already saturates the machine), then each item is
/// probed for duration (audio-only files fall back to
/// [`FALLBACK_AUDIO_DURATION_MS`] for the extract timeout), decoded to PCM,
/// transcribed, and written as a zstd JSON blob.
///
/// # Errors
/// Returns an error only if the whisper model fails to load — per-item
/// failures land in the outcome's `failed` list instead.
fn run_transcribe_items(blobs: &BlobStore, items: &[work::WorkItem]) -> Result<TranscribeOutcome> {
    let mut outcome = TranscribeOutcome::default();
    if items.is_empty() {
        return Ok(outcome);
    }
    let Some(model_dir) = crate::capability::whisper_model_dir_if_present() else {
        return Ok(outcome);
    };
    let transcriber = Transcriber::load(&model_dir)?;
    for item in items {
        match transcribe_one(blobs, &transcriber, item) {
            Ok(()) => outcome.written += 1,
            Err(err) => outcome
                .failed
                .push((item.abs_path.clone(), err.to_string())),
        }
    }
    Ok(outcome)
}

/// One pass's transcript-embed result: `chunks_written` freshly embedded
/// chunk vectors, `loaded` vectors pulled in from chunk blobs the local
/// text Lance table didn't have yet (the blob↔Lance diff — see
/// [`load_missing_text_vectors_from_blobs`]), `empty` transcripts that
/// chunked to nothing (their `ChunksEmpty` marker written), and per-item
/// `failed` (transcript blob path, reason — the item's own `abs_path` can
/// be the empty sentinel, see `work::WorkItem::abs_path`).
#[derive(Default, serde::Serialize)]
pub struct TranscriptEmbedOutcome {
    pub chunks_written: u64,
    pub loaded: u64,
    pub empty: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// What embedding one transcript's chunks produced.
enum ChunkEmbedResult {
    Written(u64),
    Empty,
}

fn embed_transcript_chunks(
    blobs: &BlobStore,
    encoder: &mut TextEncoder,
    store: &TextVectorStore,
    item: &work::WorkItem,
) -> Result<ChunkEmbedResult> {
    let transcript_path = blobs.path_for(
        &item.asset_hex,
        &Derivation::Transcript {
            model_tag: WHISPER_MODEL_TAG,
        },
    );
    let transcript = crate::index::blob_read::read_transcript_blob(&transcript_path)?;
    let chunks = chunk_segments(&transcript.segments);
    let mut rows = Vec::new();
    let mut chunk_starts = Vec::new();
    for chunk in &chunks {
        // Whitespace-only segments can chunk to empty text — nothing to
        // embed, and the encoder would just produce a meaningless vector.
        if chunk.text.trim().is_empty() {
            continue;
        }
        chunk_starts.push(chunk.start_ms);
        let path = blobs.path_for(
            &item.asset_hex,
            &Derivation::TranscriptChunk {
                model_tag: MINILM.tag,
                start_ms: chunk.start_ms,
            },
        );
        // A blob left by an interrupted earlier run: skip re-inference — the
        // always-on blob↔Lance diff (`load_missing_text_vectors_from_blobs`)
        // indexes it if the store is missing its row.
        if path.is_file() {
            continue;
        }
        let vector = encoder.embed(&chunk.text)?;
        blobs.write_vector(&path, &vector)?;
        rows.push(TextChunkRow {
            asset_hex: item.asset_hex.clone(),
            source: "transcript".to_string(),
            start_ms: ts_ms_i64(chunk.start_ms),
            end_ms: ts_ms_i64(chunk.end_ms),
            model_tag: MINILM.tag.to_string(),
            text: chunk.text.clone(),
            vector,
        });
    }
    if chunk_starts.is_empty() {
        let marker = blobs.path_for(
            &item.asset_hex,
            &Derivation::ChunksEmpty {
                model_tag: MINILM.tag,
            },
        );
        write_json_blob_uncompressed(blobs, &marker, &serde_json::json!({}))?;
        return Ok(ChunkEmbedResult::Empty);
    }
    let written = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    finish_chunk_embed(blobs, store, &item.asset_hex, rows, &chunk_starts)?;
    Ok(ChunkEmbedResult::Written(written))
}

/// Adds the freshly embedded rows to the text store, then — ONLY after the
/// add succeeds — writes the `ChunksComplete` done-marker (its body listing
/// every chunk's `start_ms` under the same `timestamps` key `OcrComplete`
/// uses, keeping the marker family consistent). A failing add returns
/// before the marker exists, so the item re-plans next pass instead of
/// counting done with vectors missing from the store.
fn finish_chunk_embed(
    blobs: &BlobStore,
    store: &TextVectorStore,
    asset_hex: &str,
    rows: Vec<TextChunkRow>,
    chunk_starts: &[u64],
) -> Result<()> {
    store.add(rows)?;
    let marker = blobs.path_for(
        asset_hex,
        &Derivation::ChunksComplete {
            model_tag: MINILM.tag,
        },
    );
    write_json_blob_uncompressed(
        blobs,
        &marker,
        &serde_json::json!({ "timestamps": chunk_starts }),
    )
}

/// Works every `TranscriptEmbed` item serially: reads the transcript blob
/// (never the source file), chunks it, embeds each non-empty chunk with a
/// `MiniLM` encoder loaded ONCE, writes chunk vector blobs, indexes the
/// chunks (text included) into the local text Lance table, and writes the
/// `ChunksComplete` done-marker only after the store add succeeds. A
/// transcript with zero non-empty chunks gets the `ChunksEmpty` marker
/// instead. Then ALWAYS performs the chunk blob↔Lance diff — even with zero
/// items or no `MiniLM` installed — see
/// [`load_missing_text_vectors_from_blobs`], the text-table analogue of
/// `run_embed_items`'s always-on self-heal.
///
/// # Errors
/// Returns an error if the `MiniLM` model fails to load, the text vector
/// store can't be opened even after one corruption-recovery retry, or the
/// blob↔Lance diff fails — per-item failures land in the outcome's
/// `failed` list instead.
fn run_transcript_embed_items(
    paths: &EmbedPaths,
    blobs: &BlobStore,
    items: &[work::WorkItem],
) -> Result<TranscriptEmbedOutcome> {
    let mut outcome = TranscriptEmbedOutcome::default();
    let store = open_or_rebuild_text(&paths.lance_dir)?;
    let model_dir = if items.is_empty() {
        None
    } else {
        crate::capability::minilm_model_dir_if_present()
    };
    if let Some(model_dir) = model_dir {
        let mut encoder = TextEncoder::load(&model_dir)?;
        for item in items {
            let transcript_path = blobs.path_for(
                &item.asset_hex,
                &Derivation::Transcript {
                    model_tag: WHISPER_MODEL_TAG,
                },
            );
            match embed_transcript_chunks(blobs, &mut encoder, &store, item) {
                Ok(ChunkEmbedResult::Written(n)) => outcome.chunks_written += n,
                Ok(ChunkEmbedResult::Empty) => outcome.empty += 1,
                Err(err) => outcome.failed.push((transcript_path, err.to_string())),
            }
        }
    }
    outcome.loaded = load_missing_text_vectors_from_blobs(&store, blobs)?;
    Ok(outcome)
}

/// The chunk blob↔Lance diff: adds every `MiniLM` chunk vector blob the
/// local text Lance table doesn't have yet, recovering each chunk's text
/// and `end_ms` by re-chunking the asset's transcript blob and matching on
/// `start_ms` (chunking is deterministic, so a blob written by any machine
/// re-chunks identically). This is what makes the text table rebuildable
/// from blobs — a teammate's synced chunk vectors, a lance dir rebuilt
/// after corruption, or a run interrupted between blob write and store add
/// all converge here with zero re-inference. A chunk blob whose transcript
/// blob is gone (or no longer chunks to that `start_ms`) is skipped with a
/// counted stderr note rather than failing the pass.
fn load_missing_text_vectors_from_blobs(store: &TextVectorStore, blobs: &BlobStore) -> Result<u64> {
    let model_tag = MINILM.tag;
    let existing = store.existing_keys(model_tag)?;
    let mut chunk_cache: std::collections::BTreeMap<
        String,
        Option<Vec<majestical_index::chunk::Chunk>>,
    > = std::collections::BTreeMap::new();
    let mut loaded = 0u64;
    let mut skipped = 0u64;
    let mut batch = Vec::new();
    for blob_ref in blobs.iter_vectors(model_tag)? {
        if blob_ref.kind != "chunk" {
            continue;
        }
        let key = (blob_ref.asset_hex.clone(), blob_ref.ts_ms);
        if existing.contains(&key) {
            continue;
        }
        let chunks = chunk_cache
            .entry(blob_ref.asset_hex.clone())
            .or_insert_with(|| load_transcript_chunks(blobs, &blob_ref.asset_hex));
        let chunk = chunks.as_ref().and_then(|chunks| {
            chunks
                .iter()
                .find(|c| ts_ms_i64(c.start_ms) == blob_ref.ts_ms)
        });
        let Some(chunk) = chunk else {
            skipped += 1;
            continue;
        };
        let vector = blobs.read_vector(&blob_ref.path)?;
        batch.push(TextChunkRow {
            asset_hex: blob_ref.asset_hex,
            source: "transcript".to_string(),
            start_ms: blob_ref.ts_ms,
            end_ms: ts_ms_i64(chunk.end_ms),
            model_tag: model_tag.to_string(),
            text: chunk.text.clone(),
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
    if skipped > 0 {
        #[expect(
            clippy::print_stderr,
            reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
        )]
        {
            eprintln!(
                "note: {skipped} chunk vector blob(s) skipped in the text-store rebuild — \
                 transcript blob missing, or its chunking no longer matches"
            );
        }
    }
    Ok(loaded)
}

/// Re-derives an asset's chunk list from its transcript blob, or `None`
/// when the blob is missing/unreadable (the caller counts and skips).
fn load_transcript_chunks(
    blobs: &BlobStore,
    asset_hex: &str,
) -> Option<Vec<majestical_index::chunk::Chunk>> {
    let path = blobs.path_for(
        asset_hex,
        &Derivation::Transcript {
            model_tag: WHISPER_MODEL_TAG,
        },
    );
    let transcript = crate::index::blob_read::read_transcript_blob(&path).ok()?;
    Some(chunk_segments(&transcript.segments))
}

/// One pass's OCR-kind result across stills and video keyframes.
#[derive(Default, serde::Serialize)]
pub struct OcrOutcome {
    pub images_written: u64,
    pub videos_done: u64,
    pub keyframes_written: u64,
    pub failed: Vec<(PathBuf, String)>,
}

fn ocr_one_still(blobs: &BlobStore, item: &work::WorkItem) -> Result<()> {
    let rgb = majestical_index::thumbs::decode_image(&item.abs_path)?;
    let result = majestical_index::ocr::recognize_text(&rgb)?;
    write_json_blob(
        blobs,
        &blobs.path_for(
            &item.asset_hex,
            &Derivation::OcrImage {
                model_tag: OCR_MODEL_TAG,
            },
        ),
        &result.to_json()?,
    )
}

/// One video's keyframe-OCR pass result.
struct VideoOcrResult {
    keyframes_written: u64,
    failures: usize,
    total: usize,
    first_failure: Option<String>,
}

fn ocr_one_keyframe(blobs: &BlobStore, item: &work::WorkItem, ts_ms: u64) -> Result<()> {
    let frame = majestical_index::video::extract_frame(&item.abs_path, ts_ms)?;
    let result = majestical_index::ocr::recognize_text(&frame)?;
    write_json_blob(
        blobs,
        &blobs.path_for(
            &item.asset_hex,
            &Derivation::OcrKeyframe {
                model_tag: OCR_MODEL_TAG,
                timestamp_ms: ts_ms,
            },
        ),
        &result.to_json()?,
    )
}

/// Works one `OcrKeyframes` video: diffs the keyframe manifest's timestamps
/// against existing per-frame OCR blobs, OCRs each missing one, and — only
/// once EVERY timestamp has a blob — writes the `OcrComplete` done-marker.
/// The marker body carries the manifest's timestamp list so a future
/// manifest change stays auditable against what was actually OCR'd.
fn ocr_video_keyframes(blobs: &BlobStore, item: &work::WorkItem) -> Result<VideoOcrResult> {
    let manifest_path = blobs.path_for(
        &item.asset_hex,
        &Derivation::KeyframeManifest {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading keyframe manifest {}", manifest_path.display()))?;
    let (_, _, timestamps) = keyframes_manifest_read(&bytes)?;

    let mut result = VideoOcrResult {
        keyframes_written: 0,
        failures: 0,
        total: timestamps.len(),
        first_failure: None,
    };
    for &ts_ms in &timestamps {
        let blob_path = blobs.path_for(
            &item.asset_hex,
            &Derivation::OcrKeyframe {
                model_tag: OCR_MODEL_TAG,
                timestamp_ms: ts_ms,
            },
        );
        if blob_path.is_file() {
            continue;
        }
        match ocr_one_keyframe(blobs, item, ts_ms) {
            Ok(()) => result.keyframes_written += 1,
            Err(err) => {
                result.failures += 1;
                result.first_failure.get_or_insert(err.to_string());
            }
        }
    }
    if result.failures == 0 {
        let marker = blobs.path_for(
            &item.asset_hex,
            &Derivation::OcrComplete {
                model_tag: OCR_MODEL_TAG,
            },
        );
        write_json_blob_uncompressed(
            blobs,
            &marker,
            &serde_json::json!({ "timestamps": timestamps }),
        )?;
    }
    Ok(result)
}

/// Works every OCR item — stills first, then videos whose keyframe manifest
/// is ready. Vision ships with macOS, so there's no capability re-check
/// here; per-item failures are collected, never propagated.
fn run_ocr_items(
    blobs: &BlobStore,
    stills: &[work::WorkItem],
    videos: &[work::WorkItem],
) -> OcrOutcome {
    let mut outcome = OcrOutcome::default();
    for item in stills {
        match ocr_one_still(blobs, item) {
            Ok(()) => outcome.images_written += 1,
            Err(err) => outcome
                .failed
                .push((item.abs_path.clone(), err.to_string())),
        }
    }
    for item in videos {
        match ocr_video_keyframes(blobs, item) {
            Ok(result) => {
                outcome.keyframes_written += result.keyframes_written;
                if result.failures == 0 {
                    outcome.videos_done += 1;
                } else {
                    let first = result.first_failure.as_deref().unwrap_or("<no reason>");
                    // No path in the message: the tuple's first element
                    // already carries it (Vision/ffmpeg reasons embed it too).
                    outcome.failed.push((
                        item.abs_path.clone(),
                        format!(
                            "{}/{} keyframes failed OCR — first failure: {first} — \
                             video incomplete, will retry",
                            result.failures, result.total,
                        ),
                    ));
                }
            }
            Err(err) => outcome
                .failed
                .push((item.abs_path.clone(), err.to_string())),
        }
    }
    outcome
}

/// One pass's PDF-text result.
#[derive(Default, serde::Serialize)]
pub struct PdfOutcome {
    pub written: u64,
    pub failed: Vec<(PathBuf, String)>,
}

fn pdf_text_one(blobs: &BlobStore, item: &work::WorkItem) -> Result<()> {
    let content = majestical_index::pdf::extract_text(&item.abs_path)?;
    write_json_blob(
        blobs,
        &blobs.path_for(
            &item.asset_hex,
            &Derivation::PdfText {
                model_tag: PDF_MODEL_TAG,
            },
        ),
        &content.to_json()?,
    )
}

/// Works every `PdfText` item — `PDFKit` ships with macOS, so there's no
/// capability re-check; per-item failures are collected, never propagated.
fn run_pdf_text_items(blobs: &BlobStore, items: &[work::WorkItem]) -> PdfOutcome {
    let mut outcome = PdfOutcome::default();
    for item in items {
        match pdf_text_one(blobs, item) {
            Ok(()) => outcome.written += 1,
            Err(err) => outcome
                .failed
                .push((item.abs_path.clone(), err.to_string())),
        }
    }
    outcome
}

/// One pass's caption-kind result: `written` caption-item completions (a
/// still's caption blob, or a video's captions blob — each with its tags
/// blob) and per-item `failed` (path, reason).
#[derive(Default, serde::Serialize)]
pub struct CaptionOutcome {
    pub written: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// Upper bound on keyframes described per video: captioning every detected
/// scene of a long video would mean hundreds of LLM round-trips per asset
/// for marginal search gain, so the runner samples evenly instead.
const MAX_DESCRIBED_KEYFRAMES: usize = 12;

/// The failure reason recorded for caption items abandoned after the first
/// backend failure in a pass — see [`run_caption_items`]'s abort policy.
const DESCRIBER_SKIPPED_REASON: &str = "describer unavailable — skipped after first failure";

/// Why one caption item failed: a `Backend` (describer) failure aborts the
/// remaining items in this pass — the backend is down for all of them, and
/// hammering it item after item just burns wall-clock — while an `Item`
/// failure (missing thumb, unreadable manifest, frame extraction) records
/// and moves on to the next item.
enum CaptionFailure {
    Backend(String),
    Item(String),
}

/// Works every `Caption` item serially against the configured describer.
/// Run-level `Ok` always: per-item failures (including a whole pass
/// abandoned to a backend outage) land in the outcome's `failed` list, and
/// items without done-blobs simply re-plan next run.
///
/// The config is re-loaded here rather than threaded from `capabilities()`:
/// caps only carries the tag string the planner needs, while the runner
/// needs the full config (base URL, model, key) — and re-reading one small
/// TOML file per pass is cheaper than widening every signature between
/// `run` and this executor to carry it.
fn run_caption_items(
    blobs: &BlobStore,
    items: &[work::WorkItem],
    env: &CaptionEnv<'_>,
) -> CaptionOutcome {
    let mut outcome = CaptionOutcome::default();
    if items.is_empty() {
        return outcome;
    }
    let config = match load_config(env.catalog_root) {
        Ok(Some(config)) => config,
        // The planner only queued Caption items because a describer was
        // configured when caps were computed; a config removed/broken since
        // then degrades to a no-op pass and the items re-plan later.
        Ok(None) => return outcome,
        Err(err) => {
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!(
                    "note: describer config unreadable ({err:#}) — captions skipped this pass"
                );
            }
            return outcome;
        }
    };
    let model_tag = config.model_tag();
    let describer = HttpDescriber::new(config, env.api_key.clone());
    for (index, item) in items.iter().enumerate() {
        match caption_one_item(blobs, &describer, item, &model_tag, &env.vocab) {
            Ok(()) => outcome.written += 1,
            Err(CaptionFailure::Item(reason)) => {
                outcome.failed.push((item.abs_path.clone(), reason));
            }
            Err(CaptionFailure::Backend(reason)) => {
                outcome.failed.push((item.abs_path.clone(), reason));
                for skipped in &items[index + 1..] {
                    outcome.failed.push((
                        skipped.abs_path.clone(),
                        DESCRIBER_SKIPPED_REASON.to_string(),
                    ));
                }
                break;
            }
        }
    }
    outcome
}

/// Routes one `Caption` item by media kind: stills (images and PDFs — the
/// planner queues both) caption their thumbnail blob; videos caption a
/// sample of their manifest keyframes.
fn caption_one_item(
    blobs: &BlobStore,
    describer: &HttpDescriber,
    item: &work::WorkItem,
    model_tag: &str,
    vocab: &[String],
) -> Result<(), CaptionFailure> {
    if media_kind(&item.abs_path.to_string_lossy()) == MediaKind::Video {
        caption_video(blobs, describer, item, model_tag, vocab)
    } else {
        caption_still(blobs, describer, item, model_tag, vocab)
    }
}

/// Captions + tag-suggests one still from its thumbnail blob — already
/// WebP, exactly the MIME type the client's data URL claims, and derived
/// identically for images and PDF page-1 renders, so no re-decode here.
/// Done (in the planner) means BOTH blobs exist, so an item retried after a
/// partial run — caption written, tags call failed — skips the caption
/// round-trip and goes straight to the missing tags half.
fn caption_still(
    blobs: &BlobStore,
    describer: &HttpDescriber,
    item: &work::WorkItem,
    model_tag: &str,
    vocab: &[String],
) -> Result<(), CaptionFailure> {
    let thumb_path = blobs.path_for(&item.asset_hex, &Derivation::Thumb);
    let webp = std::fs::read(&thumb_path).map_err(|e| {
        CaptionFailure::Item(format!(
            "reading thumbnail {}: {e} (thumbs pass must run first)",
            thumb_path.display()
        ))
    })?;
    let caption_path = blobs.path_for(&item.asset_hex, &Derivation::Caption { model_tag });
    if !caption_path.is_file() {
        let caption = describer
            .caption(&webp)
            .map_err(|e| CaptionFailure::Backend(e.to_string()))?;
        write_caption_blob(blobs, &item.asset_hex, model_tag, &caption)
            .map_err(|e| CaptionFailure::Item(e.to_string()))?;
    }
    let suggestions = describer
        .suggest_tags(TagSubject::Image(&webp), vocab)
        .map_err(|e| CaptionFailure::Backend(e.to_string()))?;
    write_tags_blob(blobs, &item.asset_hex, model_tag, &suggestions)
        .map_err(|e| CaptionFailure::Item(e.to_string()))
}

/// Captions one video's keyframes, then tag-suggests from the pooled
/// caption texts — a text-only call. Done (in the planner) means BOTH the
/// `Captions` and `Tags` blobs exist, so a video retried after a partial
/// run — captions written, tags call failed — reuses the existing captions
/// blob's texts rather than re-extracting and re-captioning every keyframe.
/// A video whose manifest lists zero keyframes still gets its (empty)
/// blobs; without them it would re-plan forever.
fn caption_video(
    blobs: &BlobStore,
    describer: &HttpDescriber,
    item: &work::WorkItem,
    model_tag: &str,
    vocab: &[String],
) -> Result<(), CaptionFailure> {
    let captions_path = blobs.path_for(&item.asset_hex, &Derivation::Captions { model_tag });
    let described = match existing_video_captions(&captions_path) {
        Some(described) => described,
        None => describe_video_keyframes(blobs, describer, item, model_tag)?,
    };
    let texts: Vec<String> = described.into_iter().map(|(_, text)| text).collect();
    let suggestions = if texts.is_empty() {
        Vec::new()
    } else {
        describer
            .suggest_tags(TagSubject::Captions(&texts), vocab)
            .map_err(|e| CaptionFailure::Backend(e.to_string()))?
    };
    write_tags_blob(blobs, &item.asset_hex, model_tag, &suggestions)
        .map_err(|e| CaptionFailure::Item(e.to_string()))
}

/// An existing `Captions` blob's described rows, or `None` when the blob is
/// missing OR unreadable — an unreadable blob gets a stderr note and is
/// treated as absent, so the caller re-describes and overwrites it rather
/// than failing the item every pass forever over the same corrupt bytes.
fn existing_video_captions(captions_path: &Path) -> Option<Vec<(u64, String)>> {
    if !captions_path.is_file() {
        return None;
    }
    match crate::index::blob_read::read_video_captions_blob(captions_path) {
        Ok(described) => Some(described),
        Err(err) => {
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!("note: unreadable captions blob ({err}) — re-describing");
            }
            None
        }
    }
}

/// The caption half of one video item: samples up to
/// [`MAX_DESCRIBED_KEYFRAMES`] evenly-spaced manifest timestamps, extracts
/// and captions each, and writes the `Captions` blob before returning the
/// described rows.
fn describe_video_keyframes(
    blobs: &BlobStore,
    describer: &HttpDescriber,
    item: &work::WorkItem,
    model_tag: &str,
) -> Result<Vec<(u64, String)>, CaptionFailure> {
    // The hardcoded `model::MODEL_TAG` matches the planner's manifest gate:
    // `plan_caption_video` only queues an item after finding a manifest
    // under `caps.model_tag`, which — when set — is always this constant.
    let manifest_path = blobs.path_for(
        &item.asset_hex,
        &Derivation::KeyframeManifest {
            model_tag: majestical_index::model::MODEL_TAG,
        },
    );
    let bytes = std::fs::read(&manifest_path).map_err(|e| {
        CaptionFailure::Item(format!(
            "reading keyframe manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let (_, _, timestamps) =
        keyframes_manifest_read(&bytes).map_err(|e| CaptionFailure::Item(e.to_string()))?;
    let mut described = Vec::new();
    for ts_ms in select_described_timestamps(&timestamps) {
        let caption = caption_video_frame(describer, item, ts_ms)?;
        described.push((ts_ms, caption.text));
    }
    let json = video_captions_json(model_tag, timestamps.len(), &described);
    let captions_path = blobs.path_for(&item.asset_hex, &Derivation::Captions { model_tag });
    write_json_blob(blobs, &captions_path, &json)
        .map_err(|e| CaptionFailure::Item(e.to_string()))?;
    Ok(described)
}

/// Extracts one keyframe and downsizes it through the same 320px WebP
/// encoder thumbnails use (the one image dialect every backend accepts)
/// before captioning it.
fn caption_video_frame(
    describer: &HttpDescriber,
    item: &work::WorkItem,
    ts_ms: u64,
) -> Result<majestical_core::ports::Caption, CaptionFailure> {
    let frame = majestical_index::video::extract_frame(&item.abs_path, ts_ms)
        .map_err(|e| CaptionFailure::Item(e.to_string()))?;
    let webp = majestical_index::thumbs::thumbnail_webp(&frame)
        .map_err(|e| CaptionFailure::Item(e.to_string()))?;
    describer
        .caption(&webp)
        .map_err(|e| CaptionFailure::Backend(e.to_string()))
}

/// Up to [`MAX_DESCRIBED_KEYFRAMES`] timestamps, evenly spaced across the
/// detected list: the stride is `len / MAX` rounded up, so short videos
/// keep every keyframe and long ones sample across their full duration
/// instead of clustering at the start.
fn select_described_timestamps(timestamps: &[u64]) -> Vec<u64> {
    let step = timestamps.len().div_ceil(MAX_DESCRIBED_KEYFRAMES).max(1);
    timestamps
        .iter()
        .copied()
        .step_by(step)
        .take(MAX_DESCRIBED_KEYFRAMES)
        .collect()
}

/// The video captions blob body, mirroring [`keyframes_manifest_json`]'s
/// hand-built-JSON precedent: `detected_keyframes` preserves the manifest's
/// full count so the sampling gap (described vs. detected) stays auditable
/// from the blob alone.
fn video_captions_json(
    model_tag: &str,
    detected_keyframes: usize,
    described: &[(u64, String)],
) -> Vec<u8> {
    serde_json::json!({
        "model_tag": model_tag,
        "detected_keyframes": detected_keyframes,
        "described": described,
    })
    .to_string()
    .into_bytes()
}

/// Writes one still's caption as a zstd JSON blob of the core
/// [`majestical_core::ports::Caption`] struct.
fn write_caption_blob(
    blobs: &BlobStore,
    asset_hex: &str,
    model_tag: &str,
    caption: &majestical_core::ports::Caption,
) -> Result<()> {
    let json = serde_json::to_vec(caption).context("serializing caption")?;
    write_json_blob(
        blobs,
        &blobs.path_for(asset_hex, &Derivation::Caption { model_tag }),
        &json,
    )
}

/// Writes an asset's tag suggestions as a zstd JSON blob of
/// `Vec<TagSuggestion>`.
fn write_tags_blob(
    blobs: &BlobStore,
    asset_hex: &str,
    model_tag: &str,
    suggestions: &[majestical_core::ports::TagSuggestion],
) -> Result<()> {
    let json = serde_json::to_vec(suggestions).context("serializing tag suggestions")?;
    write_json_blob(
        blobs,
        &blobs.path_for(asset_hex, &Derivation::Tags { model_tag }),
        &json,
    )
}

/// zstd-compresses `json` and writes it atomically at `path`.
fn write_json_blob(blobs: &BlobStore, path: &Path, json: &[u8]) -> Result<()> {
    let bytes = zstd::encode_all(json, BLOB_ZSTD_LEVEL)
        .with_context(|| format!("compressing json blob {}", path.display()))?;
    blobs.write_atomic(path, &bytes)?;
    Ok(())
}

/// Writes a small marker/manifest JSON value atomically, uncompressed —
/// same convention as the keyframe manifest.
fn write_json_blob_uncompressed(
    blobs: &BlobStore,
    path: &Path,
    value: &serde_json::Value,
) -> Result<()> {
    blobs.write_atomic(path, value.to_string().as_bytes())?;
    Ok(())
}

/// Opens the text-chunk Lance table with the same probe + corruption
/// recovery policy as [`open_or_rebuild`]. The text table shares the lance
/// dataset directory with the image-vector table, so a rebuild here removes
/// both — both are disposable, repopulated from blobs by the always-on
/// blob↔Lance diffs (`load_missing_vectors_from_blobs` for image/keyframe
/// vectors, [`load_missing_text_vectors_from_blobs`] for chunk vectors)
/// with zero re-inference.
fn open_or_rebuild_text(dir: &Path) -> Result<TextVectorStore> {
    match open_and_probe_text(dir) {
        Ok(store) => return Ok(store),
        Err(reason) => {
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!(
                    "note: lance text store at {} is unreadable ({reason}) — removing and \
                     rebuilding from blobs",
                    dir.display()
                );
            }
        }
    }
    remove_lance_state(dir)?;
    open_and_probe_text(dir).map_err(|reason| {
        anyhow::anyhow!("rebuilding lance text store at {}: {reason}", dir.display())
    })
}

/// Text-store variant of [`open_and_probe`]: open plus a cheap probe scan,
/// with lance's manifest-reader panics caught as corruption.
fn open_and_probe_text(dir: &Path) -> Result<TextVectorStore, String> {
    let owned = dir.to_path_buf();
    majestical_index::vector_store::catch_corruption(move || {
        let store = TextVectorStore::open(&owned)?;
        store.existing_keys(MINILM.tag)?;
        Ok(store)
    })
}

/// Every kind's outcome for one pass, bundled so the CLI's printers and
/// failure-report bookkeeping take one value instead of eight.
#[derive(serde::Serialize)]
pub struct IndexRunOutcome {
    pub thumbs: ThumbOutcome,
    pub embed: EmbedOutcome,
    pub keyframes: KeyframeOutcome,
    pub transcribe: TranscribeOutcome,
    pub transcript_embed: TranscriptEmbedOutcome,
    pub ocr: OcrOutcome,
    pub pdf: PdfOutcome,
    pub captions: CaptionOutcome,
}

impl IndexRunOutcome {
    /// The transcripts CLI kind spans two executors — their failures merge
    /// for reporting.
    #[must_use]
    pub fn transcript_failures(&self) -> Vec<(PathBuf, String)> {
        let mut merged = self.transcribe.failed.clone();
        merged.extend(self.transcript_embed.failed.iter().cloned());
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_row(asset_hex: &str, dims: usize) -> TextChunkRow {
        TextChunkRow {
            asset_hex: asset_hex.to_string(),
            source: "transcript".to_string(),
            start_ms: 0,
            end_ms: 2000,
            model_tag: MINILM.tag.to_string(),
            text: "hello".to_string(),
            vector: vec![0.1; dims],
        }
    }

    #[test]
    fn keyframes_manifest_round_trips_through_the_reader() {
        let bytes = keyframes_manifest_json("m1", 5, &[1500, 4500, 7500]);
        let (model_tag, detected, timestamps) =
            keyframes_manifest_read(&bytes).expect("round trip");
        assert_eq!(model_tag, "m1");
        assert_eq!(detected, 5);
        assert_eq!(timestamps, vec![1500, 4500, 7500]);
    }

    #[test]
    fn keyframes_manifest_read_rejects_malformed_bytes() {
        let err = keyframes_manifest_read(b"not json").expect_err("must reject non-json");
        assert!(err.to_string().contains("keyframe manifest"));
        let err =
            keyframes_manifest_read(b"{\"detected\": 2}").expect_err("must name the missing field");
        assert!(err.to_string().contains("model_tag"), "{err}");
    }

    /// The rebuildable-projection guard: `finish_chunk_embed` writes the
    /// `ChunksComplete` marker ONLY after the store add succeeds. A failing
    /// add (a wrong-dimension row, which `TextVectorStore::add` rejects)
    /// must leave no marker behind — the planner then re-plans the item.
    /// This is the test that catches the "marker written before/despite the
    /// add" mutation.
    #[test]
    fn finish_chunk_embed_writes_no_marker_when_the_store_add_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blobs = BlobStore::new(dir.path());
        let store = TextVectorStore::open(&dir.path().join("lance")).expect("open text store");
        let hex = "aa11aa11aa11aa11aa11aa11aa11aa11";
        let marker = blobs.path_for(
            hex,
            &Derivation::ChunksComplete {
                model_tag: MINILM.tag,
            },
        );

        finish_chunk_embed(&blobs, &store, hex, vec![chunk_row(hex, 3)], &[0])
            .expect_err("a wrong-dimension row must fail the store add");
        assert!(
            !marker.is_file(),
            "no completion marker may exist after a failed store add"
        );

        let dim = majestical_index::vector_store::TEXT_DIM;
        finish_chunk_embed(&blobs, &store, hex, vec![chunk_row(hex, dim)], &[0])
            .expect("a valid add succeeds");
        assert!(marker.is_file(), "marker written once the add succeeded");
        let assets = store.distinct_assets(MINILM.tag).expect("scan");
        assert!(assets.contains(hex));
    }

    /// The evenly-spaced sample: short videos keep every keyframe, long
    /// ones cap at [`MAX_DESCRIBED_KEYFRAMES`] spread across the whole
    /// list — never the first twelve.
    #[test]
    fn select_described_timestamps_keeps_short_lists_and_samples_long_ones() {
        assert!(select_described_timestamps(&[]).is_empty());

        let short: Vec<u64> = (0..5).map(|i| i * 1000).collect();
        assert_eq!(select_described_timestamps(&short), short);

        let long: Vec<u64> = (0..100).map(|i| i * 1000).collect();
        let selected = select_described_timestamps(&long);
        assert!(selected.len() <= MAX_DESCRIBED_KEYFRAMES, "{selected:?}");
        assert_eq!(selected[0], 0);
        assert!(
            *selected.last().expect("non-empty") >= 88_000,
            "the sample must span the full list, not cluster at the start: {selected:?}"
        );
    }

    #[test]
    fn video_captions_round_trip_through_the_reader() {
        let described = vec![
            (1500u64, "a red barn".to_string()),
            (4500, "dusk".to_string()),
        ];
        let json = video_captions_json("describe-m", 7, &described);
        let rows = crate::index::blob_read::video_captions_read(&json).expect("round trip");
        assert_eq!(rows, described);
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

    /// Services test proving `run` end-to-end against a text-file-only
    /// fixture: `.txt` files are `MediaKind::Other`, which `plan_work`
    /// (`crates/index/src/work.rs`) never queues for any kind — so this
    /// exercises the full open-catalog/build-plan/split/heal path with zero
    /// model or ffmpeg dependency, and asserts a deterministic empty pass.
    #[test]
    fn run_against_a_text_only_catalog_is_a_clean_empty_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("a.txt"), b"alpha").expect("write");
        crate::scan::scan(&mut app, &src, Some("vol1".to_string())).expect("scan");

        let req = IndexRunReq {
            kinds: crate::index::VALID_KINDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            limit: None,
            threads: Some(1),
            api_key: None,
        };
        let outcome = run(&app, &root, &req).expect("run");

        assert_eq!(outcome.thumbs.written, 0);
        assert!(outcome.thumbs.failed.is_empty());
        assert_eq!(outcome.embed.written, 0);
        assert_eq!(outcome.keyframes.videos_done, 0);
        assert_eq!(outcome.transcribe.written, 0);
        assert_eq!(outcome.ocr.images_written, 0);
        assert_eq!(outcome.pdf.written, 0);
        assert_eq!(outcome.captions.written, 0);
        assert!(outcome.transcript_failures().is_empty());

        // A second pass over the same catalog stays clean — the heal step
        // and blob↔Lance diffs are all no-ops with nothing to heal/load.
        let outcome2 = run(&app, &root, &req).expect("second run");
        assert_eq!(outcome2.thumbs.written, 0);
    }

    /// `--limit`-bounded run against a text-only catalog with `--kinds
    /// thumbs`: still a deterministic empty pass (no thumbable assets), and
    /// the narrowed `--kinds` set must not affect that.
    #[test]
    fn run_with_kinds_thumbs_and_a_limit_on_a_text_only_catalog_stays_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("a.txt"), b"alpha").expect("write");
        crate::scan::scan(&mut app, &src, Some("vol1".to_string())).expect("scan");

        let req = IndexRunReq {
            kinds: ["thumbs".to_string()].into_iter().collect(),
            limit: Some(1),
            threads: Some(1),
            api_key: None,
        };
        let outcome = run(&app, &root, &req).expect("run");
        assert_eq!(outcome.thumbs.written, 0);
        assert!(outcome.thumbs.failed.is_empty());
    }
}
