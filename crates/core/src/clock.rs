//! Wall-clock port and Hybrid Logical Clock.
use serde::{Deserialize, Serialize};

/// Stable identifier for a machine/replica participating in the CRDT.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId(pub String);

/// Port: injected so HLC logic is deterministic under test.
pub trait Clock: Send {
    /// Current wall-clock time in milliseconds since the Unix epoch.
    fn wall_ms(&self) -> u64;
}

/// HLC timestamp. Derived ordering (wall, counter, machine) is the total
/// order used for LWW merges; machine id is the deterministic tiebreaker.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub machine: MachineId,
}

/// Ceiling on how far a remote HLC may advance the local clock past
/// physical now: 24h — generous for catalogs synced by shuttle drive
/// across time zones; the point is stopping year-scale poison, not
/// millisecond skew.
pub const MAX_DRIFT_MS: u64 = 24 * 60 * 60 * 1000;

/// Result of folding a remote timestamp into the local clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Remote was ahead and within drift; adopted.
    Adopted,
    /// Remote was behind or equal; nothing to do.
    AlreadyCurrent,
    /// Remote wall time exceeded physical-now + `MAX_DRIFT_MS`. Local state
    /// advanced only to the clamp. New local events may order before the
    /// poisoned events — deliberately, so one bad peer clock cannot
    /// permanently win every LWW merge.
    ClampedFuture { remote_wall_ms: u64 },
}

/// Hybrid Logical Clock: combines wall-clock time with a logical counter so
/// timestamps are monotonic even when the wall clock stalls or moves backward,
/// and so remote events can be causally ordered after local ones via `observe`.
pub struct HlcClock {
    machine: MachineId,
    clock: Box<dyn Clock>,
    last_wall: u64,
    last_counter: u32,
}

impl HlcClock {
    /// Creates a new HLC for `machine`, sourcing wall-clock time from `clock`.
    #[must_use]
    pub fn new(machine: MachineId, clock: Box<dyn Clock>) -> Self {
        Self {
            machine,
            clock,
            last_wall: 0,
            last_counter: 0,
        }
    }

    /// Produces the next timestamp, guaranteed to be strictly greater than
    /// any previously produced or observed timestamp.
    pub fn now(&mut self) -> Hlc {
        let wall = self.clock.wall_ms();
        if wall > self.last_wall {
            self.last_wall = wall;
            self.last_counter = 0;
        } else {
            self.last_counter = self.last_counter.saturating_add(1);
        }
        Hlc {
            wall_ms: self.last_wall,
            counter: self.last_counter,
            machine: self.machine.clone(),
        }
    }

    /// Folds a remote timestamp in so subsequent local events order after
    /// it, unless doing so would advance the local clock more than
    /// `MAX_DRIFT_MS` past physical now — a poisoned or misconfigured
    /// peer clock is clamped rather than adopted outright.
    #[must_use]
    pub fn observe(&mut self, remote: &Hlc) -> ObserveOutcome {
        let physical_now = self.clock.wall_ms();
        let max_wall = physical_now.saturating_add(MAX_DRIFT_MS);
        if remote.wall_ms > max_wall {
            if max_wall > self.last_wall {
                self.last_wall = max_wall;
                self.last_counter = 0;
            }
            return ObserveOutcome::ClampedFuture {
                remote_wall_ms: remote.wall_ms,
            };
        }
        if remote.wall_ms > self.last_wall
            || (remote.wall_ms == self.last_wall && remote.counter > self.last_counter)
        {
            self.last_wall = remote.wall_ms;
            self.last_counter = remote.counter;
            ObserveOutcome::Adopted
        } else {
            ObserveOutcome::AlreadyCurrent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn wall_ms(&self) -> u64 {
            self.0
        }
    }

    struct SteppingClock(Cell<u64>);
    impl Clock for SteppingClock {
        fn wall_ms(&self) -> u64 {
            self.0.get()
        }
    }

    #[test]
    fn hlc_is_monotonic_when_wall_clock_stalls() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let a = hlc.now();
        let b = hlc.now();
        assert!(b > a, "same wall ms must bump counter");
    }

    #[test]
    fn hlc_is_monotonic_when_wall_clock_moves_backward() {
        let mut hlc = HlcClock::new(
            MachineId("m1".into()),
            Box::new(SteppingClock(Cell::new(1000))),
        );
        let a = hlc.now();
        // `tests` is a child module of `clock`, so it can reach the private
        // `clock` field directly to swap in a clock reporting an earlier
        // wall time — simulating a backward jump without needing a shared,
        // externally mutable handle into the boxed `dyn Clock` trait object.
        hlc.clock = Box::new(SteppingClock(Cell::new(500)));
        let b = hlc.now();
        assert!(
            b > a,
            "wall clock moving backward must not un-monotonic the HLC"
        );
    }

    #[test]
    fn hlc_observe_advances_past_remote() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let remote = Hlc {
            wall_ms: 5000,
            counter: 3,
            machine: MachineId("m2".into()),
        };
        assert!(matches!(hlc.observe(&remote), ObserveOutcome::Adopted));
        assert!(hlc.now() > remote, "local must order after observed remote");
    }

    #[test]
    fn observe_within_drift_is_adopted() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let remote = Hlc {
            wall_ms: 2000,
            counter: 3,
            machine: MachineId("m2".into()),
        };
        assert!(matches!(hlc.observe(&remote), ObserveOutcome::Adopted));
        assert!(hlc.now() > remote);
    }

    #[test]
    fn observe_far_future_is_clamped() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        let poison = Hlc {
            wall_ms: 1000 + MAX_DRIFT_MS + 5000,
            counter: 0,
            machine: MachineId("bad".into()),
        };
        let outcome = hlc.observe(&poison);
        assert!(matches!(outcome, ObserveOutcome::ClampedFuture { .. }));
        let next = hlc.now();
        assert!(
            next.wall_ms <= 1000 + MAX_DRIFT_MS,
            "local clock must not adopt poison"
        );
    }

    #[test]
    fn clamp_never_regresses_a_clock_already_past_the_ceiling() {
        let mut hlc = HlcClock::new(MachineId("m1".into()), Box::new(FixedClock(1000)));
        // A remote exactly at the ceiling is in-drift, so it is adopted and
        // last_wall now equals max_wall.
        let near = Hlc {
            wall_ms: 1000 + MAX_DRIFT_MS,
            counter: 9,
            machine: MachineId("near".into()),
        };
        assert_eq!(hlc.observe(&near), ObserveOutcome::Adopted);
        let before = hlc.now();
        let poison = Hlc {
            wall_ms: u64::MAX / 2,
            counter: 0,
            machine: MachineId("bad".into()),
        };
        assert!(matches!(
            hlc.observe(&poison),
            ObserveOutcome::ClampedFuture { .. }
        ));
        assert!(
            hlc.now() > before,
            "clamping must never move the clock backward"
        );
    }

    #[test]
    fn hlc_orders_by_wall_then_counter_then_machine() {
        let a = Hlc {
            wall_ms: 1,
            counter: 0,
            machine: MachineId("a".into()),
        };
        let b = Hlc {
            wall_ms: 1,
            counter: 1,
            machine: MachineId("a".into()),
        };
        let c = Hlc {
            wall_ms: 1,
            counter: 1,
            machine: MachineId("b".into()),
        };
        assert!(a < b && b < c);
    }
}
