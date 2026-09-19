//! Derivation-queue planning and execution for `maj index run`/`maj index
//! status`. Moved from `crates/cli/src/index_cmd.rs`: `VALID_KINDS`/
//! `capabilities`/`gather_sources`/`build_plan`/`workkind_name`/the
//! failure ledger live directly in this module (shared so `run` and
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
    CaptionOutcome, EmbedOutcome, IndexRunOutcome, IndexRunReq, ItemFailure, KeyframeImageOutcome,
    KeyframeOutcome, OcrOutcome, PdfOutcome, ThumbOutcome, TranscribeOutcome,
    TranscriptEmbedOutcome, run,
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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The `--kinds` values `index run`/`index status` understand. Every name
/// here must have a real executor behind it: [`build_plan`]'s `kinds` filter
/// keeps only items whose [`workkind_name`] this list contains, and
/// `run.rs`'s `split_and_cap_items` then routes each one to its kind's
/// runner. A name listed here whose split arm is a no-op would silently
/// drop real work — no compile error, no failure row, just items that
/// re-plan forever. "keyframe-images" is wired end to end:
/// `run::run_keyframe_image_items` executes it and `status`'s
/// `keyframe_images` row reports it.
pub const VALID_KINDS: &[&str] = &[
    "thumbs",
    "embeddings",
    "keyframes",
    "keyframe-images",
    "transcripts",
    "ocr",
    "pdf",
    "captions",
];

/// The state-dir file holding the failure ledger: every permanent per-item
/// failure this catalog has accumulated, which the planner holds back and
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
/// the caller-computed `caps`, narrows `items` to `kinds`, then holds back
/// every item `ledger` already remembers as a permanent failure. The per-kind
/// counters on the returned plan describe the WHOLE catalog regardless of
/// `kinds` — only `items` is narrowed to it, because the `retain` above never
/// adjusts the counters `plan_work` already set, and [`apply_ledger`]
/// inherits that same split between counters and items.
#[must_use]
pub fn build_plan(
    projection: &Projection,
    blobs: &BlobStore,
    kinds: &BTreeSet<String>,
    caps: &Capabilities,
    ledger: &Ledger,
) -> WorkPlan {
    let sources = gather_sources(projection);
    let mut plan = work::plan_work(&sources, blobs, caps);
    plan.items
        .retain(|item| kinds.contains(workkind_name(item.kind)));
    apply_ledger(plan, ledger)
}

/// One remembered permanent failure. `asset` is the key the planner matches
/// on; `path`/`error` are for display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LedgerRow {
    pub asset: String,
    pub path: String,
    pub error: String,
}

/// Every kind's remembered permanent failures as `{kind: [row, ..]}`, where
/// `kind` is the `--kinds` name ([`workkind_name`]) — so the two work kinds
/// behind `transcripts` (and behind `ocr`) share one key, exactly as the
/// planner and every wire shape name them.
pub type Ledger = BTreeMap<String, Vec<LedgerRow>>;

/// Reads the failure ledger from [`FAILURES_FILE`] in the state dir. A
/// missing file is an empty ledger (a fresh catalog has never failed at
/// anything); an unparsable one is noted and treated as empty — the next
/// [`record_failures`] rewrites it.
#[must_use]
pub fn read_ledger(state_dir: &Path, notices: &crate::notices::Notices) -> Ledger {
    let path = state_dir.join(FAILURES_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ledger::new();
    };
    let Ok(ledger) = serde_json::from_slice(&bytes) else {
        let path = path.display();
        notices.push(format!(
            "note: ignoring unparsable failure ledger at {path} — treating as empty \
             (an older version wrote a different shape; it is rebuilt by the next run)"
        ));
        return Ledger::new();
    };
    ledger
}

/// Distinguishes concurrent writers inside one process (the desktop
/// scheduler thread and a `retry_failed_items` command thread), so their
/// temp files never collide; the pid distinguishes processes.
static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `ledger` to [`FAILURES_FILE`] via temp-file-then-rename in the same
/// directory: this file is cumulative state that several writers — the
/// desktop scheduler, a desktop command, a CLI run — can read-modify-write
/// concurrently, so neither a crash nor two overlapping writers may leave a
/// torn file where a reader can see it. A crash between the write and the
/// rename leaves an inert `.tmp-*` sibling behind; nothing reads those.
fn write_ledger(state_dir: &Path, ledger: &Ledger) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let path = state_dir.join(FAILURES_FILE);
    let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = state_dir.join(format!("{FAILURES_FILE}.tmp-{}-{seq}", std::process::id()));
    let bytes = serde_json::to_vec(ledger).context("serializing the failure ledger")?;
    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("writing failure ledger {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming failure ledger into place at {}", path.display()))
}

