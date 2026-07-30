//! Verified copy engine: streams each source file once, fanning chunks to
//! every destination in parallel, then re-reads each destination
//! independently before renaming it into place. A crash can never leave an
//! unverified file at a final path — failures stay quarantined under their
//! temp name.
use crate::IngestError;
use crate::journal::{Journal, Record};
use crate::plan::{Decision, DedupeMode, IngestPlan, PlannedFile};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One verified destination: files land under `root/subdir/<rel>`.
#[derive(Debug, Clone)]
pub struct DestSpec {
    pub root: PathBuf,
    /// `/`-separated, pre-rendered (PARA dir + template), safe-relative.
    pub subdir: String,
}

pub struct EngineConfig {
    pub jobs: usize,
}

/// Destination write handle; `finish` must not return until bytes are
/// durable (fsync) — read-back verification is only meaningful after it.
pub trait Sink: Write + Send {
    /// Flushes and durably persists everything written so far.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying flush or fsync.
    fn finish(&mut self) -> std::io::Result<()>;
}

/// Opens destination sinks. The seam fault-injection tests wrap.
pub trait SinkFactory: Sync {
    /// Opens `path` (typically a `.maj-partial-*` temp name) for writing.
    ///
    /// # Errors
    /// Returns any I/O error from creating the parent directory or file.
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>>;
}

/// Real filesystem sinks: create the parent directory, create the temp
/// file, and on `finish` flush and fsync so a read-back afterward observes
/// durable bytes rather than whatever the OS still has buffered.
pub struct RealSinks;

struct FileSink(std::fs::File);

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Sink for FileSink {
    fn finish(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        self.0.sync_all()
    }
}

