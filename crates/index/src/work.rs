//! The queue IS the diff: work = (assets × required derivations) minus
//! (blobs that exist). Nothing is stored; finished work is self-evident from
//! the blob store, so runs are resumable, idempotent, and self-healing.

use std::path::PathBuf;

use majestical_core::media_kind::MediaKind;

use crate::blob::{BlobStore, Derivation, asset_hex};
use crate::model::MINILM;
use crate::ocr::OCR_MODEL_TAG;
use crate::pdf::PDF_MODEL_TAG;
use crate::transcribe::WHISPER_MODEL_TAG;

/// Extensions we know we cannot decode yet: the RAW family, plus AVIF and
/// JXL (the `image` crate build we depend on has no decoder enabled for
/// either). Planner-level so status is deterministic instead of discovered
/// by failing forever — a scanned `.avif` would otherwise retry every pass
/// under `--watch` with no way to ever succeed.
///
/// Coupled to `media_kind`'s extension table: any extension classified
/// `MediaKind::Image` there must either decode via the `image` crate or be
/// listed here — otherwise it counts `pending`, fails to decode, and
/// retry-loops under `--watch` forever, exactly the failure mode this
/// constant exists to avoid.
const UNDECODABLE_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "pef", "iiq", "3fr", "avif", "jxl",
];

/// One kind of derivable work a [`WorkItem`] can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    Thumb,
    ImageEmbed,
    Keyframes,
    /// Video or audio bytes -> a whisper transcript blob.
    Transcribe,
    /// A transcript blob -> chunk vector blobs (text embeddings).
    TranscriptEmbed,
    /// A still image -> recognized-text blob (Vision, ships with macOS).
    OcrImage,
    /// One video whose keyframe manifest is ready -> per-frame OCR blobs.
    OcrKeyframes,
    /// A PDF -> per-page extracted-text blob (`PDFKit`).
    PdfText,
    /// A still (or a video whose keyframe manifest is ready) -> a describer
    /// caption/tags blob.
    Caption,
}

/// One unit of pending work: an asset, its readable bytes, and which
/// derivation to produce.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub asset: String,
    pub asset_hex: String,
    /// Readable absolute path to the asset's bytes. For
    /// [`WorkKind::TranscriptEmbed`] items this may be an empty `PathBuf`
    /// sentinel: the runner reads the transcript blob, not the source file,
    /// so the item is planned even when no instance is reachable here (a
    /// teammate-synced transcript on a machine without the original media).
    pub abs_path: PathBuf,
    pub kind: WorkKind,
}

/// One asset as the planner sees it: its catalog id, coarse media kind, and
/// (if resolvable right now) a readable absolute path to its bytes.
#[derive(Debug, Clone)]
pub struct AssetSource {
    pub asset: String,
    pub kind: MediaKind,
    /// Resolved readable path on an online volume, if any.
    pub abs_path: Option<PathBuf>,
}

/// What this machine can currently produce. Gates embeddings/keyframes
/// (`model_tag`) and video thumbnailing/keyframing/transcription/keyframe-OCR
/// (`ffmpeg`) so the planner reports the true reason work can't run yet
/// rather than pretending it's simply pending.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub model_tag: Option<String>,
    pub ffmpeg: bool,
    /// Whisper transcription model installed.
    pub whisper: bool,
    /// `MiniLM` text-embedding model installed.
    pub text_model: bool,
    /// Model tag of the configured describer (e.g. `describe-qwen3-vl-8b`),
    /// `None` when no describer is configured on this machine.
    pub describer_tag: Option<String>,
}

/// Counts for one derivation kind across every planned asset. Every asset
/// eligible for the kind lands in exactly one bucket — per planning pass.
/// Aggregate fields cover more than one pass (`WorkPlan::transcripts` sums
/// the transcribe and transcript-embed passes), so their totals are
/// derivation counts, not asset counts: one audio asset can contribute two
/// increments to the same `KindStatus`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KindStatus {
    /// Blob already exists.
    pub done: u64,
    /// Blob missing, bytes reachable, capability available — queued.
    pub pending: u64,
    /// Blob missing and the asset's volume isn't mounted right now.
    pub offline: u64,
    /// Blob missing and the source format can't be decoded (RAW/AVIF).
    pub unsupported: u64,
    /// Blob missing and this machine has no ffmpeg installed.
    pub needs_ffmpeg: u64,
    /// Blob missing and no encoder model is installed.
    pub needs_model: u64,
}

/// The full diff between required derivations and what the blob store
/// already has.
#[derive(Debug, Default)]
pub struct WorkPlan {
    /// Priority-ordered: thumbnails, image embeddings, keyframes,
    /// transcripts, transcript embeddings, OCR, PDF text, then captions.
    pub items: Vec<WorkItem>,
    pub thumbs: KindStatus,
    pub embeddings: KindStatus,
    pub keyframes: KindStatus,
    /// Covers both [`WorkKind::Transcribe`] and [`WorkKind::TranscriptEmbed`]
    /// — totals are derivation counts, not asset counts: one audio asset with
    /// a transcript blob and its chunk vectors counts `done` twice here.
    pub transcripts: KindStatus,
    /// Covers both [`WorkKind::OcrImage`] and [`WorkKind::OcrKeyframes`].
    pub ocr: KindStatus,
    pub pdf: KindStatus,
    pub captions: KindStatus,
}

