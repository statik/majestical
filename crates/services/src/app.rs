//! Application state shared by every head (CLI, MCP, GUI): adapter wiring,
//! event emission, projection loading.
use anyhow::{Context, Result};
use majestical_core::clock::{Clock, HlcClock, MachineId, ObserveOutcome};
use majestical_core::event::{Event, EventId, Op};
use majestical_core::ports::EventLog;
use majestical_core::projection::Projection;
use majestical_sync::FileEventLog;
use std::path::{Path, PathBuf};

/// Current wall-clock time in milliseconds since the Unix epoch, shared by
/// `SystemClock` and the clamp-warning's "how far ahead" calculation below.
#[must_use]
pub fn physical_now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn wall_ms(&self) -> u64 {
        physical_now_ms()
    }
}

/// Warns on stderr when reading an event log skipped corrupt lines. Shared by
/// `App::events` (a full read) and `commands::open_catalog` (an incremental
/// or full sqlite sync) so the message can't drift between the two call
/// sites that both count skipped lines from the same underlying log.
pub fn warn_skipped_corrupt_lines(skipped: usize, catalog_root: &Path) {
    if skipped > 0 {
        // services inherits the workspace's print_stderr = "deny" (unlike
        // cli's, which allows it crate-wide since CLI diagnostics are the
        // product) — `#[expect]` documents the exception locally instead of
        // weakening the lint crate-wide. This is the same user-facing
        // stderr diagnostic moved verbatim from the pre-extraction cli
        // crate; extracting it into a rendered outcome is later work.
        #[expect(
            clippy::print_stderr,
            reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
        )]
        {
            eprintln!(
                "warning: skipped {skipped} corrupt event log line(s) in {}/events — damaged transport; affected metadata may be missing",
                catalog_root.display()
            );
        }
    }
}

pub struct App<L> {
    log: L,
    hlc: HlcClock,
    author: String,
    catalog_root: PathBuf,
}

/// The CLI's concrete adapter wiring: a real, filesystem-backed event log.
/// The `App<L>` split exists so tests and future adapters can swap it out.
pub type FsApp = App<FileEventLog>;

impl FsApp {
    /// Opens an already-initialized catalog. Errors rather than creating one
    /// implicitly — a missing catalog is almost always a typo'd path, and
    /// silently creating an empty one there would hide that.
    ///
    /// # Errors
    ///
    /// Returns an error if `root` has no `events` directory (no catalog
    /// there yet), or if the underlying event log fails to open.
    pub fn open(root: &Path, machine: &str, author: &str) -> Result<Self> {
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying event log fails to initialize.
    pub fn init(root: &Path, machine: &str, author: &str) -> Result<Self> {
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
    /// The underlying event log, for adapters (e.g. the sqlite catalog) that
    /// need to read it directly rather than through `events`/`projection`.
    pub fn log(&self) -> &L {
        &self.log
    }

    /// Loads every event currently in the log. Each call re-reads the log
    /// from disk; per-process caching arrives with the adapter refactor.
    ///
    /// Corrupt lines are skipped rather than failing the read; a warning is
    /// printed to stderr so the user knows metadata may be missing, without
    /// polluting stdout (which carries this process's data output).
    ///
    /// # Errors
    ///
    /// Returns an error if the event log cannot be read.
    pub fn events(&self) -> Result<Vec<Event>> {
        let mut skipped = 0usize;
        let events = self
            .log
            .read_all_reporting(&mut |_line| skipped += 1)
            .context("reading event log")?;
        warn_skipped_corrupt_lines(skipped, &self.catalog_root);
        Ok(events)
    }

    #[must_use]
    pub fn projection_of(events: &[Event]) -> Projection {
        let mut p = Projection::default();
        for e in events {
            p.apply(e);
        }
        p
    }

    /// # Errors
    ///
    /// Returns an error if the event log cannot be read.
    pub fn projection(&self) -> Result<Projection> {
        Ok(Self::projection_of(&self.events()?))
    }

    /// # Errors
    ///
    /// Returns an error if the event log cannot be read (to fold existing
    /// events into the clock before appending) or if appending the new
    /// events to the log fails.
    pub fn emit(&mut self, ops: Vec<Op>) -> Result<Vec<Event>> {
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
            // See the `#[expect]` note in `warn_skipped_corrupt_lines` above.
            #[expect(
                clippy::print_stderr,
                reason = "verbatim stderr diagnostic moved from cli; not yet a rendered outcome"
            )]
            {
                eprintln!(
                    "warning: {clamped} event(s) carry timestamps more than 24h in the future (worst: ~{days_ahead}d ahead) — a peer's clock may be wrong; ordering was clamped locally"
                );
            }
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
