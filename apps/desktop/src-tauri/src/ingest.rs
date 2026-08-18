//! The one in-flight ingest run: its managed state, the claim protocol
//! that keeps it to one at a time, the OS thread it copies on, and the
//! throttle its progress passes through on the way to the webview.
//!
//! Split out of `commands.rs` because none of this is a thin wrapper over a
//! services verb — a run outlives the call that started it and the webview
//! that is watching it, which is machinery of this head's own. `commands.rs`
//! keeps the five `#[tauri::command]` one-liners over the `*_impl`
//! functions here, the same shape every other verb in that file has.
use crate::commands::{CatalogCfg, CommandError, open_app};
use majestical_ingest::engine;
use majestical_services::ingest::{IngestPlanOutcome, IngestRun, UnfinishedRunsOutcome};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

/// The ingest layout template a caller gets when it sends none — the same
/// string `maj ingest --template` defaults to and MCP's `ingest_source`
/// fills in, so an unconfigured ingest lands in the same place whichever
/// head asked for it.
pub const DEFAULT_INGEST_TEMPLATE: &str = "{date}/{source-label}";

/// The Tauri event every forwarded [`IngestProgress`] is emitted under.
/// `api.ts` exports the same string for the surface's `listen` call.
pub const INGEST_PROGRESS_EVENT: &str = "ingest-progress";

/// This head plans and copies with `skip`: a file the catalog already knows
/// is left where it is. Hard-coded rather than plumbed, because the Ingest
/// surface offers no dedupe control — it is the same default `maj ingest`
/// and `ingest_source` apply when their own flag is omitted. `plan_ingest`
/// and `start_ingest` must keep using the SAME mode, or the run would copy
/// a different set of files than the plan the operator approved.
const INGEST_DEDUPE: majestical_ingest::plan::DedupeMode =
    majestical_ingest::plan::DedupeMode::Skip;

/// How long a file's `BytesCopied` events are coalesced before another one
/// is forwarded to the webview.
///
/// The engine emits one per 1 MiB copy buffer and deliberately does not
/// coalesce them — `services::ingest::run_ingest`'s doc comment hands that
/// choice to whichever head renders them, which is this one. Forwarding
/// every event would push hundreds of IPC messages a second across the
/// webview bridge to move a bar that repaints 60 times a second at best.
const BYTES_COPIED_MIN_GAP_MS: u64 = 100;

/// The stand-in for a run id in a refusal message, for the window between
/// `start_ingest` claiming the job slot and the run naming itself.
const UNNAMED_RUN: &str = "(starting)";

/// One progress notification as the webview receives it: the event, plus
/// the run it belongs to. The id rides the envelope rather than the event
/// (`engine::ProgressEvent` carries no run id, and a surface that reloaded
/// mid-run needs to know which run it is watching).
#[derive(Debug, Clone, Serialize)]
pub struct IngestProgress {
    pub run_id: String,
    pub event: engine::ProgressEvent,
}

/// Where a run's forwarded progress goes. An `Arc`, not a `&dyn Fn`,
/// because the sink outlives the call that installed it: it is moved into
/// the run's own thread and used for hours. The command wrapper passes a
/// closure over the `AppHandle`; `tests/commands.rs` passes one that
/// collects into a `Vec`.
pub type ProgressSink = Arc<dyn Fn(IngestProgress) + Send + Sync>;

