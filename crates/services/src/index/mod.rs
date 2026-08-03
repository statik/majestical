//! Derivation-queue planning and execution for `maj index run`/`maj index
//! status`. Moved from `crates/cli/src/index_cmd.rs`: `VALID_KINDS`/
//! `capabilities`/`gather_sources`/`build_plan`/`workkind_name`/the
//! failure-report reader live directly in this module (shared so `run` and
//! `status` can never plan differently for the same catalog state); the
//! derivation engine itself — every per-kind runner, the worker pool, and
//! the `text_fts` heal — lives in the `run`/`heal`/`blob_read` submodules,
//! split out to keep any one file well under the house line-length
//! comfort zone. `run`'s public surface ([`run::run`], [`run::IndexRunReq`],
//! [`run::IndexRunOutcome`], and the per-kind outcome structs) is
//! re-exported here so callers only ever need `services::index::`.
mod blob_read;
mod heal;
mod run;

pub use run::{
    CaptionOutcome, EmbedOutcome, IndexRunOutcome, IndexRunReq, KeyframeOutcome, OcrOutcome,
    PdfOutcome, ThumbOutcome, TranscribeOutcome, TranscriptEmbedOutcome, run,
};

use crate::app::FsApp;
use crate::capability::{
    DESCRIBER_REMEDY, minilm_model_dir_if_present, transcript_model_remedy,
    whisper_model_dir_if_present,
};
use crate::catalog::open_catalog;
use crate::describer_config::load_config;
use crate::error::ServiceError;
use crate::volume_identity;
use anyhow::Result;
use majestical_core::media_kind::media_kind;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;
use majestical_index::model::SIGLIP;
use majestical_index::work::{self, AssetSource, Capabilities, KindStatus, WorkKind, WorkPlan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The `--kinds` values `index run`/`index status` understand.
pub const VALID_KINDS: &[&str] = &[
    "thumbs",
    "embeddings",
    "keyframes",
    "transcripts",
    "ocr",
    "pdf",
    "captions",
];

/// The state-dir file a pass overwrites with its per-item failures, and
/// `index status` reads back.
pub const FAILURES_FILE: &str = "index-failures.json";

/// Resolves the encoder model dir only if it's actually present at every
/// file's exact size.
#[must_use]
pub fn model_dir_if_present() -> Option<PathBuf> {
    let dir = majestical_index::model::model_dir_for(&SIGLIP).ok()?;
    majestical_index::model::model_present_for(&SIGLIP, &dir).then_some(dir)
}

/// The configured describer's blob derivation tag, or `None` when no
/// describer is configured. An unreadable/unparsable `describer.toml`
/// degrades to unconfigured with a stderr note — a broken describer config
/// must never kill the rest of indexing.
fn describer_model_tag(catalog_root: &Path) -> Option<String> {
    match load_config(catalog_root) {
        Ok(config) => config.map(|c| c.model_tag()),
        Err(err) => {
            // See the `#[expect]` note on `warn_skipped_corrupt_lines` in
            // app.rs: services inherits print_stderr = "deny" crate-wide;
            // this is a verbatim stderr diagnostic moved from cli, not yet
            // a rendered outcome.
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!(
                    "note: ignoring broken describer config ({err:#}) — captions degrade to unconfigured"
                );
            }
            None
        }
    }
}

/// What this machine can currently produce: the encoder model if it's been
/// fetched into the cache, whether `ffmpeg`/`ffprobe` are on `PATH`, and
/// whether the whisper/`MiniLM` models are installed.
#[must_use]
pub fn capabilities(catalog_root: &Path) -> Capabilities {
    let model_tag = model_dir_if_present().map(|_| majestical_index::model::MODEL_TAG.to_string());
    Capabilities {
        model_tag,
        ffmpeg: majestical_index::video::ffmpeg_available(),
        whisper: whisper_model_dir_if_present().is_some(),
        text_model: minilm_model_dir_if_present().is_some(),
        describer_tag: describer_model_tag(catalog_root),
    }
}

