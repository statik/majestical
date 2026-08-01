//! `maj index run`/`maj index status`: the derived-data queue, worked from
//! the catalog projection against the on-disk blob store. main.rs owns the
//! clap definitions; this module owns behavior, following `search.rs`'s
//! precedent of keeping non-trivial verbs out of `commands.rs`.
use crate::app::FsApp;
use crate::commands::open_catalog;
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::AssetId;
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::projection::Projection;
use majestical_index::blob::{BlobStore, Derivation};
use majestical_index::chunk::chunk_segments;
use majestical_index::encoder::{Encoder, EncoderOptions};
use majestical_index::model::{MINILM, WHISPER};
use majestical_index::ocr::{OCR_MODEL_TAG, OcrResult};
use majestical_index::pdf::{PDF_MODEL_TAG, PdfContent};
use majestical_index::text_encoder::TextEncoder;
use majestical_index::transcribe::{Transcriber, Transcript, WHISPER_MODEL_TAG};
use majestical_index::vector_store::{TextChunkRow, TextVectorStore, VectorRow, VectorStore};
use majestical_index::work::{self, AssetSource, Capabilities, KindStatus, WorkKind, WorkPlan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const VALID_KINDS: &[&str] = &[
    "thumbs",
    "embeddings",
    "keyframes",
    "transcripts",
    "ocr",
    "pdf",
    "captions",
];

/// zstd level for JSON derivation blobs (transcripts, OCR, PDF text) —
/// matches the level `BlobStore::write_vector` uses for vector blobs.
const BLOB_ZSTD_LEVEL: i32 = 3;

/// The state-dir file `run_once` overwrites every pass with the pass's
/// per-item failures, and `index status` reads back — see
/// [`failure_report_json`].
const FAILURES_FILE: &str = "index-failures.json";

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
/// file's exact size, whether `ffmpeg`/`ffprobe` are on `PATH`, and whether
/// the whisper/`MiniLM` models are installed.
fn capabilities() -> Capabilities {
    let model_tag = majestical_index::model::model_dir()
        .ok()
        .filter(|dir| majestical_index::model::model_present(dir))
        .map(|_| majestical_index::model::MODEL_TAG.to_string());
    Capabilities {
        model_tag,
        ffmpeg: majestical_index::video::ffmpeg_available(),
        whisper: whisper_model_dir_if_present().is_some(),
        text_model: minilm_model_dir_if_present().is_some(),
        // Task 18 wires the configured describer's tag; until then captions
        // plan as needs_model and no Caption item is ever produced.
        describer_tag: None,
    }
}

/// The whisper cache dir, only if the single ggml weights file is present.
/// A file-presence check (not size/hash) — mirrors the spirit of
/// `model_present` without re-hashing half a gigabyte per invocation.
fn whisper_model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir_for(&WHISPER).ok()?;
    dir.join(majestical_index::transcribe::MODEL_FILE)
        .is_file()
        .then_some(dir)
}

/// The `MiniLM` cache dir, only if its ONNX graph is present.
fn minilm_model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir_for(&MINILM).ok()?;
    dir.join("model.onnx").is_file().then_some(dir)
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

