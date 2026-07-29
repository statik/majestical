//! Algebraic laws: applying any event set in any order, with any
//! duplication, yields the same projection.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
use majestical_core::projection::Projection;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum OpKind {
    Add {
        tag: String,
    },
    Remove {
        tag: String,
        observed_indices: Vec<usize>,
    },
    Set {
        field: String,
        value: String,
    },
    Seen {
        volume: String,
        path: String,
        size: u64,
    },
    VolumeObserved {
        volume: String,
        label: String,
    },
    ParaCreate {
        node: String,
        kind: ParaKind,
        name: String,
    },
    ParaRename {
        node: String,
        name: String,
    },
    ParaArchive {
        node: String,
    },
    AssetParaSet {
        node: String,
    },
    Verification {
        volume: String,
        path: String,
        algo: String,
        value: String,
        outcome: VerifyOutcome,
        hashdate_ms: u64,
    },
    Manifest {
        volume: String,
        mhl_path: String,
        generation: u32,
        roothash: String,
    },
}

fn build_events(kinds: &[(u64, OpKind)]) -> Vec<Event> {
    let asset = AssetId("xxh3:p".into());
    let ids: Vec<EventId> = (0..kinds.len())
        .map(|n| EventId(ulid::Ulid::from_parts(1, n as u128)))
        .collect();
    kinds
        .iter()
        .enumerate()
        .map(|(n, (wall, kind))| {
            let op = match kind {
                OpKind::Add { tag } => Op::TagAdd {
                    asset: asset.clone(),
                    tag: tag.clone(),
                },
                OpKind::Remove {
                    tag,
                    observed_indices,
                } => Op::TagRemove {
                    asset: asset.clone(),
                    tag: tag.clone(),
                    observed: observed_indices
                        .iter()
                        .map(|i| ids[i % kinds.len()])
                        .collect(),
                },
                OpKind::Set { field, value } => Op::FieldSet {
                    asset: asset.clone(),
                    field: field.clone(),
                    value: value.clone(),
                },
                OpKind::Seen { volume, path, size } => Op::AssetSeen {
                    asset: asset.clone(),
                    volume: volume.clone(),
                    path: path.clone(),
                    size: *size,
                },
                OpKind::VolumeObserved { volume, label } => Op::VolumeSeen {
                    volume: volume.clone(),
                    label: label.clone(),
                },
                OpKind::ParaCreate { node, kind, name } => Op::ParaNodeCreate {
                    node: node.clone(),
                    kind: *kind,
                    name: name.clone(),
                },
                OpKind::ParaRename { node, name } => Op::ParaNodeRename {
                    node: node.clone(),
                    name: name.clone(),
                },
                OpKind::ParaArchive { node } => Op::ParaNodeArchive { node: node.clone() },
                OpKind::AssetParaSet { node } => Op::AssetParaSet {
                    asset: asset.clone(),
                    node: node.clone(),
                },
                OpKind::Verification {
                    volume,
                    path,
                    algo,
                    value,
                    outcome,
                    hashdate_ms,
                } => Op::VerificationRecorded {
                    asset: asset.clone(),
                    volume: volume.clone(),
                    path: path.clone(),
                    algo: algo.clone(),
                    value: value.clone(),
                    outcome: *outcome,
                    hashdate_ms: *hashdate_ms,
                },
                OpKind::Manifest {
                    volume,
                    mhl_path,
                    generation,
                    roothash,
                } => Op::ManifestRecorded {
                    volume: volume.clone(),
                    mhl_path: mhl_path.clone(),
                    generation: *generation,
                    roothash: roothash.clone(),
                },
            };
            Event {
                id: ids[n],
                hlc: Hlc {
                    wall_ms: *wall,
                    counter: 0,
                    machine: MachineId(format!("m{}", n % 3)),
                },
                author: "prop".into(),
                op,
            }
        })
        .collect()
}