/// One kind's permanent failures as ledger rows, at most one per asset (a
/// kind spanning two work kinds can fail the same asset twice in a pass);
/// the last row for an asset wins, so the freshest error text survives.
fn rows_for(failed: &[ItemFailure]) -> Vec<LedgerRow> {
    let mut by_asset: BTreeMap<String, LedgerRow> = BTreeMap::new();
    for failure in failed.iter().filter(|f| !f.transient) {
        by_asset.insert(
            failure.asset.clone(),
            LedgerRow {
                asset: failure.asset.clone(),
                path: failure.path.display().to_string(),
                error: failure.error.clone(),
            },
        );
    }
    by_asset.into_values().collect()
}

/// This pass's PERMANENT failures as a ledger fragment, keyed by `--kinds`
/// name: transient rows are dropped (they were never really attempted — see
/// [`ItemFailure`]), and kinds with nothing permanent are omitted entirely.
/// Never written as-is: [`merge_ledger`] folds it over what is already
/// remembered, because a `--kinds`-filtered pass only ever adds.
fn permanent_failures(outcome: &IndexRunOutcome) -> Ledger {
    let sources: [(&str, Vec<ItemFailure>); 8] = [
        ("thumbs", outcome.thumbs.failed.clone()),
        ("embeddings", outcome.embed.failed.clone()),
        ("keyframes", outcome.keyframes.failed.clone()),
        ("keyframe-images", outcome.keyframe_images.failed.clone()),
        ("transcripts", outcome.transcript_failures()),
        ("ocr", outcome.ocr.failed.clone()),
        ("pdf", outcome.pdf.failed.clone()),
        ("captions", outcome.captions.failed.clone()),
    ];
    let mut ledger = Ledger::new();
    for (kind, failed) in sources {
        let rows = rows_for(&failed);
        if !rows.is_empty() {
            ledger.insert(kind.to_string(), rows);
        }
    }
    ledger
}

/// `previous` ∪ `current`, per kind, deduplicated by `asset`: a re-failed
/// item's row is replaced (fresh error text), never duplicated, and a kind
/// this pass never worked keeps its rows untouched. No run ever clears a
/// kind — only [`clear_failures`] does.
fn merge_ledger(previous: Ledger, current: Ledger) -> Ledger {
    let mut merged = previous;
    for (kind, rows) in current {
        let entry = merged.entry(kind).or_default();
        let mut by_asset: BTreeMap<String, LedgerRow> = std::mem::take(entry)
            .into_iter()
            .map(|row| (row.asset.clone(), row))
            .collect();
        for row in rows {
            by_asset.insert(row.asset.clone(), row);
        }
        *entry = by_asset.into_values().collect();
    }
    merged
}

/// After a run: folds this pass's permanent failures into the catalog's
/// failure ledger, so the planner holds those items back until an explicit
/// retry. State, not rendering — any head that calls [`run`] and then this
/// keeps `index status` truthful.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the ledger can't
/// be written.
pub fn record_failures(
    catalog_dir: &Path,
    outcome: &IndexRunOutcome,
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    record_failures_impl(catalog_dir, outcome, notices).map_err(ServiceError::from)
}

fn record_failures_impl(
    catalog_dir: &Path,
    outcome: &IndexRunOutcome,
    notices: &crate::notices::Notices,
) -> Result<()> {
    let state_dir = crate::state_dir::state_dir_for(catalog_dir, notices)?;
    let previous = read_ledger(&state_dir, notices);
    let merged = merge_ledger(previous, permanent_failures(outcome));
    write_ledger(&state_dir, &merged)
}

/// Forgets every remembered failure for `kinds`, so the next plan queues
/// those items again; returns how many rows were removed. The one way a
/// ledger row ever goes away.
///
/// # Errors
/// Returns an error if the state dir can't be resolved or the ledger can't
/// be written.
pub fn clear_failures(
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    notices: &crate::notices::Notices,
) -> Result<u64, ServiceError> {
    clear_failures_impl(catalog_dir, kinds, notices).map_err(ServiceError::from)
}

fn clear_failures_impl(
    catalog_dir: &Path,
    kinds: &BTreeSet<String>,
    notices: &crate::notices::Notices,
) -> Result<u64> {
    let state_dir = crate::state_dir::state_dir_for(catalog_dir, notices)?;
    let mut ledger = read_ledger(&state_dir, notices);
    let mut cleared = 0u64;
    for kind in kinds {
        if let Some(rows) = ledger.remove(kind) {
            cleared += rows.len() as u64;
        }
    }
    if cleared > 0 {
        write_ledger(&state_dir, &ledger)?;
    }
    Ok(cleared)
}

