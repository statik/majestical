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

fn arb_saved_search_name() -> impl Strategy<Value = String> {
    prop_oneof![Just("s1".to_string()), Just("s2".to_string())]
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
        (
            arb_asset(),
            arb_volume(),
            "[p-r]{1,3}",
            0u64..100,
            0u64..100
        )
            .prop_map(|(asset, volume, path, size, mtime_ms)| Op::AssetSeen {
                asset,
                volume,
                path,
                size,
                mtime_ms
            }),
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
        (arb_saved_search_name(), "[x-z]{1,3}")
            .prop_map(|(name, query)| Op::SavedSearchSet { name, query }),
        arb_saved_search_name().prop_map(|name| Op::SavedSearchRemove { name }),
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

/// Draws `split` from the generated vector's own length, strictly less than
/// it, so the second append is never empty and every case exercises a
/// genuine incremental step. An independent `0..40` range for `split` (as
/// this used to be, clamped with `.min(ops.len())`) let `split == ops.len()`
/// degrade to comparing two full rebuilds instead of an incremental apply
/// against one, roughly half the time.
fn arb_ops_and_split() -> impl Strategy<Value = (Vec<Op>, usize)> {
    prop::collection::vec(arb_op(), 1..40).prop_flat_map(|ops| {
        let len = ops.len();
        (Just(ops), 0..len)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn incremental_equals_full_rebuild((ops, split) in arb_ops_and_split()) {
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
        let (db_inc, proj_inc, mode) =
            SqliteCatalog::open_synced(&inc_db, &log, &mut |_| {}).expect("open");
        prop_assert!(matches!(mode, ApplyMode::Incremental { .. }),
            "second open must take the incremental path, got {mode:?}");

        // Full-rebuild path: fresh db over the same complete log.
        let full_db = dir.path().join("full.db");
        let (db_full, proj_full, _) =
            SqliteCatalog::open_synced(&full_db, &log, &mut |_| {}).expect("open");

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

/// The saved cursor must reflect the position *after* the events an
/// incremental open just consumed — not the position it read from. Without
/// that, every open would keep re-reading (and idempotently re-applying)
/// everything past the original stale cursor forever, silently degrading to
/// an O(log size) read on every open while still reporting `Incremental`.
#[test]
fn incremental_open_advances_its_cursor_so_a_later_open_sees_nothing_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let mut log = FileEventLog::init(&root, &m1).expect("init");
    let db_path = dir.path().join("catalog.db");

    let (db1, _p1, mode1) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 1");
    assert_eq!(mode1, ApplyMode::FullRebuild);
    drop(db1);

    log.append(&[ev(
        1,
        "m1",
        Op::VolumeSeen {
            volume: "v".into(),
            label: "V".into(),
        },
    )])
    .expect("append");

    let (db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 2");
    assert_eq!(mode2, ApplyMode::Incremental { applied: 1 });
    drop(db2);

    let (_db3, _p3, mode3) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 3");
    assert_eq!(
        mode3,
        ApplyMode::Incremental { applied: 0 },
        "a third open with nothing new appended must see zero new events, \
         not re-read the event already applied by the second open"
    );
}

/// A snapshot with a version this build doesn't recognize must be treated
/// exactly like a missing one — full rebuild, not a crash and not a
/// misparse of a shape the code no longer understands.
#[test]
fn a_snapshot_with_an_unrecognized_version_falls_back_to_full_rebuild() {
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
    raw.execute("UPDATE apply_snapshot SET version = 999", [])
        .expect("bump version");
    drop(raw);

    let (_db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 2");
    assert_eq!(mode2, ApplyMode::FullRebuild);
}

/// The `on_bad_line` callback must fire for corrupt lines the incremental
/// branch reads too, not only the full-rebuild branch (which is all the CLI
/// smoke tests exercise, since they never get far enough into a catalog's
/// life for a second, incremental open).
#[test]
fn incremental_open_reports_a_corrupt_line_past_the_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let log = FileEventLog::init(&root, &m1).expect("init");
    let db_path = dir.path().join("catalog.db");

    let (db1, _p1, _mode1) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 1");
    drop(db1);

    let seg = root.join("events").join("m1").join("0001.jsonl");
    let good = serde_json::to_string(&ev(
        1,
        "m1",
        Op::VolumeSeen {
            volume: "v".into(),
            label: "V".into(),
        },
    ))
    .expect("serialize");
    std::fs::write(&seg, format!("not json\n{good}\n")).expect("write segment directly");

    let mut bad_lines = Vec::new();
    let (_db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |line| bad_lines.push(line.to_string()))
            .expect("open 2");
    assert_eq!(
        bad_lines,
        vec!["not json".to_string()],
        "the corrupt line must be reported exactly once by the incremental branch"
    );
    assert_eq!(mode2, ApplyMode::Incremental { applied: 1 });
}

/// No generated proptest op ever removes rows (`TagRemove`'s `observed` list
/// is always empty there), so the property test never exercises the
/// delete-then-reinsert-fewer-rows path for `Touched::Asset`. This pins it
/// directly: an incremental apply that untags an asset must leave the tag
/// row gone, not just fail to add a new one.
#[test]
fn incremental_apply_removes_a_row_when_a_later_event_untags_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let mut log = FileEventLog::init(&root, &m1).expect("init");
    let asset = AssetId("xxh3:a".into());
    let seen = ev(
        1,
        "m1",
        Op::AssetSeen {
            asset: asset.clone(),
            volume: "v".into(),
            path: "p".into(),
            size: 1,
            mtime_ms: 0,
        },
    );
    let tag_add = ev(
        2,
        "m1",
        Op::TagAdd {
            asset: asset.clone(),
            tag: "t".into(),
        },
    );
    log.append(&[seen, tag_add.clone()]).expect("append prefix");

    let db_path = dir.path().join("catalog.db");
    drop(open_all(&db_path, &log));

    let tag_remove = ev(
        3,
        "m1",
        Op::TagRemove {
            asset: asset.clone(),
            tag: "t".into(),
            observed: vec![tag_add.id],
        },
    );
    log.append(&[tag_remove]).expect("append remove");

    let (db, _p, mode) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open incremental");
    assert_eq!(mode, ApplyMode::Incremental { applied: 1 });
    assert!(
        !dump(&db).contains("tags|"),
        "the tag row must be gone after the incremental untag, got: {}",
        dump(&db)
    );

    let fresh_db_path = dir.path().join("fresh.db");
    let (db_fresh, _p2, _mode2) =
        SqliteCatalog::open_synced(&fresh_db_path, &log, &mut |_| {}).expect("open fresh");
    assert_eq!(dump(&db), dump(&db_fresh));
}

/// `text_fts` rows come from blobs (`maj index run`), never from CRDT
/// events, so an incremental apply touching an asset (here, tagging it)
/// must leave that asset's indexed text alone — unlike `names_fts`, which
/// `Touched::Asset` does delete and reinsert.
#[test]
fn incremental_apply_preserves_text_fts_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cat");
    let m1 = MachineId("m1".into());
    let mut log = FileEventLog::init(&root, &m1).expect("init");
    let asset = AssetId("xxh3:a".into());
    log.append(&[ev(
        1,
        "m1",
        Op::AssetSeen {
            asset: asset.clone(),
            volume: "v".into(),
            path: "clip.mov".into(),
            size: 1,
            mtime_ms: 0,
        },
    )])
    .expect("append seen");

    let db_path = dir.path().join("catalog.db");
    let (mut db, _p1, mode1) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 1");
    assert_eq!(mode1, ApplyMode::FullRebuild);

    db.upsert_text_rows(
        &asset,
        "transcript",
        &[(0, "we discussed the quarterly budget")],
    )
    .expect("upsert text row");
    drop(db);

    log.append(&[ev(
        2,
        "m1",
        Op::TagAdd {
            asset: asset.clone(),
            tag: "keep".into(),
        },
    )])
    .expect("append tag");

    let (db2, _p2, mode2) =
        SqliteCatalog::open_synced(&db_path, &log, &mut |_| {}).expect("open 2");
    assert_eq!(
        mode2,
        ApplyMode::Incremental { applied: 1 },
        "this test is vacuous unless the second open takes the incremental path"
    );

    let hits = db2
        .search_text_ranked(&["budget".to_string()], None, 10)
        .expect("search after incremental apply");
    assert_eq!(
        hits.len(),
        1,
        "the text row must survive an incremental apply that touches its asset"
    );
    assert_eq!(hits[0].asset, asset);
}