/// The `--kinds` name each [`WorkKind`] answers to. One CLI kind can cover
/// two work kinds: `transcripts` spans Transcribe + `TranscriptEmbed`, and
/// `ocr` spans stills + video keyframes.
fn workkind_name(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Thumb => "thumbs",
        WorkKind::ImageEmbed => "embeddings",
        WorkKind::Keyframes => "keyframes",
        WorkKind::Transcribe | WorkKind::TranscriptEmbed => "transcripts",
        WorkKind::OcrImage | WorkKind::OcrKeyframes => "ocr",
        WorkKind::PdfText => "pdf",
        WorkKind::Caption => "captions",
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

/// One `index run` pass: builds the plan, works every kind's items, heals
/// `text_fts` from blobs, writes the per-run failure marker, and prints the
/// result.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced, a model that
/// passed its presence check fails to load, the Lance vector store can't be
/// opened even after one corruption-recovery retry, or the failure marker
/// can't be written.
fn run_once(app: &FsApp, catalog_dir: &Path, args: &RunOnceArgs<'_>) -> Result<()> {
    let (mut db, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let plan = build_plan(&projection, &blobs, args.kinds);
    let items = split_and_cap_items(plan.items, args.limit);

    let jobs = args.threads.unwrap_or_else(default_index_jobs);
    let embed_paths = EmbedPaths {
        lance_dir: state_dir.join("lance"),
        coreml_cache_dir: state_dir.join("coreml-cache"),
    };
    let outcomes = run_all_kinds(&embed_paths, &blobs, &items, jobs)?;

    heal_text_fts(&mut db, &blobs)?;
    write_failure_report(&state_dir, &failure_report_json(&outcomes))?;
    print_run_result(&outcomes, args.json);
    Ok(())
}

/// Executes every kind's items in priority order. Kinds whose capability is
/// missing (a runner's own model-presence re-check) or whose item list is
/// empty are cheap no-ops.
fn run_all_kinds(
    paths: &EmbedPaths,
    blobs: &BlobStore,
    items: &KindItems,
    jobs: usize,
) -> Result<RunOutcomes> {
    Ok(RunOutcomes {
        thumbs: run_thumb_items(blobs, &items.thumbs, jobs),
        embed: run_embed_items(paths, blobs, &items.embeds)?,
        keyframes: run_keyframe_items(paths, blobs, &items.keyframes)?,
        transcribe: run_transcribe_items(blobs, &items.transcribes)?,
        transcript_embed: run_transcript_embed_items(paths, blobs, &items.transcript_embeds)?,
        ocr: run_ocr_items(blobs, &items.ocr_images, &items.ocr_keyframes),
        pdf: run_pdf_text_items(blobs, &items.pdfs),
    })
}

/// Every kind's items for one pass, split so each executor gets exactly its
/// own queue. No `Caption` bucket: the planner produces no Caption items
/// while `describer_tag` is `None` (Task 18 wires the describer).
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
            // Unreachable while the planner gates captions on a configured
            // describer (`describer_tag: None` until Task 18).
            WorkKind::Caption => {}
        }
    }
    if let Some(limit) = limit {
        split.cap_each(limit);
    }
    split
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

/// One pass's transcribe-kind result: `written` new transcript blobs and
/// per-item `failed` (path, reason).
#[derive(Default)]
struct TranscribeOutcome {
    written: u64,
    failed: Vec<(PathBuf, String)>,
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
    let Some(model_dir) = whisper_model_dir_if_present() else {
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
#[derive(Default)]
struct TranscriptEmbedOutcome {
    chunks_written: u64,
    loaded: u64,
    empty: u64,
    failed: Vec<(PathBuf, String)>,
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
    let transcript = read_transcript_blob(&transcript_path)?;
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
        blobs.write_atomic(&marker, b"{}")?;
        return Ok(ChunkEmbedResult::Empty);
    }
    let written = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    finish_chunk_embed(blobs, store, &item.asset_hex, rows, &chunk_starts)?;
    Ok(ChunkEmbedResult::Written(written))
}

/// Adds the freshly embedded rows to the text store, then — ONLY after the
/// add succeeds — writes the `ChunksComplete` done-marker (its body listing
/// every chunk's `start_ms`, mirroring `OcrComplete`'s timestamp list). A
/// failing add returns before the marker exists, so the item re-plans next
/// pass instead of counting done with vectors missing from the store.
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
        &serde_json::json!({ "chunks": chunk_starts }),
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
        minilm_model_dir_if_present()
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
        eprintln!(
            "note: {skipped} chunk vector blob(s) skipped in the text-store rebuild — \
             transcript blob missing, or its chunking no longer matches"
        );
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
    let transcript = read_transcript_blob(&path).ok()?;
    Some(chunk_segments(&transcript.segments))
}

