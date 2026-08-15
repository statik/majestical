//! Algebraic laws: applying any event set in any order, with any
//! duplication, yields the same projection.
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
use majestical_core::projection::{Projection, Touched};
use proptest::prelude::*;
use std::collections::BTreeSet;

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
        mtime_ms: u64,
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
    SavedSearchSet {
        name: String,
        query: String,
    },
    SavedSearchRemove {
        name: String,
    },
    TagRename {
        from: String,
        to: String,
    },
}

/// Builds the `Op` one `OpKind` describes. Split out of `build_events` purely
/// to keep that function under the crate's max-function-length lint.
fn op_from_kind(asset: &AssetId, kind: &OpKind, ids: &[EventId]) -> Op {
    match kind {
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
                .map(|i| ids[i % ids.len()])
                .collect(),
        },
        OpKind::Set { field, value } => Op::FieldSet {
            asset: asset.clone(),
            field: field.clone(),
            value: value.clone(),
        },
        OpKind::Seen {
            volume,
            path,
            size,
            mtime_ms,
        } => Op::AssetSeen {
            asset: asset.clone(),
            volume: volume.clone(),
            path: path.clone(),
            size: *size,
            mtime_ms: *mtime_ms,
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
        OpKind::SavedSearchSet { name, query } => Op::SavedSearchSet {
            name: name.clone(),
            query: query.clone(),
        },
        OpKind::SavedSearchRemove { name } => Op::SavedSearchRemove { name: name.clone() },
        OpKind::TagRename { from, to } => Op::TagRenamed {
            from: from.clone(),
            to: to.clone(),
        },
    }
}

fn build_events(kinds: &[(u64, OpKind)]) -> Vec<Event> {
    let asset = AssetId("xxh3:p".into());
    let ids: Vec<EventId> = (0..kinds.len())
        .map(|n| EventId(ulid::Ulid::from_parts(1, n as u128)))
        .collect();
    kinds
        .iter()
        .enumerate()
        .map(|(n, (wall, kind))| Event {
            id: ids[n],
            hlc: Hlc {
                wall_ms: *wall,
                counter: 0,
                machine: MachineId(format!("m{}", n % 3)),
            },
            author: "prop".into(),
            op: op_from_kind(&asset, kind, &ids),
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
        ("[v-w]{1,2}", "[p-r]{1,4}", 0u64..100, 0u64..100).prop_map(
            |(volume, path, size, mtime_ms)| OpKind::Seen {
                volume,
                path,
                size,
                mtime_ms
            }
        ),
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
        ("[a-b]", "[q-r]{1,2}").prop_map(|(name, query)| OpKind::SavedSearchSet { name, query }),
        "[a-b]".prop_map(|name| OpKind::SavedSearchRemove { name }),
        // Both ends drawn from the same three-letter alphabet the tag adds
        // use, so generated renames collide with real tags and with each
        // other: chains, self-renames, and two-rename cycles all show up.
        ("[a-c]{1,3}", "[a-c]{1,3}").prop_map(|(from, to)| OpKind::TagRename { from, to }),
    ]
}

/// Every asset's effective tags — the read-time view, which structural
/// projection equality does not by itself exercise: `resolve_alias` runs
/// here and nowhere in `apply`.
fn effective_tags(p: &Projection) -> Vec<(AssetId, BTreeSet<String>)> {
    p.assets().map(|(id, _)| (id.clone(), p.tags(id))).collect()
}

/// Asserts `tags()` handed back a *resolved* name: either nothing aliases
/// it, or it sits on a cycle the walk deliberately breaks. Comparing
/// effective tags across apply orders cannot catch a resolver that ignores
/// the alias map — that failure is equally wrong in every order — so this
/// is the check with teeth: under a no-op resolver a plain `a -> b` rename
/// reads back as "a" while "b" aliases nothing, and the walk below lands
/// somewhere other than where it started.
fn assert_fully_resolved(p: &Projection, tag: &str) -> Result<(), TestCaseError> {
    let mut seen = BTreeSet::new();
    let mut current = tag;
    while let Some(next) = p.tag_alias_target(current) {
        if !seen.insert(current.to_string()) {
            // Reaching a repeat is only correct if the repeat is `tag`
            // itself. Landing on some *other* name means `tag` sat on the
            // tail running into the cycle (`a -> b -> c -> b` handed back
            // "a"), which the walk should have followed to the entry.
            prop_assert_eq!(
                current,
                tag,
                "tags() returned a name on the tail leading into a cycle, \
                 not the cycle entry the walk stops at"
            );
            return Ok(());
        }
        current = next;
    }
    prop_assert_eq!(
        current,
        tag,
        "tags() returned a name that a rename had already moved on from"
    );
    Ok(())
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
        // The tracking path shares the same idempotence guard: replaying
        // every event a further time through it must touch nothing.
        for e in &events {
            prop_assert_eq!(fwd.apply_tracking(e), Touched::Nothing);
        }
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
        // Read back through the alias chain in every order, so the
        // resolution path — not just the stored state — is under the
        // property, and check each name it hands out is really resolved.
        let tags_fwd = effective_tags(&fwd);
        prop_assert_eq!(&tags_fwd, &effective_tags(&rev));
        prop_assert_eq!(&tags_fwd, &effective_tags(&shf));
        for (_, tags) in &tags_fwd {
            for tag in tags {
                assert_fully_resolved(&fwd, tag)?;
            }
        }
    }
}