fn is_undecodable(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| UNDECODABLE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Diffs `sources` against `blobs` under `caps`, producing a priority-ordered
/// work queue plus per-kind status counts. Assets whose id isn't `xxh3:`-
/// prefixed are skipped entirely. [`MediaKind::Other`] has no derivation at
/// all; [`MediaKind::Audio`] is skipped by the thumbnail/image-embed/keyframe
/// passes (no visual thumbnail for audio) and covered by transcribe instead.
/// [`MediaKind::Pdf`] joins the thumbnail and image-embed passes (page 1
/// renders like a still — see `pdf::render_first_page`) on top of its own
/// PDF-text/caption passes.
///
/// Ten passes over `sources` (rather than one) so `items` comes out
/// globally priority-ordered — every thumbnail before every image embedding
/// before every keyframe set before every transcript before every transcript
/// embedding before every OCR item before every PDF text item before every
/// caption — instead of grouped per asset.
#[must_use]
pub fn plan_work(sources: &[AssetSource], blobs: &BlobStore, caps: &Capabilities) -> WorkPlan {
    let mut plan = WorkPlan::default();
    for source in sources.iter().filter(|s| match s.kind {
        MediaKind::Image | MediaKind::Video | MediaKind::Pdf => true,
        MediaKind::Audio | MediaKind::Other => false,
    }) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_thumb(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| match s.kind {
        MediaKind::Image | MediaKind::Pdf => true,
        MediaKind::Video | MediaKind::Audio | MediaKind::Other => false,
    }) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_image_embed(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Video) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_keyframes(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| match s.kind {
        MediaKind::Video | MediaKind::Audio => true,
        MediaKind::Image | MediaKind::Pdf | MediaKind::Other => false,
    }) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_transcribe(source, hex, blobs, caps, &mut plan);
        }
    }
    // Blob-driven, not kind-driven: a transcript can exist for any asset
    // that went through `plan_transcribe`, or one synced in from a teammate
    // for an asset this machine can't even read — so every source is
    // considered, not just `Video`/`Audio`.
    for source in sources {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_transcript_embed(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Image) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_ocr_image(source, hex, blobs, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Video) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_ocr_keyframes(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Pdf) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_pdf_text(source, hex, blobs, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| match s.kind {
        MediaKind::Image | MediaKind::Pdf => true,
        MediaKind::Video | MediaKind::Audio | MediaKind::Other => false,
    }) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_caption_still(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Video) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_caption_video(source, hex, blobs, caps, &mut plan);
        }
    }
    plan
}

/// THUMBS: blob exists -> done; else offline (no path) / unsupported
/// (RAW/AVIF ext) / `needs_ffmpeg` (video without ffmpeg) / pending+item, in
/// that order.
fn plan_thumb(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let path = blobs.path_for(hex, &Derivation::Thumb);
    if path.is_file() {
        plan.thumbs.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.thumbs.offline += 1;
        return;
    };
    if is_undecodable(abs_path) {
        plan.thumbs.unsupported += 1;
        return;
    }
    if source.kind == MediaKind::Video && !caps.ffmpeg {
        plan.thumbs.needs_ffmpeg += 1;
        return;
    }
    plan.thumbs.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::Thumb,
    });
}

/// IMAGE EMBED (`MediaKind::Image | MediaKind::Pdf` — a PDF's first page
/// renders like a still): no model -> `needs_model`; blob exists -> done;
/// else offline/unsupported/pending+item.
fn plan_image_embed(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let Some(model_tag) = &caps.model_tag else {
        plan.embeddings.needs_model += 1;
        return;
    };
    let path = blobs.path_for(hex, &Derivation::ImageEmbedding { model_tag });
    if path.is_file() {
        plan.embeddings.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.embeddings.offline += 1;
        return;
    };
    if is_undecodable(abs_path) {
        plan.embeddings.unsupported += 1;
        return;
    }
    plan.embeddings.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::ImageEmbed,
    });
}

/// KEYFRAMES (`MediaKind::Video` only): no model -> `needs_model` (checked
/// before everything else — a documented precedence, since it gates the same
/// work `needs_ffmpeg`/offline would); manifest blob exists -> done; else
/// offline (no path) / `needs_ffmpeg` / pending+item, in that order — the
/// same offline-before-ffmpeg precedence `plan_thumb` and `plan_image_embed`
/// use, so one offline video classifies consistently across every kind.
fn plan_keyframes(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let Some(model_tag) = &caps.model_tag else {
        plan.keyframes.needs_model += 1;
        return;
    };
    let path = blobs.path_for(hex, &Derivation::KeyframeManifest { model_tag });
    if path.is_file() {
        plan.keyframes.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.keyframes.offline += 1;
        return;
    };
    if !caps.ffmpeg {
        plan.keyframes.needs_ffmpeg += 1;
        return;
    }
    plan.keyframes.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::Keyframes,
    });
}

/// TRANSCRIBE (`MediaKind::Video | MediaKind::Audio`): blob exists -> done
/// (checked BEFORE the whisper gate — the transcript blob path doesn't
/// depend on any capability, and a transcript synced in from a teammate
/// must count done even on a whisper-less machine, or status lies on
/// sync-consumer machines); else no whisper -> `needs_model`; else offline
/// (no path) / `needs_ffmpeg` (audio/video bytes need ffmpeg to decode to
/// PCM) / pending+item, in that order.
fn plan_transcribe(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let path = blobs.path_for(
        hex,
        &Derivation::Transcript {
            model_tag: WHISPER_MODEL_TAG,
        },
    );
    if path.is_file() {
        plan.transcripts.done += 1;
        return;
    }
    if !caps.whisper {
        plan.transcripts.needs_model += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.transcripts.offline += 1;
        return;
    };
    if !caps.ffmpeg {
        plan.transcripts.needs_ffmpeg += 1;
        return;
    }
    plan.transcripts.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::Transcribe,
    });
}

/// TRANSCRIPT EMBED (any kind whose Transcript blob exists — blob-driven,
/// not kind-driven): no transcript blob -> skip entirely, silently (an asset
/// with no transcript isn't this kind's work to count; `plan_transcribe`
/// already counted it). Blob exists -> check
/// [`BlobStore::has_chunk_completion`] against `MINILM`'s fixed model tag
/// (unlike `ImageEmbedding`, the marker path doesn't depend on
/// `caps.model_tag`, so done can be checked before any capability gate) ->
/// done; else no text model -> `needs_model`; else pending+item. Individual
/// chunk blobs without a completion marker count PENDING — an interrupted
/// run may have written blobs without indexing them, and re-planning is
/// cheap since the runner skips blobs that already exist. Never offline:
/// this reads a blob, not the source file, so a transcript synced in from a
/// teammate can be chunked even when the original video/audio isn't
/// reachable here.
fn plan_transcript_embed(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let transcript_path = blobs.path_for(
        hex,
        &Derivation::Transcript {
            model_tag: WHISPER_MODEL_TAG,
        },
    );
    if !transcript_path.is_file() {
        return;
    }
    if blobs.has_chunk_completion(hex, MINILM.tag) {
        plan.transcripts.done += 1;
        return;
    }
    if !caps.text_model {
        plan.transcripts.needs_model += 1;
        return;
    }
    plan.transcripts.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: source.abs_path.clone().unwrap_or_default(),
        kind: WorkKind::TranscriptEmbed,
    });
}

