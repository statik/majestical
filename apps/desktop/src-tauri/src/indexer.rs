//! The background index scheduler: one loop thread, power-aware, spawned in
//! `setup` next to `restore_persisted_catalog`. The `ingest.rs` pattern
//! exactly — this module owns the state struct, the loop thread, and the
//! two `#[tauri::command]` one-liners over `*_impl` functions here.
//!
//! Unlike an ingest run, the scheduler has no caller waiting on it and no
//! single job to name: it reads the currently selected catalog from
//! [`crate::commands::AppState`] on every tick rather than capturing one at
//! spawn time, so a catalog change from `initialize_catalog`/
//! `use_existing_catalog` re-aims the loop for free on its very next tick —
//! no extra signaling between that command and this module is needed.
use crate::commands::{AppState, CatalogCfg, CommandError, open_app, selected_catalog};
use crate::power::{POWER_PROBE_AVAILABLE, read_power_state};
use majestical_services::autopilot::{
    HoldReason, PowerSource, PowerState, SchedulerDecision, ThrottleOverride, autopilot_decision,
};
use majestical_services::index::{self, IndexRunReq, IndexStatusOutcome, VALID_KINDS};
use serde::Serialize;
use std::sync::{PoisonError, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

/// How often the loop checks in when there is nothing to do right now: no
/// catalog selected, a hold, or a failed batch backing off.
const TICK: Duration = Duration::from_secs(30);

/// Items per batch, under both Low and Full throttle — this pass's own item
/// cap, independent of how many are actually pending.
const BATCH_LIMIT: usize = 25;

/// Pace between Low-throttle batches, so a battery-powered Mac gets gaps to
/// let the disk and CPU idle between passes rather than one batch running
/// into the next back to back.
const PACE_LOW: Duration = Duration::from_secs(5);

/// Managed state: the scheduler's throttle override and the last tick's
/// findings, read by `scheduler_state`/`set_throttle` and written by the
/// loop thread.
pub struct SchedulerState(pub RwLock<SchedulerShared>);

/// The scheduler's live state. `throttle` is the only field a caller can
/// change ([`set_throttle`]); the rest is the loop thread's own report of
/// what it last saw and did.
pub struct SchedulerShared {
    pub throttle: ThrottleOverride,
    pub last_decision: Option<SchedulerDecision>,
    pub power: PowerState,
    /// The sum of every kind's `pending` row from the last status poll —
    /// see [`pending_items`].
    pub pending_items: u64,
    /// Whether a batch is executing right now.
    pub running: bool,
    /// The last failed batch's message, kept for Health; cleared on the
    /// next batch that succeeds.
    pub last_error: Option<String>,
}

impl Default for SchedulerShared {
    fn default() -> Self {
        Self {
            throttle: ThrottleOverride::Auto,
            last_decision: None,
            power: PowerState {
                source: PowerSource::Unknown,
                low_power_mode: false,
            },
            pending_items: 0,
            running: false,
            last_error: None,
        }
    }
}

/// Wire outcome for `scheduler_state`/`set_throttle`.
#[derive(Serialize)]
pub struct SchedulerStateOutcome {
    pub available: bool,
    pub throttle: ThrottleOverride,
    pub power: PowerState,
    pub decision: Option<SchedulerDecision>,
    pub pending_items: u64,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// The scheduler's single pending count: the sum of every kind's `pending`
/// row from `index::status` — thumbs, embeddings, keyframes, keyframe
/// images, transcripts, OCR, PDF text, and captions. Pure so it is testable
/// without a catalog.
#[must_use]
pub fn pending_items(status: &IndexStatusOutcome) -> u64 {
    status.thumbs.pending
        + status.embeddings.pending
        + status.keyframes.pending
        + status.keyframe_images.pending
        + status.transcripts.pending
        + status.ocr.pending
        + status.pdf.pending
        + status.captions.pending
}

/// Maps a scheduler decision to the one batch it authorizes: `RunLow` caps
/// to a single worker thread, `RunFull` leaves the services default
/// (CPU-scaled) parallelism, and a `Hold` — for any reason — authorizes
/// nothing. Pure and no-I/O, so this is testable without a thread, a
/// catalog, or a power probe.
#[must_use]
pub fn batch_request(decision: SchedulerDecision) -> Option<IndexRunReq> {
    let threads = match decision {
        SchedulerDecision::RunLow => Some(1),
        SchedulerDecision::RunFull => None,
        SchedulerDecision::Hold(
            HoldReason::Paused | HoldReason::LowPowerMode | HoldReason::NoPendingWork,
        ) => return None,
    };
    Some(IndexRunReq {
        kinds: VALID_KINDS.iter().map(|s| (*s).to_string()).collect(),
        limit: Some(BATCH_LIMIT),
        threads,
        api_key: None,
    })
}

/// How long to sleep after a batch succeeds: immediately again under
/// `RunFull` (drain the queue as fast as the machine allows), or
/// [`PACE_LOW`] under `RunLow`. Only ever called for those two — a `Hold`
/// has no batch to succeed — but written exhaustively so a new decision
/// variant cannot slip past it unnoticed; its `Hold` arm falls back to
/// [`TICK`], the same pace an idle tick already sleeps.
fn success_pace(decision: SchedulerDecision) -> Duration {
    match decision {
        SchedulerDecision::RunFull => Duration::ZERO,
        SchedulerDecision::RunLow => PACE_LOW,
        SchedulerDecision::Hold(
            HoldReason::Paused | HoldReason::LowPowerMode | HoldReason::NoPendingWork,
        ) => TICK,
    }
}

/// Polls pending work and power, decides this tick's mode, and publishes
/// both into `scheduler` before returning the decision and the batch it
/// authorizes (`None` for a hold) — so a caller reading `scheduler_state`
/// mid-tick sees this tick's numbers even if the batch itself is still to
/// come.
fn poll_and_decide(
    cfg: &CatalogCfg,
    scheduler: &SchedulerState,
) -> Result<(SchedulerDecision, Option<IndexRunReq>), CommandError> {
    let fs_app = open_app(cfg)?;
    let status = index::status(&fs_app, &cfg.catalog)?;
    let pending = pending_items(&status);
    let power = read_power_state();
    let throttle = scheduler
        .0
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .throttle;
    let decision = autopilot_decision(power, throttle, pending);
    let mut shared = scheduler.0.write().unwrap_or_else(PoisonError::into_inner);
    shared.power = power;
    shared.pending_items = pending;
    shared.last_decision = Some(decision);
    drop(shared);
    Ok((decision, batch_request(decision)))
}

/// Runs one batch — a fresh `FsApp`, same "commands open their own" rule
/// `commands.rs` follows, since the loop's own catalog handle would
/// otherwise go stale across ticks — and records its outcome. Returns how
/// long to sleep before the next tick.
fn run_batch(
    cfg: &CatalogCfg,
    scheduler: &SchedulerState,
    decision: SchedulerDecision,
    req: &IndexRunReq,
) -> Duration {
    scheduler
        .0
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .running = true;
    let result: Result<(), CommandError> = open_app(cfg).and_then(|fs_app| {
        index::run(&fs_app, &cfg.catalog, req)?;
        Ok(())
    });
    let mut shared = scheduler.0.write().unwrap_or_else(PoisonError::into_inner);
    shared.running = false;
    match result {
        Ok(()) => {
            shared.last_error = None;
            drop(shared);
            success_pace(decision)
        }
        Err(err) => {
            shared.last_error = Some(err.message);
            drop(shared);
            TICK
        }
    }
}

/// One pass of the loop: hold when no catalog is selected, else poll,
/// decide, and — on `RunFull`/`RunLow` — run one batch. Returns how long to
/// sleep before the next pass. Every path sleeps at least [`PACE_LOW`]
/// except a `RunFull` batch that just succeeded, so this never spins hot.
fn run_tick(app: &AppHandle) -> Duration {
    let state = app.state::<AppState>();
    let Some(cfg) = selected_catalog(&state) else {
        return TICK;
    };
    let scheduler = app.state::<SchedulerState>();
    let (decision, req) = match poll_and_decide(&cfg, &scheduler) {
        Ok(pair) => pair,
        Err(err) => {
            scheduler
                .0
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .last_error = Some(err.message);
            return TICK;
        }
    };
    let Some(req) = req else {
        return TICK;
    };
    run_batch(&cfg, &scheduler, decision, &req)
}

/// The loop body: forever, run one tick and sleep for whatever it decided.
/// Runs for the life of the process — nothing ever joins this thread, the
/// same "outlives everything" shape `ingest.rs`'s run thread has.
fn run_loop(app: &AppHandle) {
    loop {
        let pause = run_tick(app);
        std::thread::sleep(pause);
    }
}

/// Spawns the scheduler loop. Called from `setup` right after
/// `restore_persisted_catalog`, so a catalog restored on this launch is
/// visible on the loop's very first tick.
///
/// `Builder::spawn`, not `thread::spawn`: the latter panics if the OS
/// refuses the thread, which would abort `setup` and the app would never
/// open a window over one missing background thread. A refusal here instead
/// lands in `last_error` — indexing never starts, but everything else still
/// works.
pub fn spawn_loop(app: &AppHandle) {
    let handle = app.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("index-scheduler".to_string())
        .spawn(move || run_loop(&handle))
    {
        let scheduler = app.state::<SchedulerState>();
        scheduler
            .0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .last_error = Some(format!(
            "could not start the scheduler loop's thread: {err}"
        ));
    }
}

/// What `scheduler_state`/`set_throttle` answer with: the power probe's
/// platform availability plus everything the loop last saw and did.
#[must_use]
fn scheduler_state_impl(state: &SchedulerState) -> SchedulerStateOutcome {
    let shared = state.0.read().unwrap_or_else(PoisonError::into_inner);
    SchedulerStateOutcome {
        available: POWER_PROBE_AVAILABLE,
        throttle: shared.throttle,
        power: shared.power,
        decision: shared.last_decision,
        pending_items: shared.pending_items,
        running: shared.running,
        last_error: shared.last_error.clone(),
    }
}

/// Changes the throttle override. Takes effect at the next batch boundary —
/// batches are short by construction (`BATCH_LIMIT` items), which IS the
/// pause latency, the same "between files" doctrine `ingest.rs`'s cancel
/// follows.
#[must_use]
fn set_throttle_impl(state: &SchedulerState, throttle: ThrottleOverride) -> SchedulerStateOutcome {
    state
        .0
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .throttle = throttle;
    scheduler_state_impl(state)
}

#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn scheduler_state(state: State<'_, SchedulerState>) -> SchedulerStateOutcome {
    scheduler_state_impl(&state)
}

#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "tauri::command hands a handler its state and arguments by value"
)]
#[tauri::command]
pub fn set_throttle(
    state: State<'_, SchedulerState>,
    throttle: ThrottleOverride,
) -> SchedulerStateOutcome {
    set_throttle_impl(&state, throttle)
}

