//! The sync acceptance criterion, per the phase 6 spec: random
//! interleavings of append/write-blob/push/pull across machines and
//! locations, then a final full round — every machine converges to the
//! same event set and the same blob set. Reuses nothing but the public
//! transfer API; if this holds, projection equality follows from the
//! already-proven commutative idempotent apply.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
use majestical_sync::FileEventLog;
use majestical_sync::transfer::{execute, plan_transfer};
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MACHINES: usize = 3;
const LOCATIONS: usize = 2;

#[derive(Debug, Clone)]
enum Step {
    Append { machine: usize, count: u8 },
    WriteBlob { machine: usize },
    Push { machine: usize, location: usize },
    Pull { machine: usize, location: usize },
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..MACHINES, 1u8..4).prop_map(|(machine, count)| Step::Append { machine, count }),
        (0..MACHINES).prop_map(|machine| Step::WriteBlob { machine }),
        (0..MACHINES, 0..LOCATIONS)
            .prop_map(|(machine, location)| Step::Push { machine, location }),
        (0..MACHINES, 0..LOCATIONS)
            .prop_map(|(machine, location)| Step::Pull { machine, location }),
    ]
}

/// Builds a synthetic tag-add event uniquely identified by `(n, machine)`
/// packed into the ULID's random component — a collision here would
/// silently dedupe two distinct events into one entry in the
/// `BTreeSet<String>` the property compares against, corrupting the
/// "no event may be lost" count instead of failing loudly. `wall_ms: n` is
/// a monotonic per-machine counter standing in for a timestamp, not a real
/// clock reading — HLC total order isn't what this property tests.
fn ev(machine: usize, n: u64) -> Event {
    let unique = (u128::from(n) << 8) | machine as u128;
    Event {
        id: EventId(ulid::Ulid::from_parts(n, unique)),
        hlc: Hlc {
            wall_ms: n,
            counter: 0,
            machine: MachineId(format!("m{machine}")),
        },
        author: "prop".into(),
        op: Op::TagAdd {
            asset: AssetId("xxh3:aa".into()),
            tag: format!("t{machine}-{n}"),
        },
    }
}

// `#[cfg(test)]` on these helpers is required, not redundant — see
// `crates/cli/tests/common/mod.rs`'s `maj_as` doc comment for the full
// rationale (a different crate, but the same `clippy.toml`-driven reason).
/// The event ids visible from `root`, opened as `machine`.
/// `FileEventLog::read_all` walks EVERY machine directory under `root` —
/// `machine` does not filter the result down to one machine's own events;
/// it only selects which reader `open`s the log. That distinction matters
/// because `open` is not a pure read: it creates `machine`'s own segment
/// directory under `root` if it doesn't already exist (see `open`'s own
/// doc), so `machine` must always name a machine that genuinely already
/// exists at `root` — passing an arbitrary id would silently plant an
/// empty directory as a side effect of merely computing this set. Every
/// call site below passes a machine root its own real machine id for
/// exactly this reason.
#[cfg(test)]
fn event_ids(root: &Path, machine: &MachineId) -> BTreeSet<String> {
    FileEventLog::open(root, machine)
        .expect("open")
        .read_all()
        .expect("read")
        .into_iter()
        .map(|e| e.id.0.to_string())
        .collect()
}

/// Runs one transfer and asserts it fully succeeded: `execute` reports
/// per-file failures in `TransferOutcome::failures` rather than erroring
/// the whole run, so a real bug (a segment that silently failed to copy)
/// would otherwise pass `execute`'s `Ok` and hide behind an incomplete
/// transfer instead of failing this test loudly.
#[cfg(test)]
fn sync_pair(src: &Path, dst: &Path) {
    let plan = plan_transfer(src, dst).expect("plan");
    let outcome = execute(src, dst, &plan).expect("execute");
    assert!(
        outcome.failures.is_empty(),
        "no planned file may fail to copy: {:?}",
        outcome.failures
    );
}

/// Writes one blob at a path unique to `machine` — distinct paths per
/// machine so the union set observed at convergence has exactly
/// [`MACHINES`] entries, one per planted blob. Content is padded by
/// `machine`'s own index so the planted blobs also differ in SIZE, not
/// just path — otherwise every machine's base string is the same length
/// and the size component of [`blob_paths`]'s tuple set would coincidentally
/// agree across machines, masking a bug that swapped two same-sized blobs.
#[cfg(test)]
fn write_machine_blob(root: &Path, machine: usize) {
    let path = root
        .join("blobs")
        .join(format!("m{machine}"))
        .join("blob.bin");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let mut content = format!("blob-from-m{machine}").into_bytes();
    content.extend(std::iter::repeat_n(b'.', machine));
    std::fs::write(path, content).expect("write blob");
}

/// Writes an additional, script-driven blob under `machine`'s root, at a
/// path distinct from both [`write_machine_blob`]'s plant and every other
/// call for this `machine` (`n` is the caller's own per-machine counter) —
/// so [`Step::WriteBlob`] genuinely models blob traffic happening mid-script,
/// interleaved with push/pull, rather than only at setup.
#[cfg(test)]
fn write_numbered_blob(root: &Path, machine: usize, n: u64) {
    let path = root
        .join("blobs")
        .join(format!("m{machine}"))
        .join(format!("w{n}.bin"));
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, format!("w-m{machine}-{n}")).expect("write blob");
}