/// Milliseconds since this process first asked — a monotonic stamp for
/// [`BytesThrottle`], which only ever compares two of them.
fn monotonic_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    u64::try_from(
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[derive(Default)]
/// One in-flight file's throttle bookkeeping: the size `FileStarted`
/// announced, and when its last `BytesCopied` was forwarded (`None` until
/// one has been).
struct FileThrottle {
    size: Option<u64>,
    last_ms: Option<u64>,
}

/// Coalesces the engine's `BytesCopied` stream on its way to the webview.
/// Shared by every worker thread in a run, so the bookkeeping is behind a
/// `Mutex` — the clock is injectable so the rule is testable without
/// sleeping.
struct BytesThrottle {
    now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
    /// One entry per file currently being copied, dropped when it lands.
    files: Mutex<BTreeMap<String, FileThrottle>>,
}

impl BytesThrottle {
    fn new() -> Self {
        Self::with_clock(Box::new(monotonic_ms))
    }

    fn with_clock(now_ms: Box<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            now_ms,
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// Whether `event` is forwarded.
    ///
    /// Everything except `BytesCopied` passes unconditionally: there is one
    /// `FileStarted`/`FilePlaced`/`FileFailed` per file and one
    /// `RunStarted`/`RunStopped` per run, and those are what the surface
    /// counts. A `BytesCopied` passes when either
    ///
    /// - it is the file's last (`bytes_done` reached the size `FileStarted`
    ///   announced), so the final count always lands however tight the
    ///   window — a file copied inside one window still finishes its bar; or
    /// - [`BYTES_COPIED_MIN_GAP_MS`] has passed since the last one forwarded
    ///   for that same file. The first is always forwarded (nothing to be
    ///   too soon after), so a slow file's bar starts moving immediately.
    ///
    /// The window is per file, not per run: with several workers copying at
    /// once, one file's chatter must not starve another file's bar.
    fn admit(&self, event: &engine::ProgressEvent) -> bool {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        match event {
            engine::ProgressEvent::FileStarted { rel, size } => {
                files.entry(rel.clone()).or_default().size = Some(*size);
                true
            }
            engine::ProgressEvent::BytesCopied { rel, bytes_done } => {
                let now = (self.now_ms)();
                let file = files.entry(rel.clone()).or_default();
                let complete = file.size == Some(*bytes_done);
                let due = file
                    .last_ms
                    .is_none_or(|last| now.saturating_sub(last) >= BYTES_COPIED_MIN_GAP_MS);
                if complete || due {
                    file.last_ms = Some(now);
                    return true;
                }
                false
            }
            // A finished file will never be spoken of again, so its
            // bookkeeping goes with it — a 200k-file card must not leave
            // 200k entries behind.
            engine::ProgressEvent::FilePlaced { rel }
            | engine::ProgressEvent::FileFailed { rel, .. } => {
                files.remove(rel);
                true
            }
            engine::ProgressEvent::RunStarted { .. }
            | engine::ProgressEvent::FileVerified { .. }
            | engine::ProgressEvent::RunStopped { .. } => true,
        }
    }
}

/// How a run ended, as the surface reads it back after the fact. A failure
/// is a value here, not a lost call: the run outlives the webview, so the
/// error that ended it has to survive a reload too.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FinishedIngest {
    Done { run: IngestRun },
    Failed { error: CommandError },
}

/// The one in-flight ingest (the spec's single-job rule). Every field is
/// shared with the run's own thread, so the whole job clones by `Arc`.
#[derive(Clone)]
pub struct IngestJob {
    /// The run's id, filled exactly once by its first notice line. A
    /// `OnceLock` rather than a `String` because the slot is claimed before
    /// the id exists: `run_ingest` mints it, and claiming only afterwards
    /// would let two starts race past the single-job check.
    pub run_id: Arc<OnceLock<String>>,
    /// This job's OWN cancel flag — never `services::ingest::silent_control`'s,
    /// whose flag is a process-wide static: one store into that would
    /// permanently cancel every later silent run in this process.
    pub cancel: Arc<engine::CancelFlag>,
    /// `Some` once the run's thread is done, whatever the outcome.
    pub finished: Arc<Mutex<Option<Arc<FinishedIngest>>>>,
}

impl IngestJob {
    fn new() -> Self {
        Self {
            run_id: Arc::new(OnceLock::new()),
            cancel: Arc::new(engine::CancelFlag::new(false)),
            finished: Arc::new(Mutex::new(None)),
        }
    }

    fn outcome(&self) -> Option<Arc<FinishedIngest>> {
        self.finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Managed state: the last ingest job started, still running or not.
/// `None` until the first one.
pub struct IngestState(pub RwLock<Option<IngestJob>>);

/// What `ingest_state` answers with. `api.ts` names the same shape
/// `IngestState` — the `Wire` suffix here only keeps it apart from the
/// managed [`IngestState`] it is read from.
#[derive(Debug, Serialize)]
pub struct IngestStateWire {
    /// The in-flight run's id. Absent when nothing is running — and also
    /// for the moment between [`start_ingest_impl`] claiming the slot and
    /// the run naming itself, which is why `busy` exists separately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<String>,
    /// Whether a run holds the slot right now, named yet or not — the
    /// authority on whether Start can be offered.
    pub busy: bool,
    /// The last run to finish, until another one starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished: Option<Arc<FinishedIngest>>,
}

/// One `start_ingest` request. A struct rather than five more positional
/// arguments: the command wrapper builds it by field name, so no
/// transposition of two same-typed paths can slip through.
pub struct StartIngest {
    pub source: PathBuf,
    pub dests: Vec<PathBuf>,
    /// Target PARA node (`<kind>/<name>` or a node id).
    pub para: String,
    /// Layout inside the node; `None` means [`DEFAULT_INGEST_TEMPLATE`].
    pub template: Option<String>,
    /// A previous run's id to continue, from `list_unfinished_ingests`.
    pub resume: Option<String>,
}

impl StartIngest {
    fn template(&self) -> String {
        self.template
            .clone()
            .unwrap_or_else(|| DEFAULT_INGEST_TEMPLATE.to_string())
    }
}

/// Everything the run's thread owns, bundled so the thread body stays
/// inside the house 5-positional-parameter limit.
struct IngestJobArgs {
    cfg: CatalogCfg,
    req: StartIngest,
    emit: ProgressSink,
    job: IngestJob,
    /// Carries the run id — or a failure that arrived before there was one
    /// — back to the `start_ingest` call still waiting to return it.
    started: std::sync::mpsc::Sender<Result<String, CommandError>>,
}

/// The run id in `run_ingest`'s one notice line, `run <id> — resume with:
/// --resume <id>` (`services::ingest::run_ingest_impl`), which it emits
/// right after choosing the id and before any copying. Parsed rather than
/// plumbed: the notice is the only place the services layer publishes the
/// id before the run finishes, and its format is part of that verb's
/// documented contract.
fn run_id_from_notice(line: &str) -> Option<&str> {
    line.strip_prefix("run ")?.split_whitespace().next()
}

/// `maj ingest`'s own first check, mirrored here: a single file walks as a
/// one-entry plan whose `rel` is that file's own name, which would copy it
/// to a destination path nobody asked for. Both ingest paths run this —
/// [`plan_ingest_impl`] and the run thread's own plan pass — so a start
/// that never previewed is guarded too.
fn ensure_ingest_source(source: &Path) -> Result<(), CommandError> {
    if source.is_dir() {
        return Ok(());
    }
    Err(CommandError::new(format!(
        "source must be a directory: {}",
        source.display()
    )))
}

/// Plans an ingest without copying anything: walks `source`, diffs it
/// against every asset the catalog knows, and renders the destination
/// subdir. Destination roots are deliberately not an argument — the plan
/// does not depend on where the bytes will land, only `start_ingest` does.
///
/// # Errors
/// Returns an error if no catalog is selected, `source` is not a directory,
/// `para` doesn't resolve to an active PARA node, or the source walk or the
/// template fails.
pub fn plan_ingest_impl(
    cfg: &CatalogCfg,
    source: &Path,
    para: &str,
    template: Option<String>,
) -> Result<IngestPlanOutcome, CommandError> {
    ensure_ingest_source(source)?;
    let app = open_app(cfg)?;
    let template = template.unwrap_or_else(|| DEFAULT_INGEST_TEMPLATE.to_string());
    Ok(majestical_services::ingest::plan(
        &app,
        source,
        para,
        &template,
        INGEST_DEDUPE,
    )?)
}

/// Publishes a fresh job into `state`, or refuses because one is still
/// running. Claiming the slot here — before the run has a name — is what
/// makes the single-job rule race-free: two starts arriving together can
/// never both get past this write lock.
fn claim_ingest_slot(state: &IngestState) -> Result<IngestJob, CommandError> {
    let mut slot = state.0.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(live) = slot.as_ref()
        && live.outcome().is_none()
    {
        return Err(CommandError::new(format!(
            "ingest run {} is still going — stop it or wait for it to finish",
            live.run_id.get().map_or(UNNAMED_RUN, String::as_str)
        )));
    }
    let job = IngestJob::new();
    *slot = Some(job.clone());
    Ok(job)
}

/// Starts a verified copy on its own thread and returns the run's id.
///
/// The run outlives this call, and outlives the webview: it returns as soon
/// as `run_ingest` has named the run — its first notice, emitted before any
/// byte is copied — leaving the copy itself to the thread. What the caller
/// waits through is the planning pass (a walk, and a hash of every file
/// whose size matches something the catalog knows), which is why the
/// command wrapper hands this to the blocking pool.
///
/// # Errors
/// Returns an error if a run is already in flight (naming it), or if this
/// one failed before it was ever named — an unresolvable PARA target, an
/// unreadable source, a `resume` id with no journal. Anything that goes
/// wrong afterwards is not this call's to report: it lands in the job's
/// outcome, where [`ingest_state_impl`] finds it.
pub fn start_ingest_impl(
    cfg: &CatalogCfg,
    state: &IngestState,
    req: StartIngest,
    emit: ProgressSink,
) -> Result<String, CommandError> {
    let job = claim_ingest_slot(state)?;
    let (started, named) = std::sync::mpsc::channel();
    let args = IngestJobArgs {
        cfg: cfg.clone(),
        req,
        emit,
        job,
        started,
    };
    // A plain OS thread, not `tauri::async_runtime::spawn_blocking`: the
    // engine is synchronous and a real ingest runs for hours, which would
    // hold a blocking-pool worker hostage for the whole copy — every other
    // command that pool serves (search, plan, the mount table) would queue
    // behind it. The same "own thread" reasoning `majestical_services::
    // runtime` applies to Lance, for a different reason.
    //
    // `Builder::spawn`, not `thread::spawn`: the latter panics when the OS
    // refuses the thread, and this one is called with the job slot already
    // claimed — a panic here would leave the slot held by a run that never
    // started, refusing every later start for the life of the process. The
    // name shows up in a crash report and in a debugger's thread list.
    let job = args.job.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("ingest-run".to_string())
        .spawn(move || run_ingest_job(&args))
    {
        let error = CommandError::new(format!("could not start the ingest run's thread: {err}"));
        *job.finished.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Arc::new(FinishedIngest::Failed {
                error: error.clone(),
            }));
        return Err(error);
    }
    // The thread's sender is dropped on every exit path it has, so this
    // cannot outlive the thread even if the run never reaches its notice.
    named.recv().unwrap_or_else(|_| {
        Err(CommandError::new(
            "the ingest run ended without naming itself",
        ))
    })
}

/// A panic payload's message, for the two shapes `panic!` produces.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return message;
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.as_str();
    }
    "no message"
}