/// OCR, STILLS (`MediaKind::Image` only): no capability gate (Vision ships
/// with macOS). Blob exists -> done; else offline (no path) / unsupported
/// (RAW/AVIF ext) / pending+item, in that order — same precedence as
/// `plan_thumb`. The unsupported gate matters here too, not just for
/// thumbs/embeddings: `ocr::recognize_text` takes an already-decoded
/// `image::RgbImage`, so the runner decodes via the same `image` crate
/// pipeline that can't handle RAW/AVIF/JXL — without this gate those
/// formats would retry forever every `--watch` pass instead of settling
/// into `unsupported`.
fn plan_ocr_image(source: &AssetSource, hex: &str, blobs: &BlobStore, plan: &mut WorkPlan) {
    let path = blobs.path_for(
        hex,
        &Derivation::OcrImage {
            model_tag: OCR_MODEL_TAG,
        },
    );
    if path.is_file() {
        plan.ocr.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.ocr.offline += 1;
        return;
    };
    if is_undecodable(abs_path) {
        plan.ocr.unsupported += 1;
        return;
    }
    plan.ocr.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::OcrImage,
    });
}

/// OCR, VIDEO KEYFRAMES (`MediaKind::Video` only): no keyframe manifest blob
/// (under the vision `caps.model_tag`, including when that capability is
/// itself missing) -> skip entirely, not pending, not counted — the
/// keyframes pass owns that signal, and re-planning here before it exists
/// would just be noise every pass. Manifest present -> check the
/// [`Derivation::OcrComplete`] marker (the runner writes it once every
/// manifest timestamp has an OCR blob — far cheaper than diffing every
/// timestamp here on every status call) -> done; else offline (no path) /
/// `needs_ffmpeg` (per-timestamp frames are extracted via ffmpeg) /
/// pending+item.
fn plan_ocr_keyframes(
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
    let done_path = blobs.path_for(
        hex,
        &Derivation::OcrComplete {
            model_tag: OCR_MODEL_TAG,
        },
    );
    if done_path.is_file() {
        plan.ocr.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.ocr.offline += 1;
        return;
    };
    if !caps.ffmpeg {
        plan.ocr.needs_ffmpeg += 1;
        return;
    }
    plan.ocr.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::OcrKeyframes,
    });
}

/// PDF TEXT (`MediaKind::Pdf` only): no capability gate (`PDFKit` ships with
/// macOS). Blob exists -> done; else offline (no path) / pending+item.
fn plan_pdf_text(source: &AssetSource, hex: &str, blobs: &BlobStore, plan: &mut WorkPlan) {
    let path = blobs.path_for(
        hex,
        &Derivation::PdfText {
            model_tag: PDF_MODEL_TAG,
        },
    );
    if path.is_file() {
        plan.pdf.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.pdf.offline += 1;
        return;
    };
    plan.pdf.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::PdfText,
    });
}

/// CAPTIONS, STILLS (`MediaKind::Image | MediaKind::Pdf` — a PDF page
/// renders like a still): no describer configured -> `needs_model` (checked
/// first: the [`Derivation::Caption`] blob path depends on the describer's
/// tag, so done can't be checked without it, same reasoning as
/// `plan_image_embed`'s model gate). BOTH the caption and
/// [`Derivation::Tags`] blobs exist -> done — a caption blob alone means a
/// run died (or hit a backend failure) between the two writes, and counting
/// it done would leave the tags blob missing forever; the runner skips the
/// completed caption half on retry. Else offline (no path) / pending+item.
fn plan_caption_still(
    source: &AssetSource,
    hex: &str,
    blobs: &BlobStore,
    caps: &Capabilities,
    plan: &mut WorkPlan,
) {
    let Some(describer_tag) = caps.describer_tag.as_deref() else {
        plan.captions.needs_model += 1;
        return;
    };
    let caption_path = blobs.path_for(
        hex,
        &Derivation::Caption {
            model_tag: describer_tag,
        },
    );
    let tags_path = blobs.path_for(
        hex,
        &Derivation::Tags {
            model_tag: describer_tag,
        },
    );
    if caption_path.is_file() && tags_path.is_file() {
        plan.captions.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.captions.offline += 1;
        return;
    };
    plan.captions.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::Caption,
    });
}

