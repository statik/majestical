//! Incremental apply must be observationally identical to a full rebuild.
#![cfg(test)] // clippy.toml test exemptions key on the literal attribute

use std::path::Path;

use majestical_catalog_sqlite::{ApplyMode, SqliteCatalog};
use majestical_core::clock::{Hlc, MachineId};
use majestical_core::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
use majestical_sync::FileEventLog;
use proptest::prelude::*;
use ulid::Ulid;

fn ev(n: u64, machine: &str, op: Op) -> Event {
    Event {
        id: EventId(Ulid::from_parts(1, u128::from(n))),
        hlc: Hlc {
            wall_ms: n,
            counter: 0,
            machine: MachineId(machine.into()),
        },
        author: machine.into(),
        op,
    }
}

fn arb_asset() -> impl Strategy<Value = AssetId> {
    prop_oneof![
        Just(AssetId("xxh3:a".into())),
        Just(AssetId("xxh3:b".into())),
    ]
}

fn arb_volume() -> impl Strategy<Value = String> {
    prop_oneof![Just("v".to_string()), Just("w".to_string())]
}

fn arb_node() -> impl Strategy<Value = String> {
    prop_oneof![Just("N1".to_string()), Just("N2".to_string())]
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

/// One op of every `Touched`-mapped `Op` variant, over tiny value domains so
/// generated ops collide on the same asset/node/volume — mirroring
/// `crdt_properties.rs`'s `arb_kind` strategy.
fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (arb_asset(), arb_volume(), "[p-r]{1,3}", 0u64..100).prop_map(
            |(asset, volume, path, size)| Op::AssetSeen {
                asset,
                volume,
                path,
                size
            }
        ),
        (arb_volume(), "[a-c]{1,3}").prop_map(|(volume, label)| Op::VolumeSeen { volume, label }),
        (arb_asset(), "[a-c]{1,3}").prop_map(|(asset, tag)| Op::TagAdd { asset, tag }),
        (arb_asset(), "[a-c]{1,3}").prop_map(|(asset, tag)| Op::TagRemove {
            asset,
            tag,
            observed: vec![],
        }),
        (arb_asset(), "[f-h]{1,2}", "[x-z]{1,3}").prop_map(|(asset, field, value)| {
            Op::FieldSet {
                asset,
                field,
                value,
            }
        }),
        (arb_node(), arb_para_kind(), "[a-z]{1,8}")
            .prop_map(|(node, kind, name)| { Op::ParaNodeCreate { node, kind, name } }),
        (arb_node(), "[a-z]{1,8}").prop_map(|(node, name)| Op::ParaNodeRename { node, name }),
        arb_node().prop_map(|node| Op::ParaNodeArchive { node }),
        (arb_asset(), arb_node()).prop_map(|(asset, node)| Op::AssetParaSet { asset, node }),
        (
            arb_asset(),
            arb_volume(),
            "[p-r]{1,3}",
            "[x-z]{1,3}",
            "[0-1]{1,2}",
            arb_verify_outcome(),
            0u64..3,
        )
            .prop_map(|(asset, volume, path, algo, value, outcome, hashdate_ms)| {
                Op::VerificationRecorded {
                    asset,
                    volume,
                    path,
                    algo,
                    value,
                    outcome,
                    hashdate_ms,
                }
            }),
        (arb_volume(), "[p-r]{1,3}", 1u32..3, "[x-z]{1,3}").prop_map(
            |(volume, mhl_path, generation, roothash)| Op::ManifestRecorded {
                volume,
                mhl_path,
                generation,
                roothash,
            }
        ),
    ]
}

fn dump(db: &SqliteCatalog) -> String {
    db.debug_dump().expect("dump")
}

fn open_all(db_path: &Path, log: &FileEventLog) -> SqliteCatalog {
    let (db, _projection, _mode) =
        SqliteCatalog::open_synced(db_path, log, &mut |_| {}).expect("open");
    db
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn incremental_equals_full_rebuild(
        ops in prop::collection::vec(arb_op(), 1..40),
        split in 0usize..40,
    ) {
        let split = split.min(ops.len());
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let m1 = MachineId("m1".into());
        let mut log = FileEventLog::init(&root, &m1).expect("init");
        let events: Vec<Event> = ops.iter().enumerate()
            .map(|(i, op)| ev(u64::try_from(i).expect("small") + 1, "m1", op.clone()))
            .collect();

        // Incremental path: open after the prefix, then again after the rest.
        let inc_db = dir.path().join("inc.db");
        log.append(&events[..split]).expect("append prefix");
        drop(open_all(&inc_db, &log));
        log.append(&events[split..]).expect("append rest");
        let (db_inc, proj_inc, mode) = SqliteCatalog::open_synced(&inc_db, &log, &mut |_| {}).expect("open");
        if split < events.len() {
            prop_assert!(matches!(mode, ApplyMode::Incremental { .. }),
                "second open must take the incremental path, got {mode:?}");
        }

        // Full-rebuild path: fresh db over the same complete log.
        let full_db = dir.path().join("full.db");
        let (db_full, proj_full, _) = SqliteCatalog::open_synced(&full_db, &log, &mut |_| {}).expect("open");

        prop_assert_eq!(proj_inc, proj_full);
        prop_assert_eq!(dump(&db_inc), dump(&db_full));
    }
}

#[test]
fn zero_new_events_is_a_noop_incremental_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let mut log = FileEventLog::init(&root, &m1).expect("init");
    log.append(&[ev(
        1,
        "m1",
        Op::VolumeSeen {
            volume: "v".into(),
            label: "V".into(),
        },
    )])
    .expect("append");
    let db_path = dir.path().join("catalog.db");
    let (db1, _p1, mode1) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 1");
    assert_eq!(mode1, ApplyMode::FullRebuild);
    drop(db1);
    let (_db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 2");
    assert_eq!(mode2, ApplyMode::Incremental { applied: 0 });
}

#[test]
fn corrupt_snapshot_falls_back_to_full_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let mut log = FileEventLog::init(&root, &m1).expect("init");
    log.append(&[ev(
        1,
        "m1",
        Op::VolumeSeen {
            volume: "v".into(),
            label: "V".into(),
        },
    )])
    .expect("append");
    let db_path = dir.path().join("catalog.db");
    let (db1, _p1, _mode1) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 1");
    drop(db1);

    let raw = rusqlite::Connection::open(&db_path).expect("raw open");
    raw.execute("UPDATE apply_snapshot SET projection = 'garbage'", [])
        .expect("corrupt snapshot");
    drop(raw);

    let (db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 2");
    assert_eq!(mode2, ApplyMode::FullRebuild);

    let fresh_db_path = dir.path().join("fresh.db");
    let (db_fresh, _p3, _mode3) =
        SqliteCatalog::open_synced(&fresh_db_path, &log, &mut |_| {}).expect("open fresh");
    assert_eq!(dump(&db2), dump(&db_fresh));
}
