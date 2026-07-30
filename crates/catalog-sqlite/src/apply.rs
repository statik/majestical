//! Populating the projection tables: `rebuild`'s full drop/recreate/insert,
//! `open_synced`/`apply_touched`'s incremental resume-from-cursor path, and
//! the per-entity insert helpers both share.
//!
//! `saved_searches` has no `CatalogStore` trait method: the CLI reads saved
//! searches straight from the `Projection` it already holds (`open_catalog`
//! returns one alongside the `SqliteCatalog`), so there's no query path that
//! needs this table today. It exists anyway, kept in sync by `rebuild` and
//! `apply_touched` like every other entity, for future surfaces (a GUI or an
//! MCP server) that won't hold a `Projection` in memory, and so `debug_dump`
//! stays a complete equivalence check between the incremental and rebuild
//! paths.
use crate::{ApplyMode, CatalogError, SNAPSHOT_VERSION, SqliteCatalog};
use majestical_core::event::{AssetId, ParaKind, VerifyOutcome};
use majestical_core::media_kind::media_kind;
use majestical_core::ports::{EventLog, LogCursor};
use majestical_core::projection::{
    AssetState, ManifestRecord, ParaNodeState, Projection, Touched, VolumeState,
};
use rusqlite::Transaction;
use std::collections::BTreeSet;
use std::path::Path;