/// One pass's OCR-kind result across stills and video keyframes.
#[derive(Default)]
struct OcrOutcome {
    images_written: u64,
    videos_done: u64,
    keyframes_written: u64,
    failed: Vec<(PathBuf, String)>,
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
                    outcome.failed.push((
                        item.abs_path.clone(),
                        format!(
                            "{}: {}/{} keyframes failed OCR — first failure: {first} — \
                             video incomplete, will retry",
                            item.abs_path.display(),
                            result.failures,
                            result.total,
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
#[derive(Default)]
struct PdfOutcome {
    written: u64,
    failed: Vec<(PathBuf, String)>,
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

/// Reads and parses a zstd JSON transcript blob.
fn read_transcript_blob(path: &Path) -> Result<Transcript> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading transcript blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing transcript blob {}", path.display()))?;
    Ok(Transcript::from_json(&json)?)
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
        Err(reason) => eprintln!(
            "note: lance text store at {} is unreadable ({reason}) — removing and \
             rebuilding from blobs",
            dir.display()
        ),
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

fn failed_json(failed: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
        .collect()
}

/// Every kind's outcome for one pass, bundled so the printers and the
/// failure report take one value instead of seven.
struct RunOutcomes {
    thumbs: ThumbOutcome,
    embed: EmbedOutcome,
    keyframes: KeyframeOutcome,
    transcribe: TranscribeOutcome,
    transcript_embed: TranscriptEmbedOutcome,
    ocr: OcrOutcome,
    pdf: PdfOutcome,
}

impl RunOutcomes {
    /// The transcripts CLI kind spans two executors — their failures merge
    /// for reporting.
    fn transcript_failures(&self) -> Vec<(PathBuf, String)> {
        let mut merged = self.transcribe.failed.clone();
        merged.extend(self.transcript_embed.failed.iter().cloned());
        merged
    }
}

fn run_result_json(o: &RunOutcomes) -> serde_json::Value {
    serde_json::json!({
        "thumbnails": { "written": o.thumbs.written, "failed": failed_json(&o.thumbs.failed) },
        "embeddings": {
            "written": o.embed.written,
            "loaded_from_blobs": o.embed.loaded,
            "failed": failed_json(&o.embed.failed),
        },
        "keyframes": {
            "videos_done": o.keyframes.videos_done,
            "keyframes_written": o.keyframes.keyframes_written,
            "keyframes_failed": o.keyframes.keyframes_failed,
            "failed": failed_json(&o.keyframes.failed),
        },
        "transcripts": {
            "transcribed": o.transcribe.written,
            "chunks_written": o.transcript_embed.chunks_written,
            "chunks_loaded_from_blobs": o.transcript_embed.loaded,
            "chunks_empty": o.transcript_embed.empty,
            "failed": failed_json(&o.transcript_failures()),
        },
        "ocr": {
            "images_written": o.ocr.images_written,
            "videos_done": o.ocr.videos_done,
            "keyframes_written": o.ocr.keyframes_written,
            "failed": failed_json(&o.ocr.failed),
        },
        "pdf": { "written": o.pdf.written, "failed": failed_json(&o.pdf.failed) },
        // Task 18 wires the describer; the key exists now so consumers see a
        // stable shape.
        "captions": { "written": 0, "failed": [] },
    })
}

fn print_run_result(o: &RunOutcomes, json: bool) {
    if json {
        println!("{}", run_result_json(o));
    } else {
        println!(
            "thumbnails: {} written, {} failed",
            o.thumbs.written,
            o.thumbs.failed.len()
        );
        println!(
            "embeddings: {} written, {} loaded from blobs, {} failed",
            o.embed.written,
            o.embed.loaded,
            o.embed.failed.len()
        );
        println!(
            "keyframes: {} videos, {} frames embedded, {} frame failures, {} videos failed",
            o.keyframes.videos_done,
            o.keyframes.keyframes_written,
            o.keyframes.keyframes_failed,
            o.keyframes.failed.len()
        );
        println!(
            "transcripts: {} transcribed, {} chunks embedded, {} loaded from blobs, {} empty, \
             {} failed",
            o.transcribe.written,
            o.transcript_embed.chunks_written,
            o.transcript_embed.loaded,
            o.transcript_embed.empty,
            o.transcribe.failed.len() + o.transcript_embed.failed.len()
        );
        println!(
            "ocr: {} images, {} videos completed, {} keyframes, {} failed",
            o.ocr.images_written,
            o.ocr.videos_done,
            o.ocr.keyframes_written,
            o.ocr.failed.len()
        );
        println!(
            "pdf: {} written, {} failed",
            o.pdf.written,
            o.pdf.failed.len()
        );
    }
    // No path prefix here: every `IndexError` display already embeds the
    // path it failed on (the structured path is still available in the
    // `--json` branch above, for callers that want it out-of-band).
    for (_, err) in o
        .thumbs
        .failed
        .iter()
        .chain(&o.embed.failed)
        .chain(&o.keyframes.failed)
        .chain(&o.transcribe.failed)
        .chain(&o.transcript_embed.failed)
        .chain(&o.ocr.failed)
        .chain(&o.pdf.failed)
    {
        eprintln!("failed: {err}");
    }
}

/// The per-run failure marker's JSON body: `{kind: [{path, error}, ..]}`,
/// kinds with no failures omitted. An empty pass writes `{}` — the file is
/// always overwritten, so a clean run clears the previous run's failures.
fn failure_report_json(o: &RunOutcomes) -> serde_json::Value {
    let kinds: [(&str, Vec<(PathBuf, String)>); 6] = [
        ("thumbs", o.thumbs.failed.clone()),
        ("embeddings", o.embed.failed.clone()),
        ("keyframes", o.keyframes.failed.clone()),
        ("transcripts", o.transcript_failures()),
        ("ocr", o.ocr.failed.clone()),
        ("pdf", o.pdf.failed.clone()),
    ];
    let mut map = serde_json::Map::new();
    for (kind, failed) in kinds {
        if !failed.is_empty() {
            map.insert(kind.to_string(), failed_json(&failed).into());
        }
    }
    serde_json::Value::Object(map)
}

fn write_failure_report(state_dir: &Path, report: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let path = state_dir.join(FAILURES_FILE);
    std::fs::write(&path, report.to_string())
        .with_context(|| format!("writing failure report {}", path.display()))
}

/// Reads the last run's failure marker; a missing or unparsable file is an
/// empty report (a fresh catalog has no last run to report on).
fn read_failure_report(state_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(state_dir.join(FAILURES_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// The blob↔`text_fts` diff, run at the end of EVERY pass (mirroring
/// `load_missing_vectors_from_blobs`'s role for Lance): any asset with a
/// transcript/OCR/PDF-text blob but no `text_fts` rows for that source gets
/// its rows rebuilt from the blob. `db.text_assets(source)` makes the pass
/// cheap when nothing changed; a blob that decodes to no usable text is
/// re-examined each pass rather than tracked (rare, and decoding one small
/// blob is cheap). Caption healing lands with the describer in Task 18.
///
/// # Errors
/// Returns an error on a blob-walk or sqlite failure; an individual
/// undecodable blob is reported to stderr and skipped instead.
fn heal_text_fts(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    heal_transcript_rows(db, blobs)?;
    heal_ocr_rows(db, blobs)?;
    heal_pdf_rows(db, blobs)?;
    Ok(())
}

fn heal_transcript_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("transcript")?;
    for (hex, _, path) in blobs.iter_named("transcript.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let transcript = match read_transcript_blob(&path) {
            Ok(transcript) => transcript,
            Err(err) => {
                eprintln!("note: skipping unreadable transcript blob: {err}");
                continue;
            }
        };
        let chunks = chunk_segments(&transcript.segments);
        let rows: Vec<(i64, &str)> = chunks
            .iter()
            .filter(|c| !c.text.trim().is_empty())
            .map(|c| (ts_ms_i64(c.start_ms), c.text.as_str()))
            .collect();
        if !rows.is_empty() {
            db.upsert_text_rows(&asset, "transcript", &rows)?;
        }
    }
    Ok(())
}

/// Reads and joins one OCR blob's recognized lines into a single content
/// string (newline-separated, preserving line order).
fn read_ocr_blob_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading ocr blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing ocr blob {}", path.display()))?;
    let result = OcrResult::from_json(&json)?;
    Ok(result
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Heals OCR rows from stills (`image.json.zst`, locator -1) and from
/// completed videos. Video enumeration rides the `ocr-complete.json`
/// markers: kf blobs have variable names, so instead of a new `BlobStore`
/// walker, each marker's sibling `kf-<ts>.json.zst` files are read directly
/// — partially-OCR'd videos (no marker yet) are picked up once complete.
fn heal_ocr_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("ocr")?;
    for (hex, _, path) in blobs.iter_named("image.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        match read_ocr_blob_text(&path) {
            Ok(text) if !text.trim().is_empty() => {
                db.upsert_text_rows(&asset, "ocr", &[(-1, text.as_str())])?;
            }
            Ok(_) => {}
            Err(err) => eprintln!("note: skipping unreadable ocr blob: {err}"),
        }
    }
    for (hex, _, marker_path) in blobs.iter_named("ocr-complete.json")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let rows = keyframe_ocr_rows(&marker_path);
        let row_refs: Vec<(i64, &str)> =
            rows.iter().map(|(ts, text)| (*ts, text.as_str())).collect();
        if !row_refs.is_empty() {
            db.upsert_text_rows(&asset, "ocr", &row_refs)?;
        }
    }
    Ok(())
}

/// Collects `(ts_ms, text)` rows from the `kf-<ts>.json.zst` OCR blobs
/// sitting beside a video's `ocr-complete.json` marker, sorted by
/// timestamp; unreadable entries are reported and skipped.
fn keyframe_ocr_rows(marker_path: &Path) -> Vec<(i64, String)> {
    let Some(dir) = marker_path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(ts_ms) = name
            .strip_prefix("kf-")
            .and_then(|rest| rest.strip_suffix(".json.zst"))
            .and_then(|ms| ms.parse::<i64>().ok())
        else {
            continue;
        };
        match read_ocr_blob_text(&entry.path()) {
            Ok(text) if !text.trim().is_empty() => rows.push((ts_ms, text)),
            Ok(_) => {}
            Err(err) => eprintln!("note: skipping unreadable keyframe ocr blob: {err}"),
        }
    }
    rows.sort_unstable_by_key(|(ts, _)| *ts);
    rows
}

fn heal_pdf_rows(db: &mut SqliteCatalog, blobs: &BlobStore) -> Result<()> {
    let covered = db.text_assets("pdf")?;
    for (hex, _, path) in blobs.iter_named("text.json.zst")? {
        let asset = AssetId(format!("xxh3:{hex}"));
        if covered.contains(&asset) {
            continue;
        }
        let content = match read_pdf_blob(&path) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("note: skipping unreadable pdf text blob: {err}");
                continue;
            }
        };
        let rows: Vec<(i64, &str)> = content
            .pages
            .iter()
            .enumerate()
            .filter(|(_, page)| !page.trim().is_empty())
            .map(|(index, page)| {
                // Locator is the 1-based page number.
                (i64::try_from(index + 1).unwrap_or(i64::MAX), page.as_str())
            })
            .collect();
        if !rows.is_empty() {
            db.upsert_text_rows(&asset, "pdf", &rows)?;
        }
    }
    Ok(())
}

fn read_pdf_blob(path: &Path) -> Result<PdfContent> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading pdf text blob {}", path.display()))?;
    let json = zstd::decode_all(&bytes[..])
        .with_context(|| format!("decompressing pdf text blob {}", path.display()))?;
    Ok(PdfContent::from_json(&json)?)
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

/// Remedy lines for capability gaps, printed under the per-kind status
/// lines: each names the exact command that closes the gap.
fn print_status_remedies(plan: &WorkPlan, caps: &Capabilities) {
    if plan.transcripts.needs_model > 0 {
        let mut fetches = Vec::new();
        if !caps.whisper {
            fetches.push(format!("--only {}", WHISPER.tag));
        }
        if !caps.text_model {
            fetches.push(format!("--only {}", MINILM.tag));
        }
        if !fetches.is_empty() {
            println!(
                "transcripts needs model: run `maj model fetch {}`",
                fetches.join(" ")
            );
        }
    }
    if plan.captions.needs_model > 0 {
        println!("captions needs model: run `maj describer set` to configure a backend");
    }
}

/// Per-kind failure lines from the last run's marker, e.g.
/// `pdf failed last run: 1 (broken.pdf: not a valid pdf)`.
fn print_last_run_failures(failures: &serde_json::Map<String, serde_json::Value>) {
    for (kind, list) in failures {
        let Some(entries) = list.as_array() else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }
        let first = entries[0]["error"].as_str().unwrap_or("<unknown reason>");
        println!("{kind} failed last run: {} ({first})", entries.len());
    }
}