/// The catalog's failure ledger exactly as it stands — what `index status`
/// reports and what an MCP dry run counts a retry would clear.
///
/// # Errors
/// Returns an error if the state dir can't be resolved.
pub fn known_failures(
    catalog_dir: &Path,
    notices: &crate::notices::Notices,
) -> Result<Ledger, ServiceError> {
    let state_dir = crate::state_dir::state_dir_for(catalog_dir, notices)?;
    Ok(read_ledger(&state_dir, notices))
}

/// The [`KindStatus`] counters one [`WorkKind`] reports into — the same
/// mapping [`workkind_name`] makes, on the plan's own fields.
fn kind_status_mut(plan: &mut WorkPlan, kind: WorkKind) -> &mut KindStatus {
    match kind {
        WorkKind::Thumb => &mut plan.thumbs,
        WorkKind::ImageEmbed => &mut plan.embeddings,
        WorkKind::Keyframes => &mut plan.keyframes,
        WorkKind::KeyframeImages => &mut plan.keyframe_images,
        WorkKind::Transcribe | WorkKind::TranscriptEmbed => &mut plan.transcripts,
        WorkKind::OcrImage | WorkKind::OcrKeyframes => &mut plan.ocr,
        WorkKind::PdfText => &mut plan.pdf,
        WorkKind::Caption => &mut plan.captions,
    }
}

/// Holds back every planned item the ledger already remembers as a
/// permanent failure of that (`--kinds` name, asset) pair: the item leaves
/// `items`, and its kind's `pending` count moves to `failed` — so `index
/// status` says "held back", never a silent zero.
///
/// The ledger keys by the `--kinds` name, not by [`WorkKind`], so a row under
/// a two-stage kind holds back BOTH stages for that asset: a `transcripts`
/// row holds back both Transcribe and `TranscriptEmbed` — including a
/// `TranscriptEmbed` item that a teammate-synced transcript would otherwise
/// make ready to run — and an `ocr` row holds back both `OcrImage` and
/// `OcrKeyframes`. Both stay held until [`clear_failures`] drops that key;
/// finer (kind, not just `--kinds` name)-level keying is a recorded
/// deferral.
#[must_use]
pub fn apply_ledger(plan: WorkPlan, ledger: &Ledger) -> WorkPlan {
    if ledger.is_empty() {
        return plan;
    }
    let mut plan = plan;
    let mut held: Vec<WorkKind> = Vec::new();
    plan.items.retain(|item| {
        let known = ledger
            .get(workkind_name(item.kind))
            .is_some_and(|rows| rows.iter().any(|row| row.asset == item.asset));
        if known {
            held.push(item.kind);
        }
        !known
    });
    for kind in held {
        let status = kind_status_mut(&mut plan, kind);
        status.pending = status.pending.saturating_sub(1);
        status.failed += 1;
    }
    plan
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
    /// Held back by the failure ledger until an explicit retry.
    pub failed: u64,
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
            failed: status.failed,
        }
    }
}