impl SinkFactory for RealSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        Ok(Box::new(FileSink(file)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub rel: String,
    pub size: u64,
    pub xxh3: String,
    pub xxh64: String,
    /// Final path under each destination root, `/`-separated relative.
    pub dest_rel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFile {
    pub rel: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub placed: Vec<PlacedFile>,
    pub failed: Vec<FailedFile>,
    pub skipped_duplicates: Vec<String>,
    pub rejected: Vec<FailedFile>,
    pub skipped_resumed: usize,
    /// Internal engine notes (currently: recovered lock poisoning) that are
    /// not about any specific file. Kept separate from `failed` so a
    /// consumer building per-file records (e.g. Task 7's
    /// `VerificationRecorded` events) never mistakes an engine-internal
    /// note for a real file failure.
    pub diagnostics: Vec<String>,
}

/// Runs the copy/verify/place pipeline for every file in `plan` not already
/// resumed or skipped, fanning each source read to every destination.
///
/// # Errors
/// Returns `IngestError` only for journal I/O failures — a checkpoint that
/// cannot be written makes the run's resume state untrustworthy, so it
/// aborts rather than continuing blind. Every per-file copy or verification
/// problem is recorded in the returned `Outcome` instead.
pub fn run(
    plan: &IngestPlan,
    dests: &[DestSpec],
    resume: &BTreeSet<String>,
    journal: &mut Journal,
    sinks: &dyn SinkFactory,
    config: &EngineConfig,
) -> Result<Outcome, IngestError> {
    check_at_least_one_dest(dests)?;
    check_shared_subdir(dests)?;
    let (queue, mut outcome) = partition_plan(plan, resume);
    let (mut placed, mut failed, diagnostics) =
        run_workers(queue, dests, journal, sinks, config.jobs)?;
    sweep_missing(dests, &mut placed, &mut failed, journal)?;
    placed.sort_by(|a, b| a.rel.cmp(&b.rel));
    failed.sort_by(|a, b| a.rel.cmp(&b.rel));
    outcome.placed = placed;
    outcome.failed = failed;
    outcome.diagnostics = diagnostics;
    Ok(outcome)
}

/// A run needs somewhere to put files; an empty destination list would
/// silently do nothing (every file "succeeds" at zero places), which is
/// worse than a loud error.
///
/// # Errors
/// Returns `IngestError::NoDestinations` if `dests` is empty.
fn check_at_least_one_dest(dests: &[DestSpec]) -> Result<(), IngestError> {
    if dests.is_empty() {
        return Err(IngestError::NoDestinations);
    }
    Ok(())
}

/// `dest_rel_for` and `sweep_missing` both assume every destination in a
/// run shares one subdir (the caller renders it once from the PARA layout
/// template). Task 7 is expected to always satisfy this, but a silent
/// violation would produce confidently wrong paths rather than a visible
/// error, so it is checked up front instead.
///
/// # Errors
/// Returns `IngestError::MismatchedSubdirs` if any two destinations differ.
fn check_shared_subdir(dests: &[DestSpec]) -> Result<(), IngestError> {
    let Some(first) = dests.first() else {
        return Ok(());
    };
    for dest in &dests[1..] {
        if dest.subdir != first.subdir {
            return Err(IngestError::MismatchedSubdirs {
                first: first.subdir.clone(),
                other: dest.subdir.clone(),
            });
        }
    }
    Ok(())
}

/// Splits `plan.files` into work the pool must copy and the parts of
/// `Outcome` that are already known without touching a file: rejections
/// from planning, dedupe skips, and files a previous run already placed.
fn partition_plan(
    plan: &IngestPlan,
    resume: &BTreeSet<String>,
) -> (VecDeque<PlannedFile>, Outcome) {
    let mut queue = VecDeque::new();
    let mut outcome = Outcome::default();
    for pf in &plan.files {
        match &pf.decision {
            Decision::Rejected { reason } => outcome.rejected.push(FailedFile {
                rel: pf.rel.clone(),
                reason: reason.clone(),
            }),
            Decision::Duplicate {
                action: DedupeMode::Skip,
                ..
            } => outcome.skipped_duplicates.push(pf.rel.clone()),
            // CopyAnyway and Link both copy bytes here; Link's hard-link
            // semantics (skip the byte copy, hard-link instead) is Task 7's
            // wiring concern once a real CLI mode selects it.
            Decision::Duplicate {
                action: DedupeMode::CopyAnyway | DedupeMode::Link,
                ..
            }
            | Decision::Copy => {
                if resume.contains(&pf.rel) {
                    outcome.skipped_resumed += 1;
                } else {
                    queue.push_back(pf.clone());
                }
            }
        }
    }
    (queue, outcome)
}

/// End-of-run missing-file sweep: a rename that "succeeded" onto a drive
/// that was later yanked, or a destination whose parent directory got
/// tampered with, must not stay a silent success. Every file this run
/// believes it placed is checked at every destination; a miss demotes it
/// to failed and is journaled as `FileFailed` so the journal and `Outcome`
/// agree — otherwise a resumed run would still see the stale `FilePlaced`
/// record and skip re-copying a file that is, in fact, gone.
///
/// # Errors
/// Returns `IngestError` only if the journal write for a demotion fails.
fn sweep_missing(
    dests: &[DestSpec],
    placed: &mut Vec<PlacedFile>,
    failed: &mut Vec<FailedFile>,
    journal: &mut Journal,
) -> Result<(), IngestError> {
    let mut still_placed = Vec::with_capacity(placed.len());
    for file in placed.drain(..) {
        let missing_roots: Vec<String> = dests
            .iter()
            .filter(|dest| !dest.root.join(&file.dest_rel).is_file())
            .map(|dest| dest.root.display().to_string())
            .collect();
        if missing_roots.is_empty() {
            still_placed.push(file);
        } else {
            let reason = format!(
                "placed but missing at end-of-run sweep: {}",
                missing_roots.join(", ")
            );
            journal.append(&Record::FileFailed {
                rel: file.rel.clone(),
                reason: reason.clone(),
            })?;
            failed.push(FailedFile {
                rel: file.rel.clone(),
                reason,
            });
        }
    }
    *placed = still_placed;
    Ok(())
}

/// Collects diagnostic notes raised by recovered lock poisoning so they are
/// never silently dropped, without making a poisoned lock fatal to the run.
struct LockDiagnostics {
    notes: Mutex<Vec<String>>,
}

impl LockDiagnostics {
    fn new() -> Self {
        Self {
            notes: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, note: String) {
        // If this mutex itself were poisoned there would be nowhere further
        // to report to, so recover unconditionally here — this is the base
        // case, not a pattern to repeat elsewhere.
        let mut notes = self
            .notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        notes.push(note);
    }

    fn drain(self) -> Vec<String> {
        self.notes
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Locks `mutex`, recovering from poisoning instead of panicking. A
/// poisoned std `Mutex` means some other worker thread panicked while
/// holding it; our critical sections never panic, so the guarded data is
/// left consistent and safe to keep using. We still don't want that fact
/// hidden, so the recovery is noted in `diagnostics` for the caller to
/// surface later rather than silently swallowed.
fn lock_or_note<'a, T>(
    mutex: &'a Mutex<T>,
    diagnostics: &LockDiagnostics,
    what: &str,
) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            diagnostics.record(format!(
                "{what} lock poisoned — a worker thread panicked earlier; continuing with recovered state"
            ));
            poisoned.into_inner()
        }
    }
}

/// Shared state one worker thread needs. Bundled into one reference so
/// `worker_loop` and `copy_one` take a single parameter instead of the half
/// dozen individually threaded locks this would otherwise require.
struct WorkerContext<'a> {
    queue: &'a Mutex<VecDeque<PlannedFile>>,
    results: &'a Mutex<(Vec<PlacedFile>, Vec<FailedFile>)>,
    journal: &'a Mutex<&'a mut Journal>,
    dests: &'a [DestSpec],
    sinks: &'a dyn SinkFactory,
    diagnostics: &'a LockDiagnostics,
    abort: &'a Mutex<Option<IngestError>>,
}

