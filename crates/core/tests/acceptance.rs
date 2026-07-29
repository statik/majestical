//! Acceptance tests at the hexagon boundary: fake clock, in-memory
//! machines, real CRDT semantics.
//!
//! Steps return `Result` instead of asserting/panicking: this binary is a
//! `harness = false` integration test, so it is not compiled under
//! `cfg(test)` the way `#[test]` functions are, and the workspace denies
//! `panic`/`unwrap_used` outside test code.
use cucumber::{World, given, then, when};
use majestical_core::clock::{Clock, HlcClock, MAX_DRIFT_MS, MachineId, ObserveOutcome};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_core::projection::Projection;
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

struct TickClock(u64);
impl Clock for TickClock {
    fn wall_ms(&self) -> u64 {
        self.0
    }
}

struct Machine {
    name: String,
    hlc: HlcClock,
    log: Vec<Event>,
    projection: Projection,
    seq: u64,
    // Well-behaved peers never trigger `ObserveOutcome::ClampedFuture`, so
    // `ingest` asserts against it by default (see below). The
    // poisoned-clock scenario deliberately breaks that assumption: the "has
    // a clock far in the future" step flips `allow_clamps` on every machine
    // in the world (not just the poisoned one), because it's whichever
    // *other* machine ingests the poisoned event that would otherwise trip
    // the assert.
    allow_clamps: bool,
    // Count of `ClampedFuture` outcomes this machine has observed via
    // `ingest`, so a scenario can assert the clamp actually fired rather
    // than merely not panicking.
    clamps: usize,
}

impl Machine {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            hlc: HlcClock::new(MachineId(name.into()), Box::new(TickClock(1))),
            log: Vec::new(),
            projection: Projection::default(),
            seq: 0,
            allow_clamps: false,
            clamps: 0,
        }
    }

    fn emit(&mut self, op: Op) {
        self.seq += 1;
        // Salted with the machine name so two machines' independently
        // numbered events never collide in `EventId` space — a bare
        // per-machine counter would let one machine's event silently
        // displace another's during `Projection::apply`'s de-dup.
        // `DefaultHasher`'s output isn't stable across Rust releases, but
        // that's fine here: ids only need to be unique within a single test
        // run. Real clients mint random ULIDs; this deterministic scheme is
        // test-only plumbing and must not be copied into sync code.
        let mut hasher = DefaultHasher::new();
        self.name.hash(&mut hasher);
        self.seq.hash(&mut hasher);
        let random = u128::from(hasher.finish());
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(self.seq, random)),
            hlc: self.hlc.now(),
            author: "test".into(),
            op,
        };
        self.projection.apply(&e);
        self.log.push(e);
    }

    fn ingest(&mut self, events: &[Event]) {
        for e in events {
            // This harness models well-behaved peers by default, so every
            // observe here must be Adopted or AlreadyCurrent unless the
            // scenario has deliberately poisoned a peer's clock (see the
            // "has a clock far in the future" step, which sets
            // `allow_clamps`); the un-poisoned clamp behavior itself is
            // exercised by clock.rs's own unit tests. When clamps are
            // allowed, a ClampedFuture outcome is counted instead of
            // asserted against, so a scenario can later confirm the clamp
            // actually fired.
            let outcome = self.hlc.observe(&e.hlc);
            let clamped = matches!(outcome, ObserveOutcome::ClampedFuture { .. });
            assert!(
                self.allow_clamps || !clamped,
                "acceptance harness observed a poisoned-clock outcome unexpectedly on machine {}",
                self.name
            );
            if clamped {
                self.clamps += 1;
            }
            self.projection.apply(e);
            if !self.log.iter().any(|x| x.id == e.id) {
                self.log.push(e.clone());
            }
        }
    }
}

impl std::fmt::Debug for Machine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Machine(seq={})", self.seq)
    }
}

#[derive(Debug, World)]
#[world(init = Self::new)]
struct CatalogWorld {
    machines: BTreeMap<String, Machine>,
}

impl CatalogWorld {
    // Both machines exist from the start of every scenario. A machine
    // created lazily on first mention would start with an empty log and
    // an empty projection, so a step reached before any exchange (e.g.
    // "bob removes tag X") would cite no observed adds — a no-op remove
    // that silently exercises nothing.
    fn new() -> Self {
        let mut machines = BTreeMap::new();
        machines.insert("amy".to_string(), Machine::new("amy"));
        machines.insert("bob".to_string(), Machine::new("bob"));
        Self { machines }
    }

    // Deliberately strict: no lazy creation. A typo'd or unseeded machine
    // name in a `.feature` file must fail loudly rather than silently
    // starting a fresh machine with an empty log and projection.
    fn machine(&mut self, name: &str) -> Result<&mut Machine, String> {
        self.machines.get_mut(name).ok_or_else(|| {
            format!(
                "unknown machine {name} — scenarios may only use the seeded machines (amy, bob)"
            )
        })
    }
}