/// The set of `(path, size)` pairs under `root/blobs`, path relative to
/// `blobs/` — the blob-side analogue of [`event_ids`]. Size is part of the
/// identity, not just the path: the engine's own diff in `plan_blobs` is
/// size-based (a size mismatch is a torn copy that gets re-planned), so a
/// convergence check that only compared paths would pass a blob that
/// landed with truncated content at the right name — the exact torn-copy
/// case the engine exists to catch. A location root has no dedicated
/// walker of its own (unlike [`FileEventLog`] for events), so this walks
/// the tree directly.
#[cfg(test)]
fn blob_paths(root: &Path) -> BTreeSet<(PathBuf, u64)> {
    let blobs = root.join("blobs");
    let mut out = BTreeSet::new();
    let mut stack = vec![blobs.clone()];
    while let Some(dir) = stack.pop() {
        // NotFound is the one tolerated case — a machine root with no
        // blobs dir at all is a valid, empty peer, same as the engine's
        // own `plan_blobs`. Any other error (permission denied, a stale
        // mount) is a real test-environment bug and must panic loudly
        // rather than silently reporting an empty blob set.
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("read {}: {e}", dir.display()),
        };
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                stack.push(path);
            } else {
                let size = entry.metadata().expect("metadata").len();
                let rel = path
                    .strip_prefix(&blobs)
                    .expect("under blobs/")
                    .to_path_buf();
                out.insert((rel, size));
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn machines_converge_after_a_final_full_round(script in prop::collection::vec(step_strategy(), 0..40)) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut counters = [0u64; MACHINES];
        let mut blob_counters = [0u64; MACHINES];
        let mut planted_blobs = MACHINES;
        let machine_roots: Vec<_> = (0..MACHINES).map(|m| {
            let root = dir.path().join(format!("machine{m}"));
            FileEventLog::init(&root, &MachineId(format!("m{m}"))).expect("init");
            write_machine_blob(&root, m);
            root
        }).collect();
        let location_roots: Vec<_> = (0..LOCATIONS).map(|l| {
            let root = dir.path().join(format!("location{l}"));
            std::fs::create_dir_all(root.join("events")).expect("mkdir");
            std::fs::create_dir_all(root.join("blobs")).expect("mkdir");
            root
        }).collect();

        for step in script {
            match step {
                Step::Append { machine, count } => {
                    let id = MachineId(format!("m{machine}"));
                    let mut log = FileEventLog::open(&machine_roots[machine], &id).expect("open");
                    let batch: Vec<Event> = (0..count).map(|_| {
                        counters[machine] += 1;
                        ev(machine, counters[machine])
                    }).collect();
                    log.append(&batch).expect("append");
                }
                Step::WriteBlob { machine } => {
                    blob_counters[machine] += 1;
                    write_numbered_blob(&machine_roots[machine], machine, blob_counters[machine]);
                    planted_blobs += 1;
                }
                Step::Push { machine, location } =>
                    sync_pair(&machine_roots[machine], &location_roots[location]),
                Step::Pull { machine, location } =>
                    sync_pair(&location_roots[location], &machine_roots[machine]),
            }
        }

        // Final full round: everyone pushes everywhere, then everyone
        // pulls everywhere — one round suffices because phase 1 (every
        // machine pushing to every location) leaves each location holding
        // exactly the union of every machine's pre-phase knowledge, and
        // phase 2 (every machine pulling from every location) distributes
        // that union back out. Every pull below therefore sees every push
        // above; pushing and pulling interleaved instead could leave a
        // machine's late push invisible to a peer that already pulled from
        // the same location.
        for root in &machine_roots {
            for loc in &location_roots {
                sync_pair(root, loc);
            }
        }
        for root in &machine_roots {
            for loc in &location_roots {
                sync_pair(loc, root);
            }
        }

        let reference = event_ids(&machine_roots[0], &MachineId("m0".into()));
        let total: u64 = counters.iter().sum();
        prop_assert_eq!(reference.len() as u64, total, "no event may be lost");
        for (m, root) in machine_roots.iter().enumerate().skip(1) {
            let ids = event_ids(root, &MachineId(format!("m{m}")));
            prop_assert_eq!(&ids, &reference, "machine {} diverged on events", m);
        }

        let blob_reference = blob_paths(&machine_roots[0]);
        prop_assert_eq!(
            blob_reference.len(),
            planted_blobs,
            "every planted blob (the initial per-machine plant plus every \
             Step::WriteBlob) must survive: {:?}",
            blob_reference
        );
        for (m, root) in machine_roots.iter().enumerate().skip(1) {
            prop_assert_eq!(&blob_paths(root), &blob_reference, "machine {} diverged on blobs", m);
        }
        for (l, root) in location_roots.iter().enumerate() {
            // Blobs only, not events: a location root has no machine of its
            // own to pass to `event_ids`, and `FileEventLog::open`'s
            // directory-creating side effect (see `event_ids`'s doc) means
            // there's no id that could be passed there without planting a
            // spurious machine dir in the location's own events/ tree.
            prop_assert_eq!(&blob_paths(root), &blob_reference, "location {} diverged on blobs", l);
        }
    }
}
