//! `maj index run`/`maj index status`: the derived-data queue, worked from
//! the catalog projection against the on-disk blob store. main.rs owns the
//! clap definitions; this module owns behavior, following `search.rs`'s
//! precedent of keeping non-trivial verbs out of `commands.rs`.
use crate::app::FsApp;
use crate::commands::open_catalog;
use crate::volume_identity;
use anyhow::{Context, Result};
use majestical_core::media_kind::media_kind;
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
/// file's exact size. ffmpeg detection lands with the video task; until then
/// keyframes honestly report needs-ffmpeg.
fn capabilities() -> Capabilities {
    let model_tag = majestical_index::model::model_dir()
        .ok()
        .filter(|dir| majestical_index::model::model_present(dir))
        .map(|_| majestical_index::model::MODEL_TAG.to_string());
    Capabilities {
        model_tag,
        ffmpeg: false,
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

fn decode_and_write_thumb(blobs: &BlobStore, item: &work::WorkItem) -> Result<()> {
    let rgb = majestical_index::thumbs::decode_image(&item.abs_path)?;
    let webp = majestical_index::thumbs::thumbnail_webp(&rgb)?;
    let path = blobs.path_for(&item.asset_hex, &Derivation::Thumb);
    blobs.write_atomic(&path, &webp)?;
    Ok(())
}

/// Works every thumbnail item with `jobs` parallel workers sharing one
/// atomic cursor into `items` — a plain work-stealing pool without pulling in
/// a thread-pool dependency for one queue.
fn run_thumb_items(
    blobs: &BlobStore,
    items: &[work::WorkItem],
    jobs: usize,
) -> (u64, Vec<(PathBuf, String)>) {
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
    (written.load(Ordering::Relaxed), failed)
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
/// that happens after `run_once` narrows further to kinds that actually
/// have an executor (thumbnails and embeddings so far; keyframes still
/// need PR 8's ffmpeg detection), so a mixed-kind plan never lets a
/// non-executable kind consume `--limit`'s budget ahead of the executable
/// ones.
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

/// One `index run` pass: builds the plan, works every thumbnail and
/// embedding item (keyframes still need PR 8's ffmpeg detection), and
/// prints the result.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced, or if the Lance
/// vector store can't be opened even after one corruption-recovery retry.
fn run_once(
    app: &FsApp,
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    limit: Option<usize>,
    threads: Option<usize>,
    json: bool,
) -> Result<()> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let plan = build_plan(&projection, &blobs, kinds);
    let (thumb_items, embed_items) = split_and_cap_items(plan.items, limit);

    let jobs = threads.unwrap_or_else(default_index_jobs);
    let (written, failed) = run_thumb_items(&blobs, &thumb_items, jobs);

    let embed_paths = EmbedPaths {
        lance_dir: state_dir.join("lance"),
        coreml_cache_dir: state_dir.join("coreml-cache"),
    };
    let embed_outcome = run_embed_items(&embed_paths, &blobs, &embed_items)?;

    print_run_result(written, &failed, &embed_outcome, json);
    Ok(())
}

/// Splits `items` into thumbnail and image-embed items (keyframe items, if
/// any slip through `kinds`, have no executor yet and are dropped), then
/// caps each kind independently at `limit` — now that both kinds have
/// executors, `--limit` bounds each one's per-pass budget the same way.
fn split_and_cap_items(
    items: Vec<work::WorkItem>,
    limit: Option<usize>,
) -> (Vec<work::WorkItem>, Vec<work::WorkItem>) {
    let mut thumbs = Vec::new();
    let mut embeds = Vec::new();
    for item in items {
        match item.kind {
            WorkKind::Thumb => thumbs.push(item),
            WorkKind::ImageEmbed => embeds.push(item),
            WorkKind::Keyframes => {}
        }
    }
    if let Some(limit) = limit {
        thumbs.truncate(limit);
        embeds.truncate(limit);
    }
    (thumbs, embeds)
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
/// [`load_missing_vectors_from_blobs`].
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

fn failed_json(failed: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
        .collect()
}

fn print_run_result(written: u64, failed: &[(PathBuf, String)], embed: &EmbedOutcome, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbnails": { "written": written, "failed": failed_json(failed) },
                "embeddings": {
                    "written": embed.written,
                    "loaded_from_blobs": embed.loaded,
                    "failed": failed_json(&embed.failed),
                },
            })
        );
    } else {
        println!("thumbnails: {written} written, {} failed", failed.len());
        println!(
            "embeddings: {} written, {} loaded from blobs, {} failed",
            embed.written,
            embed.loaded,
            embed.failed.len()
        );
    }
    // No path prefix here: every `IndexError` display already embeds the
    // path it failed on (the structured path is still available in the
    // `--json` branch above, for callers that want it out-of-band).
    for (_, err) in failed.iter().chain(&embed.failed) {
        eprintln!("failed: {err}");
    }
}

/// Works the derivation queue once, or repeatedly (`--watch`, a 5s poll
/// loop) so newly scanned assets get picked up without a manual re-run.
///
/// # Errors
/// Returns an error if `--kinds` names an unknown kind, or the catalog can't
/// be opened/synced.
pub(crate) fn cmd_index_run(app: &FsApp, catalog_dir: &Path, args: &IndexRunArgs) -> Result<()> {
    let kinds = parse_kinds(args.kinds.as_deref())?;
    loop {
        run_once(
            app,
            catalog_dir,
            &kinds,
            args.limit,
            args.threads,
            args.json,
        )?;
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