/// Everything `maj index status` renders: every kind's queue counts, plus
/// the remedy lines gated on whether that kind actually has anything
/// waiting on a missing model (`None` when there's nothing to remedy), and
/// the failure ledger exactly as read off disk.
#[derive(serde::Serialize)]
pub struct IndexStatusOutcome {
    pub thumbs: KindStatusRow,
    pub embeddings: KindStatusRow,
    pub keyframes: KindStatusRow,
    /// Serialized under the `--kinds` name — see [`IndexRunOutcome`]'s own
    /// `keyframe-images` field for why.
    #[serde(rename = "keyframe-images")]
    pub keyframe_images: KindStatusRow,
    pub transcripts: KindStatusRow,
    pub ocr: KindStatusRow,
    pub pdf: KindStatusRow,
    pub captions: KindStatusRow,
    pub transcripts_remedy: Option<String>,
    pub captions_remedy: Option<String>,
    /// Every permanent failure this catalog remembers, keyed by `--kinds`
    /// name — the items the planner holds back until `--retry-failed`.
    pub failed: Ledger,
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
/// just not executed — plus every failure the ledger remembers.
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
    let ledger = read_ledger(&state_dir, app.notices());
    let plan = build_plan(&projection, &blobs, &kinds, &caps, &ledger);
    let transcripts_remedy = (plan.transcripts.needs_model > 0)
        .then(|| transcript_model_remedy(caps.whisper, caps.text_model))
        .flatten();
    let captions_remedy = (plan.captions.needs_model > 0).then(|| DESCRIBER_REMEDY.to_string());
    push_platform_unavailable_notices(&plan, app.notices());
    Ok(IndexStatusOutcome {
        thumbs: (&plan.thumbs).into(),
        embeddings: (&plan.embeddings).into(),
        keyframes: (&plan.keyframes).into(),
        keyframe_images: (&plan.keyframe_images).into(),
        transcripts: (&plan.transcripts).into(),
        ocr: (&plan.ocr).into(),
        pdf: (&plan.pdf).into(),
        captions: (&plan.captions).into(),
        transcripts_remedy,
        captions_remedy,
        failed: ledger,
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
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
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

    /// The `keyframe-images` row `index status` renders, driven by two
    /// assets per bucket so a counter that increments once for two assets
    /// (or once per pass) can't pass: two videos with a completion marker
    /// count `done`, two with only a manifest count `pending` and keep their
    /// work items — which they only do because `VALID_KINDS` names the kind
    /// (`build_plan` drops every item whose kind the caller didn't request).
    /// Nothing here decodes: planning only diffs blob paths.
    #[test]
    fn keyframe_image_status_counts_two_assets_per_bucket_and_keeps_their_items() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        for name in ["a.mov", "b.mov", "c.mov", "d.mov"] {
            std::fs::write(src.join(name), format!("not a real mov: {name}")).expect("write");
        }
        // Auto-detected volume identity (`None`) so these resolve as online.
        crate::scan::scan(&mut app, &src, None).expect("scan");

        let (_, projection) = open_catalog(&app, &root).expect("open catalog");
        let blobs = BlobStore::new(&root);
        let model_tag = majestical_index::model::MODEL_TAG;
        let mut hexes: Vec<String> = gather_sources(&projection)
            .iter()
            .filter_map(|source| Some(source.asset.strip_prefix("xxh3:")?.to_string()))
            .collect();
        hexes.sort();
        assert_eq!(hexes.len(), 4, "four scanned videos");
        for hex in &hexes {
            let manifest = blobs.path_for(
                hex,
                &majestical_index::blob::Derivation::KeyframeManifest { model_tag },
            );
            blobs
                .write_atomic(
                    &manifest,
                    br#"{"model_tag":"m","detected":0,"timestamps":[]}"#,
                )
                .expect("seed manifest");
        }
        for hex in &hexes[..2] {
            let marker = blobs.path_for(
                hex,
                &majestical_index::blob::Derivation::KeyframeImagesComplete { model_tag },
            );
            blobs
                .write_atomic(&marker, br#"{"timestamps":[]}"#)
                .expect("seed");
        }

        let caps = Capabilities {
            model_tag: Some(model_tag.to_string()),
            ffmpeg: true,
            whisper: false,
            text_model: false,
            describer_tag: None,
        };
        let kinds: BTreeSet<String> = VALID_KINDS.iter().map(|s| (*s).to_string()).collect();
        let plan = build_plan(&projection, &blobs, &kinds, &caps, &Ledger::new());

        let row = KindStatusRow::from(&plan.keyframe_images);
        assert_eq!(row.done, 2, "two videos carry the completion marker");
        assert_eq!(row.pending, 2, "two have a manifest but no marker");
        assert_eq!(row.offline, 0);
        assert_eq!(row.needs_ffmpeg, 0);
        assert_eq!(row.needs_model, 0);
        assert_eq!(
            plan.items
                .iter()
                .filter(|item| item.kind == WorkKind::KeyframeImages)
                .count(),
            2,
            "the two pending videos keep their work items"
        );

        // The same plan under a `--kinds thumbs` request keeps the counts
        // (status always reports every kind) but drops the items.
        let thumbs_only: BTreeSet<String> = ["thumbs".to_string()].into();
        let narrowed = build_plan(&projection, &blobs, &thumbs_only, &caps, &Ledger::new());
        assert_eq!(narrowed.keyframe_images.pending, 2);
        assert!(
            !narrowed
                .items
                .iter()
                .any(|item| item.kind == WorkKind::KeyframeImages)
        );
    }

    /// `status` end to end, not `apply_ledger` in isolation: plant one real
    /// image so a Thumb item plans, ledger that asset as a known `thumbs`
    /// failure the way `record_failures` would, then confirm `status` holds
    /// it back — pins that `status_impl` actually wires `read_ledger` into
    /// `build_plan` rather than planning against an empty ledger. A Thumb
    /// item for an image never depends on ffmpeg/the encoder model/whisper,
    /// so this holds on every OS this crate builds for.
    #[test]
    fn status_holds_back_a_ledgered_item_and_reports_it_as_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        image::RgbImage::new(4, 4)
            .save(src.join("photo.png"))
            .expect("write a real PNG for the planner to see");
        // Auto-detected volume identity (`None`) so `gather_sources` resolves
        // this as online.
        crate::scan::scan(&mut app, &src, None).expect("scan");

        let (_, projection) = open_catalog(&app, &root).expect("open catalog");
        let asset = gather_sources(&projection)
            .into_iter()
            .next()
            .expect("the one scanned image source")
            .asset;

        let notices = crate::notices::Notices::new();
        let state_dir = crate::state_dir::state_dir_for(&root, &notices).expect("state dir");
        write_ledger(&state_dir, &ledger_of(&[("thumbs", &[asset.as_str()])])).expect("seed");

        let outcome = status(&app, &root).expect("status");
        assert_eq!(
            outcome.thumbs.pending, 0,
            "pending: {}",
            outcome.thumbs.pending
        );
        assert_eq!(
            outcome.thumbs.failed, 1,
            "failed: {}",
            outcome.thumbs.failed
        );
        assert_eq!(
            outcome.failed.get("thumbs").map(Vec::len),
            Some(1),
            "{:?}",
            outcome.failed
        );
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

    fn work_item(asset: &str, kind: WorkKind) -> work::WorkItem {
        work::WorkItem {
            asset: asset.to_string(),
            asset_hex: asset.trim_start_matches("xxh3:").to_string(),
            abs_path: PathBuf::from("/media/x"),
            kind,
        }
    }

    fn row(asset: &str, error: &str) -> LedgerRow {
        LedgerRow {
            asset: asset.to_string(),
            path: "/media/x".to_string(),
            error: error.to_string(),
        }
    }

    fn ledger_of(entries: &[(&str, &[&str])]) -> Ledger {
        entries
            .iter()
            .map(|(kind, assets)| {
                let rows = assets.iter().map(|a| row(a, "boom")).collect();
                ((*kind).to_string(), rows)
            })
            .collect()
    }

    fn failure(asset: &str, error: &str, transient: bool) -> ItemFailure {
        ItemFailure {
            asset: asset.to_string(),
            path: PathBuf::from("/media/x"),
            error: error.to_string(),
            transient,
        }
    }

    /// A kind the pass never worked keeps its rows verbatim, and a kind it
    /// re-failed keeps exactly one row per asset — carrying the fresh error
    /// text, never a duplicate.
    #[test]
    fn merge_ledger_replaces_a_refailed_row_and_keeps_other_kinds() {
        let previous: Ledger = BTreeMap::from([
            (
                "pdf".to_string(),
                vec![row("xxh3:aa", "not a valid pdf"), row("xxh3:bb", "stale")],
            ),
            ("thumbs".to_string(), vec![row("xxh3:cc", "old")]),
        ]);
        let current: Ledger = BTreeMap::from([(
            "pdf".to_string(),
            vec![row("xxh3:aa", "still broken"), row("xxh3:dd", "new")],
        )]);

        let merged = merge_ledger(previous, current);
        let pdf = &merged["pdf"];
        assert_eq!(
            pdf.len(),
            3,
            "one row per asset, never a duplicate: {pdf:?}"
        );
        let refailed = pdf
            .iter()
            .find(|r| r.asset == "xxh3:aa")
            .expect("the re-failed row");
        assert_eq!(refailed.error, "still broken");
        assert!(pdf.iter().any(|r| r.asset == "xxh3:bb"));
        assert!(pdf.iter().any(|r| r.asset == "xxh3:dd"));
        assert_eq!(merged["thumbs"], vec![row("xxh3:cc", "old")]);
    }

    #[test]
    fn permanent_failures_drops_transient_rows() {
        let outcome = IndexRunOutcome {
            thumbs: ThumbOutcome {
                written: 0,
                failed: vec![
                    failure("xxh3:aa", "volume vanished", true),
                    failure("xxh3:bb", "decode failed", false),
                ],
            },
            ..IndexRunOutcome::default()
        };
        let ledger = permanent_failures(&outcome);
        assert_eq!(ledger.len(), 1, "{ledger:?}");
        let thumbs = &ledger["thumbs"];
        assert_eq!(thumbs.len(), 1, "{thumbs:?}");
        assert_eq!(thumbs[0].asset, "xxh3:bb");
        assert_eq!(thumbs[0].error, "decode failed");
    }

    /// One `--kinds` name can span two work kinds; both stages' permanent
    /// failures land under that one key, never under a Rust field name.
    #[test]
    fn permanent_failures_keys_both_transcript_stages_and_both_ocr_kinds_by_their_kinds_name() {
        let outcome = IndexRunOutcome {
            transcribe: TranscribeOutcome {
                written: 0,
                failed: vec![failure("xxh3:aa", "whisper refused", false)],
            },
            transcript_embed: TranscriptEmbedOutcome {
                failed: vec![failure("xxh3:bb", "chunking failed", false)],
                ..TranscriptEmbedOutcome::default()
            },
            ocr: OcrOutcome {
                failed: vec![
                    failure("xxh3:cc", "vision refused the still", false),
                    failure("xxh3:dd", "vision refused a keyframe", false),
                ],
                ..OcrOutcome::default()
            },
            ..IndexRunOutcome::default()
        };
        let ledger = permanent_failures(&outcome);
        let assets: Vec<&str> = ledger["transcripts"]
            .iter()
            .map(|r| r.asset.as_str())
            .collect();
        assert_eq!(assets, vec!["xxh3:aa", "xxh3:bb"], "{ledger:?}");
        assert_eq!(ledger["ocr"].len(), 2, "{ledger:?}");
        assert!(
            !ledger.contains_key("transcript_embed"),
            "no Rust field names on the wire: {ledger:?}"
        );
    }

    #[test]
    fn apply_ledger_moves_a_matching_item_from_pending_to_failed() {
        let plan = || WorkPlan {
            items: vec![
                work_item("xxh3:aa", WorkKind::Thumb),
                work_item("xxh3:bb", WorkKind::Thumb),
            ],
            thumbs: KindStatus {
                pending: 2,
                ..KindStatus::default()
            },
            ..WorkPlan::default()
        };

        let held = apply_ledger(plan(), &ledger_of(&[("thumbs", &["xxh3:aa"])]));
        assert_eq!(held.items.len(), 1, "the known failure is held back");
        assert_eq!(held.items[0].asset, "xxh3:bb");
        assert_eq!(held.thumbs.pending, 1);
        assert_eq!(held.thumbs.failed, 1);

        // The same asset remembered under a DIFFERENT kind holds nothing
        // back: the ledger is keyed by (kind, asset), not asset alone.
        let untouched = apply_ledger(plan(), &ledger_of(&[("pdf", &["xxh3:aa"])]));
        assert_eq!(untouched.items.len(), 2);
        assert_eq!(untouched.thumbs.pending, 2);
        assert_eq!(untouched.thumbs.failed, 0);
    }

    /// Both work kinds behind one `--kinds` name resolve to the same ledger
    /// key and the same [`KindStatus`] counters.
    #[test]
    fn apply_ledger_maps_both_transcript_work_kinds_to_the_transcripts_key() {
        let plan = WorkPlan {
            items: vec![
                work_item("xxh3:aa", WorkKind::Transcribe),
                work_item("xxh3:bb", WorkKind::TranscriptEmbed),
                work_item("xxh3:cc", WorkKind::OcrImage),
                work_item("xxh3:dd", WorkKind::OcrKeyframes),
            ],
            transcripts: KindStatus {
                pending: 2,
                ..KindStatus::default()
            },
            ocr: KindStatus {
                pending: 2,
                ..KindStatus::default()
            },
            ..WorkPlan::default()
        };
        let ledger = ledger_of(&[
            ("transcripts", &["xxh3:aa", "xxh3:bb"]),
            ("ocr", &["xxh3:cc", "xxh3:dd"]),
        ]);

        let held = apply_ledger(plan, &ledger);
        assert!(held.items.is_empty(), "{:?}", held.items);
        assert_eq!(held.transcripts.pending, 0);
        assert_eq!(held.transcripts.failed, 2);
        assert_eq!(held.ocr.pending, 0);
        assert_eq!(held.ocr.failed, 2);
    }

    #[test]
    fn clear_failures_removes_only_the_named_kinds_and_reports_the_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let notices = crate::notices::Notices::new();
        let state_dir = crate::state_dir::state_dir_for(&root, &notices).expect("state dir");
        write_ledger(
            &state_dir,
            &ledger_of(&[("thumbs", &["xxh3:aa", "xxh3:bb"]), ("pdf", &["xxh3:cc"])]),
        )
        .expect("seed");

        let kinds: BTreeSet<String> = ["thumbs".to_string()].into();
        let cleared = clear_failures(&root, &kinds, &notices).expect("clear");
        assert_eq!(cleared, 2, "both thumbs rows were dropped");

        let remaining = known_failures(&root, &notices).expect("known");
        assert!(!remaining.contains_key("thumbs"), "{remaining:?}");
        assert_eq!(remaining["pdf"].len(), 1, "{remaining:?}");
    }

    /// A retry on a catalog that remembers nothing is a no-op on disk: the
    /// ledger file is written only when a row was actually dropped, so a
    /// clean catalog stays without one.
    #[test]
    fn clear_failures_with_nothing_to_clear_writes_no_ledger_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let notices = crate::notices::Notices::new();
        let state_dir = crate::state_dir::state_dir_for(&root, &notices).expect("state dir");

        let kinds: BTreeSet<String> = ["thumbs".to_string()].into();
        let cleared = clear_failures(&root, &kinds, &notices).expect("clear");
        assert_eq!(cleared, 0);
        assert!(
            !state_dir.join(FAILURES_FILE).exists(),
            "an empty clear must not create the ledger file"
        );
    }

