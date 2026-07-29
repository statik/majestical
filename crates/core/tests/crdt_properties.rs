//! Algebraic laws: applying any event set in any order, with any
//! duplication, yields the same projection.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op};
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
