//! The sync acceptance criterion, per the phase 6 spec: random
//! interleavings of append/push/pull across machines and locations, then a
//! final full round — every machine converges to the same event set and
//! the same blob set. Reuses nothing but the public transfer API; if this
//! holds, projection equality follows from the already-proven commutative
//! idempotent apply.
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
    Append { machine: usize, events: u8 },
    Push { machine: usize, location: usize },
    Pull { machine: usize, location: usize },
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..MACHINES, 1u8..4).prop_map(|(machine, events)| Step::Append { machine, events }),
        (0..MACHINES, 0..LOCATIONS)
            .prop_map(|(machine, location)| Step::Push { machine, location }),
        (0..MACHINES, 0..LOCATIONS)
            .prop_map(|(machine, location)| Step::Pull { machine, location }),
    ]
}

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

// `#[cfg(test)]` on these helpers is not redundant despite every file under
// `tests/` already building with `--cfg test`: this repo's `clippy.toml`
// sets `allow-expect-in-tests`, and clippy's in-test detection for that
// config keys off `#[test]`/`#[cfg(test)]` directly on the item, not the
// ambient test-binary cfg — dropping it reintroduces `expect_used` errors
// under `-D warnings`. Same pattern as `crates/cli/tests/sync_smoke.rs`.
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
/// [`MACHINES`] entries, one per planted blob.
#[cfg(test)]
fn write_machine_blob(root: &Path, machine: usize) {
    let path = root
        .join("blobs")
        .join(format!("m{machine}"))
        .join("blob.bin");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, format!("blob-from-m{machine}")).expect("write blob");
}

/// The set of blob paths under `root/blobs`, relative to `blobs/` — the
/// blob-side analogue of [`event_ids`]. A location root has no dedicated
/// walker of its own (unlike [`FileEventLog`] for events), so this walks
/// the tree directly.
#[cfg(test)]
fn blob_paths(root: &Path) -> BTreeSet<PathBuf> {
    let blobs = root.join("blobs");
    let mut out = BTreeSet::new();
    let mut stack = vec![blobs.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                stack.push(path);
            } else {
                out.insert(
                    path.strip_prefix(&blobs)
                        .expect("under blobs/")
                        .to_path_buf(),
                );
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn machines_converge_after_a_final_full_round(script in prop::collection::vec(step_strategy(), 0..40)) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut counters = [0u64; MACHINES];
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
                Step::Append { machine, events } => {
                    let id = MachineId(format!("m{machine}"));
                    let mut log = FileEventLog::open(&machine_roots[machine], &id).expect("open");
                    let batch: Vec<Event> = (0..events).map(|_| {
                        counters[machine] += 1;
                        ev(machine, counters[machine])
                    }).collect();
                    log.append(&batch).expect("append");
                }
                Step::Push { machine, location } =>
                    sync_pair(&machine_roots[machine], &location_roots[location]),
                Step::Pull { machine, location } =>
                    sync_pair(&location_roots[location], &machine_roots[machine]),
            }
        }

        // Final full round: everyone pushes everywhere, then everyone
        // pulls everywhere — one round suffices because push carries
        // gossiped segments (and blobs), not just the pusher's own. Every
        // pull below therefore sees every push above; pushing and pulling
        // interleaved instead could leave a machine's late push invisible
        // to a peer that already pulled from the same location.
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
            MACHINES,
            "one planted blob per machine must survive: {:?}",
            blob_reference
        );
        for (m, root) in machine_roots.iter().enumerate().skip(1) {
            prop_assert_eq!(&blob_paths(root), &blob_reference, "machine {} diverged on blobs", m);
        }
        for (l, root) in location_roots.iter().enumerate() {
            prop_assert_eq!(&blob_paths(root), &blob_reference, "location {} diverged on blobs", l);
        }
    }
}