#[cfg(test)]
mod tests {
    use super::{
        BATCH_LIMIT, HoldReason, IndexStatusOutcome, PowerSource, PowerState, SchedulerDecision,
        SchedulerStateOutcome, ThrottleOverride, batch_request, pending_items,
    };
    use majestical_services::index::KindStatusRow;

    fn kind_row(pending: u64) -> KindStatusRow {
        KindStatusRow {
            done: 0,
            pending,
            offline: 0,
            unsupported: 0,
            needs_ffmpeg: 0,
            needs_model: 0,
        }
    }

    fn status_with_pending(counts: [u64; 8]) -> IndexStatusOutcome {
        IndexStatusOutcome {
            thumbs: kind_row(counts[0]),
            embeddings: kind_row(counts[1]),
            keyframes: kind_row(counts[2]),
            keyframe_images: kind_row(counts[3]),
            transcripts: kind_row(counts[4]),
            ocr: kind_row(counts[5]),
            pdf: kind_row(counts[6]),
            captions: kind_row(counts[7]),
            transcripts_remedy: None,
            captions_remedy: None,
            failed_last_run: serde_json::Value::Object(serde_json::Map::new()),
            notices: Vec::new(),
        }
    }

    #[test]
    fn pending_items_sums_every_kind() {
        let status = status_with_pending([1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(pending_items(&status), 36);
    }

    #[test]
    fn pending_items_is_zero_when_every_kind_is_done() {
        let status = status_with_pending([0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(pending_items(&status), 0);
    }

    #[test]
    fn batch_request_run_low_caps_to_one_thread() {
        let req = batch_request(SchedulerDecision::RunLow).expect("a batch");
        assert_eq!(req.limit, Some(BATCH_LIMIT));
        assert_eq!(req.threads, Some(1));
    }

    #[test]
    fn batch_request_run_full_uses_default_parallelism() {
        let req = batch_request(SchedulerDecision::RunFull).expect("a batch");
        assert_eq!(req.limit, Some(BATCH_LIMIT));
        assert_eq!(req.threads, None);
    }

    #[test]
    fn batch_request_hold_authorizes_nothing_for_any_reason() {
        for reason in [
            HoldReason::Paused,
            HoldReason::LowPowerMode,
            HoldReason::NoPendingWork,
        ] {
            assert!(batch_request(SchedulerDecision::Hold(reason)).is_none());
        }
    }

    #[test]
    fn wire_shape_run_full_omits_last_error() {
        let outcome = SchedulerStateOutcome {
            available: true,
            throttle: ThrottleOverride::Auto,
            power: PowerState {
                source: PowerSource::Ac,
                low_power_mode: false,
            },
            decision: Some(SchedulerDecision::RunFull),
            pending_items: 12,
            running: true,
            last_error: None,
        };
        let value = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "available": true,
                "throttle": "auto",
                "power": {"source": "ac", "low_power_mode": false},
                "decision": {"mode": "run_full"},
                "pending_items": 12,
                "running": true,
            })
        );
    }

    #[test]
    fn wire_shape_held_carries_hold_reason_and_last_error() {
        let outcome = SchedulerStateOutcome {
            available: true,
            throttle: ThrottleOverride::Paused,
            power: PowerState {
                source: PowerSource::Battery,
                low_power_mode: false,
            },
            decision: Some(SchedulerDecision::Hold(HoldReason::Paused)),
            pending_items: 4,
            running: false,
            last_error: Some("the last batch failed: disk full".to_string()),
        };
        let value = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "available": true,
                "throttle": "paused",
                "power": {"source": "battery", "low_power_mode": false},
                "decision": {"mode": "hold", "hold_reason": "paused"},
                "pending_items": 4,
                "running": false,
                "last_error": "the last batch failed: disk full",
            })
        );
    }
}