    /// `write_ledger` writes through a `<FAILURES_FILE>.tmp-<pid>-<seq>`
    /// sibling and renames it into place — a reader must never observe a
    /// torn write, and the success path leaves no temp file behind (a crash
    /// mid-write can leave one; it is inert, nothing reads it).
    #[test]
    fn write_ledger_leaves_no_tmp_file_behind() {
        let state_dir = tempfile::tempdir().expect("tempdir");
        write_ledger(state_dir.path(), &ledger_of(&[("thumbs", &["xxh3:aa"])])).expect("write");

        let leftover_tmp: Vec<_> = std::fs::read_dir(state_dir.path())
            .expect("read state dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftover_tmp.is_empty(), "{leftover_tmp:?}");

        let ledger = read_ledger(state_dir.path(), &crate::notices::Notices::new());
        assert_eq!(ledger["thumbs"], vec![row("xxh3:aa", "boom")]);
    }

    #[test]
    fn read_ledger_treats_missing_and_unparsable_as_empty_with_a_notice() {
        let state = tempfile::tempdir().expect("tempdir");
        let notices = crate::notices::Notices::new();
        assert!(read_ledger(state.path(), &notices).is_empty(), "missing");
        assert!(
            notices.drain().is_empty(),
            "a fresh catalog has no ledger and nothing to say about it"
        );

        std::fs::write(state.path().join(FAILURES_FILE), b"{ not json").expect("plant");
        let ledger = read_ledger(state.path(), &notices);
        assert!(ledger.is_empty(), "{ledger:?}");
        let drained = notices.drain();
        assert_eq!(drained.len(), 1, "{drained:?}");
        assert!(
            drained[0].contains("ignoring unparsable failure ledger"),
            "{drained:?}"
        );
        assert!(
            drained[0].contains("rebuilt by the next run"),
            "the notice must say the ledger self-heals on the next run: {drained:?}"
        );
    }

    /// The `--retry-failed` sequence at the state-dir level: a run records
    /// its permanent failures, and the clear that a retry does first drops
    /// exactly the requested kinds' rows.
    #[test]
    fn run_with_retry_failed_clears_the_requested_kinds_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let notices = crate::notices::Notices::new();
        let outcome = IndexRunOutcome {
            thumbs: ThumbOutcome {
                written: 0,
                failed: vec![failure("xxh3:aa", "decode failed", false)],
            },
            pdf: PdfOutcome {
                written: 0,
                failed: vec![failure("xxh3:bb", "not a valid pdf", false)],
            },
            ..IndexRunOutcome::default()
        };
        record_failures(&root, &outcome, &notices).expect("record");
        assert_eq!(known_failures(&root, &notices).expect("known").len(), 2);

        let kinds: BTreeSet<String> = ["thumbs".to_string()].into();
        let cleared = clear_failures(&root, &kinds, &notices).expect("clear");
        assert_eq!(cleared, 1);

        let remaining = known_failures(&root, &notices).expect("known");
        assert!(!remaining.contains_key("thumbs"), "{remaining:?}");
        assert_eq!(remaining["pdf"][0].error, "not a valid pdf");
    }