impl SqliteCatalog {
    /// Opens the catalog at `db_path` and brings it up to date with `log`,
    /// applying only the events past whatever cursor was last saved here —
    /// or, when there's no usable saved state, rebuilding from scratch.
    /// "No usable saved state" covers: the first-ever open; a missing,
    /// corrupt, or version-mismatched snapshot; and a resume read that
    /// fails for any reason, whether that's a stale cursor `log` can no
    /// longer resume from, or an I/O error reading it — in the latter case
    /// the retry costs one wasted resume attempt, and the same error
    /// propagates from the full read that follows if it isn't transient.
    /// Returns the resulting projection alongside the mode actually taken,
    /// so a caller (or a test) can tell which path ran.
    ///
    /// Corrupt log lines are skipped rather than failing the read, same as
    /// `EventLog::read_since_reporting`; `on_bad_line` is handed straight
    /// through so a caller can warn about them — this library never prints,
    /// since a disposable catalog file has no business writing to a
    /// process's stderr.
    ///
    /// # Errors
    /// Returns an error if the database can't be opened, the log can't be
    /// read, or a write fails.
    pub fn open_synced(
        db_path: &Path,
        log: &dyn EventLog,
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<(Self, Projection, ApplyMode), CatalogError> {
        let mut db = Self::open(db_path)?;
        if let Some((cursors, mut projection)) = db.load_apply_state() {
            match log.read_since_reporting(&cursors, on_bad_line) {
                Ok((events, new_cursors)) => {
                    if events.is_empty() {
                        return Ok((db, projection, ApplyMode::Incremental { applied: 0 }));
                    }
                    let mut touched = BTreeSet::new();
                    for event in &events {
                        touched.insert(projection.apply_tracking(event));
                    }
                    touched.remove(&Touched::Nothing);
                    let applied = events.len();
                    db.apply_touched(&projection, &touched, &new_cursors)?;
                    return Ok((db, projection, ApplyMode::Incremental { applied }));
                }
                Err(_resume_failed) => { /* fall through to full rebuild */ }
            }
        }
        let (events, cursors) = log.read_since_reporting(&[], on_bad_line)?;
        let mut projection = Projection::default();
        for event in &events {
            projection.apply(event);
        }
        db.rebuild(&projection)?;
        db.save_apply_state(&cursors, &projection)?;
        Ok((db, projection, ApplyMode::FullRebuild))
    }

    /// Recreates the catalog's projection tables from `projection`, wholesale.
    ///
    /// Any existing tables are dropped first — the database is a disposable
    /// projection, not a source of truth. The drop, create, and every insert
    /// run inside one transaction, so a failure partway through leaves the
    /// previous catalog intact for readers instead of an emptied database.
    ///
    /// # Errors
    /// Returns an error if a table can't be (re)created or a write fails.
    pub fn rebuild(&mut self, projection: &Projection) -> Result<(), CatalogError> {
        let tx = self.conn.transaction()?;
        Self::create_tables(&tx)?;
        Self::insert_assets(&tx, projection)?;
        Self::insert_volumes(&tx, projection)?;
        Self::insert_para_nodes(&tx, projection)?;
        Self::insert_manifests(&tx, projection)?;
        Self::insert_saved_searches(&tx, projection)?;
        tx.commit()?;

        Ok(())
    }

    /// Applies only the touched entities to the existing tables: for each
    /// entity, its rows are deleted and, if it still exists in `projection`,
    /// reinserted from scratch — cheaper than `rebuild`'s full drop/recreate
    /// when only a handful of entities changed. Saves `cursors` and
    /// `projection` as the new apply-state snapshot in the same transaction.
    ///
    /// # Errors
    /// Returns an error if serializing the snapshot or any write fails.
    pub fn apply_touched(
        &mut self,
        projection: &Projection,
        touched: &BTreeSet<Touched>,
        cursors: &[LogCursor],
    ) -> Result<(), CatalogError> {
        let snapshot_json = serde_json::to_string(projection)?;
        let tx = self.conn.transaction()?;
        for t in touched {
            match t {
                Touched::Nothing => {}
                Touched::SavedSearch(name) => {
                    tx.execute("DELETE FROM saved_searches WHERE name = ?1", [name])?;
                    if let Some(query) = projection.saved_search(name) {
                        tx.execute(
                            "INSERT INTO saved_searches (name, query) VALUES (?1, ?2)",
                            (name, query),
                        )?;
                    }
                }
                Touched::Asset(id) => {
                    tx.execute("DELETE FROM tags WHERE asset = ?1", [&id.0])?;
                    tx.execute("DELETE FROM instances WHERE asset = ?1", [&id.0])?;
                    tx.execute("DELETE FROM names_fts WHERE asset = ?1", [&id.0])?;
                    tx.execute("DELETE FROM asset_para WHERE asset = ?1", [&id.0])?;
                    tx.execute("DELETE FROM verifications WHERE asset = ?1", [&id.0])?;
                    tx.execute("DELETE FROM assets WHERE id = ?1", [&id.0])?;
                    if let Some((_, state)) = projection.assets().find(|(a, _)| *a == id) {
                        Self::insert_one_asset(&tx, projection, id, state)?;
                    }
                }
                Touched::Volume(id) => {
                    tx.execute("DELETE FROM volumes WHERE id = ?1", [id])?;
                    if let Some((_, state)) = projection.volumes().find(|(v, _)| *v == id) {
                        Self::insert_one_volume(&tx, id, state)?;
                    }
                }
                Touched::ParaNode(id) => {
                    tx.execute("DELETE FROM para_nodes WHERE id = ?1", [id])?;
                    if let Some(state) = projection.para_node(id) {
                        Self::insert_one_para_node(&tx, id, state)?;
                    }
                }
                Touched::Manifests(volume) => {
                    tx.execute("DELETE FROM manifests WHERE volume = ?1", [volume])?;
                    Self::insert_manifests_for(&tx, projection, volume)?;
                }
            }
        }
        Self::write_apply_state(&tx, cursors, &snapshot_json)?;
        tx.commit()?;
        Ok(())
    }

    /// Reads the last-saved (cursors, projection) pair, or `None` if there is
    /// none usable — missing table, version mismatch, or a value that fails
    /// to parse. Any of these just means "rebuild from scratch"; the
    /// database is disposable, so none of them are logged.
    fn load_apply_state(&self) -> Option<(Vec<LogCursor>, Projection)> {
        let (version, projection_json): (i64, String) = self
            .conn
            .query_row(
                "SELECT version, projection FROM apply_snapshot WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        if version != SNAPSHOT_VERSION {
            return None;
        }
        let projection: Projection = serde_json::from_str(&projection_json).ok()?;
        let mut stmt = self
            .conn
            .prepare("SELECT machine, segment, offset FROM apply_cursors ORDER BY machine, segment")
            .ok()?;
        let rows = stmt
            .query_map([], |r| {
                let offset: i64 = r.get(2)?;
                Ok(LogCursor {
                    machine: r.get(0)?,
                    segment: r.get(1)?,
                    offset: u64::try_from(offset).unwrap_or(0),
                })
            })
            .ok()?;
        let mut cursors = Vec::new();
        for row in rows {
            cursors.push(row.ok()?);
        }
        Some((cursors, projection))
    }

    /// Writes `cursors` and `projection` as the new apply-state snapshot, in
    /// one transaction.
    fn save_apply_state(
        &mut self,
        cursors: &[LogCursor],
        projection: &Projection,
    ) -> Result<(), CatalogError> {
        let snapshot_json = serde_json::to_string(projection)?;
        let tx = self.conn.transaction()?;
        Self::write_apply_state(&tx, cursors, &snapshot_json)?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces `apply_cursors` and upserts the single `apply_snapshot` row.
    /// Shared by `save_apply_state` and `apply_touched`, both of which need
    /// this as one step inside a larger transaction.
    fn write_apply_state(
        tx: &Transaction,
        cursors: &[LogCursor],
        snapshot_json: &str,
    ) -> rusqlite::Result<()> {
        tx.execute("DELETE FROM apply_cursors", [])?;
        for c in cursors {
            let offset = i64::try_from(c.offset).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO apply_cursors (machine, segment, offset) VALUES (?1, ?2, ?3)",
                (&c.machine, &c.segment, offset),
            )?;
        }
        tx.execute(
            "INSERT INTO apply_snapshot (id, version, projection) VALUES (1, ?1, ?2) \
             ON CONFLICT (id) DO UPDATE SET version = excluded.version, \
             projection = excluded.projection",
            (SNAPSHOT_VERSION, snapshot_json),
        )?;
        Ok(())
    }

    /// Populates `assets`, `instances`, `tags`, `asset_para`, and
    /// `verifications` — everything keyed off an individual asset.
    fn insert_assets(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (asset, state) in projection.assets() {
            Self::insert_one_asset(tx, projection, asset, state)?;
        }
        Ok(())
    }

    /// Inserts one asset's row plus every `instances`/`tags`/`asset_para`/
    /// `verifications` row derived from it. Shared by the bulk `rebuild` path
    /// and incremental apply's per-asset delete-and-reinsert.
    fn insert_one_asset(
        tx: &Transaction,
        projection: &Projection,
        asset: &AssetId,
        state: &AssetState,
    ) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO assets (id) VALUES (?1)", [&asset.0])?;
        let mut names = BTreeSet::new();
        for ((volume, path), info) in &state.instances {
            tx.execute(
                "INSERT INTO instances (asset, volume, path, size, mtime_ms, kind) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (
                    &asset.0,
                    volume,
                    path,
                    info.size,
                    info.mtime_ms,
                    media_kind(path).as_str(),
                ),
            )?;
            if let Some(name) = path.rsplit('/').next() {
                names.insert(name.to_string());
            }
        }
        for name in &names {
            tx.execute(
                "INSERT INTO names_fts (name, asset) VALUES (?1, ?2)",
                (name, &asset.0),
            )?;
        }
        for tag in projection.tags(asset) {
            tx.execute(
                "INSERT INTO tags (asset, tag) VALUES (?1, ?2)",
                (&asset.0, &tag),
            )?;
        }
        if let Some(node) = projection.asset_para(asset) {
            tx.execute(
                "INSERT INTO asset_para (asset, node) VALUES (?1, ?2)",
                (&asset.0, node),
            )?;
        }
        for record in projection.verifications(asset) {
            let hashdate_ms = i64::try_from(record.hashdate_ms).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO verifications \
                 (asset, volume, path, algo, value, outcome, hashdate_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    &asset.0,
                    &record.volume,
                    &record.path,
                    &record.algo,
                    &record.value,
                    verify_outcome_wire(record.outcome),
                    hashdate_ms,
                ),
            )?;
        }
        Ok(())
    }

    fn insert_volumes(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (id, state) in projection.volumes() {
            Self::insert_one_volume(tx, id, state)?;
        }
        Ok(())
    }

    fn insert_one_volume(tx: &Transaction, id: &str, state: &VolumeState) -> rusqlite::Result<()> {
        let label = state.label().unwrap_or("");
        let last_seen_ms = state.last_seen().map_or(0, |hlc| hlc.wall_ms);
        let last_seen_ms = i64::try_from(last_seen_ms).unwrap_or(i64::MAX);
        tx.execute(
            "INSERT INTO volumes (id, label, last_seen_ms) VALUES (?1, ?2, ?3)",
            (id, label, last_seen_ms),
        )?;
        Ok(())
    }

    fn insert_para_nodes(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (id, state) in projection.para_nodes() {
            Self::insert_one_para_node(tx, id, state)?;
        }
        Ok(())
    }

    fn insert_one_para_node(
        tx: &Transaction,
        id: &str,
        state: &ParaNodeState,
    ) -> rusqlite::Result<()> {
        // A node with a rename observed before its create has no kind or
        // name yet; it materializes once the create event arrives, so it
        // is skipped rather than inserted with placeholder values.
        let (Some(kind), Some(name)) = (state.kind(), state.name()) else {
            return Ok(());
        };
        tx.execute(
            "INSERT INTO para_nodes (id, kind, name, archived) VALUES (?1, ?2, ?3, ?4)",
            (id, para_kind_wire(kind), name, state.archived()),
        )?;
        Ok(())
    }

    fn insert_manifests(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        let mut volumes = BTreeSet::new();
        for (volume, _) in projection.all_manifests() {
            volumes.insert(volume.clone());
        }
        for volume in volumes {
            Self::insert_manifests_for(tx, projection, &volume)?;
        }
        Ok(())
    }

    /// Inserts every manifest generation recorded for one volume. Shared by
    /// the bulk `rebuild` path and incremental apply's per-volume
    /// delete-and-reinsert on `Touched::Manifests`.
    fn insert_manifests_for(
        tx: &Transaction,
        projection: &Projection,
        volume: &str,
    ) -> rusqlite::Result<()> {
        for record in projection.manifests(volume) {
            Self::insert_one_manifest(tx, volume, record)?;
        }
        Ok(())
    }

    fn insert_one_manifest(
        tx: &Transaction,
        volume: &str,
        record: &ManifestRecord,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO manifests (volume, generation, mhl_path, roothash) \
             VALUES (?1, ?2, ?3, ?4)",
            (
                volume,
                record.generation,
                &record.mhl_path,
                &record.roothash,
            ),
        )?;
        Ok(())
    }

    /// Populates `saved_searches` from the projection's full set. Shared by
    /// the bulk `rebuild` path; incremental apply instead deletes and
    /// reinserts one row at a time per `Touched::SavedSearch`.
    fn insert_saved_searches(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (name, query) in projection.saved_searches() {
            tx.execute(
                "INSERT INTO saved_searches (name, query) VALUES (?1, ?2)",
                (name, query),
            )?;
        }
        Ok(())
    }

    /// Dumps every projection table's rows (not `apply_cursors`/
    /// `apply_snapshot` — cursors legitimately differ between an incremental
    /// open and a full rebuild over the same log), each row ordered by its
    /// full column list for a deterministic string. A test/debug aid for
    /// comparing two catalogs' contents, not a stable format — callers must
    /// not parse it.
    ///
    /// # Errors
    /// Returns an error if a query fails.
    pub fn debug_dump(&self) -> Result<String, CatalogError> {
        let tables: [(&str, &str); 10] = [
            ("assets", "id"),
            ("instances", "asset, volume, path, size, mtime_ms, kind"),
            ("tags", "asset, tag"),
            ("volumes", "id, label, last_seen_ms"),
            ("para_nodes", "id, kind, name, archived"),
            ("asset_para", "asset, node"),
            (
                "verifications",
                "asset, volume, path, algo, value, outcome, hashdate_ms",
            ),
            ("manifests", "volume, generation, mhl_path, roothash"),
            ("names_fts", "asset, name"),
            ("saved_searches", "name, query"),
        ];
        let mut out = String::new();
        for (table, cols) in tables {
            let n = cols.split(',').count();
            let sql = format!("SELECT {cols} FROM {table} ORDER BY {cols}");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([], |r| {
                let mut cells = Vec::with_capacity(n);
                for i in 0..n {
                    let value: rusqlite::types::Value = r.get(i)?;
                    cells.push(format!("{value:?}"));
                }
                Ok(cells.join("|"))
            })?;
            for row in rows {
                out.push_str(table);
                out.push('|');
                out.push_str(&row?);
                out.push('\n');
            }
        }
        Ok(out)
    }
}