/// Builds one [`AssetSource`] per catalog asset that has at least one
/// recorded instance: kind from the first instance's path, and an absolute
/// path to the first instance whose volume is currently mounted and whose
/// bytes are actually present on disk.
#[must_use]
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

/// The `--kinds` name each [`WorkKind`] answers to. One CLI kind can cover
/// two work kinds: `transcripts` spans Transcribe + `TranscriptEmbed`, and
/// `ocr` spans stills + video keyframes.
#[must_use]
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
/// (so `--watch` sees newly scanned assets), diffs against `blobs` under
/// the caller-computed `caps`, then narrows `items` to `kinds`.
#[must_use]
pub fn build_plan(
    projection: &Projection,
    blobs: &BlobStore,
    kinds: &BTreeSet<String>,
    caps: &Capabilities,
) -> WorkPlan {
    let sources = gather_sources(projection);
    let mut plan = work::plan_work(&sources, blobs, caps);
    plan.items
        .retain(|item| kinds.contains(workkind_name(item.kind)));
    plan
}

/// Reads the last run's failure marker. A missing file is an empty report
/// (a fresh catalog has no last run to report on); an unparsable one is
/// noted on stderr and treated as empty — the next run overwrites it.
#[must_use]
pub fn read_failure_report(state_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    let path = state_dir.join(FAILURES_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return serde_json::Map::new();
    };
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(&bytes) {
        map
    } else {
        #[expect(
            clippy::print_stderr,
            reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
        )]
        {
            eprintln!(
                "note: ignoring unparsable failure report at {} — treating as empty",
                path.display()
            );
        }
        serde_json::Map::new()
    }
}

/// One derivation kind's queue counts, mirroring [`KindStatus`] as a
/// serializable row.
#[derive(serde::Serialize)]
pub struct KindStatusRow {
    pub done: u64,
    pub pending: u64,
    pub offline: u64,
    pub unsupported: u64,
    pub needs_ffmpeg: u64,
    pub needs_model: u64,
}

impl From<&KindStatus> for KindStatusRow {
    fn from(status: &KindStatus) -> Self {
        Self {
            done: status.done,
            pending: status.pending,
            offline: status.offline,
            unsupported: status.unsupported,
            needs_ffmpeg: status.needs_ffmpeg,
            needs_model: status.needs_model,
        }
    }
}

/// Everything `maj index status` renders: every kind's queue counts, plus
/// the remedy lines gated on whether that kind actually has anything
/// waiting on a missing model (`None` when there's nothing to remedy), and
/// the last run's per-item failures exactly as read off disk.
#[derive(serde::Serialize)]
pub struct IndexStatusOutcome {
    pub thumbs: KindStatusRow,
    pub embeddings: KindStatusRow,
    pub keyframes: KindStatusRow,
    pub transcripts: KindStatusRow,
    pub ocr: KindStatusRow,
    pub pdf: KindStatusRow,
    pub captions: KindStatusRow,
    pub transcripts_remedy: Option<String>,
    pub captions_remedy: Option<&'static str>,
    pub failed_last_run: serde_json::Value,
}

/// `maj index status`: the derivation queue's current state per kind
/// without doing any work — a diff against the blob store, same as `run`,
/// just not executed — plus the last run's per-item failures.
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced or the state dir
/// can't be resolved.
pub fn status(app: &FsApp, catalog_dir: &Path) -> Result<IndexStatusOutcome, ServiceError> {
    status_impl(app, catalog_dir).map_err(ServiceError::from)
}