/// The body of a run's own thread: copy, then publish the outcome into the
/// job's slot — whatever it turned out to be, so the slot always stops
/// looking live.
///
/// A panic counts as "whatever it turned out to be". Without
/// `catch_unwind` a panicking run would unwind past the publish and leave
/// `finished` empty forever: the slot would read `busy` for the life of the
/// process and refuse every later start, which is a wedged app rather than
/// one failed copy. The panic can come from anywhere the engine reaches,
/// including a progress sink of ours running on a worker thread — the
/// engine's `thread::scope` re-raises a worker's panic here.
fn run_ingest_job(args: &IngestJobArgs) {
    // `AssertUnwindSafe`: the shared state this closure touches is either
    // behind a `Mutex` (recovered from poisoning at every use) or write-once
    // (`OnceLock`), so a panic cannot leave a half-updated value visible.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ingest_job_result(args)));
    let finished = match result.unwrap_or_else(|payload| {
        Err(CommandError::new(format!(
            "the ingest run panicked: {}",
            panic_message(payload.as_ref())
        )))
    }) {
        Ok(run) => FinishedIngest::Done { run },
        Err(error) => FinishedIngest::Failed { error },
    };
    let answer = match &finished {
        FinishedIngest::Done { .. } => None,
        FinishedIngest::Failed { error } => Some(error.clone()),
    };
    // Published before the answer goes out, not after: `start_ingest`
    // returns the instant that send lands, and a caller that reads
    // `ingest_state` right afterwards must not find the slot still held by
    // a run that has already ended.
    *args
        .job
        .finished
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(finished));
    // A failure that beat the run's first notice — a panic included — is
    // the answer `start_ingest` is still blocked waiting for; one that came
    // after it lands in the slot alone (this send fails harmlessly once
    // that receiver is gone).
    if let Some(error) = answer {
        let _ = args.started.send(Err(error));
    }
}