/// What the worker pool produced: placed files, failed files, and any
/// lock-poisoning diagnostics (kept separate from `failed` — see
/// `Outcome::diagnostics`).
type WorkerOutput = (Vec<PlacedFile>, Vec<FailedFile>, Vec<String>);

/// Runs `jobs` worker threads over `queue`, each pulling one file at a time
/// until the queue is empty or a journal I/O error asks everyone to stop.
fn run_workers(
    queue: VecDeque<PlannedFile>,
    dests: &[DestSpec],
    journal: &mut Journal,
    sinks: &dyn SinkFactory,
    jobs: usize,
) -> Result<WorkerOutput, IngestError> {
    let queue = Mutex::new(queue);
    let results: Mutex<(Vec<PlacedFile>, Vec<FailedFile>)> = Mutex::new((Vec::new(), Vec::new()));
    let journal_mutex = Mutex::new(journal);
    let abort: Mutex<Option<IngestError>> = Mutex::new(None);
    let diagnostics = LockDiagnostics::new();
    let ctx = WorkerContext {
        queue: &queue,
        results: &results,
        journal: &journal_mutex,
        dests,
        sinks,
        diagnostics: &diagnostics,
        abort: &abort,
    };

    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| worker_loop(&ctx));
        }
    });

    if let Some(err) = abort
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        return Err(err);
    }
    let (placed, failed) = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok((placed, failed, diagnostics.drain()))
}

fn worker_loop(ctx: &WorkerContext<'_>) {
    loop {
        if lock_or_note(ctx.abort, ctx.diagnostics, "abort").is_some() {
            break;
        }
        let next = {
            let mut guard = lock_or_note(ctx.queue, ctx.diagnostics, "queue");
            guard.pop_front()
        };
        let Some(pf) = next else { break };
        match copy_one(&pf, ctx) {
            Ok(placed) => push_result(ctx, Ok(placed)),
            Err(CopyOutcome::Failed(failed)) => push_result(ctx, Err(failed)),
            Err(CopyOutcome::Abort(err)) => {
                let mut guard = lock_or_note(ctx.abort, ctx.diagnostics, "abort");
                if guard.is_none() {
                    *guard = Some(err);
                }
                break;
            }
        }
    }
}

fn push_result(ctx: &WorkerContext<'_>, result: Result<PlacedFile, FailedFile>) {
    let mut guard = lock_or_note(ctx.results, ctx.diagnostics, "results");
    match result {
        Ok(placed) => guard.0.push(placed),
        Err(failed) => guard.1.push(failed),
    }
}

/// A per-file copy either fails (recorded in the `Outcome`) or must abort
/// the whole run (a journal write failed — resume state can no longer be
/// trusted).
enum CopyOutcome {
    Failed(FailedFile),
    Abort(IngestError),
}

fn append_or_abort(ctx: &WorkerContext<'_>, record: &Record) -> Result<(), CopyOutcome> {
    let mut guard = lock_or_note(ctx.journal, ctx.diagnostics, "journal");
    guard.append(record).map_err(CopyOutcome::Abort)
}

