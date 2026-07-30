//! `maj index run`/`maj index status`: the derived-data queue, worked from
//! the catalog projection against the on-disk blob store. main.rs owns the
//! clap definitions; this module owns behavior, following `search.rs`'s
//! precedent of keeping non-trivial verbs out of `commands.rs`.
use crate::app::FsApp;
use crate::commands::open_catalog;
use crate::volume_identity;
use anyhow::Result;
use majestical_core::media_kind::media_kind;
use majestical_core::projection::Projection;
use majestical_index::blob::{BlobStore, Derivation};
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

/// What this machine can currently produce. Hardcoded until the encoder
/// model fetch and ffmpeg detection land in later tasks: today only
/// thumbnailing is possible, so `run`/`status` report needs-model and
/// needs-ffmpeg honestly instead of claiming capabilities that don't exist
/// yet on this branch.
fn capabilities() -> Capabilities {
    Capabilities {
        model_tag: None,
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
/// (so `--watch` sees newly scanned assets), diffs against the blob store,
/// then narrows to `kinds` and `limit`.
fn build_plan(
    projection: &Projection,
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    limit: Option<usize>,
) -> WorkPlan {
    let sources = gather_sources(projection);
    let blobs = BlobStore::new(catalog_dir);
    let caps = capabilities();
    let mut plan = work::plan_work(&sources, &blobs, &caps);
    plan.items
        .retain(|item| kinds.contains(workkind_name(item.kind)));
    if let Some(limit) = limit {
        plan.items.truncate(limit);
    }
    plan
}

/// One `index run` pass: builds the plan, works every thumbnail item (the
/// only kind with an executor this task — embeddings and keyframes need the
/// encoder model and ffmpeg detection from later tasks), and prints the
/// result.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced.
fn run_once(
    app: &FsApp,
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    limit: Option<usize>,
    threads: Option<usize>,
    json: bool,
) -> Result<()> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let plan = build_plan(&projection, catalog_dir, kinds, limit);
    let thumb_items: Vec<work::WorkItem> = plan
        .items
        .into_iter()
        .filter(|i| i.kind == WorkKind::Thumb)
        .collect();
    let blobs = BlobStore::new(catalog_dir);
    let jobs = threads.unwrap_or_else(default_index_jobs);
    let (written, failed) = run_thumb_items(&blobs, &thumb_items, jobs);
    print_run_result(written, &failed, json);
    Ok(())
}

fn print_run_result(written: u64, failed: &[(PathBuf, String)], json: bool) {
    if json {
        let failed_json: Vec<_> = failed
            .iter()
            .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "thumbnails": { "written": written, "failed": failed_json } })
        );
    } else {
        println!("thumbnails: {written} written, {} failed", failed.len());
    }
    for (path, err) in failed {
        eprintln!("failed {}: {err}", path.display());
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
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    let plan = build_plan(&projection, catalog_dir, &kinds, None);
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
}