/// Plans and runs one ingest, forwarding progress through the throttle and
/// naming the run as soon as it has a name.
fn ingest_job_result(args: &IngestJobArgs) -> Result<IngestRun, CommandError> {
    ensure_ingest_source(&args.req.source)?;
    let mut app = open_app(&args.cfg)?;
    let planned = majestical_services::ingest::plan(
        &app,
        &args.req.source,
        &args.req.para,
        &args.req.template(),
        INGEST_DEDUPE,
    )?;
    let throttle = BytesThrottle::new();
    let emit: &(dyn Fn(IngestProgress) + Send + Sync) = args.emit.as_ref();
    let run_id = &args.job.run_id;
    let progress = move |event: engine::ProgressEvent| {
        // `run_ingest` names the run through `notice` before the engine
        // emits its first event, so this never drops one. Dropping beats
        // the alternative if that ever changes: an event stamped with an
        // empty run id would look to the surface like it belongs to some
        // other run, and correlating it wrongly is worse than not seeing it.
        let Some(run_id) = run_id.get() else {
            return;
        };
        if !throttle.admit(&event) {
            return;
        }
        emit(IngestProgress {
            run_id: run_id.clone(),
            event,
        });
    };
    Ok(majestical_services::ingest::run_ingest(
        &mut app,
        &args.cfg.catalog,
        &majestical_services::ingest::ExecuteIngest {
            plan: &planned.plan,
            source: &args.req.source,
            dest: &args.req.dests,
            subdir: &planned.subdir,
            node_id: &planned.node_id,
            source_volume: (&planned.source_volume_id, &planned.source_volume_label),
            // The services default: CPU cores, capped at 8.
            jobs: None,
            resume: args.req.resume.as_deref(),
            control: &engine::RunControl {
                progress: &progress,
                cancel: args.job.cancel.as_ref(),
            },
        },
        &mut |line: &str| {
            let Some(id) = run_id_from_notice(line) else {
                return;
            };
            let _ = args.job.run_id.set(id.to_string());
            let _ = args.started.send(Ok(id.to_string()));
        },
    )?)
}

