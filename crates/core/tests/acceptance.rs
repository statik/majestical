//! Acceptance tests at the hexagon boundary: fake clock, in-memory
//! machines, real CRDT semantics.
//!
//! Steps return `Result` instead of asserting/panicking: this binary is a
//! `harness = false` integration test, so it is not compiled under
//! `cfg(test)` the way `#[test]` functions are, and the workspace denies
//! `panic`/`unwrap_used` outside test code.
use cucumber::{World, given, then, when};
use majestical_core::clock::{Clock, HlcClock, MachineId};
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
}

impl Machine {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            hlc: HlcClock::new(MachineId(name.into()), Box::new(TickClock(1))),
            log: Vec::new(),
            projection: Projection::default(),
            seq: 0,
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
            // The clamp scenario (poisoned future clocks) arrives in a
            // later task; ignore the outcome here.
            let _ = self.hlc.observe(&e.hlc);
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

fn main() {
    futures::executor::block_on(CatalogWorld::run("tests/features"));
}