fn status_impl(app: &FsApp, catalog_dir: &Path) -> Result<IndexStatusOutcome> {
    let (_, projection) = open_catalog(app, catalog_dir)?;
    let state_dir = crate::state_dir::state_dir_for(catalog_dir)?;
    let blobs = BlobStore::new(catalog_dir);
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    let caps = capabilities(catalog_dir);
    let plan = build_plan(&projection, &blobs, &kinds, &caps);
    let failures = read_failure_report(&state_dir);
    let transcripts_remedy = (plan.transcripts.needs_model > 0)
        .then(|| transcript_model_remedy(caps.whisper, caps.text_model))
        .flatten();
    let captions_remedy = (plan.captions.needs_model > 0).then_some(DESCRIBER_REMEDY);
    Ok(IndexStatusOutcome {
        thumbs: (&plan.thumbs).into(),
        embeddings: (&plan.embeddings).into(),
        keyframes: (&plan.keyframes).into(),
        transcripts: (&plan.transcripts).into(),
        ocr: (&plan.ocr).into(),
        pdf: (&plan.pdf).into(),
        captions: (&plan.captions).into(),
        transcripts_remedy,
        captions_remedy,
        failed_last_run: serde_json::Value::Object(failures),
    })
}

/// `maj model fetch`: downloads every registered model (or, with `only`,
/// exactly the named tags) into its cache dir, verifying every file's
/// sha256 before it's installed. `progress` is called for every line this
/// used to print unconditionally to stdout: the cache-dir announcement,
/// each file's own download/verify status (from
/// `majestical_index::model::fetch_spec`), and the per-model "ready" line —
/// mirroring `crate::ingest::run_ingest`'s `notice` callback, since this is
/// the same "streams to stdout while it runs" shape rather than a single
/// end-of-run outcome. Moved from
/// `crates/cli/src/index_cmd.rs::cmd_model_fetch`.
///
/// # Errors
/// Returns an error if `only` names an unknown tag, the cache directory
/// can't be resolved, or any file fails to download or verify.
pub fn model_fetch(
    verify: bool,
    only: &[String],
    progress: &mut dyn FnMut(&str),
) -> Result<(), ServiceError> {
    model_fetch_impl(verify, only, progress).map_err(ServiceError::from)
}

fn model_fetch_impl(verify: bool, only: &[String], progress: &mut dyn FnMut(&str)) -> Result<()> {
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
        progress(&format!("model cache: {}", dir.display()));
        model::fetch_spec(spec, verify, progress)?;
        progress(&format!("model '{}' ready", spec.tag));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_fetch_rejects_an_unknown_only_tag() {
        let mut lines = Vec::new();
        let err = model_fetch(false, &["not-a-real-model".to_string()], &mut |line| {
            lines.push(line.to_string());
        })
        .expect_err("must fail");
        assert!(err.to_string().contains("unknown model tag"));
        assert!(
            lines.is_empty(),
            "an unknown tag must fail before any progress prints"
        );
    }

    #[test]
    fn status_of_an_empty_catalog_has_every_kind_at_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = FsApp::init(&root, "m1", "m1").expect("init");
        let outcome = status(&app, &root).expect("status");
        assert_eq!(outcome.thumbs.pending, 0);
        assert_eq!(outcome.thumbs.done, 0);
        assert!(
            outcome
                .failed_last_run
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
    }

    #[test]
    fn workkind_name_covers_every_kind() {
        assert_eq!(workkind_name(WorkKind::Thumb), "thumbs");
        assert_eq!(workkind_name(WorkKind::ImageEmbed), "embeddings");
        assert_eq!(workkind_name(WorkKind::Keyframes), "keyframes");
        assert_eq!(workkind_name(WorkKind::Transcribe), "transcripts");
        assert_eq!(workkind_name(WorkKind::TranscriptEmbed), "transcripts");
        assert_eq!(workkind_name(WorkKind::OcrImage), "ocr");
        assert_eq!(workkind_name(WorkKind::OcrKeyframes), "ocr");
        assert_eq!(workkind_name(WorkKind::PdfText), "pdf");
        assert_eq!(workkind_name(WorkKind::Caption), "captions");
    }
}