/// Reports the queue's current state per derivation kind without doing any
/// work — a diff against the blob store, same as `run`, just not executed —
/// plus the last run's per-item failures from the failure marker.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced or the state dir
/// can't be resolved.
pub(crate) fn cmd_index_status(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    // `build_plan` computes capabilities internally; recomputed here only to
    // phrase the remedy lines (which model is actually missing).
    let caps = capabilities();
    let plan = build_plan(&projection, &blobs, &kinds);
    let failures = read_failure_report(&state_dir);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbs": kind_status_json(&plan.thumbs),
                "embeddings": kind_status_json(&plan.embeddings),
                "keyframes": kind_status_json(&plan.keyframes),
                "transcripts": kind_status_json(&plan.transcripts),
                "ocr": kind_status_json(&plan.ocr),
                "pdf": kind_status_json(&plan.pdf),
                "captions": kind_status_json(&plan.captions),
                "failed_last_run": serde_json::Value::Object(failures),
            })
        );
    } else {
        print_kind_status("thumbs", &plan.thumbs);
        print_kind_status("embeddings", &plan.embeddings);
        print_kind_status("keyframes", &plan.keyframes);
        print_kind_status("transcripts", &plan.transcripts);
        print_kind_status("ocr", &plan.ocr);
        print_kind_status("pdf", &plan.pdf);
        print_kind_status("captions", &plan.captions);
        print_status_remedies(&plan, &caps);
        print_last_run_failures(&failures);
    }
    Ok(())
}

