//! CLI application state: adapter wiring, event emission, projection loading.
use anyhow::{Context, Result};
use majestical_core::clock::{Clock, HlcClock, MachineId, ObserveOutcome};
use majestical_core::event::{Event, EventId, Op};
use majestical_core::ports::EventLog;
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::path::{Path, PathBuf};

/// Current wall-clock time in milliseconds since the Unix epoch, shared by
/// `SystemClock` and the clamp-warning's "how far ahead" calculation below.
pub(crate) fn physical_now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

pub(crate) struct SystemClock;
impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        physical_now_ms()
    }
}

pub(crate) struct App<L> {
    log: L,
    hlc: HlcClock,
    author: String,
    catalog_root: PathBuf,
}

/// The CLI's concrete adapter wiring: a real, filesystem-backed event log.
/// The `App<L>` split exists so tests and future adapters can swap it out.
pub(crate) type FsApp = App<FileEventLog>;

impl FsApp {
    /// Opens an already-initialized catalog. Errors rather than creating one
    /// implicitly — a missing catalog is almost always a typo'd path, and
    /// silently creating an empty one there would hide that.
    pub(crate) fn open(root: &Path, machine: &str, author: &str) -> Result<Self> {
        anyhow::ensure!(
            root.join("events").is_dir(),
            "no catalog at {} — run `maj catalog init` first",
            root.display()
        );
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::open(root, &machine)
            .with_context(|| format!("opening catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine, Box::new(SystemClock)),
            author: author.to_string(),
            catalog_root: root.to_path_buf(),
        })
    }

    /// Creates a fresh catalog at `root` (`maj catalog init`).
    pub(crate) fn init(root: &Path, machine: &str, author: &str) -> Result<Self> {
        let machine = MachineId(machine.to_string());
        let log = FileEventLog::init(root, &machine)
            .with_context(|| format!("initializing catalog at {}", root.display()))?;
        Ok(Self {
            log,
            hlc: HlcClock::new(machine, Box::new(SystemClock)),
            author: author.to_string(),
            catalog_root: root.to_path_buf(),
        })
    }
}

impl<L: EventLog> App<L> {
    /// Loads every event currently in the log. Each call re-reads the log
    /// from disk; per-process caching arrives with the adapter refactor.
    ///
    /// Corrupt lines are skipped rather than failing the read; a warning is
    /// printed to stderr so the user knows metadata may be missing, without
    /// polluting stdout (which carries this process's data output).
    pub(crate) fn events(&self) -> Result<Vec<Event>> {
        let mut skipped = 0usize;
        let events = self
            .log
            .read_all_reporting(&mut |_line| skipped += 1)
            .context("reading event log")?;
        if skipped > 0 {
            eprintln!(
                "warning: skipped {skipped} corrupt event log line(s) in {}/events — a torn write or damaged transport; affected metadata may be missing",
                self.catalog_root.display()
            );
        }
        Ok(events)
    }

    pub(crate) fn projection_of(events: &[Event]) -> Projection {
        let mut p = Projection::default();
        for e in events {
            p.apply(e);
        }
        p
    }

    pub(crate) fn projection(&self) -> Result<Projection> {
        Ok(Self::projection_of(&self.events()?))
    }

    pub(crate) fn emit(&mut self, ops: Vec<Op>) -> Result<Vec<Event>> {
        // Fold existing log into the clock so new events order after it.
        // A peer's clock may be wrong; count how many events got clamped
        // rather than adopted, and track the worst offender, so the
        // operator can be warned once below.
        let mut clamped = 0usize;
        let mut worst_remote_wall_ms = 0u64;
        for e in self.events()? {
            if let ObserveOutcome::ClampedFuture { remote_wall_ms } = self.hlc.observe(&e.hlc) {
                clamped += 1;
                worst_remote_wall_ms = worst_remote_wall_ms.max(remote_wall_ms);
            }
        }
        if clamped > 0 {
            let days_ahead =
                worst_remote_wall_ms.saturating_sub(physical_now_ms()) / (24 * 60 * 60 * 1000);
            eprintln!(
                "warning: {clamped} event(s) carry timestamps more than 24h in the future (worst: ~{days_ahead}d ahead) — a peer's clock may be wrong; ordering was clamped locally"
            );
        }
        // ulid 3.x generates through a monotonic Generator; on same-millisecond
        // random-part overflow (astronomically rare), fall back to a fresh
        // random id rather than failing the whole emit.
        let mut ulid_gen = ulid::Generator::new();
        let events: Vec<Event> = ops
            .into_iter()
            .map(|op| {
                let hlc = self.hlc.now();
                let id = match ulid_gen.generate() {
                    Ok(id) => id,
                    Err(overflow) => overflow.commit_overflow_random(),
                };
                Event {
                    id: EventId(id),
                    hlc,
                    author: self.author.clone(),
                    op,
                }
            })
            .collect();
        self.log.append(&events).context("appending events")?;
        Ok(events)
    }
}