fn arb_para_kind() -> impl Strategy<Value = ParaKind> {
    prop_oneof![
        Just(ParaKind::Project),
        Just(ParaKind::Area),
        Just(ParaKind::Resource),
        Just(ParaKind::Archive),
    ]
}

fn arb_verify_outcome() -> impl Strategy<Value = VerifyOutcome> {
    prop_oneof![
        Just(VerifyOutcome::Original),
        Just(VerifyOutcome::Verified),
        Just(VerifyOutcome::Failed),
    ]
}

/// Small node-id pool ("N1"/"N2") so generated ops collide on the same node,
/// exercising LWW and archive-monotonicity rather than always hitting fresh
/// nodes.
fn arb_node() -> impl Strategy<Value = String> {
    "N[1-2]".prop_map(String::from)
}

fn arb_kind() -> impl Strategy<Value = OpKind> {
    prop_oneof![
        "[a-c]{1,3}".prop_map(|tag| OpKind::Add { tag }),
        ("[a-c]{1,3}", proptest::collection::vec(0usize..30, 0..3)).prop_map(
            |(tag, observed_indices)| OpKind::Remove {
                tag,
                observed_indices
            }
        ),
        ("[f-h]{1,2}", "[x-z]{1,3}").prop_map(|(field, value)| OpKind::Set { field, value }),
        ("[v-w]{1,2}", "[p-r]{1,4}", 0u64..100).prop_map(|(volume, path, size)| OpKind::Seen {
            volume,
            path,
            size
        }),
        ("[v-w]{1,2}", "[a-c]{1,3}")
            .prop_map(|(volume, label)| OpKind::VolumeObserved { volume, label }),
        (arb_node(), arb_para_kind(), "[a-z]{1,8}")
            .prop_map(|(node, kind, name)| { OpKind::ParaCreate { node, kind, name } }),
        (arb_node(), "[a-z]{1,8}").prop_map(|(node, name)| OpKind::ParaRename { node, name }),
        arb_node().prop_map(|node| OpKind::ParaArchive { node }),
        arb_node().prop_map(|node| OpKind::AssetParaSet { node }),
        (
            "[v-w]{1,2}",
            "[p-r]{1,4}",
            "[x-z]{1,3}",
            "[0-1]{1,2}",
            arb_verify_outcome(),
            0u64..5,
        )
            .prop_map(|(volume, path, algo, value, outcome, hashdate_ms)| {
                OpKind::Verification {
                    volume,
                    path,
                    algo,
                    value,
                    outcome,
                    hashdate_ms,
                }
            }),
        ("[v-w]{1,2}", "[p-r]{1,4}", 0u32..3, "[x-z]{1,3}").prop_map(
            |(volume, mhl_path, generation, roothash)| OpKind::Manifest {
                volume,
                mhl_path,
                generation,
                roothash,
            }
        ),
    ]
}

proptest! {
    #[test]
    fn projection_is_order_independent(
        kinds in proptest::collection::vec((1u64..50, arb_kind()), 1..30),
        shuffle_seed in any::<u64>(),
    ) {
        let events = build_events(&kinds);
        // Forward with duplication.
        let mut fwd = Projection::default();
        for e in &events { fwd.apply(e); fwd.apply(e); }
        // Reversed.
        let mut rev = Projection::default();
        for e in events.iter().rev() { rev.apply(e); }
        // Deterministic pseudo-shuffle from the seed.
        let mut shuffled: Vec<&Event> = events.iter().collect();
        let n = shuffled.len();
        let mut s = shuffle_seed;
        for i in (1..n).rev() {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bound = i as u64 + 1;
            let idx = usize::try_from(s % bound).expect("result is bounded by usize len n");
            shuffled.swap(i, idx);
        }
        let mut shf = Projection::default();
        for e in shuffled { shf.apply(e); }
        prop_assert_eq!(&fwd, &rev);
        prop_assert_eq!(&fwd, &shf);
    }
}