/// The wire string for a `ParaKind`. Matches `Op::ParaNodeCreate`'s
/// `serde(rename_all = "snake_case")` encoding — pinned by
/// `event::tests::para_and_ingest_ops_wire_formats_are_stable` — so a stored
/// value round-trips identically to what an event carried.
fn para_kind_wire(kind: ParaKind) -> &'static str {
    match kind {
        ParaKind::Project => "project",
        ParaKind::Area => "area",
        ParaKind::Resource => "resource",
        ParaKind::Archive => "archive",
    }
}

/// The wire string for a `VerifyOutcome`. Matches
/// `Op::VerificationRecorded`'s `serde(rename_all = "snake_case")` encoding —
/// pinned by the same golden test as `para_kind_wire`.
fn verify_outcome_wire(outcome: VerifyOutcome) -> &'static str {
    match outcome {
        VerifyOutcome::Original => "original",
        VerifyOutcome::Verified => "verified",
        VerifyOutcome::Failed => "failed",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{Event, EventId, Op};
    use majestical_core::ports::Filter;

    #[test]
    fn rebuild_then_query_by_tag() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:aa".into());
        let b = AssetId("xxh3:bb".into());
        for (n, op) in [
            Op::AssetSeen {
                asset: a.clone(),
                volume: "card1".into(),
                path: "clips/sunset.mov".into(),
                size: 42,
                mtime_ms: 0,
            },
            Op::TagAdd {
                asset: a.clone(),
                tag: "topic/drone".into(),
            },
            Op::AssetSeen {
                asset: b.clone(),
                volume: "card1".into(),
                path: "clips/other.mov".into(),
                size: 7,
                mtime_ms: 0,
            },
        ]
        .into_iter()
        .enumerate()
        {
            p.apply(&Event {
                id: EventId(ulid::Ulid::from_parts(1, n as u128)),
                hlc: Hlc {
                    wall_ms: 1,
                    counter: u32::try_from(n).expect("small"),
                    machine: MachineId("m1".into()),
                },
                author: "t".into(),
                op,
            });
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = SqliteCatalog::open(&dir.path().join("catalog.db")).expect("open");
        db.rebuild(&p).expect("rebuild");
        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "topic/drone".into(),
                negated: false,
            }])
            .expect("tag query"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "nothing".into(),
                negated: false,
            }])
            .expect("empty tag query"),
            BTreeSet::new()
        );
    }

    #[test]
    fn rebuild_populates_volumes_and_asset_counts() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:aa".into());
        let b = AssetId("xxh3:bb".into());
        for (n, op) in [
            Op::VolumeSeen {
                volume: "card1".into(),
                label: "card-a".into(),
            },
            Op::AssetSeen {
                asset: a.clone(),
                volume: "card1".into(),
                path: "clips/a.mov".into(),
                size: 1,
                mtime_ms: 0,
            },
            Op::AssetSeen {
                asset: b.clone(),
                volume: "card1".into(),
                path: "clips/b.mov".into(),
                size: 2,
                mtime_ms: 0,
            },
        ]
        .into_iter()
        .enumerate()
        {
            p.apply(&Event {
                id: EventId(ulid::Ulid::from_parts(1, n as u128)),
                hlc: Hlc {
                    wall_ms: u64::try_from(n).expect("small") + 1,
                    counter: 0,
                    machine: MachineId("m1".into()),
                },
                author: "t".into(),
                op,
            });
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = SqliteCatalog::open(&dir.path().join("catalog.db")).expect("open");
        db.rebuild(&p).expect("rebuild");

        let volumes = db.volumes().expect("volumes query");
        assert_eq!(
            volumes,
            vec![("card1".to_string(), "card-a".to_string(), 1)]
        );

        let counts = db.volume_asset_counts().expect("counts query");
        assert_eq!(counts, vec![("card1".to_string(), 2)]);
    }

    #[test]
    fn rebuild_discards_previous_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");
        let mut db = SqliteCatalog::open(&path).expect("open");

        let a = AssetId("xxh3:aa".into());
        let mut p1 = Projection::default();
        p1.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(1, 0)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: a.clone(),
                tag: "keep-a".into(),
            },
        });
        db.rebuild(&p1).expect("rebuild a");

        let b = AssetId("xxh3:bb".into());
        let mut p2 = Projection::default();
        p2.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(2, 0)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: b.clone(),
                tag: "keep-b".into(),
            },
        });
        db.rebuild(&p2).expect("rebuild b");

        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "keep-a".into(),
                negated: false,
            }])
            .expect("query a"),
            BTreeSet::new(),
            "rebuild must discard the previous projection's data"
        );
        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "keep-b".into(),
                negated: false,
            }])
            .expect("query b"),
            BTreeSet::from([b])
        );
    }

    /// Applies `ops` in order (each event gets an increasing `wall_ms`, so
    /// HLCs are distinct) to a fresh `Projection`, then rebuilds a fresh
    /// `SqliteCatalog` at `path` from it.
    pub(crate) fn rebuild_from_ops(path: &std::path::Path, ops: Vec<Op>) -> SqliteCatalog {
        let mut p = Projection::default();
        for (n, op) in ops.into_iter().enumerate() {
            p.apply(&Event {
                id: EventId(ulid::Ulid::from_parts(1, n as u128)),
                hlc: Hlc {
                    wall_ms: u64::try_from(n).expect("small") + 1,
                    counter: 0,
                    machine: MachineId("m1".into()),
                },
                author: "t".into(),
                op,
            });
        }
        let mut db = SqliteCatalog::open(path).expect("open");
        db.rebuild(&p).expect("rebuild");
        db
    }

    #[test]
    fn rebuild_populates_para_nodes_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::ParaNodeCreate {
                node: "N1".into(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            }],
        );
        assert_eq!(
            db.para_nodes().expect("para query"),
            vec![(
                "N1".to_string(),
                "project".to_string(),
                "client-x".to_string(),
                false
            )]
        );
    }

    #[test]
    fn rebuild_populates_asset_para_table() {
        let asset = AssetId("xxh3:aa".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: asset.clone(),
                    volume: "V1".into(),
                    path: "a.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetParaSet {
                    asset: asset.clone(),
                    node: "N1".into(),
                },
            ],
        );
        // Direct row query — a skipped `asset_para` insert leaves the table
        // empty, which this discriminates as a query failure rather than
        // silently reading through a table checked elsewhere.
        let row: (String, String) = db
            .conn
            .query_row("SELECT asset, node FROM asset_para", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("asset_para row");
        assert_eq!(row, ("xxh3:aa".to_string(), "N1".to_string()));
    }

    #[test]
    fn rebuild_populates_verifications_table() {
        let asset = AssetId("xxh3:aa".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: asset.clone(),
                    volume: "V1".into(),
                    path: "a.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::VerificationRecorded {
                    asset: asset.clone(),
                    volume: "V1".into(),
                    path: "a.mov".into(),
                    algo: "xxh64".into(),
                    value: "00".into(),
                    outcome: VerifyOutcome::Verified,
                    hashdate_ms: 42,
                },
            ],
        );
        let row: (String, String, String, String, String, String, i64) = db
            .conn
            .query_row(
                "SELECT asset, volume, path, algo, value, outcome, hashdate_ms \
                 FROM verifications",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .expect("verifications row");
        assert_eq!(
            row,
            (
                "xxh3:aa".to_string(),
                "V1".to_string(),
                "a.mov".to_string(),
                "xxh64".to_string(),
                "00".to_string(),
                "verified".to_string(),
                42,
            )
        );
    }

    #[test]
    fn rebuild_populates_manifests_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::ManifestRecorded {
                volume: "V1".into(),
                mhl_path: "ascmhl/0001_dest_x.mhl".into(),
                generation: 1,
                roothash: "xxh64:aa".into(),
            }],
        );
        let row: (String, i64, String, String) = db
            .conn
            .query_row(
                "SELECT volume, generation, mhl_path, roothash FROM manifests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("manifests row");
        assert_eq!(
            row,
            (
                "V1".to_string(),
                1,
                "ascmhl/0001_dest_x.mhl".to_string(),
                "xxh64:aa".to_string()
            )
        );
    }

    #[test]
    fn rebuild_keeps_manifests_that_differ_only_in_roothash() {
        // Same (volume, generation, mhl_path) with a different roothash is
        // exactly what roothash exists to catch — a tampered or re-recorded
        // manifest — so it must not collide with the earlier record and
        // abort the whole rebuild transaction.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::ManifestRecorded {
                    volume: "V1".into(),
                    mhl_path: "ascmhl/0001_dest_x.mhl".into(),
                    generation: 1,
                    roothash: "xxh64:aa".into(),
                },
                Op::ManifestRecorded {
                    volume: "V1".into(),
                    mhl_path: "ascmhl/0001_dest_x.mhl".into(),
                    generation: 1,
                    roothash: "xxh64:bb".into(),
                },
            ],
        );
        let mut stmt = db
            .conn
            .prepare("SELECT roothash FROM manifests ORDER BY roothash")
            .expect("prepare");
        let roothashes: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            roothashes,
            vec!["xxh64:aa".to_string(), "xxh64:bb".to_string()]
        );
    }

    #[test]
    fn fts_rows_follow_incremental_asset_updates() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "old_name.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );

        let mut renamed = Projection::default();
        renamed.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(2, 0)),
            hlc: Hlc {
                wall_ms: 2,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "new_name.mov".into(),
                size: 1,
                mtime_ms: 0,
            },
        });
        db.apply_touched(&renamed, &BTreeSet::from([Touched::Asset(a.clone())]), &[])
            .expect("apply touched");

        assert!(
            db.search_names_ranked(&["old".to_string()], 10)
                .expect("search old")
                .is_empty(),
            "the old name must no longer be indexed"
        );
        let hits = db
            .search_names_ranked(&["new".to_string()], 10)
            .expect("search new");
        assert_eq!(
            hits.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![a]
        );
    }

    /// A `SavedSearchSet` applied incrementally, then a `SavedSearchRemove`
    /// applied incrementally on top, must each leave the catalog identical to
    /// a fresh rebuild from the same op history up to that point.
    #[test]
    fn saved_searches_follow_incremental_set_and_remove() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = SqliteCatalog::open(&dir.path().join("catalog.db")).expect("open");
        db.rebuild(&Projection::default()).expect("initial rebuild");

        let mut projection = Projection::default();
        projection.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(1, 0)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::SavedSearchSet {
                name: "keepers".into(),
                query: "tag:keep".into(),
            },
        });
        db.apply_touched(
            &projection,
            &BTreeSet::from([Touched::SavedSearch("keepers".into())]),
            &[],
        )
        .expect("apply set");

        let fresh_after_set = rebuild_from_ops(
            &dir.path().join("fresh1.db"),
            vec![Op::SavedSearchSet {
                name: "keepers".into(),
                query: "tag:keep".into(),
            }],
        );
        assert_eq!(
            db.debug_dump().expect("dump"),
            fresh_after_set.debug_dump().expect("dump")
        );

        projection.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(2, 0)),
            hlc: Hlc {
                wall_ms: 2,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::SavedSearchRemove {
                name: "keepers".into(),
            },
        });
        db.apply_touched(
            &projection,
            &BTreeSet::from([Touched::SavedSearch("keepers".into())]),
            &[],
        )
        .expect("apply remove");

        let fresh_after_remove = rebuild_from_ops(
            &dir.path().join("fresh2.db"),
            vec![
                Op::SavedSearchSet {
                    name: "keepers".into(),
                    query: "tag:keep".into(),
                },
                Op::SavedSearchRemove {
                    name: "keepers".into(),
                },
            ],
        );
        let dump = db.debug_dump().expect("dump");
        assert_eq!(dump, fresh_after_remove.debug_dump().expect("dump"));
        assert!(
            !dump.contains("saved_searches|"),
            "the saved search row must be gone after the incremental remove, got: {dump}"
        );
    }
}