/// Asks the in-flight run to stop. Cancellation is cooperative and
/// file-granular (the engine checks between files), so the run ends after
/// whatever is in flight, resumable by its id.
///
/// Idempotent, and silent when nothing is running: the flag belongs to that
/// one job, so setting it twice — or setting a finished job's — changes
/// nothing. The surface can wire Stop unconditionally.
pub fn cancel_ingest_impl(state: &IngestState) {
    if let Some(job) = state
        .0
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        job.cancel.store(true, Ordering::Relaxed);
    }
}

/// What the surface renders on mount, including after a reload mid-run.
///
/// The finished run's own `IngestRun` — never the events a surface
/// accumulated — is the authority on what a run placed: the engine's
/// end-of-run sweep can demote a file it already announced as `FilePlaced`
/// (see `engine::ProgressEvent`'s doc comment), and that demotion is
/// reported in the outcome only, never as a second event.
///
/// `RunStopped` is not that authority either, and does not mean this call
/// will answer with an outcome yet: it says the copy loop ended, and the
/// sweep, the ASC MHL generation per destination, and the catalog events
/// all land after it — seconds later on a big run. A surface that saw
/// `run_stopped` polls until `busy` is false, and reads `finished` then.
#[must_use]
pub fn ingest_state_impl(state: &IngestState) -> IngestStateWire {
    let slot = state.0.read().unwrap_or_else(PoisonError::into_inner);
    let Some(job) = slot.as_ref() else {
        return IngestStateWire {
            running: None,
            busy: false,
            finished: None,
        };
    };
    let finished = job.outcome();
    IngestStateWire {
        running: if finished.is_none() {
            job.run_id.get().cloned()
        } else {
            None
        },
        busy: finished.is_none(),
        finished,
    }
}

/// Every run whose journal shows planned files that never landed, newest
/// first — the resume candidates. Reads this machine's run journals, not
/// the event log.
///
/// # Errors
/// Returns an error if no catalog is selected, or the state directory's
/// `runs/` can't be listed at all (one unreadable journal is a notice on
/// the outcome, not a failure).
pub fn list_unfinished_ingests_impl(
    cfg: &CatalogCfg,
) -> Result<UnfinishedRunsOutcome, CommandError> {
    Ok(majestical_services::ingest::ingest_unfinished(
        &cfg.catalog,
    )?)
}