/// CAPTIONS, VIDEO (`MediaKind::Video` only): no keyframe manifest blob
/// (under the vision `caps.model_tag`, including when that capability is
/// itself missing) -> skip entirely, silently — waits for the keyframes
/// pass, same precedent as `plan_ocr_keyframes`. Manifest present -> no
/// describer configured -> `needs_model`; BOTH the [`Derivation::Captions`]
/// and [`Derivation::Tags`] blobs exist -> done (the same two-blob rule as
/// [`plan_caption_still`], for the same partial-run reason); else offline
/// (no path) / pending+item.
fn plan_caption_video(
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
    let Some(describer_tag) = caps.describer_tag.as_deref() else {
        plan.captions.needs_model += 1;
        return;
    };
    let captions_path = blobs.path_for(
        hex,
        &Derivation::Captions {
            model_tag: describer_tag,
        },
    );
    let tags_path = blobs.path_for(
        hex,
        &Derivation::Tags {
            model_tag: describer_tag,
        },
    );
    if captions_path.is_file() && tags_path.is_file() {
        plan.captions.done += 1;
        return;
    }
    let Some(abs_path) = &source.abs_path else {
        plan.captions.offline += 1;
        return;
    };
    plan.captions.pending += 1;
    plan.items.push(WorkItem {
        asset: source.asset.clone(),
        asset_hex: hex.to_string(),
        abs_path: abs_path.clone(),
        kind: WorkKind::Caption,
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AssetSource, Capabilities, WorkKind, plan_work};
    use crate::blob::{BlobStore, Derivation};
    use crate::model::MINILM;
    use crate::ocr::OCR_MODEL_TAG;
    use crate::transcribe::WHISPER_MODEL_TAG;
    use majestical_core::media_kind::MediaKind;

    /// No capability turned on — every gated kind reports `needs_model`/
    /// `needs_ffmpeg` rather than planning work.
    fn base_caps() -> Capabilities {
        Capabilities {
            model_tag: None,
            ffmpeg: false,
            whisper: false,
            text_model: false,
            describer_tag: None,
        }
    }

    /// Every capability this machine can have turned on, so every kind's
    /// gate is open and only blob/path state decides done/offline/pending.
    fn full_caps() -> Capabilities {
        Capabilities {
            model_tag: Some("m1".into()),
            ffmpeg: true,
            whisper: true,
            text_model: true,
            describer_tag: Some("describe-qwen3-vl-8b".into()),
        }
    }

    fn source(asset: &str, kind: MediaKind, abs_path: Option<&str>) -> AssetSource {
        AssetSource {
            asset: asset.to_string(),
            kind,
            abs_path: abs_path.map(PathBuf::from),
        }
    }

    #[test]
    fn plans_missing_thumbs_and_counts_statuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = base_caps();
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:bb22bb22bb22bb22bb22bb22bb22bb22".into(),
                kind: MediaKind::Image,
                abs_path: None,
            },
            AssetSource {
                asset: "xxh3:cc33cc33cc33cc33cc33cc33cc33cc33".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/c.mov".into()),
            },
            AssetSource {
                asset: "xxh3:dd44dd44dd44dd44dd44dd44dd44dd44".into(),
                kind: MediaKind::Other,
                abs_path: Some("/tmp/d.txt".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);
        assert_eq!(plan.thumbs.pending, 1);
        assert_eq!(plan.thumbs.offline, 1);
        assert_eq!(plan.thumbs.needs_ffmpeg, 1, "video thumb needs ffmpeg");
        assert_eq!(
            plan.embeddings.needs_model, 2,
            "both Image assets (aa11, bb22) are embedding-eligible with no model installed"
        );
        assert_eq!(plan.keyframes.needs_model, 1, "cc33 needs a model too");
        assert_eq!(
            plan.items.len(),
            2,
            "aa11 is the only online image: its thumb, and its ungated OCR item \
             (OCR has no capability gate — see plan_ocr_image)"
        );
        assert!(matches!(plan.items[0].kind, WorkKind::Thumb));
        assert!(matches!(plan.items[1].kind, WorkKind::OcrImage));
    }

    #[test]
    fn existing_blobs_count_done_and_raw_images_are_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "aa11aa11aa11aa11aa11aa11aa11aa11";
        let thumb = store.path_for(hex, &Derivation::Thumb);
        store.write_atomic(&thumb, b"x").expect("seed thumb");
        let caps = Capabilities {
            model_tag: Some("m1".into()),
            ..base_caps()
        };
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:ee55ee55ee55ee55ee55ee55ee55ee55".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/e.cr3".into()),
            },
            AssetSource {
                asset: "xxh3:ff66ff66ff66ff66ff66ff66ff66ff66".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/f.avif".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);
        assert_eq!(plan.thumbs.done, 1);
        assert_eq!(
            plan.thumbs.unsupported, 2,
            "RAW and AVIF are both planner-level unsupported"
        );
        assert_eq!(plan.embeddings.unsupported, 2);
        assert_eq!(plan.embeddings.pending, 1, "aa11 embedding is embeddable");
        assert_eq!(
            plan.ocr.unsupported, 2,
            "RAW and AVIF are unsupported for OCR too"
        );
        assert_eq!(
            plan.items.len(),
            2,
            "aa11's ImageEmbed and OcrImage — ee55/ff66 are undecodable for both \
             (recognize_text takes an already-decoded image::RgbImage, same as \
             embeddings): {:?}",
            plan.items
        );
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.kind == WorkKind::ImageEmbed)
                .count(),
            1
        );
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.kind == WorkKind::OcrImage)
                .count(),
            1,
            "only aa11 (the decodable PNG) gets an OcrImage item"
        );
    }

    /// The RAW/JXL extensions this PR added to `media_kind`'s table (pef,
    /// iiq, 3fr, jxl) must also be in `UNDECODABLE_EXTS` — otherwise they'd
    /// count `pending`, fail to decode, and retry every `--watch` pass
    /// forever instead of settling into `unsupported`.
    #[test]
    fn newly_classified_raw_and_jxl_extensions_are_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = base_caps();
        let sources = vec![
            AssetSource {
                asset: "xxh3:aef1aef1aef1aef1aef1aef1aef1aef1".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/shot.pef".into()),
            },
            AssetSource {
                asset: "xxh3:11a111a111a111a111a111a111a111a1".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/shot.iiq".into()),
            },
            AssetSource {
                asset: "xxh3:3fb13fb13fb13fb13fb13fb13fb13fb1".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/shot.3fr".into()),
            },
            AssetSource {
                asset: "xxh3:cd31cd31cd31cd31cd31cd31cd31cd31".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/shot.jxl".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);
        assert_eq!(
            plan.thumbs.unsupported, 4,
            "pef, iiq, 3fr, and jxl must all classify as unsupported, not pending"
        );
        assert_eq!(plan.thumbs.pending, 0);
        assert_eq!(
            plan.ocr.unsupported, 4,
            "the same extensions must also be unsupported for OCR, not pending — \
             recognize_text takes an already-decoded image::RgbImage, same as thumbs"
        );
        assert!(
            plan.items.is_empty(),
            "no work item should be queued for any of them: {:?}",
            plan.items
        );
    }

    /// `items` is globally priority-ordered — every thumbnail before every
    /// image embedding before every keyframe set — not grouped per asset, so
    /// a run that dies halfway has produced the cheapest, most-visible
    /// derivations first.
    #[test]
    fn items_are_globally_ordered_thumbs_then_embeds_then_keyframes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            model_tag: Some("m1".into()),
            ffmpeg: true,
            ..base_caps()
        };
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:cc33cc33cc33cc33cc33cc33cc33cc33".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/c.mov".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);
        let order: Vec<(WorkKind, &str)> = plan
            .items
            .iter()
            .map(|i| (i.kind, i.asset.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                (WorkKind::Thumb, "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11"),
                (WorkKind::Thumb, "xxh3:cc33cc33cc33cc33cc33cc33cc33cc33"),
                (
                    WorkKind::ImageEmbed,
                    "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11"
                ),
                (WorkKind::Keyframes, "xxh3:cc33cc33cc33cc33cc33cc33cc33cc33"),
                (WorkKind::OcrImage, "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11"),
            ],
            "both thumbs must precede the embedding, which must precede the keyframes, \
             which must precede aa11's ungated OCR item"
        );
    }

    /// The `MediaKind::Video` check in `plan_thumb`'s ffmpeg gate must gate
    /// the video asset, not merely produce the right *counts*: a prior
    /// version of this test only checked `plan.thumbs.pending`/
    /// `needs_ffmpeg` counts, which stay `1`/`1` even if the gate is
    /// inverted (`kind != Video`) — the image and video assets simply trade
    /// places in the two buckets. Asserting which asset lands in `items`
    /// (identity, not count) is what actually pins the gate to the video
    /// kind.
    #[test]
    fn plan_thumb_ffmpeg_gate_applies_only_to_the_video_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = base_caps();
        let sources = vec![
            AssetSource {
                asset: "xxh3:10a110a110a110a110a110a110a110a1".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/img1.png".into()),
            },
            AssetSource {
                asset: "xxh3:70d170d170d170d170d170d170d170d1".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/vid1.mov".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);

        assert!(
            plan.items
                .iter()
                .any(|i| i.asset == "xxh3:10a110a110a110a110a110a110a110a1"
                    && matches!(i.kind, WorkKind::Thumb)),
            "an image thumb must be queued even with no ffmpeg installed"
        );
        assert!(
            !plan
                .items
                .iter()
                .any(|i| i.asset == "xxh3:70d170d170d170d170d170d170d170d1"
                    && matches!(i.kind, WorkKind::Thumb)),
            "a video thumb must not be queued without ffmpeg"
        );
    }

    #[test]
    fn plan_image_embed_counts_done_and_offline_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            model_tag: Some("m1".into()),
            ..base_caps()
        };
        let done_blob = store.path_for(
            "aa11aa11aa11aa11aa11aa11aa11aa11",
            &Derivation::ImageEmbedding { model_tag: "m1" },
        );
        store
            .write_atomic(&done_blob, b"x")
            .expect("seed embedding blob");
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:bb22bb22bb22bb22bb22bb22bb22bb22".into(),
                kind: MediaKind::Image,
                abs_path: None,
            },
        ];
        let plan = plan_work(&sources, &store, &caps);

        assert_eq!(
            plan.embeddings.done, 1,
            "aa11 already has an embedding blob"
        );
        assert_eq!(plan.embeddings.offline, 1, "bb22 has no readable path");
        assert!(
            !plan
                .items
                .iter()
                .any(|i| matches!(i.kind, WorkKind::ImageEmbed)),
            "no embed item should be queued: {:?}",
            plan.items
        );
    }

    #[test]
    fn plan_keyframes_counts_done_offline_and_needs_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            model_tag: Some("m1".into()),
            ..base_caps()
        };
        let done_blob = store.path_for(
            "aa11aa11aa11aa11aa11aa11aa11aa11",
            &Derivation::KeyframeManifest { model_tag: "m1" },
        );
        store
            .write_atomic(&done_blob, b"[]")
            .expect("seed manifest blob");
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11aa11aa11aa11aa11aa11aa11aa11".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/a.mov".into()),
            },
            AssetSource {
                asset: "xxh3:bb22bb22bb22bb22bb22bb22bb22bb22".into(),
                kind: MediaKind::Video,
                abs_path: None,
            },
            AssetSource {
                asset: "xxh3:cc33cc33cc33cc33cc33cc33cc33cc33".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/c.mov".into()),
            },
        ];
        let plan = plan_work(&sources, &store, &caps);

        assert_eq!(
            plan.keyframes.done, 1,
            "aa11 already has a keyframe manifest"
        );
        assert_eq!(plan.keyframes.offline, 1, "bb22 has no readable path");
        assert_eq!(
            plan.keyframes.needs_ffmpeg, 1,
            "cc33 has no manifest and no ffmpeg"
        );
        assert_eq!(plan.items.len(), 0, "no asset should be queued");
    }

    #[test]
    fn transcript_planned_for_video_and_audio_with_whisper_and_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let sources = vec![
            source(
                "xxh3:70707070707070707070707070707070",
                MediaKind::Video,
                Some("/tmp/v.mov"),
            ),
            source(
                "xxh3:a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
                MediaKind::Audio,
                Some("/tmp/a.m4a"),
            ),
            source(
                "xxh3:10101010101010101010101010101010",
                MediaKind::Image,
                Some("/tmp/i.jpg"),
            ),
        ];
        let plan = plan_work(&sources, &store, &full_caps());

        let transcribe_items: Vec<_> = plan
            .items
            .iter()
            .filter(|i| i.kind == WorkKind::Transcribe)
            .collect();
        assert_eq!(
            transcribe_items.len(),
            2,
            "video + audio, never image: {:?}",
            plan.items
        );
        assert!(
            transcribe_items
                .iter()
                .any(|i| i.asset == "xxh3:70707070707070707070707070707070")
        );
        assert!(
            transcribe_items
                .iter()
                .any(|i| i.asset == "xxh3:a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1")
        );
    }

    #[test]
    fn transcript_needs_model_without_whisper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            ffmpeg: true,
            ..base_caps()
        };
        let sources = vec![source(
            "xxh3:70707070707070707070707070707070",
            MediaKind::Video,
            Some("/tmp/v.mov"),
        )];
        let plan = plan_work(&sources, &store, &caps);

        assert_eq!(plan.transcripts.needs_model, 1);
        assert!(!plan.items.iter().any(|i| i.kind == WorkKind::Transcribe));
    }

    #[test]
    fn transcript_needs_ffmpeg_without_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            whisper: true,
            ..base_caps()
        };
        let sources = vec![source(
            "xxh3:70707070707070707070707070707070",
            MediaKind::Video,
            Some("/tmp/v.mov"),
        )];
        let plan = plan_work(&sources, &store, &caps);

        assert_eq!(plan.transcripts.needs_ffmpeg, 1);
        assert!(!plan.items.iter().any(|i| i.kind == WorkKind::Transcribe));
    }

    /// F4: a transcript blob synced in from a teammate counts `done` even on
    /// a whisper-less machine — the done check must precede the whisper
    /// gate, or status lies on sync-consumer machines.
    #[test]
    fn transcript_blob_counts_done_without_whisper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "dd22dd22dd22dd22dd22dd22dd22dd22";
        let transcript = store.path_for(
            hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript, b"{}")
            .expect("seed transcript");
        // whisper=false; text_model=true so the transcript-embed pass plans
        // an item rather than muddying needs_model.
        let caps = Capabilities {
            text_model: true,
            ..base_caps()
        };
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Audio, None)];
        let plan = plan_work(&sources, &store, &caps);

        assert_eq!(
            plan.transcripts.done, 1,
            "the synced transcript blob must count done, not needs_model"
        );
        assert_eq!(plan.transcripts.needs_model, 0);
    }

    /// PDF preview feeds the existing pipeline: with every capability on, a
    /// Pdf asset plans Thumb + `ImageEmbed` items alongside its own
    /// `PdfText`.
    #[test]
    fn pdf_assets_plan_thumb_image_embed_and_pdf_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let sources = vec![source(
            "xxh3:adf1adf1adf1adf1adf1adf1adf1adf1",
            MediaKind::Pdf,
            Some("/tmp/doc.pdf"),
        )];
        let plan = plan_work(&sources, &store, &full_caps());

        for kind in [WorkKind::Thumb, WorkKind::ImageEmbed, WorkKind::PdfText] {
            assert!(
                plan.items
                    .iter()
                    .any(|i| i.asset == "xxh3:adf1adf1adf1adf1adf1adf1adf1adf1" && i.kind == kind),
                "expected a {kind:?} item for the pdf: {:?}",
                plan.items
            );
        }
    }

    #[test]
    fn transcript_embed_planned_when_transcript_blob_exists_but_chunks_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "aa11aa11aa11aa11aa11aa11aa11aa11";
        let transcript = store.path_for(
            hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript, b"{}")
            .expect("seed transcript");
        // No ffmpeg/whisper on this machine — the teammate-synced transcript
        // scenario: the transcript blob arrived from elsewhere, only the
        // text-embedding model is present locally.
        let caps = Capabilities {
            text_model: true,
            ..base_caps()
        };
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Video, None)];
        let plan = plan_work(&sources, &store, &caps);

        assert!(
            plan.items
                .iter()
                .any(|i| i.kind == WorkKind::TranscriptEmbed),
            "{:?}",
            plan.items
        );
    }

    #[test]
    fn transcript_embed_done_when_completion_marker_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "bb11bb11bb11bb11bb11bb11bb11bb11";
        let transcript = store.path_for(
            hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript, b"{}")
            .expect("seed transcript");
        let chunk = store.path_for(
            hex,
            &Derivation::TranscriptChunk {
                model_tag: MINILM.tag,
                start_ms: 0,
            },
        );
        store.write_vector(&chunk, &[0.1, 0.2]).expect("seed chunk");
        let marker = store.path_for(
            hex,
            &Derivation::ChunksComplete {
                model_tag: MINILM.tag,
            },
        );
        store
            .write_atomic(&marker, b"{}")
            .expect("seed completion marker");
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Video, Some("/tmp/v.mov"))];
        let plan = plan_work(&sources, &store, &full_caps());

        assert!(
            !plan
                .items
                .iter()
                .any(|i| i.kind == WorkKind::TranscriptEmbed),
            "the completion marker means done: {:?}",
            plan.items
        );
        assert_eq!(
            plan.transcripts.done, 2,
            "both the transcribe kind (blob exists) and the embed kind (marker exists) count done"
        );
    }

    /// The rebuildable-projection invariant: chunk blobs are written before
    /// the vector-store add, so a chunk blob WITHOUT the completion marker
    /// can mean an interrupted, partially indexed run — it must re-plan as
    /// pending, never count done.
    #[test]
    fn transcript_embed_chunk_blob_without_marker_is_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "ee22ee22ee22ee22ee22ee22ee22ee22";
        let transcript = store.path_for(
            hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript, b"{}")
            .expect("seed transcript");
        let chunk = store.path_for(
            hex,
            &Derivation::TranscriptChunk {
                model_tag: MINILM.tag,
                start_ms: 0,
            },
        );
        store.write_vector(&chunk, &[0.1, 0.2]).expect("seed chunk");
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Audio, Some("/tmp/a.m4a"))];
        let plan = plan_work(&sources, &store, &full_caps());

        assert!(
            plan.items
                .iter()
                .any(|i| i.kind == WorkKind::TranscriptEmbed),
            "a chunk blob without the completion marker must re-plan: {:?}",
            plan.items
        );
        assert_eq!(plan.transcripts.pending, 1);
    }

    #[test]
    fn transcript_embed_done_when_empty_marker_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "cc11cc11cc11cc11cc11cc11cc11cc11";
        let transcript = store.path_for(
            hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript, b"{}")
            .expect("seed transcript");
        let marker = store.path_for(
            hex,
            &Derivation::ChunksEmpty {
                model_tag: MINILM.tag,
            },
        );
        store.write_atomic(&marker, b"{}").expect("seed marker");
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Audio, Some("/tmp/a.m4a"))];
        let plan = plan_work(&sources, &store, &full_caps());

        assert!(
            !plan
                .items
                .iter()
                .any(|i| i.kind == WorkKind::TranscriptEmbed),
            "the empty-transcript marker must count as done: {:?}",
            plan.items
        );
    }

    #[test]
    fn ocr_planned_for_stills_and_pdf_text_for_pdfs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let sources = vec![
            source(
                "xxh3:10a110a110a110a110a110a110a110a1",
                MediaKind::Image,
                Some("/tmp/i.jpg"),
            ),
            source(
                "xxh3:adf1adf1adf1adf1adf1adf1adf1adf1",
                MediaKind::Pdf,
                Some("/tmp/p.pdf"),
            ),
        ];
        let plan = plan_work(&sources, &store, &base_caps());

        assert!(
            plan.items
                .iter()
                .any(|i| i.asset == "xxh3:10a110a110a110a110a110a110a110a1"
                    && i.kind == WorkKind::OcrImage),
            "{:?}",
            plan.items
        );
        assert!(
            plan.items
                .iter()
                .any(|i| i.asset == "xxh3:adf1adf1adf1adf1adf1adf1adf1adf1"
                    && i.kind == WorkKind::PdfText),
            "{:?}",
            plan.items
        );
    }

    #[test]
    fn ocr_keyframes_planned_only_with_manifest_and_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps_full = full_caps();

        let no_manifest = vec![source(
            "xxh3:0bad0bad0bad0bad0bad0bad0bad0bad",
            MediaKind::Video,
            Some("/tmp/nomanifest.mov"),
        )];
        let plan_no_manifest = plan_work(&no_manifest, &store, &caps_full);
        assert!(
            !plan_no_manifest
                .items
                .iter()
                .any(|i| i.kind == WorkKind::OcrKeyframes),
            "no manifest yet, must wait for the keyframes pass: {:?}",
            plan_no_manifest.items
        );

        let hex = "dd11dd11dd11dd11dd11dd11dd11dd11";
        let manifest = store.path_for(hex, &Derivation::KeyframeManifest { model_tag: "m1" });
        store.write_atomic(&manifest, b"[]").expect("seed manifest");
        let asset = format!("xxh3:{hex}");
        let with_manifest = vec![source(
            &asset,
            MediaKind::Video,
            Some("/tmp/withmanifest.mov"),
        )];

        let plan_with_manifest = plan_work(&with_manifest, &store, &caps_full);
        assert_eq!(
            plan_with_manifest
                .items
                .iter()
                .filter(|i| i.kind == WorkKind::OcrKeyframes)
                .count(),
            1,
            "{:?}",
            plan_with_manifest.items
        );

        let caps_no_ffmpeg = Capabilities {
            ffmpeg: false,
            ..full_caps()
        };
        let plan_no_ffmpeg = plan_work(&with_manifest, &store, &caps_no_ffmpeg);
        assert_eq!(plan_no_ffmpeg.ocr.needs_ffmpeg, 1);
        assert!(
            !plan_no_ffmpeg
                .items
                .iter()
                .any(|i| i.kind == WorkKind::OcrKeyframes)
        );
    }

    #[test]
    fn ocr_done_when_ocr_blob_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "ee11ee11ee11ee11ee11ee11ee11ee11";
        let ocr_blob = store.path_for(
            hex,
            &Derivation::OcrImage {
                model_tag: OCR_MODEL_TAG,
            },
        );
        store.write_atomic(&ocr_blob, b"{}").expect("seed ocr blob");
        let asset = format!("xxh3:{hex}");
        let sources = vec![source(&asset, MediaKind::Image, Some("/tmp/i.jpg"))];
        let plan = plan_work(&sources, &store, &base_caps());

        assert_eq!(plan.ocr.done, 1);
        assert!(!plan.items.iter().any(|i| i.kind == WorkKind::OcrImage));
    }

    #[test]
    fn captions_planned_only_with_describer_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());

        let caps_no_describer = Capabilities {
            describer_tag: None,
            ..full_caps()
        };
        let image_sources = vec![source(
            "xxh3:10a110a110a110a110a110a110a110a1",
            MediaKind::Image,
            Some("/tmp/i.jpg"),
        )];
        let plan_no_describer = plan_work(&image_sources, &store, &caps_no_describer);
        assert_eq!(plan_no_describer.captions.needs_model, 1);
        assert!(
            !plan_no_describer
                .items
                .iter()
                .any(|i| i.kind == WorkKind::Caption)
        );

        let plan_with_describer = plan_work(&image_sources, &store, &full_caps());
        assert!(
            plan_with_describer
                .items
                .iter()
                .any(|i| i.asset == "xxh3:10a110a110a110a110a110a110a110a1"
                    && i.kind == WorkKind::Caption)
        );

        let hex = "ff11ff11ff11ff11ff11ff11ff11ff11";
        let manifest = store.path_for(hex, &Derivation::KeyframeManifest { model_tag: "m1" });
        store.write_atomic(&manifest, b"[]").expect("seed manifest");
        let asset = format!("xxh3:{hex}");
        let video_with_manifest = vec![source(&asset, MediaKind::Video, Some("/tmp/v.mov"))];
        let plan_video = plan_work(&video_with_manifest, &store, &full_caps());
        assert!(
            plan_video
                .items
                .iter()
                .any(|i| i.asset == asset && i.kind == WorkKind::Caption),
            "{:?}",
            plan_video.items
        );

        let video_no_manifest = vec![source(
            "xxh3:1bad1bad1bad1bad1bad1bad1bad1bad",
            MediaKind::Video,
            Some("/tmp/v2.mov"),
        )];
        let plan_no_manifest = plan_work(&video_no_manifest, &store, &full_caps());
        assert!(
            !plan_no_manifest
                .items
                .iter()
                .any(|i| i.kind == WorkKind::Caption),
            "waits for the keyframes pass: {:?}",
            plan_no_manifest.items
        );
    }

    #[test]
    fn caption_done_when_caption_and_tags_blobs_both_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let describer_tag = "describe-qwen3-vl-8b";

        let still_hex = "aa22aa22aa22aa22aa22aa22aa22aa22";
        for derivation in [
            Derivation::Caption {
                model_tag: describer_tag,
            },
            Derivation::Tags {
                model_tag: describer_tag,
            },
        ] {
            store
                .write_atomic(&store.path_for(still_hex, &derivation), b"{}")
                .expect("seed still blob");
        }
        let still_asset = format!("xxh3:{still_hex}");

        let video_hex = "bb22bb22bb22bb22bb22bb22bb22bb22";
        let manifest = store.path_for(video_hex, &Derivation::KeyframeManifest { model_tag: "m1" });
        store.write_atomic(&manifest, b"[]").expect("seed manifest");
        for derivation in [
            Derivation::Captions {
                model_tag: describer_tag,
            },
            Derivation::Tags {
                model_tag: describer_tag,
            },
        ] {
            store
                .write_atomic(&store.path_for(video_hex, &derivation), b"{}")
                .expect("seed video blob");
        }
        let video_asset = format!("xxh3:{video_hex}");

        let sources = vec![
            source(&still_asset, MediaKind::Image, Some("/tmp/i.jpg")),
            source(&video_asset, MediaKind::Video, Some("/tmp/v.mov")),
        ];
        let plan = plan_work(&sources, &store, &full_caps());

        assert_eq!(plan.captions.done, 2, "{:?}", plan.items);
        assert!(!plan.items.iter().any(|i| i.kind == WorkKind::Caption));
    }

    /// The partial-item invariant: a caption derivation blob WITHOUT its
    /// tags blob means a run that died (or hit a backend failure) between
    /// the two writes — the item must re-plan as pending, never count done,
    /// or the tags blob stays missing forever. Pins both the still and the
    /// video shape of the two-blob done check.
    #[test]
    fn caption_blob_without_tags_blob_is_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let describer_tag = "describe-qwen3-vl-8b";

        let still_hex = "cc22cc22cc22cc22cc22cc22cc22cc22";
        let caption_blob = store.path_for(
            still_hex,
            &Derivation::Caption {
                model_tag: describer_tag,
            },
        );
        store
            .write_atomic(&caption_blob, b"{}")
            .expect("seed caption blob");
        let still_asset = format!("xxh3:{still_hex}");

        let video_hex = "dd33dd33dd33dd33dd33dd33dd33dd33";
        let manifest = store.path_for(video_hex, &Derivation::KeyframeManifest { model_tag: "m1" });
        store.write_atomic(&manifest, b"[]").expect("seed manifest");
        let captions_blob = store.path_for(
            video_hex,
            &Derivation::Captions {
                model_tag: describer_tag,
            },
        );
        store
            .write_atomic(&captions_blob, b"{}")
            .expect("seed captions blob");
        let video_asset = format!("xxh3:{video_hex}");

        let sources = vec![
            source(&still_asset, MediaKind::Image, Some("/tmp/i.jpg")),
            source(&video_asset, MediaKind::Video, Some("/tmp/v.mov")),
        ];
        let plan = plan_work(&sources, &store, &full_caps());

        assert_eq!(plan.captions.done, 0, "{:?}", plan.items);
        assert_eq!(plan.captions.pending, 2);
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.kind == WorkKind::Caption)
                .count(),
            2,
            "both half-finished items must re-plan: {:?}",
            plan.items
        );
    }

    #[test]
    fn priority_order_is_thumbs_embeds_keyframes_transcripts_ocr_pdf_captions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = full_caps();

        // A video with a pre-existing keyframe manifest, so its OCR/caption
        // passes have something to react to in this same run (they'd
        // otherwise wait for a keyframes pass that hasn't happened yet).
        let video_with_manifest_hex = "70117011701170117011701170117011";
        let manifest = store.path_for(
            video_with_manifest_hex,
            &Derivation::KeyframeManifest { model_tag: "m1" },
        );
        store.write_atomic(&manifest, b"[]").expect("seed manifest");
        let video_with_manifest_asset = format!("xxh3:{video_with_manifest_hex}");

        // A transcript blob with no chunks yet, so the transcript-embed pass
        // has something to plan in this same run.
        let transcript_hex = "75117511751175117511751175117511";
        let transcript_blob = store.path_for(
            transcript_hex,
            &Derivation::Transcript {
                model_tag: WHISPER_MODEL_TAG,
            },
        );
        store
            .write_atomic(&transcript_blob, b"{}")
            .expect("seed transcript");
        let transcript_asset = format!("xxh3:{transcript_hex}");

        let sources = vec![
            source(
                "xxh3:10a110a110a110a110a110a110a110a1",
                MediaKind::Image,
                Some("/tmp/i.jpg"),
            ),
            source(
                "xxh3:70d170d170d170d170d170d170d170d1",
                MediaKind::Video,
                Some("/tmp/v.mov"),
            ),
            source(
                "xxh3:a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1",
                MediaKind::Audio,
                Some("/tmp/a.m4a"),
            ),
            source(&transcript_asset, MediaKind::Audio, None),
            source(
                &video_with_manifest_asset,
                MediaKind::Video,
                Some("/tmp/vm.mov"),
            ),
            source(
                "xxh3:adf1adf1adf1adf1adf1adf1adf1adf1",
                MediaKind::Pdf,
                Some("/tmp/p.pdf"),
            ),
        ];
        let plan = plan_work(&sources, &store, &caps);

        let first_index = |kind: WorkKind| plan.items.iter().position(|i| i.kind == kind);

        let thumb = first_index(WorkKind::Thumb).expect("a Thumb item");
        let image_embed = first_index(WorkKind::ImageEmbed).expect("an ImageEmbed item");
        let keyframes = first_index(WorkKind::Keyframes).expect("a Keyframes item");
        let transcribe = first_index(WorkKind::Transcribe).expect("a Transcribe item");
        let transcript_embed =
            first_index(WorkKind::TranscriptEmbed).expect("a TranscriptEmbed item");
        let ocr = [WorkKind::OcrImage, WorkKind::OcrKeyframes]
            .into_iter()
            .filter_map(first_index)
            .min()
            .expect("an OCR item");
        let pdf = first_index(WorkKind::PdfText).expect("a PdfText item");
        let caption = first_index(WorkKind::Caption).expect("a Caption item");

        assert!(thumb <= image_embed, "{:?}", plan.items);
        assert!(image_embed <= keyframes, "{:?}", plan.items);
        assert!(keyframes <= transcribe, "{:?}", plan.items);
        assert!(transcribe <= transcript_embed, "{:?}", plan.items);
        assert!(transcript_embed <= ocr, "{:?}", plan.items);
        assert!(ocr <= pdf, "{:?}", plan.items);
        assert!(pdf <= caption, "{:?}", plan.items);
    }
}