fn asset(name: &str) -> AssetId {
    AssetId(format!("xxh3:{name}"))
}

#[given(expr = "machine {string} tags asset {string} with {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn tag_add(w: &mut CatalogWorld, m: String, a: String, tag: String) -> Result<(), String> {
    w.machine(&m)?.emit(Op::TagAdd {
        asset: asset(&a),
        tag,
    });
    Ok(())
}

#[given(expr = "machine {string} removes tag {string} from asset {string}")]
#[when(expr = "machine {string} removes tag {string} from asset {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn tag_rm(w: &mut CatalogWorld, m: String, tag: String, a: String) -> Result<(), String> {
    let machine = w.machine(&m)?;
    let observed = machine.projection.tag_add_ids(&asset(&a), &tag);
    if observed.is_empty() {
        return Err(format!(
            "remove would cite nothing — machine {m} hasn't seen any adds for tag {tag:?}; \
             scenario ordering bug?"
        ));
    }
    machine.emit(Op::TagRemove {
        asset: asset(&a),
        tag,
        observed,
    });
    Ok(())
}

#[given(expr = "machine {string} observes volume {string} labeled {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn observe_volume(
    w: &mut CatalogWorld,
    m: String,
    volume: String,
    label: String,
) -> Result<(), String> {
    w.machine(&m)?.emit(Op::VolumeSeen { volume, label });
    Ok(())
}

#[given(expr = "machine {string} has a clock far in the future")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn poison_clock(w: &mut CatalogWorld, m: String) -> Result<(), String> {
    // Every machine's TickClock is fixed at wall_ms 1 (see Machine::new), so
    // anything past 1 + MAX_DRIFT_MS is unambiguously beyond drift from any
    // well-behaved peer's perspective — poisoning ordering rather than
    // merely nudging it.
    const FAR_FUTURE_MS: u64 = 1 + MAX_DRIFT_MS + 1_000_000;
    let machine = w.machine(&m)?;
    // Rebuilding the HlcClock resets last_wall/last_counter to 0, which
    // would normally un-monotonic the clock — safe only here because
    // FAR_FUTURE_MS dominates any timestamp this machine could have already
    // produced or observed, so the very next `now()` still moves forward.
    machine.hlc = HlcClock::new(MachineId(m.clone()), Box::new(TickClock(FAR_FUTURE_MS)));
    // From this point on, any machine that ingests this one's events must
    // tolerate a ClampedFuture outcome — that's the behavior this scenario
    // exists to exercise, not a harness bug.
    for other in w.machines.values_mut() {
        other.allow_clamps = true;
    }
    Ok(())
}

#[given("the machines exchange event logs")]
#[when("the machines exchange event logs")]
fn exchange(w: &mut CatalogWorld) {
    let all: Vec<Event> = w
        .machines
        .values()
        .flat_map(|m| m.log.iter().cloned())
        .collect();
    for m in w.machines.values_mut() {
        m.ingest(&all);
    }
}

#[then(expr = "both machines see tags {string} on asset {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn assert_tags(w: &mut CatalogWorld, expected: String, a: String) -> Result<(), String> {
    // Sorted so feature authors can list tags in any order — `Projection::tags`
    // always returns them in ascending order via its underlying `BTreeSet`.
    let mut want: Vec<&str> = expected.split(", ").collect();
    want.sort_unstable();
    for (name, m) in &w.machines {
        let got: Vec<String> = m.projection.tags(&asset(&a)).into_iter().collect();
        if got != want {
            return Err(format!(
                "machine {name} diverged: got {got:?}, want {want:?} (both compared ascending-sorted)"
            ));
        }
    }
    Ok(())
}

#[then(expr = "both machines see volume {string} labeled {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn assert_volume_label(w: &mut CatalogWorld, volume: String, want: String) -> Result<(), String> {
    for (name, m) in &w.machines {
        let got = m
            .projection
            .volumes()
            .find(|(id, _)| **id == volume)
            .and_then(|(_, state)| state.label());
        if got != Some(want.as_str()) {
            return Err(format!(
                "machine {name} diverged on volume {volume:?}: got {got:?}, want {want:?}"
            ));
        }
    }
    Ok(())
}

#[then(expr = "machine {string} clamped a far-future timestamp")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn assert_clamped(w: &mut CatalogWorld, m: String) -> Result<(), String> {
    let machine = w.machine(&m)?;
    if machine.clamps == 0 {
        return Err(format!(
            "machine {m} never observed a ClampedFuture outcome — the clock-poisoning \
             scenario didn't actually exercise the clamp"
        ));
    }
    Ok(())
}

fn main() {
    futures::executor::block_on(CatalogWorld::run("tests/features"));
}