/// The throttle's clock is injectable only from inside this module, and
/// the notice parser is private to it — everything else this file exposes
/// is driven end to end by `tests/commands.rs` against real catalogs.
#[cfg(test)]
mod tests {
    use super::{BYTES_COPIED_MIN_GAP_MS, BytesThrottle, run_id_from_notice};
    use majestical_ingest::engine::ProgressEvent;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A throttle whose clock is a counter the test moves by hand.
    fn throttle_at(clock: &std::sync::Arc<AtomicU64>) -> BytesThrottle {
        let clock = std::sync::Arc::clone(clock);
        BytesThrottle::with_clock(Box::new(move || clock.load(Ordering::Relaxed)))
    }

    fn bytes(rel: &str, bytes_done: u64) -> ProgressEvent {
        ProgressEvent::BytesCopied {
            rel: rel.to_string(),
            bytes_done,
        }
    }

    /// The rule the webview's bar depends on: within one window a file
    /// forwards its first chunk and then nothing until the window elapses.
    #[test]
    fn the_throttle_drops_bytes_copied_inside_its_window() {
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let throttle = throttle_at(&clock);
        assert!(throttle.admit(&ProgressEvent::FileStarted {
            rel: "a.mov".to_string(),
            size: 10_000,
        }));

        assert!(
            throttle.admit(&bytes("a.mov", 1000)),
            "the first always goes"
        );
        clock.store(BYTES_COPIED_MIN_GAP_MS - 1, Ordering::Relaxed);
        assert!(!throttle.admit(&bytes("a.mov", 2000)), "too soon");
        clock.store(BYTES_COPIED_MIN_GAP_MS, Ordering::Relaxed);
        assert!(throttle.admit(&bytes("a.mov", 3000)), "the window elapsed");
        assert!(
            !throttle.admit(&bytes("a.mov", 4000)),
            "the window restarts from the last forwarded event"
        );
    }

    /// A file that copies entirely inside one window still has to finish
    /// its bar, so the last chunk is never dropped.
    #[test]
    fn the_throttle_always_forwards_a_files_final_byte_count() {
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let throttle = throttle_at(&clock);
        throttle.admit(&ProgressEvent::FileStarted {
            rel: "a.mov".to_string(),
            size: 3000,
        });
        assert!(throttle.admit(&bytes("a.mov", 1000)));
        assert!(!throttle.admit(&bytes("a.mov", 2000)), "mid-file, too soon");
        assert!(
            throttle.admit(&bytes("a.mov", 3000)),
            "the size FileStarted announced was reached"
        );
    }

    /// The window is per file: one file's chatter must not silence
    /// another's, whichever worker thread it came from.
    #[test]
    fn the_throttle_windows_each_file_separately() {
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let throttle = throttle_at(&clock);
        assert!(throttle.admit(&bytes("a.mov", 1000)));
        assert!(throttle.admit(&bytes("b.mov", 1000)));
        assert!(!throttle.admit(&bytes("a.mov", 2000)));
    }

    /// Every other event is the surface's counting material — one per file
    /// or per run, never throttled.
    #[test]
    fn the_throttle_forwards_every_non_bytes_event() {
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let throttle = throttle_at(&clock);
        for event in [
            ProgressEvent::RunStarted {
                files_total: 2,
                bytes_total: 20,
            },
            ProgressEvent::FileVerified {
                rel: "a.mov".to_string(),
                dest_root: "/dest".to_string(),
            },
            ProgressEvent::FilePlaced {
                rel: "a.mov".to_string(),
            },
            ProgressEvent::FileFailed {
                rel: "b.mov".to_string(),
                reason: "unreadable".to_string(),
            },
            ProgressEvent::RunStopped { cancelled: true },
        ] {
            assert!(throttle.admit(&event), "{event:?}");
        }
    }

    /// The exact line `services::ingest::run_ingest` emits, which is where
    /// this head learns the run's id.
    #[test]
    fn the_run_id_comes_out_of_the_resume_notice() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            run_id_from_notice(&format!("run {id} — resume with: --resume {id}")),
            Some(id)
        );
        assert_eq!(run_id_from_notice("skipping an unreadable entry"), None);
    }
}
