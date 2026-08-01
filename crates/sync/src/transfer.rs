//! Stateless set-union transfer between two sync roots (`events/` +
//! `blobs/`). Every plan is a fresh diff of real files — no cached sync
//! state anywhere, so an interrupted transfer converges on the next run by
//! construction (the same diff-as-queue shape as `maj index run`).
//!
//! Rules: segments are append-only with a single appender, so a shorter
//! copy is a strict prefix — transfer longer-wins, whole-file, via
//! temp+rename (atomic; a race between two pushers leaves one complete
//! valid file and the next sync restores any missing tail). Blobs are
//! immutable and derivation-keyed — presence is the diff, a size mismatch
//! is a torn copy from some non-atomic tool and is re-copied. Nothing is
//! ever deleted or truncated, in either direction.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("sync transfer io at {}: {source} — check the location is accessible", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl TransferError {
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Priority classes, in transfer order: an interrupted first sync should
/// leave the destination browsable (thumbs, then the small JSON) before it
/// is semantically searchable (vectors) or transcript-complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlobClass {
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

impl BlobClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thumbs => "thumbs",
            Self::Metadata => "metadata",
            Self::Vectors => "vectors",
            Self::Transcripts => "transcripts",
        }
    }
}

/// Classifies a blob file by name. The names are pinned by
/// `crates/index/src/blob.rs::path_for`; anything unrecognized lands in
/// `Metadata` (small JSON) rather than being skipped — sync must move
/// every blob, known shape or not.
#[must_use]
pub fn classify_blob(file_name: &str) -> BlobClass {
    if file_name == "thumb-320.webp" {
        return BlobClass::Thumbs;
    }
    if file_name == "transcript.json.zst" {
        return BlobClass::Transcripts;
    }
    if file_name.ends_with(".f32le.zst") {
        return BlobClass::Vectors;
    }
    BlobClass::Metadata
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentCopy {
    pub machine: String,
    pub segment: String,
    pub src_len: u64,
    /// Destination length before the copy (0 when absent) — the offset new
    /// events start at, used by pull to count what arrived.
    pub dst_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobCopy {
    /// Path relative to `blobs/`.
    pub rel: PathBuf,
    pub class: BlobClass,
    pub size: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub segments: Vec<SegmentCopy>,
    pub blobs: Vec<BlobCopy>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferOutcome {
    pub segments_copied: usize,
    pub segment_bytes: u64,
    pub blobs_copied: usize,
    pub blob_bytes: u64,
    /// `(machine, events)` counted from each copied segment's new byte
    /// range — what a pull reports as "applied N events from M machines".
    pub events_added: Vec<(String, usize)>,
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Temp leftovers older than this are swept at the start of `execute` —
/// young ones may belong to a concurrent pusher and are left alone.
const STALE_TEMP_MS: u128 = 60 * 60 * 1000;

/// Diff `src` against `dst`: segments where the destination is missing or
/// shorter, blobs where the destination is missing or size-mismatched.
/// Blobs come back priority-ordered ([`BlobClass`] then path). A `src`
/// with no `events/` or `blobs/` contributes nothing for that half — a
/// fresh location is a valid, empty peer.
///
/// # Errors
/// Returns [`TransferError::Io`] if a directory that exists can't be read.
pub fn plan_transfer(src: &Path, dst: &Path) -> Result<TransferPlan, TransferError> {
    let mut plan = TransferPlan::default();
    plan_segments(src, dst, &mut plan)?;
    plan_blobs(src, dst, &mut plan)?;
    plan.blobs
        .sort_by(|a, b| (a.class, &a.rel).cmp(&(b.class, &b.rel)));
    Ok(plan)
}

/// DESTINATION-side length only: absent (or unreadable) = 0 = "needs the
/// copy", which is self-correcting — if the file is unreadable rather than
/// absent, the planned copy fails loudly in `execute`. Never use this for
/// SOURCE lengths: a source metadata failure must propagate, or the file
/// silently drops out of the plan (Task 2's review caught exactly this
/// swallow-and-degrade pattern in rotation).
fn dst_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

fn plan_segments(src: &Path, dst: &Path, plan: &mut TransferPlan) -> Result<(), TransferError> {
    let events = src.join("events");
    let Ok(machines) = std::fs::read_dir(&events) else {
        return Ok(());
    };
    for machine in machines {
        let machine = machine.map_err(TransferError::io(&events))?;
        let machine_path = machine.path();
        let is_dir = machine
            .file_type()
            .map_err(TransferError::io(&machine_path))?
            .is_dir();
        if !is_dir {
            continue;
        }
        let machine_name = machine.file_name().to_string_lossy().into_owned();
        let entries = std::fs::read_dir(&machine_path).map_err(TransferError::io(&machine_path))?;
        for entry in entries {
            let entry = entry.map_err(TransferError::io(&machine_path))?;
            let path = entry.path();
            let is_seg = entry
                .file_type()
                .map_err(TransferError::io(&path))?
                .is_file()
                && path.extension().is_some_and(|x| x == "jsonl");
            if !is_seg {
                continue;
            }
            let segment = entry.file_name().to_string_lossy().into_owned();
            let src_len = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_len = dst_len(&dst.join("events").join(&machine_name).join(&segment));
            if src_len > dst_len {
                plan.segments.push(SegmentCopy {
                    machine: machine_name.clone(),
                    segment,
                    src_len,
                    dst_len,
                });
            }
        }
    }
    plan.segments
        .sort_by(|a, b| (&a.machine, &a.segment).cmp(&(&b.machine, &b.segment)));
    Ok(())
}

/// Recursive walk of `blobs/` collecting files as `blobs/`-relative paths.
/// The destination's `tmp/` staging dir never appears because temp files
/// live under `<root>/tmp`, a sibling of `blobs/`, not inside it.
fn plan_blobs(src: &Path, dst: &Path, plan: &mut TransferPlan) -> Result<(), TransferError> {
    let src_blobs = src.join("blobs");
    let dst_blobs = dst.join("blobs");
    let mut stack = vec![src_blobs.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue; // absent tree half — a fresh location
        };
        for entry in entries {
            let entry = entry.map_err(TransferError::io(&dir))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(TransferError::io(&path))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&src_blobs) else {
                continue;
            };
            let size = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_path = dst_blobs.join(rel);
            if !dst_path.is_file() || dst_len(&dst_path) != size {
                let name = entry.file_name().to_string_lossy().into_owned();
                plan.blobs.push(BlobCopy {
                    rel: rel.to_path_buf(),
                    class: classify_blob(&name),
                    size,
                });
            }
        }
    }
    Ok(())
}

/// Copies everything in `plan` from `src` to `dst` via `<dst>/tmp` staging
/// and an atomic rename, sweeping stale temp leftovers first. Segment
/// copies do not re-check the destination length right before renaming: a
/// concurrent pusher racing us leaves one complete valid file either way,
/// and the next sync restores any missing tail (see the module doc).
/// Re-checking would only narrow, never close, the window.
///
/// # Errors
/// Returns [`TransferError::Io`] on any staging, copy, or rename failure.
pub fn execute(
    src: &Path,
    dst: &Path,
    plan: &TransferPlan,
) -> Result<TransferOutcome, TransferError> {
    let staging = dst.join("tmp");
    std::fs::create_dir_all(&staging).map_err(TransferError::io(&staging))?;
    sweep_stale_temps(&staging);
    let mut outcome = TransferOutcome::default();
    for seg in &plan.segments {
        let from = src.join("events").join(&seg.machine).join(&seg.segment);
        let to = dst.join("events").join(&seg.machine).join(&seg.segment);
        copy_via_temp(&from, &to, &staging)?;
        outcome.segments_copied += 1;
        outcome.segment_bytes += seg.src_len.saturating_sub(seg.dst_len);
        let mut count = 0usize;
        let (events, _) = crate::FileEventLog::read_segment_since(&from, seg.dst_len, |_| {})
            .map_err(|e| match e {
                crate::LogError::Io { path, source } => TransferError::Io { path, source },
                other => TransferError::Io {
                    path: from.clone(),
                    source: std::io::Error::other(other.to_string()),
                },
            })?;
        count += events.len();
        if count > 0 {
            outcome.events_added.push((seg.machine.clone(), count));
        }
    }
    for blob in &plan.blobs {
        let from = src.join("blobs").join(&blob.rel);
        let to = dst.join("blobs").join(&blob.rel);
        copy_via_temp(&from, &to, &staging)?;
        outcome.blobs_copied += 1;
        outcome.blob_bytes += blob.size;
    }
    Ok(outcome)
}

/// Best-effort: a leftover that can't be inspected or removed is skipped,
/// never fatal — it is invisible to planning either way.
fn sweep_stale_temps(staging: &Path) {
    let Ok(entries) = std::fs::read_dir(staging) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let age_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(0, |d| d.as_millis());
        if age_ms > STALE_TEMP_MS {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn copy_via_temp(from: &Path, to: &Path, staging: &Path) -> Result<(), TransferError> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = staging.join(format!("{}-{seq}.part", std::process::id()));
    std::fs::copy(from, &tmp).map_err(TransferError::io(from))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(TransferError::io(parent))?;
    }
    std::fs::rename(&tmp, to).map_err(TransferError::io(to))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileEventLog;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};

    fn ev(n: u64) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(n, u128::from(n))),
            hlc: Hlc {
                wall_ms: n,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: AssetId("xxh3:aa".into()),
                tag: "t".into(),
            },
        }
    }

    fn write_blob(root: &std::path::Path, rel: &str, bytes: &[u8]) {
        let path = root.join("blobs").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write blob");
    }

    #[test]
    fn classify_covers_every_blob_shape() {
        assert_eq!(classify_blob("thumb-320.webp"), BlobClass::Thumbs);
        assert_eq!(classify_blob("transcript.json.zst"), BlobClass::Transcripts);
        assert_eq!(classify_blob("image.f32le.zst"), BlobClass::Vectors);
        assert_eq!(classify_blob("kf-1500.f32le.zst"), BlobClass::Vectors);
        assert_eq!(classify_blob("chunk-0.f32le.zst"), BlobClass::Vectors);
        for name in [
            "keyframes.json",
            "image.json.zst",
            "kf-1500.json.zst",
            "text.json.zst",
            "caption.json.zst",
            "captions.json.zst",
            "tags.json.zst",
            "ocr-complete.json",
            "chunks-empty.json",
            "chunks-complete.json",
        ] {
            assert_eq!(classify_blob(name), BlobClass::Metadata, "{name}");
        }
    }

    #[test]
    fn plan_is_priority_ordered_and_execute_converges() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1), ev(2)]).expect("append");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        write_blob(
            src.path(),
            "aa/aahex/siglip2-b16-v1/transcript.json.zst",
            b"t",
        );
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"w");
        write_blob(src.path(), "aa/aahex/siglip2-b16-v1/image.f32le.zst", b"v");
        write_blob(src.path(), "aa/aahex/tags.json.zst", b"j");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(plan.segments.len(), 1);
        let classes: Vec<BlobClass> = plan.blobs.iter().map(|b| b.class).collect();
        assert_eq!(
            classes,
            vec![
                BlobClass::Thumbs,
                BlobClass::Metadata,
                BlobClass::Vectors,
                BlobClass::Transcripts
            ],
            "blob plan must be priority-ordered"
        );

        let outcome = execute(src.path(), dst.path(), &plan).expect("execute");
        assert_eq!(outcome.segments_copied, 1);
        assert_eq!(outcome.blobs_copied, 4);
        let events_new: usize = outcome.events_added.iter().map(|(_, n)| *n).sum();
        assert_eq!(events_new, 2);

        let replan = plan_transfer(src.path(), dst.path()).expect("replan");
        assert!(
            replan.segments.is_empty() && replan.blobs.is_empty(),
            "a second plan after execute must be empty — sync converged"
        );
    }

    #[test]
    fn longer_destination_segment_is_never_truncated() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut src_log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        src_log.append(&[ev(1)]).expect("append");
        let mut dst_log = FileEventLog::init(dst.path(), &MachineId("m1".into())).expect("init");
        dst_log.append(&[ev(1), ev(2)]).expect("append");
        let dst_seg = dst.path().join("events/m1/0001.jsonl");
        let longer = std::fs::metadata(&dst_seg).expect("meta").len();

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert!(
            plan.segments.is_empty(),
            "destination is ahead — nothing to push"
        );
        execute(src.path(), dst.path(), &plan).expect("execute");
        assert_eq!(
            std::fs::metadata(&dst_seg).expect("meta").len(),
            longer,
            "sync must never truncate"
        );
    }

    #[test]
    fn truncated_destination_segment_is_restored_whole() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1), ev(2), ev(3)]).expect("append");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        execute(src.path(), dst.path(), &plan).expect("execute");
        // Sabotage: an external tool truncates the replica.
        let dst_seg = dst.path().join("events/m1/0001.jsonl");
        let full = std::fs::metadata(&dst_seg).expect("meta").len();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&dst_seg)
            .expect("open");
        f.set_len(10).expect("truncate");
        let plan = plan_transfer(src.path(), dst.path()).expect("replan");
        assert_eq!(plan.segments.len(), 1, "shorter replica must be re-planned");
        execute(src.path(), dst.path(), &plan).expect("re-execute");
        assert_eq!(std::fs::metadata(&dst_seg).expect("meta").len(), full);
    }

    #[test]
    fn size_mismatched_blob_is_recopied_and_temp_leftovers_are_ignored() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(src.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"full-content");
        write_blob(dst.path(), "aa/aahex/thumb-320.webp", b"torn");
        // A leftover temp file from a killed sync must not appear in a plan.
        let tmp = dst.path().join("tmp");
        std::fs::create_dir_all(&tmp).expect("mkdir tmp");
        std::fs::write(tmp.join("12345-0.part"), b"junk").expect("write junk");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(plan.blobs.len(), 1, "size mismatch = torn copy = re-copy");
        execute(src.path(), dst.path(), &plan).expect("execute");
        let healed = std::fs::read(dst.path().join("blobs/aa/aahex/thumb-320.webp")).expect("read");
        assert_eq!(healed, b"full-content");
        let plan_back = plan_transfer(dst.path(), src.path()).expect("reverse plan");
        assert!(
            plan_back.blobs.is_empty(),
            "tmp/ leftovers must never be planned as blobs"
        );
    }
}