fn fail(ctx: &WorkerContext<'_>, rel: &str, reason: String) -> Result<PlacedFile, CopyOutcome> {
    append_or_abort(
        ctx,
        &Record::FileFailed {
            rel: rel.to_string(),
            reason: reason.clone(),
        },
    )?;
    Err(CopyOutcome::Failed(FailedFile {
        rel: rel.to_string(),
        reason,
    }))
}

/// Copies one file: journal the plan, stream the source once into every
/// destination's temp file, verify each destination independently, and
/// journal the outcome. Per-destination failures are isolated in
/// `verify_and_place`; only a journal I/O error escalates to `Abort`.
fn copy_one(pf: &PlannedFile, ctx: &WorkerContext<'_>) -> Result<PlacedFile, CopyOutcome> {
    append_or_abort(ctx, &Record::FilePlanned { file: pf.clone() })?;

    let token = ulid::Ulid::generate().to_string();
    let mut attempts = open_sinks(ctx.dests, &pf.rel, &token, ctx.sinks);

    let stream = match stream_to_sinks(&pf.source, &mut attempts) {
        Ok(stream) => stream,
        Err(reason) => return fail(ctx, &pf.rel, reason),
    };

    if let Some(prehash) = &pf.prehash
        && *prehash != stream.xxh3_hex
    {
        // Every destination already has a full (if now-stale) copy on disk
        // under its temp name at this point — the hint applies here too.
        let reason = format!(
            "source changed between planning and copy: expected xxh3 {prehash}, computed {}; \
             delete the partial(s) and re-run",
            stream.xxh3_hex
        );
        return fail(ctx, &pf.rel, reason);
    }

    append_or_abort(
        ctx,
        &Record::FileCopied {
            rel: pf.rel.clone(),
        },
    )?;

    let failures = verify_and_place(&mut attempts, &stream.xxh64_hex);
    if failures.is_empty() {
        append_or_abort(
            ctx,
            &Record::FileVerified {
                rel: pf.rel.clone(),
            },
        )?;
        append_or_abort(
            ctx,
            &Record::FilePlaced {
                rel: pf.rel.clone(),
            },
        )?;
        return Ok(PlacedFile {
            rel: pf.rel.clone(),
            size: stream.size,
            xxh3: stream.xxh3_hex,
            xxh64: stream.xxh64_hex,
            dest_rel: dest_rel_for(ctx.dests, &pf.rel),
        });
    }

    // Each failure string already carries its own hint where a partial
    // genuinely remains (read-back mismatch); sink-open/write/finish
    // failures and read-back I/O errors have no confirmed partial to point
    // at, so no blanket suffix is added here.
    fail(ctx, &pf.rel, failures.join("; "))
}

fn dest_rel_for(dests: &[DestSpec], rel: &str) -> String {
    // All destinations in one run share a single subdir (the caller renders
    // it once from the PARA layout + template), so the placed record only
    // needs to store it once.
    dests
        .first()
        .map_or_else(|| rel.to_string(), |dest| format!("{}/{rel}", dest.subdir))
}

/// One destination's in-flight temp file: where it will end up, where it
/// currently lives, and its open sink (or the reason it has none).
struct DestAttempt {
    root: PathBuf,
    final_path: PathBuf,
    temp_path: PathBuf,
    sink: Option<Box<dyn Sink>>,
    error: Option<String>,
}

/// `root/subdir/rel` is the final path; the temp path lives beside it under
/// `.maj-partial-<token>-<name>` so a crash mid-copy never leaves an
/// unverified file under its real name.
fn dest_paths(dest: &DestSpec, rel: &str, token: &str) -> (PathBuf, PathBuf) {
    let final_path = dest.root.join(&dest.subdir).join(rel);
    let file_name = final_path
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let temp_path = final_path.with_file_name(format!(".maj-partial-{token}-{file_name}"));
    (final_path, temp_path)
}

fn open_sinks(
    dests: &[DestSpec],
    rel: &str,
    token: &str,
    sinks: &dyn SinkFactory,
) -> Vec<DestAttempt> {
    dests
        .iter()
        .map(|dest| {
            let (final_path, temp_path) = dest_paths(dest, rel, token);
            match sinks.open(&temp_path) {
                Ok(sink) => DestAttempt {
                    root: dest.root.clone(),
                    final_path,
                    temp_path,
                    sink: Some(sink),
                    error: None,
                },
                Err(err) => DestAttempt {
                    root: dest.root.clone(),
                    final_path,
                    temp_path: temp_path.clone(),
                    sink: None,
                    error: Some(format!(
                        "opening destination {}: {err}",
                        temp_path.display()
                    )),
                },
            }
        })
        .collect()
}

