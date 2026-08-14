//! Derivation-queue planning and execution for `maj index run`/`maj index
//! status`. Moved from `crates/cli/src/index_cmd.rs`: `VALID_KINDS`/
//! `capabilities`/`gather_sources`/`build_plan`/`workkind_name`/the
//! failure-report reader live directly in this module (shared so `run` and
//! `status` can never plan differently for the same catalog state); the
//! derivation engine itself — every per-kind runner, the worker pool, and
//! the `text_fts` heal — lives in the `run`/`heal`/`blob_read` submodules,
//! split out to keep any one file well under the house line-length
//! comfort zone. [`blobs`] is the read side heads serve derived blobs
//! through. `run`'s public surface ([`run::run`], [`run::IndexRunReq`],
//! [`run::IndexRunOutcome`], and the per-kind outcome structs) is
//! re-exported here so callers only ever need `services::index::`.
mod blob_read;
pub mod blobs;
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
use anyhow::{Context, Result};
use majestical_core::media_kind::media_kind;
use majestical_core::projection::Projection;
use majestical_index::blob::BlobStore;
use majestical_index::model::SIGLIP;
use majestical_index::work::{self, AssetSource, Capabilities, KindStatus, WorkKind, WorkPlan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The `--kinds` values `index run`/`index status` understand. Adding
/// "keyframe-images" here (wiring up `WorkKind::KeyframeImages`) requires
/// replacing the no-op arm in `run.rs`'s `split_and_cap_items` with a real
/// executor first — today that arm is unreachable because `build_plan`'s
/// `kinds` filter drops every `KeyframeImages` item before it gets there;
/// the moment this list names it, that arm silently drops real work instead,
/// with no compile error and no test to catch it.
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
/// degrades to unconfigured with a notice — a broken describer config must
/// never kill the rest of indexing.
fn describer_model_tag(catalog_root: &Path, notices: &crate::notices::Notices) -> Option<String> {
    match load_config(catalog_root, notices) {
        Ok(config) => config.map(|c| c.model_tag()),
        Err(err) => {
            notices.push(format!(
                "note: ignoring broken describer config ({err:#}) — captions degrade to unconfigured"
            ));
            None
        }
    }
}

/// What this machine can currently produce: the encoder model if it's been
/// fetched into the cache, whether `ffmpeg`/`ffprobe` are on `PATH`, and
/// whether the whisper/`MiniLM` models are installed.
#[must_use]
pub fn capabilities(catalog_root: &Path, notices: &crate::notices::Notices) -> Capabilities {
    let model_tag = model_dir_if_present().map(|_| majestical_index::model::MODEL_TAG.to_string());
    Capabilities {
        model_tag,
        ffmpeg: majestical_index::video::ffmpeg_available(),
        whisper: whisper_model_dir_if_present().is_some(),
        text_model: minilm_model_dir_if_present().is_some(),
        describer_tag: describer_model_tag(catalog_root, notices),
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
        WorkKind::KeyframeImages => "keyframe-images",
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
/// noted and treated as empty — the next run overwrites it.
#[must_use]
pub fn read_failure_report(
    state_dir: &Path,
    notices: &crate::notices::Notices,
) -> serde_json::Map<String, serde_json::Value> {
    let path = state_dir.join(FAILURES_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return serde_json::Map::new();
    };
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(&bytes) {
        map
    } else {
        notices.push(format!(
            "note: ignoring unparsable failure report at {} — treating as empty",
            path.display()
        ));
        serde_json::Map::new()
    }
}

/// One kind's per-item failures as `{path, error}` rows — shared by
/// [`failure_report_json`]'s per-kind map and (independently) the CLI's own
/// `--json` rendering of a run's failures.
fn failed_json(failed: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
        .collect()
}

/// This pass's failures as `{kind: [{path, error}, ..]}`, kinds with no
/// failures omitted. Never written to disk as-is: [`merge_failure_report`]
/// first folds it over the previous report so a `--kinds`-filtered run only
/// speaks for the kinds it actually worked.
fn failure_report_json(o: &IndexRunOutcome) -> serde_json::Value {
    let kinds: [(&str, Vec<(PathBuf, String)>); 7] = [
        ("thumbs", o.thumbs.failed.clone()),
        ("embeddings", o.embed.failed.clone()),
        ("keyframes", o.keyframes.failed.clone()),
        ("transcripts", o.transcript_failures()),
        ("ocr", o.ocr.failed.clone()),
        ("pdf", o.pdf.failed.clone()),
        ("captions", o.captions.failed.clone()),
    ];
    let mut map = serde_json::Map::new();
    for (kind, failed) in kinds {
        if !failed.is_empty() {
            map.insert(kind.to_string(), failed_json(&failed).into());
        }
    }
    serde_json::Value::Object(map)
}

/// Folds this pass's failures over the previous report: keys for every kind
/// in this pass's `--kinds` set are replaced (cleared when the kind now has
/// no failures), while kinds the pass never worked keep their old record —
/// `index run --kinds thumbs` must not erase a pdf failure whose item was
/// never retried.
fn merge_failure_report(
    previous: serde_json::Map<String, serde_json::Value>,
    current: &serde_json::Value,
    kinds: &BTreeSet<String>,
) -> serde_json::Value {
    let mut merged = previous;
    for kind in kinds {
        merged.remove(kind);
    }
    if let Some(current) = current.as_object() {
        for (kind, failures) in current {
            merged.insert(kind.clone(), failures.clone());
        }
    }
    serde_json::Value::Object(merged)
}

fn write_failure_report(state_dir: &Path, report: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let path = state_dir.join(FAILURES_FILE);
    std::fs::write(&path, report.to_string())
        .with_context(|| format!("writing failure report {}", path.display()))
}

/// Folds one `index run` pass's failures into the on-disk failure marker
/// `index status` reads back — state, not rendering, so it lives here
/// rather than in the CLI's `run_once` (moved from
/// `crates/cli/src/index_cmd.rs::run_once`'s bookkeeping half). Any head
/// that calls [`run`] and then this keeps `index status` truthful.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the marker can't
/// be written.
pub fn update_failure_report(
    catalog_dir: &Path,
    outcome: &IndexRunOutcome,
    kinds: &BTreeSet<String>,
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    update_failure_report_impl(catalog_dir, outcome, kinds, notices).map_err(ServiceError::from)
}

fn update_failure_report_impl(
    catalog_dir: &Path,
    outcome: &IndexRunOutcome,
    kinds: &BTreeSet<String>,
    notices: &crate::notices::Notices,
) -> Result<()> {
    let state_dir = crate::state_dir::state_dir_for(catalog_dir, notices)?;
    let previous = read_failure_report(&state_dir, notices);
    let current = failure_report_json(outcome);
    let merged = merge_failure_report(previous, &current, kinds);
    write_failure_report(&state_dir, &merged)
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
    pub captions_remedy: Option<String>,
    pub failed_last_run: serde_json::Value,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Names the platform gap for one Apple-only derivation kind excluded from
/// `plan`, never a silent zero: `index status` must say why `n` assets that
/// are otherwise eligible aren't queued, not just fail to mention them.
fn platform_unavailable_notice(capability: &str, framework: &str, count: u64) -> String {
    format!(
        "{capability} is unavailable in this build (requires {framework}, macOS only) — \
         {count} eligible asset(s) are not queued"
    )
}

/// Pushes one notice per non-zero platform-exclusion count on `plan`
/// (`ocr_unavailable`/`pdf_unavailable` — see `majestical_index::work`) onto
/// `notices`, the same sink `status_impl` already drains into its outcome.
/// On macOS both counts are always zero (Vision and `PDFKit` ship with the
/// OS), so this pushes nothing there — see
/// `macos_status_carries_no_platform_unavailable_notice`.
fn push_platform_unavailable_notices(plan: &WorkPlan, notices: &crate::notices::Notices) {
    if plan.ocr_unavailable > 0 {
        notices.push(platform_unavailable_notice(
            "OCR",
            "the Vision framework",
            plan.ocr_unavailable,
        ));
    }
    if plan.pdf_unavailable > 0 {
        notices.push(platform_unavailable_notice(
            "PDF text extraction",
            "PDFKit",
            plan.pdf_unavailable,
        ));
    }
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
    let state_dir = crate::state_dir::state_dir_for(catalog_dir, app.notices())?;
    let blobs = BlobStore::new(catalog_dir);
    let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
    let caps = capabilities(catalog_dir, app.notices());
    let plan = build_plan(&projection, &blobs, &kinds, &caps);
    let failures = read_failure_report(&state_dir, app.notices());
    let transcripts_remedy = (plan.transcripts.needs_model > 0)
        .then(|| transcript_model_remedy(caps.whisper, caps.text_model))
        .flatten();
    let captions_remedy = (plan.captions.needs_model > 0).then(|| DESCRIBER_REMEDY.to_string());
    push_platform_unavailable_notices(&plan, app.notices());
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
        notices: app.notices().drain(),
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

    /// The notice text itself, independent of any real platform: pins the
    /// exact wording clause (c) requires (capability, framework, macOS
    /// remedy, count) regardless of which OS runs this test.
    #[test]
    fn platform_unavailable_notice_names_capability_framework_and_count() {
        let notice = platform_unavailable_notice("OCR", "the Vision framework", 3);
        assert!(notice.contains("OCR"), "{notice}");
        assert!(notice.contains("the Vision framework"), "{notice}");
        assert!(notice.contains("macOS"), "{notice}");
        assert!(notice.contains("3 eligible asset(s)"), "{notice}");
    }

    /// The sink-wiring itself, independent of any real platform: only
    /// non-zero exclusion counts push a notice, and each pushes exactly one
    /// — pins "never a silent zero" without depending on `ocr::AVAILABLE`/
    /// `pdf::AVAILABLE` actually being false on the machine running this
    /// test.
    #[test]
    fn push_platform_unavailable_notices_pushes_one_notice_per_nonzero_count() {
        let plan = WorkPlan {
            ocr_unavailable: 2,
            ..WorkPlan::default()
        };
        let notices = crate::notices::Notices::new();
        push_platform_unavailable_notices(&plan, &notices);
        let drained = notices.drain();
        assert_eq!(drained.len(), 1, "{drained:?}");
        assert!(drained[0].contains("OCR"), "{drained:?}");

        let plan_both = WorkPlan {
            ocr_unavailable: 1,
            pdf_unavailable: 1,
            ..WorkPlan::default()
        };
        let notices = crate::notices::Notices::new();
        push_platform_unavailable_notices(&plan_both, &notices);
        assert_eq!(notices.drain().len(), 2);

        let plan_none = WorkPlan::default();
        let notices = crate::notices::Notices::new();
        push_platform_unavailable_notices(&plan_none, &notices);
        assert!(notices.drain().is_empty());
    }

    /// Phase 7C Task 9 clause (c), macOS shape: an online image and an
    /// online PDF are both eligible-but-currently-unindexed sources, yet on
    /// macOS (Vision + `PDFKit` both ship with the OS) `status` must carry
    /// NO platform-unavailable notice — the notice is a genuine platform
    /// gap, never decoration. Mirrored off-macOS in
    /// `off_macos_status_names_the_ocr_and_pdf_platform_gap`.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_status_carries_no_platform_unavailable_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("photo.jpg"), b"not really a jpeg").expect("write image");
        std::fs::write(src.join("doc.pdf"), b"not really a pdf").expect("write pdf");
        // Auto-detected volume identity (`None`) so `gather_sources` resolves
        // these as online — a fixed `--volume` string wouldn't match
        // `volume_identity::mounted_volumes()`'s real device ids.
        crate::scan::scan(&mut app, &src, None).expect("scan");

        let outcome = status(&app, &root).expect("status");
        assert!(
            outcome
                .notices
                .iter()
                .all(|n| !n.contains("unavailable in this build")),
            "{:?}",
            outcome.notices
        );
    }

    /// Phase 7C Task 9 clause (c), off-macOS mirror of
    /// `macos_status_carries_no_platform_unavailable_notice`: the same
    /// online image and PDF, but on a build where Vision/`PDFKit` don't
    /// exist. `status` must carry one notice per capability naming it, the
    /// missing framework, and the one eligible-but-unqueued asset. Can't run
    /// on this (macOS) dev machine — CI-proven off-macOS in Task 10 — but
    /// exercises only ordinary services-crate calls, no platform-gated
    /// symbols, so it compiles cleanly if the `cfg` were ever flipped
    /// locally.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_status_names_the_ocr_and_pdf_platform_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("photo.jpg"), b"not really a jpeg").expect("write image");
        std::fs::write(src.join("doc.pdf"), b"not really a pdf").expect("write pdf");
        crate::scan::scan(&mut app, &src, None).expect("scan");

        let outcome = status(&app, &root).expect("status");

        let ocr_notice = outcome
            .notices
            .iter()
            .find(|n| n.contains("OCR"))
            .expect("an OCR platform-unavailable notice");
        assert!(ocr_notice.contains("the Vision framework"), "{ocr_notice}");
        assert!(ocr_notice.contains("macOS"), "{ocr_notice}");
        assert!(ocr_notice.contains("1 eligible asset(s)"), "{ocr_notice}");

        let pdf_notice = outcome
            .notices
            .iter()
            .find(|n| n.contains("PDF text extraction"))
            .expect("a PDF platform-unavailable notice");
        assert!(pdf_notice.contains("PDFKit"), "{pdf_notice}");
        assert!(pdf_notice.contains("macOS"), "{pdf_notice}");
        assert!(pdf_notice.contains("1 eligible asset(s)"), "{pdf_notice}");
    }

    #[test]
    fn workkind_name_covers_every_kind() {
        assert_eq!(workkind_name(WorkKind::Thumb), "thumbs");
        assert_eq!(workkind_name(WorkKind::ImageEmbed), "embeddings");
        assert_eq!(workkind_name(WorkKind::Keyframes), "keyframes");
        assert_eq!(workkind_name(WorkKind::KeyframeImages), "keyframe-images");
        assert_eq!(workkind_name(WorkKind::Transcribe), "transcripts");
        assert_eq!(workkind_name(WorkKind::TranscriptEmbed), "transcripts");
        assert_eq!(workkind_name(WorkKind::OcrImage), "ocr");
        assert_eq!(workkind_name(WorkKind::OcrKeyframes), "ocr");
        assert_eq!(workkind_name(WorkKind::PdfText), "pdf");
        assert_eq!(workkind_name(WorkKind::Caption), "captions");
    }

    #[test]
    fn failure_report_json_includes_only_kinds_with_failures() {
        let outcomes = IndexRunOutcome {
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
            captions: CaptionOutcome::default(),
            notices: Vec::new(),
        };
        let report = failure_report_json(&outcomes);
        let map = report.as_object().expect("object");
        assert_eq!(map.len(), 1, "only the pdf kind failed: {report}");
        let entries = map["pdf"].as_array().expect("array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["path"], "/media/broken.pdf");
        assert_eq!(entries[0]["error"], "not a valid pdf");
    }

    /// A `--kinds`-filtered pass only speaks for its own kinds: it replaces
    /// (or clears) their keys and preserves every other kind's record.
    #[test]
    fn merge_failure_report_preserves_kinds_the_pass_never_worked() {
        let mut previous = serde_json::Map::new();
        previous.insert(
            "pdf".to_string(),
            serde_json::json!([{ "path": "/m/broken.pdf", "error": "not a valid pdf" }]),
        );
        previous.insert(
            "thumbs".to_string(),
            serde_json::json!([{ "path": "/m/old.png", "error": "stale" }]),
        );

        // A thumbs-only pass with no failures: clears thumbs, keeps pdf.
        let kinds: BTreeSet<String> = ["thumbs".to_string()].into();
        let merged = merge_failure_report(previous.clone(), &serde_json::json!({}), &kinds);
        let map = merged.as_object().expect("object");
        assert!(map.contains_key("pdf"), "pdf record must survive: {merged}");
        assert!(
            !map.contains_key("thumbs"),
            "a clean thumbs pass clears its own record: {merged}"
        );

        // A pdf pass with a fresh failure: replaces pdf, keeps thumbs.
        let kinds: BTreeSet<String> = ["pdf".to_string()].into();
        let current = serde_json::json!({
            "pdf": [{ "path": "/m/broken.pdf", "error": "still broken" }],
        });
        let merged = merge_failure_report(previous, &current, &kinds);
        let map = merged.as_object().expect("object");
        assert_eq!(map["pdf"][0]["error"], "still broken");
        assert_eq!(map["thumbs"][0]["error"], "stale");
    }

    #[test]
    fn update_failure_report_writes_the_state_dir_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let outcome = IndexRunOutcome {
            thumbs: ThumbOutcome {
                written: 0,
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
            captions: CaptionOutcome::default(),
            notices: Vec::new(),
        };
        let kinds: BTreeSet<String> = ["pdf".to_string()].into();
        let notices = crate::notices::Notices::new();
        update_failure_report(&root, &outcome, &kinds, &notices).expect("update");
        let state_dir = crate::state_dir::state_dir_for(&root, &notices).expect("state dir");
        let report = read_failure_report(&state_dir, &notices);
        assert_eq!(report["pdf"][0]["error"], "not a valid pdf");
    }

    /// `status` is the verb that shows the last run's failures, so its own
    /// read of a corrupt marker is exactly the diagnostic a caller needs —
    /// pins that the outcome actually carries it home rather than the sink
    /// being drained into a value nobody reads.
    #[test]
    fn status_carries_the_unparsable_failure_report_note_on_its_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = FsApp::init(&root, "m1", "m1").expect("init");
        let state_dir = crate::state_dir::state_dir_for(&root, app.notices()).expect("state dir");
        std::fs::write(state_dir.join(FAILURES_FILE), b"{ not json").expect("plant");
        let outcome = status(&app, &root).expect("status");
        assert!(
            outcome
                .notices
                .iter()
                .any(|n| n.contains("ignoring unparsable failure report")),
            "{:?}",
            outcome.notices
        );
    }

    /// A marker file the next run will overwrite anyway must never be a hard
    /// failure — it degrades to an empty report plus one notice.
    #[test]
    fn unparsable_failure_report_is_a_notice() {
        let state = tempfile::tempdir().expect("tempdir");
        std::fs::write(state.path().join(FAILURES_FILE), b"{ not json").expect("plant");
        let notices = crate::notices::Notices::new();
        let report = read_failure_report(state.path(), &notices);
        assert!(report.is_empty());
        let drained = notices.drain();
        assert_eq!(drained.len(), 1);
        assert!(
            drained[0].contains("ignoring unparsable failure report"),
            "{drained:?}"
        );
    }
}