    /// `status` is the verb that shows known failures, so its own read of a
    /// corrupt ledger is exactly the diagnostic a caller needs — pins that
    /// the outcome actually carries it home rather than the sink being
    /// drained into a value nobody reads.
    #[test]
    fn status_carries_the_unparsable_failure_ledger_note_on_its_outcome() {
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
                .any(|n| n.contains("ignoring unparsable failure ledger")),
            "{:?}",
            outcome.notices
        );
    }

    /// The broken line is the one holding the key: the notice renders the
    /// whole error chain, so it must name the line without quoting it.
    #[test]
    fn the_broken_describer_config_notice_names_the_line_and_never_quotes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let app = FsApp::init(&root, "m1", "m1").expect("init");
        let path = crate::describer_config::config_path(&root, app.notices()).expect("config path");
        std::fs::write(
            &path,
            "backend = \"open-router\"\nmodel = \"m\"\napi_key = \"sk-test\" oops\n",
        )
        .expect("plant broken config");

        let outcome = status(&app, &root).expect("status");

        let about_the_config: Vec<&String> = outcome
            .notices
            .iter()
            .filter(|n| n.contains("ignoring broken describer config"))
            .collect();
        assert!(!about_the_config.is_empty(), "{:?}", outcome.notices);
        for notice in about_the_config {
            assert!(notice.contains("describer.toml: line 3: "), "{notice}");
        }
        assert!(
            outcome.notices.iter().all(|n| !n.contains("sk-test")),
            "{:?}",
            outcome.notices
        );
    }

    /// Random passes, small asset alphabet so the same id recurs across
    /// kinds and within one kind: the ledger a pass contributes is exactly
    /// its permanent failures, one row per asset per kind, and no transient
    /// failure ever reaches it. Some transient rows carry the credentials
    /// reasons the caption pass records for a rejected key or an empty
    /// account — the rows this ledger must never remember.
    #[test]
    fn no_transient_failure_ever_reaches_the_ledger() {
        use crate::capability::{OPENROUTER_KEY_REJECTED_REASON, OPENROUTER_OUT_OF_CREDIT_REASON};
        use proptest::prelude::*;

        fn row() -> impl Strategy<Value = (&'static str, bool)> {
            prop_oneof![
                Just(("boom", false)),
                Just(("boom", true)),
                Just((OPENROUTER_KEY_REJECTED_REASON, true)),
                Just((OPENROUTER_OUT_OF_CREDIT_REASON, true)),
            ]
        }

        fn failures() -> impl Strategy<Value = Vec<ItemFailure>> {
            prop::collection::vec(
                ("xxh3:[a-d]", row()).prop_map(|(asset, (error, transient))| ItemFailure {
                    asset,
                    path: PathBuf::from("/media/x"),
                    error: error.to_string(),
                    transient,
                }),
                0..5usize,
            )
        }

        proptest!(|(lists in prop::collection::vec(failures(), 9..=9))| {
            let outcome = outcome_with_failures(&lists);
            let ledger = permanent_failures(&outcome);

            let mut expected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for (kind, list) in KIND_SOURCES.iter().zip(lists.iter()) {
                for f in list.iter().filter(|f| !f.transient) {
                    expected
                        .entry((*kind).to_string())
                        .or_default()
                        .insert(f.asset.clone());
                }
            }

            let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for (kind, rows) in &ledger {
                let assets: BTreeSet<String> = rows.iter().map(|r| r.asset.clone()).collect();
                prop_assert_eq!(assets.len(), rows.len(), "one row per asset: {:?}", rows);
                prop_assert!(
                    rows.iter().all(|r| r.error != OPENROUTER_KEY_REJECTED_REASON
                        && r.error != OPENROUTER_OUT_OF_CREDIT_REASON),
                    "a credentials reason reached the ledger: {:?}",
                    rows
                );
                actual.insert(kind.clone(), assets);
            }
            prop_assert_eq!(actual, expected);
        });
    }

    /// The nine `*Outcome.failed` lists in [`KIND_SOURCES`] order, so the
    /// property test can drive every executor's failures from one vector.
    fn outcome_with_failures(lists: &[Vec<ItemFailure>]) -> IndexRunOutcome {
        let at = |i: usize| lists.get(i).cloned().unwrap_or_default();
        IndexRunOutcome {
            thumbs: ThumbOutcome {
                written: 0,
                failed: at(0),
            },
            embed: EmbedOutcome {
                failed: at(1),
                ..EmbedOutcome::default()
            },
            keyframes: KeyframeOutcome {
                failed: at(2),
                ..KeyframeOutcome::default()
            },
            keyframe_images: KeyframeImageOutcome {
                failed: at(3),
                ..KeyframeImageOutcome::default()
            },
            transcribe: TranscribeOutcome {
                written: 0,
                failed: at(4),
            },
            transcript_embed: TranscriptEmbedOutcome {
                failed: at(5),
                ..TranscriptEmbedOutcome::default()
            },
            ocr: OcrOutcome {
                failed: at(6),
                ..OcrOutcome::default()
            },
            pdf: PdfOutcome {
                written: 0,
                failed: at(7),
            },
            captions: CaptionOutcome {
                failed: at(8),
                ..CaptionOutcome::default()
            },
            notices: Vec::new(),
        }
    }

    /// The `--kinds` name each of the nine `*Outcome.failed` lists feeds,
    /// in the order [`outcome_with_failures`] fills them.
    const KIND_SOURCES: [&str; 9] = [
        "thumbs",
        "embeddings",
        "keyframes",
        "keyframe-images",
        "transcripts",
        "transcripts",
        "ocr",
        "pdf",
        "captions",
    ];
}
