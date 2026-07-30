//! The queue IS the diff: work = (assets × required derivations) minus
//! (blobs that exist). Nothing is stored; finished work is self-evident from
//! the blob store, so runs are resumable, idempotent, and self-healing.

use std::path::PathBuf;

use majestical_core::media_kind::MediaKind;

use crate::blob::{BlobStore, Derivation, asset_hex};

/// Extensions we know we cannot decode yet: the RAW family, plus AVIF (the
/// `image` crate build we depend on has no AVIF decoder enabled). Planner-
/// level so status is deterministic instead of discovered by failing
/// forever — a scanned `.avif` would otherwise retry every pass under
/// `--watch` with no way to ever succeed.
const UNDECODABLE_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "avif",
];

/// One kind of derivable work a [`WorkItem`] can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    Thumb,
    ImageEmbed,
    Keyframes,
}

/// One unit of pending work: an asset, its readable bytes, and which
/// derivation to produce.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub asset: String,
    pub asset_hex: String,
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
/// (`model_tag`) and video thumbnailing/keyframing (`ffmpeg`) so the planner
/// reports the true reason work can't run yet rather than pretending it's
/// simply pending.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub model_tag: Option<String>,
    pub ffmpeg: bool,
}

/// Counts for one derivation kind across every planned asset. Every asset
/// eligible for the kind lands in exactly one bucket.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KindStatus {
    /// Blob already exists.
    pub done: u64,
    /// Blob missing, bytes reachable, capability available — queued.
    pub pending: u64,
    /// Blob missing and the asset's volume isn't mounted right now.
    pub offline: u64,
    /// Blob missing and the source format can't be decoded (RAW).
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
    /// Priority-ordered: thumbnails, then image embeddings, then keyframes.
    pub items: Vec<WorkItem>,
    pub thumbs: KindStatus,
    pub embeddings: KindStatus,
    pub keyframes: KindStatus,
}

fn is_undecodable(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| UNDECODABLE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Diffs `sources` against `blobs` under `caps`, producing a priority-ordered
/// work queue plus per-kind status counts. Assets whose id isn't `xxh3:`-
/// prefixed or whose kind is [`MediaKind::Other`] are skipped entirely — the
/// planner has no derivation to offer them.
///
/// Three passes over `sources` (rather than one) so `items` comes out
/// globally priority-ordered — every thumbnail before every image embedding
/// before every keyframe set — instead of grouped per asset.
#[must_use]
pub fn plan_work(sources: &[AssetSource], blobs: &BlobStore, caps: &Capabilities) -> WorkPlan {
    let mut plan = WorkPlan::default();
    for source in sources.iter().filter(|s| s.kind != MediaKind::Other) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_thumb(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Image) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_image_embed(source, hex, blobs, caps, &mut plan);
        }
    }
    for source in sources.iter().filter(|s| s.kind == MediaKind::Video) {
        if let Some(hex) = asset_hex(&source.asset) {
            plan_keyframes(source, hex, blobs, caps, &mut plan);
        }
    }
    plan
}

/// THUMBS: blob exists -> done; else offline (no path) / unsupported (RAW
/// ext) / `needs_ffmpeg` (video without ffmpeg) / pending+item, in that order.
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

/// IMAGE EMBED (`MediaKind::Image` only): no model -> `needs_model`; blob
/// exists -> done; else offline/unsupported/pending+item.
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

#[cfg(test)]
mod tests {
    use super::{AssetSource, Capabilities, WorkKind, plan_work};
    use crate::blob::{BlobStore, Derivation};
    use majestical_core::media_kind::MediaKind;

    #[test]
    fn plans_missing_thumbs_and_counts_statuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let caps = Capabilities {
            model_tag: None,
            ffmpeg: false,
        };
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:bb22".into(),
                kind: MediaKind::Image,
                abs_path: None,
            },
            AssetSource {
                asset: "xxh3:cc33".into(),
                kind: MediaKind::Video,
                abs_path: Some("/tmp/c.mov".into()),
            },
            AssetSource {
                asset: "xxh3:dd44".into(),
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
        assert_eq!(plan.items.len(), 1, "only aa11's thumb has bytes ready");
        assert!(matches!(plan.items[0].kind, WorkKind::Thumb));
    }

    #[test]
    fn existing_blobs_count_done_and_raw_images_are_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = BlobStore::new(dir.path());
        let hex = "aa11";
        let thumb = store.path_for(hex, &Derivation::Thumb);
        store.write_atomic(&thumb, b"x").expect("seed thumb");
        let caps = Capabilities {
            model_tag: Some("m1".into()),
            ffmpeg: false,
        };
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:ee55".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/e.cr3".into()),
            },
            AssetSource {
                asset: "xxh3:ff66".into(),
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
        assert_eq!(plan.items.len(), 1, "one ImageEmbed item for aa11");
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
        };
        let sources = vec![
            AssetSource {
                asset: "xxh3:aa11".into(),
                kind: MediaKind::Image,
                abs_path: Some("/tmp/a.png".into()),
            },
            AssetSource {
                asset: "xxh3:cc33".into(),
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
                (WorkKind::Thumb, "xxh3:aa11"),
                (WorkKind::Thumb, "xxh3:cc33"),
                (WorkKind::ImageEmbed, "xxh3:aa11"),
                (WorkKind::Keyframes, "xxh3:cc33"),
            ],
            "both thumbs must precede the embedding, which must precede the keyframes"
        );
    }
}
