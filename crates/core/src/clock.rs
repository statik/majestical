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

    /// Folds a remote timestamp in so subsequent local events order after it.
    pub fn observe(&mut self, remote: &Hlc) {
        if remote.wall_ms > self.last_wall
            || (remote.wall_ms == self.last_wall && remote.counter > self.last_counter)
        {
            self.last_wall = remote.wall_ms;
            self.last_counter = remote.counter;
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
        hlc.observe(&remote);
        assert!(hlc.now() > remote, "local must order after observed remote");
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
