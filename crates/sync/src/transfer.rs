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
//!
//! What's silently skipped, by design: a symlinked directory is never
//! descended (a stated non-goal — this is what keeps the walk from
//! looping on a symlink cycle), though a symlinked blob FILE is followed
//! and synced like any other file. Also skipped: any non-`.jsonl` file
//! inside a machine directory, and any file placed directly under
//! `events/` rather than inside a machine subdirectory — neither is a
//! shape this format produces.
//!
//! Partial failure: `execute` attempts every planned file independently
//! and keeps going past a single file's failure — a source that vanished
//! between `plan` and `execute`, a permission error mid-copy — rather
//! than aborting the whole run; see [`TransferOutcome::failures`]. `Err`
//! from `execute` is reserved for failures that make the run meaningless
//! before it starts, such as being unable to create the `<dst>/tmp`
//! staging directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("sync transfer io at {}: {source} — check the location is accessible", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("reading landed events from {}: {source}", path.display())]
    SegmentRead {
        path: PathBuf,
        #[source]
        source: crate::LogError,
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
    /// `(machine, events)` aggregated across every segment copied for that
    /// machine this run — what a pull reports as "applied N events from M
    /// machines". Counts are read back from the DESTINATION file after its
    /// rename, so they reflect exactly what landed, not what `execute`
    /// attempted to send — reading the source instead would risk an
    /// overcount if it kept growing under a concurrent local writer after
    /// the copy completed. A corrupt line within the counted range is
    /// silently dropped from the count (reporting only; the event log's
    /// own torn-tail and bad-line handling is unaffected).
    pub events_added: Vec<(String, usize)>,
    /// `(source path, error display)` for every planned file whose own
    /// copy attempt failed — the source vanished between `plan` and
    /// `execute`, a permission error mid-copy, and so on. `execute` keeps
    /// going past these: every other planned file still gets its own
    /// independent attempt, and because a plan is always a fresh diff, a
    /// later run retries whatever's still missing.
    pub failures: Vec<(PathBuf, String)>,
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Temp leftovers older than this are swept at the start of `execute` —
/// young ones may belong to a concurrent pusher and are left alone.
const STALE_TEMP_MS: u128 = 60 * 60 * 1000;

/// Diff `src` against `dst`: segments where the destination is missing or
/// shorter, blobs where the destination is missing or size-mismatched.
/// Blobs come back priority-ordered ([`BlobClass`] then path). A `src`
/// with no `events/` or `blobs/` directory at all is a valid, empty peer
/// and contributes nothing for that half; any other read failure (a
/// permission error, a stale mount) propagates instead of being treated
/// the same way.
///
/// # Errors
/// Returns [`TransferError::Io`] if a directory exists but can't be read.
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

/// True if `path` names a regular file: either directly (`file_type` says
/// so) or through exactly one symlink hop. A broken symlink, or one that
/// resolves to a directory, is treated as "not a file" — silently, since
/// this only gates inclusion in a diff, never a hard error. See the module
/// doc's symlink policy: files are followed, directories are not.
fn is_effectively_file(file_type: std::fs::FileType, path: &Path) -> bool {
    if file_type.is_file() {
        return true;
    }
    file_type.is_symlink() && std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

fn plan_segments(src: &Path, dst: &Path, plan: &mut TransferPlan) -> Result<(), TransferError> {
    let events = src.join("events");
    let machines = match std::fs::read_dir(&events) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(TransferError::io(&events)(e)),
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
            let file_type = entry.file_type().map_err(TransferError::io(&path))?;
            let is_seg = is_effectively_file(file_type, &path)
                && path.extension().is_some_and(|x| x == "jsonl");
            if !is_seg {
                continue;
            }
            let segment = entry.file_name().to_string_lossy().into_owned();
            let src_len = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_bytes = dst_len(&dst.join("events").join(&machine_name).join(&segment));
            if src_len > dst_bytes {
                plan.segments.push(SegmentCopy {
                    machine: machine_name.clone(),
                    segment,
                    src_len,
                    dst_len: dst_bytes,
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
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // absent tree half — a fresh location
            Err(e) => return Err(TransferError::io(&dir)(e)),
        };
        for entry in entries {
            let entry = entry.map_err(TransferError::io(&dir))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(TransferError::io(&path))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !is_effectively_file(file_type, &path) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&src_blobs) else {
                continue;
            };
            let size = std::fs::metadata(&path)
                .map_err(TransferError::io(&path))?
                .len();
            let dst_path = dst_blobs.join(rel);
            let needs_copy = match std::fs::metadata(&dst_path) {
                Ok(meta) => !meta.is_file() || meta.len() != size,
                Err(_) => true,
            };
            if needs_copy {
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

/// Outcome of copying one planned segment, before it's folded into the
/// run's [`TransferOutcome`].
struct SegmentResult {
    bytes: u64,
    events: usize,
}

/// Copies one planned segment and counts what landed. On any failure —
/// copy or the destination re-read — returns the source path and the
/// error's display text, matching [`TransferOutcome::failures`]'s shape.
fn copy_one_segment(
    src: &Path,
    dst: &Path,
    staging: &Path,
    run_id: ulid::Ulid,
    seg: &SegmentCopy,
) -> Result<SegmentResult, (PathBuf, String)> {
    let from = src.join("events").join(&seg.machine).join(&seg.segment);
    let to = dst.join("events").join(&seg.machine).join(&seg.segment);
    copy_via_temp(&from, &to, staging, run_id).map_err(|e| (from.clone(), e.to_string()))?;
    let events = count_landed_events(&to, seg.dst_len).map_err(|e| (from, e.to_string()))?;
    Ok(SegmentResult {
        bytes: seg.src_len.saturating_sub(seg.dst_len),
        events,
    })
}

/// Counts events newly present in the copied destination segment, from
/// `from_offset` (the destination's length before this copy) to its new
/// end. Reads the DESTINATION, not the source: after `copy_via_temp`'s
/// whole-file replace, `to` is exactly what landed, so counting from it
/// can't overcount even if the source keeps growing under a concurrent
/// local writer after the copy completes — reading the source instead
/// would risk exactly that race.
fn count_landed_events(to: &Path, from_offset: u64) -> Result<usize, TransferError> {
    let (events, _) =
        crate::FileEventLog::read_segment_since(to, from_offset, |_| {}).map_err(|source| {
            TransferError::SegmentRead {
                path: to.to_path_buf(),
                source,
            }
        })?;
    Ok(events.len())
}

/// Copies one planned blob. On failure, returns the source path and the
/// error's display text, matching [`TransferOutcome::failures`]'s shape.
fn copy_one_blob(
    src: &Path,
    dst: &Path,
    staging: &Path,
    run_id: ulid::Ulid,
    blob: &BlobCopy,
) -> Result<u64, (PathBuf, String)> {
    let from = src.join("blobs").join(&blob.rel);
    let to = dst.join("blobs").join(&blob.rel);
    copy_via_temp(&from, &to, staging, run_id).map_err(|e| (from, e.to_string()))?;
    Ok(blob.size)
}

/// Copies everything in `plan` from `src` to `dst` via `<dst>/tmp` staging
/// and an atomic rename, sweeping stale temp leftovers first. Segment
/// copies do not re-check the destination length right before renaming: a
/// concurrent pusher racing us leaves one complete valid file either way,
/// and the next sync restores any missing tail (see the module doc).
/// Re-checking would only narrow, never close, the window.
///
/// A single file's copy failure — a source that vanished between `plan`
/// and `execute`, a permission error mid-copy — is recorded in
/// [`TransferOutcome::failures`] rather than aborting the run: every other
/// planned file still gets its own independent attempt. `Err` is reserved
/// for failures that make the whole run meaningless — today, only being
/// unable to create the `<dst>/tmp` staging directory.
///
/// # Errors
/// Returns [`TransferError::Io`] if the `<dst>/tmp` staging directory
/// can't be created.
pub fn execute(
    src: &Path,
    dst: &Path,
    plan: &TransferPlan,
) -> Result<TransferOutcome, TransferError> {
    let staging = dst.join("tmp");
    std::fs::create_dir_all(&staging).map_err(TransferError::io(&staging))?;
    sweep_stale_temps(&staging);
    let run_id = ulid::Ulid::generate();
    let mut outcome = TransferOutcome::default();
    let mut events_by_machine: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for seg in &plan.segments {
        match copy_one_segment(src, dst, &staging, run_id, seg) {
            Ok(result) => {
                outcome.segments_copied += 1;
                outcome.segment_bytes += result.bytes;
                if result.events > 0 {
                    *events_by_machine.entry(seg.machine.clone()).or_insert(0) += result.events;
                }
            }
            Err(failure) => outcome.failures.push(failure),
        }
    }
    outcome.events_added = events_by_machine.into_iter().collect();

    for blob in &plan.blobs {
        match copy_one_blob(src, dst, &staging, run_id, blob) {
            Ok(size) => {
                outcome.blobs_copied += 1;
                outcome.blob_bytes += size;
            }
            Err(failure) => outcome.failures.push(failure),
        }
    }
    Ok(outcome)
}

/// Best-effort: a leftover that can't be inspected or removed is skipped,
/// never fatal — it is invisible to planning either way. Age is measured
/// against this machine's local clock, not the peer's: a peer whose clock
/// runs more than an hour behind ours could have a live, in-progress temp
/// file swept out from under it. The hour-long margin and the
/// keep-on-unreadable fallback above bound how often that can bite,
/// without eliminating it.
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

/// Copies `from` to `to` via a temp file in `staging` plus an atomic
/// rename, so a reader never observes a partially written destination.
/// `run_id` (minted once per [`execute`] call) plus the OS pid plus a
/// process-local sequence number keep temp names unique even across two
/// machines racing into the same shared `<dst>/tmp` — equal pids on
/// different hosts are common and would otherwise collide. Any failure —
/// copy, mkdir, or rename — removes the temp file rather than leaving it
/// for the next stale-temp sweep to find.
fn copy_via_temp(
    from: &Path,
    to: &Path,
    staging: &Path,
    run_id: ulid::Ulid,
) -> Result<(), TransferError> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = staging.join(format!("{}-{run_id}-{seq}.part", std::process::id()));
    std::fs::copy(from, &tmp)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map_err(TransferError::io(from))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
            .map_err(TransferError::io(parent))?;
    }
    std::fs::rename(&tmp, to)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map_err(TransferError::io(to))?;
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

    /// Sets `path`'s mtime to a fixed date well over an hour in the past, so
    /// [`sweep_stale_temps`]'s age guard treats it as stale. Shells out to
    /// `touch -t` (BSD and GNU agree on this flag's format) rather than
    /// adding a `filetime` dependency for one test.
    fn backdate(path: &std::path::Path) {
        let status = std::process::Command::new("touch")
            .arg("-t")
            .arg("202001010000")
            .arg(path)
            .status()
            .expect("run touch -t");
        assert!(
            status.success(),
            "touch -t must succeed to backdate {path:?}"
        );
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
        assert!(outcome.failures.is_empty());
        let events_new: usize = outcome.events_added.iter().map(|(_, n)| *n).sum();
        assert_eq!(events_new, 2);

        let replan = plan_transfer(src.path(), dst.path()).expect("replan");
        assert!(
            replan.segments.is_empty() && replan.blobs.is_empty(),
            "a second plan after execute must be empty — sync converged"
        );
    }

    #[test]
    fn execute_counts_only_the_new_delta_on_a_second_push() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1), ev(2)]).expect("append");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        execute(src.path(), dst.path(), &plan).expect("first execute");

        // A second push after more events land must count and transfer
        // only the new delta, not the whole (already-replicated) segment.
        log.append(&[ev(3), ev(4), ev(5)]).expect("append more");
        let plan2 = plan_transfer(src.path(), dst.path()).expect("plan2");
        assert_eq!(
            plan2.segments.len(),
            1,
            "the grown segment must be replanned"
        );
        let expected_delta = plan2.segments[0].src_len - plan2.segments[0].dst_len;
        let outcome2 = execute(src.path(), dst.path(), &plan2).expect("execute2");
        assert_eq!(
            outcome2.events_added,
            vec![("m1".to_string(), 3)],
            "events_added must count only the new delta (3), not all 5 events"
        );
        assert_eq!(
            outcome2.segment_bytes, expected_delta,
            "segment_bytes must equal exactly the transferred delta"
        );
    }

    #[test]
    fn events_added_aggregates_multiple_segments_per_machine_into_one_entry() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1)]).expect("append seg1");
        // Force a second segment for the same machine.
        let seg2 = src.path().join("events/m1/0002.jsonl");
        let line = serde_json::to_string(&ev(2)).expect("serialize");
        std::fs::write(&seg2, format!("{line}\n")).expect("write segment 2");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(
            plan.segments.len(),
            2,
            "both segments for m1 must be planned"
        );
        let outcome = execute(src.path(), dst.path(), &plan).expect("execute");
        assert_eq!(
            outcome.events_added,
            vec![("m1".to_string(), 2)],
            "two segments for the same machine must aggregate into one entry"
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

    #[test]
    fn stale_temp_leftovers_are_swept_and_fresh_ones_survive() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let tmp = dst.path().join("tmp");
        std::fs::create_dir_all(&tmp).expect("mkdir tmp");
        let fresh = tmp.join("12345-fresh.part");
        std::fs::write(&fresh, b"junk").expect("write junk");
        let stale = tmp.join("99999-stale.part");
        std::fs::write(&stale, b"stale junk").expect("write stale junk");
        backdate(&stale);

        execute(src.path(), dst.path(), &TransferPlan::default())
            .expect("execute with an empty plan just runs the sweep");

        assert!(
            fresh.exists(),
            "a young temp file may belong to a concurrent pusher and must survive the sweep"
        );
        assert!(
            !stale.exists(),
            "a temp file older than the stale threshold must be swept"
        );
    }

    #[test]
    fn zero_byte_blob_present_at_src_and_absent_at_dst_is_copied() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(src.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        write_blob(src.path(), "aa/aahex/tags.json.zst", b"");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(
            plan.blobs.len(),
            1,
            "a 0-byte blob missing at dst must still be planned — size equality \
             alone (0 == 0) can't distinguish absence from a match"
        );
        execute(src.path(), dst.path(), &plan).expect("execute");
        assert!(
            dst.path().join("blobs/aa/aahex/tags.json.zst").is_file(),
            "the 0-byte blob must be copied to the destination"
        );
    }

    #[test]
    fn segments_are_sorted_by_machine_then_segment() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        // Init "m1" first so directory-iteration order (unspecified) cannot
        // accidentally already match the expected sorted order.
        let mut log_m1 = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init m1");
        log_m1.append(&[ev(1)]).expect("append m1");
        let mut log_m0 = FileEventLog::open(src.path(), &MachineId("m0".into())).expect("open m0");
        log_m0.append(&[ev(2)]).expect("append m0");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        let machines: Vec<&str> = plan.segments.iter().map(|s| s.machine.as_str()).collect();
        assert_eq!(
            machines,
            vec!["m0", "m1"],
            "plan.segments must be sorted machine-then-segment, not left in directory order"
        );
    }

    #[test]
    fn stray_files_are_ignored_by_the_segment_walk() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(src.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1)]).expect("append");
        // A non-.jsonl file inside the machine dir must not be planned.
        std::fs::write(src.path().join("events/m1/.DS_Store"), b"junk").expect("write stray");
        // A file directly under events/, not inside any machine dir, must
        // be skipped entirely — plan_segments only descends one level.
        std::fs::write(src.path().join("events/loose.jsonl"), b"junk").expect("write loose");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(
            plan.segments.len(),
            1,
            "only the real segment must be planned"
        );
        assert_eq!(plan.segments[0].segment, "0001.jsonl");
    }

    #[test]
    #[cfg(unix)]
    fn permission_denied_source_directory_propagates_as_error() {
        use std::os::unix::fs::PermissionsExt;
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"x");
        let subtree = src.path().join("blobs/aa");
        let original_perms = std::fs::metadata(&subtree).expect("meta").permissions();
        std::fs::set_permissions(&subtree, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");

        let result = plan_transfer(src.path(), dst.path());

        // Restore permissions unconditionally so the tempdir can be cleaned
        // up regardless of what the assertion below finds.
        std::fs::set_permissions(&subtree, original_perms).expect("restore perms");

        assert!(
            matches!(result, Err(TransferError::Io { .. })),
            "a permission-denied source subtree must propagate as an error, \
             not be silently treated as an absent (empty) peer: {result:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_blob_file_syncs_but_symlinked_directory_is_not_descended() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(src.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");

        // A symlinked FILE inside the real tree: must be planned and copied.
        let real_file = src.path().join("real-thumb.webp");
        std::fs::write(&real_file, b"real bytes").expect("write real file");
        let linked_path = src.path().join("blobs/aa/aahex/thumb-320.webp");
        std::fs::create_dir_all(linked_path.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&real_file, &linked_path).expect("symlink file");

        // A symlinked DIRECTORY inside the tree: must not be descended,
        // even though it contains a blob that would otherwise be planned.
        let real_dir = src.path().join("real-dir");
        std::fs::create_dir_all(&real_dir).expect("mkdir real dir");
        std::fs::write(real_dir.join("hidden.json"), b"should not sync").expect("write hidden");
        let linked_dir = src.path().join("blobs/bb");
        std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("symlink dir");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        let rels: Vec<String> = plan
            .blobs
            .iter()
            .map(|b| b.rel.to_string_lossy().into_owned())
            .collect();
        assert!(
            rels.iter().any(|r| r.ends_with("thumb-320.webp")),
            "a symlinked blob FILE must be planned: {rels:?}"
        );
        assert!(
            !rels.iter().any(|r| r.contains("hidden.json")),
            "a symlinked blob DIRECTORY must not be descended: {rels:?}"
        );

        execute(src.path(), dst.path(), &plan).expect("execute");
        let copied =
            std::fs::read(dst.path().join("blobs/aa/aahex/thumb-320.webp")).expect("read copied");
        assert_eq!(
            copied, b"real bytes",
            "the symlinked file's content must be copied through the link"
        );
    }

    #[test]
    fn missing_source_blob_between_plan_and_execute_is_recorded_as_a_failure() {
        let src = tempfile::tempdir().expect("tempdir");
        let dst = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(src.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("events")).expect("skeleton");
        std::fs::create_dir_all(dst.path().join("blobs")).expect("skeleton");
        write_blob(src.path(), "aa/aahex/thumb-320.webp", b"thumb");
        write_blob(src.path(), "bb/bbhex/thumb-320.webp", b"thumb2");

        let plan = plan_transfer(src.path(), dst.path()).expect("plan");
        assert_eq!(plan.blobs.len(), 2);

        // Sabotage: one planned source file vanishes before execute runs.
        let vanished = src.path().join("blobs/aa/aahex/thumb-320.webp");
        std::fs::remove_file(&vanished).expect("remove");

        let outcome = execute(src.path(), dst.path(), &plan)
            .expect("execute must not abort on a per-file failure");
        assert_eq!(
            outcome.blobs_copied, 1,
            "the surviving blob must still land"
        );
        assert!(
            dst.path().join("blobs/bb/bbhex/thumb-320.webp").is_file(),
            "the surviving blob must be copied"
        );
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(
            outcome.failures[0].0, vanished,
            "failures must name the missing source path"
        );
    }
}