struct StreamResult {
    xxh64_hex: String,
    xxh3_hex: String,
    size: u64,
}

/// Streams `source` once with a 1 MiB buffer, updating an xxh64 and an
/// xxh3-128 hasher over the source bytes and fanning each chunk out to
/// every still-open destination sink. A write or finish failure on one
/// destination marks that destination failed and stops writing to it, but
/// does not stop the source read — the other destinations still need the
/// rest of the bytes.
fn stream_to_sinks(source: &Path, attempts: &mut [DestAttempt]) -> Result<StreamResult, String> {
    let file = std::fs::File::open(source)
        .map_err(|err| format!("reading {}: {err}", source.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut xxh64 = xxhash_rust::xxh64::Xxh64::new(0);
    let mut xxh3 = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    let mut size: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|err| format!("reading {}: {err}", source.display()))?;
        if n == 0 {
            break;
        }
        xxh64.update(&buf[..n]);
        xxh3.update(&buf[..n]);
        size += n as u64;
        write_chunk_to_open_sinks(attempts, &buf[..n]);
    }
    finish_open_sinks(attempts);
    Ok(StreamResult {
        xxh64_hex: format!("{:016x}", xxh64.digest()),
        xxh3_hex: format!("{:032x}", xxh3.digest128()),
        size,
    })
}

fn write_chunk_to_open_sinks(attempts: &mut [DestAttempt], chunk: &[u8]) {
    for attempt in attempts.iter_mut() {
        if let Some(sink) = attempt.sink.as_mut()
            && let Err(err) = sink.write_all(chunk)
        {
            attempt.error = Some(format!("writing {}: {err}", attempt.temp_path.display()));
            attempt.sink = None;
        }
    }
}

fn finish_open_sinks(attempts: &mut [DestAttempt]) {
    for attempt in attempts.iter_mut() {
        if let Some(sink) = attempt.sink.as_mut()
            && let Err(err) = sink.finish()
        {
            attempt.error = Some(format!("finishing {}: {err}", attempt.temp_path.display()));
            attempt.sink = None;
        }
    }
}

/// Re-reads each destination's temp file independently and compares its
/// xxh64 against the source's. Only a destination whose bytes match gets
/// renamed into place; a mismatch or read-back error is recorded as that
/// destination's failure and its temp file is left quarantined under its
/// partial name. One destination's failure never blocks another's rename.
fn verify_and_place(attempts: &mut [DestAttempt], expected_xxh64_hex: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for attempt in attempts.iter_mut() {
        if attempt.sink.is_none() {
            if let Some(err) = &attempt.error {
                failures.push(format!("{}: {err}", attempt.root.display()));
            }
            continue;
        }
        match readback_xxh64(&attempt.temp_path) {
            Ok(hex) if hex == expected_xxh64_hex => {
                if let Err(err) = std::fs::rename(&attempt.temp_path, &attempt.final_path) {
                    failures.push(format!(
                        "{}: renaming into place: {err}",
                        attempt.root.display()
                    ));
                }
            }
            Ok(hex) => {
                // A partial genuinely remains at `temp_path`, quarantined
                // under its `.maj-partial-` name — the hint applies.
                failures.push(format!(
                    "{}: read-back mismatch (expected {expected_xxh64_hex}, got {hex}); \
                     partial kept at {}; delete it and re-run to retry this destination",
                    attempt.root.display(),
                    attempt.temp_path.display()
                ));
            }
            Err(err) => {
                failures.push(format!(
                    "{}: read-back failed: {err}",
                    attempt.root.display()
                ));
            }
        }
    }
    failures
}

fn readback_xxh64(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = xxhash_rust::xxh64::Xxh64::new(0);
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differing_subdirs_across_destinations_are_rejected() {
        let dests = vec![
            DestSpec {
                root: PathBuf::from("/d1"),
                subdir: "Projects/x/day1".into(),
            },
            DestSpec {
                root: PathBuf::from("/d2"),
                subdir: "Projects/x/day2".into(),
            },
        ];
        assert!(check_shared_subdir(&dests).is_err());
    }

    #[test]
    fn no_destinations_is_rejected() {
        assert!(check_at_least_one_dest(&[]).is_err());
    }
}