/// Downloads model weights into the shared cache (`MAJ_MODEL_DIR`, or the
/// platform data dir — see [`majestical_index::model::model_dir_for`]),
/// verifying every file's sha256 before it's installed. Fetches every
/// registered model unless `only` narrows it to specific tags.
///
/// # Errors
/// Returns an error if `only` names an unknown tag, the cache directory
/// can't be resolved, or any file fails to download or verify.
pub(crate) fn cmd_model_fetch(verify: bool, only: &[String]) -> Result<()> {
    use majestical_index::model;

    let known: Vec<&str> = model::ALL_MODELS.iter().map(|m| m.tag).collect();
    for tag in only {
        anyhow::ensure!(
            known.contains(&tag.as_str()),
            "unknown model tag {tag}; known: {}",
            known.join(", ")
        );
    }
    for spec in model::ALL_MODELS {
        if !only.is_empty() && !only.iter().any(|t| t == spec.tag) {
            continue;
        }
        let dir = model::model_dir_for(spec)?;
        println!("model cache: {}", dir.display());
        model::fetch_spec(spec, verify, &mut |line| println!("{line}"))?;
        println!("model '{}' ready", spec.tag);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kinds_defaults_to_every_kind() {
        let kinds = parse_kinds(None).expect("default kinds");
        assert_eq!(kinds.len(), VALID_KINDS.len());
        for kind in VALID_KINDS {
            assert!(kinds.contains(*kind), "default must include {kind}");
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

    #[test]
    fn failure_report_json_includes_only_kinds_with_failures() {
        let outcomes = RunOutcomes {
            thumbs: ThumbOutcome {
                written: 1,
                failed: Vec::new(),
            },
            embed: EmbedOutcome {
                written: 0,
                loaded: 0,
                failed: Vec::new(),
            },
            keyframes: KeyframeOutcome::default(),
            transcribe: TranscribeOutcome::default(),
            transcript_embed: TranscriptEmbedOutcome::default(),
            ocr: OcrOutcome::default(),
            pdf: PdfOutcome {
                written: 0,
                failed: vec![(PathBuf::from("/media/broken.pdf"), "not a valid pdf".into())],
            },
        };
        let report = failure_report_json(&outcomes);
        let map = report.as_object().expect("object");
        assert_eq!(map.len(), 1, "only the pdf kind failed: {report}");
        let entries = map["pdf"].as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["path"], "/media/broken.pdf");
        assert_eq!(entries[0]["error"], "not a valid pdf");
    }

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
