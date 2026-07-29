//! `SQLite` projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection` (incremental apply and
//! FTS5/sqlite-vec arrive in later phases).
use majestical_core::event::{AssetId, ParaKind, VerifyOutcome};
use majestical_core::ports::{CatalogStore, PortError};
use majestical_core::projection::Projection;
use rusqlite::{Connection, Transaction};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog db: {0} — delete the file and re-run")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct SqliteCatalog {
    conn: Connection,
}

impl SqliteCatalog {
    /// Opens (creating if needed) the catalog database at `path`. Does not
    /// populate it — call `rebuild` to (re)populate the projection tables.
    ///
    /// # Errors
    /// Returns an error if the database file can't be opened.
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        let conn = Connection::open(path)?;
        Ok(Self { conn })
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
        tx.commit()?;

        Ok(())
    }

    fn create_tables(tx: &Transaction) -> rusqlite::Result<()> {
        tx.execute_batch(
            "DROP TABLE IF EXISTS tags;
             DROP TABLE IF EXISTS instances;
             DROP TABLE IF EXISTS assets;
             DROP TABLE IF EXISTS volumes;
             DROP TABLE IF EXISTS para_nodes;
             DROP TABLE IF EXISTS asset_para;
             DROP TABLE IF EXISTS verifications;
             DROP TABLE IF EXISTS manifests;
             CREATE TABLE assets (id TEXT PRIMARY KEY);
             CREATE TABLE instances (
               asset TEXT NOT NULL REFERENCES assets(id),
               volume TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
               PRIMARY KEY (asset, volume, path)
             );
             CREATE TABLE tags (
               asset TEXT NOT NULL REFERENCES assets(id),
               tag TEXT NOT NULL, PRIMARY KEY (asset, tag)
             );
             CREATE INDEX tags_by_tag ON tags (tag);
             CREATE TABLE volumes (
               id TEXT PRIMARY KEY,
               label TEXT NOT NULL,
               last_seen_ms INTEGER NOT NULL
             );
             CREATE TABLE para_nodes (
               id TEXT PRIMARY KEY, kind TEXT NOT NULL,
               name TEXT NOT NULL, archived INTEGER NOT NULL
             );
             CREATE TABLE asset_para (
               asset TEXT NOT NULL PRIMARY KEY REFERENCES assets(id),
               node TEXT NOT NULL
             );
             CREATE TABLE verifications (
               asset TEXT NOT NULL, volume TEXT NOT NULL, path TEXT NOT NULL,
               algo TEXT NOT NULL, value TEXT NOT NULL, outcome TEXT NOT NULL,
               hashdate_ms INTEGER NOT NULL
             );
             CREATE TABLE manifests (
               volume TEXT NOT NULL, generation INTEGER NOT NULL,
               mhl_path TEXT NOT NULL, roothash TEXT NOT NULL,
               PRIMARY KEY (volume, generation, mhl_path)
             );",
        )
    }

    /// Populates `assets`, `instances`, `tags`, `asset_para`, and
    /// `verifications` — everything keyed off an individual asset.
    fn insert_assets(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (asset, state) in projection.assets() {
            tx.execute("INSERT INTO assets (id) VALUES (?1)", [&asset.0])?;
            for (volume, path, size) in &state.instances {
                tx.execute(
                    "INSERT INTO instances (asset, volume, path, size) VALUES (?1, ?2, ?3, ?4)",
                    (&asset.0, volume, path, size),
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
        }
        Ok(())
    }

    fn insert_volumes(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (id, state) in projection.volumes() {
            let label = state.label().unwrap_or("");
            let last_seen_ms = state.last_seen().map_or(0, |hlc| hlc.wall_ms);
            let last_seen_ms = i64::try_from(last_seen_ms).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO volumes (id, label, last_seen_ms) VALUES (?1, ?2, ?3)",
                (id, label, last_seen_ms),
            )?;
        }
        Ok(())
    }

    fn insert_para_nodes(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (id, state) in projection.para_nodes() {
            // A node with a rename observed before its create has no kind or
            // name yet; it materializes once the create event arrives, so it
            // is skipped rather than inserted with placeholder values.
            let (Some(kind), Some(name)) = (state.kind(), state.name()) else {
                continue;
            };
            tx.execute(
                "INSERT INTO para_nodes (id, kind, name, archived) VALUES (?1, ?2, ?3, ?4)",
                (id, para_kind_wire(kind), name, state.archived()),
            )?;
        }
        Ok(())
    }

    fn insert_manifests(tx: &Transaction, projection: &Projection) -> rusqlite::Result<()> {
        for (volume, record) in projection.manifests_by_volume() {
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
        }
        Ok(())
    }

    /// Finds assets tagged with `tag` exactly.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn search_by_tag(&self, tag: &str) -> Result<Vec<AssetId>, CatalogError> {
        self.query("SELECT asset FROM tags WHERE tag = ?1 ORDER BY asset", tag)
    }

    /// Case-insensitive (ASCII only — proper Unicode folding arrives with FTS)
    /// substring match on the full instance path.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn search_by_name(&self, needle: &str) -> Result<Vec<AssetId>, CatalogError> {
        self.query(
            "SELECT DISTINCT asset FROM instances \
             WHERE path LIKE '%' || ?1 || '%' ESCAPE '\\' ORDER BY asset",
            &escape_like(needle),
        )
    }

    fn query(&self, sql: &str, param: &str) -> Result<Vec<AssetId>, CatalogError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([param], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(AssetId(row?));
        }
        Ok(out)
    }

    /// Every volume the catalog has ever seen: (id, label, `last_seen_ms`),
    /// ordered by id.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn volumes(&self) -> Result<Vec<(String, String, u64)>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, last_seen_ms FROM volumes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, label, last_seen_ms) = row?;
            out.push((id, label, u64::try_from(last_seen_ms).unwrap_or(0)));
        }
        Ok(out)
    }

    /// Every PARA node: (id, kind, name, archived), ordered by id.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn para_nodes(&self) -> Result<Vec<(String, String, String, bool)>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, name, archived FROM para_nodes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Distinct asset count per volume, derived from `instances`, ordered by
    /// volume.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT volume, COUNT(DISTINCT asset) FROM instances GROUP BY volume ORDER BY volume",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (volume, count) = row?;
            out.push((volume, u64::try_from(count).unwrap_or(0)));
        }
        Ok(out)
    }
}

/// Escapes `\`, `%`, and `_` so a user-supplied needle is matched as a
/// literal substring rather than interpreted as SQL `LIKE` wildcards.
fn escape_like(needle: &str) -> String {
    let mut escaped = String::with_capacity(needle.len());
    for c in needle.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
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

impl CatalogStore for SqliteCatalog {
    fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError> {
        Self::rebuild(self, projection).map_err(|e| PortError::new("catalog store", e))
    }

    fn search_by_tag(&self, tag: &str) -> Result<Vec<AssetId>, PortError> {
        Self::search_by_tag(self, tag).map_err(|e| PortError::new("catalog store", e))
    }

    fn search_by_name(&self, needle: &str) -> Result<Vec<AssetId>, PortError> {
        Self::search_by_name(self, needle).map_err(|e| PortError::new("catalog store", e))
    }

    fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError> {
        Self::volumes(self).map_err(|e| PortError::new("catalog store", e))
    }

    fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError> {
        Self::volume_asset_counts(self).map_err(|e| PortError::new("catalog store", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
    use majestical_core::projection::Projection;

    #[test]
    fn rebuild_then_query_by_tag_and_name() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:aa".into());
        let b = AssetId("xxh3:bb".into());
        for (n, op) in [
            Op::AssetSeen {
                asset: a.clone(),
                volume: "card1".into(),
                path: "clips/sunset.mov".into(),
                size: 42,
            },
            Op::TagAdd {
                asset: a.clone(),
                tag: "topic/drone".into(),
            },
            // Literal `%` and `_` in a path — these must be matched as literal
            // characters, not interpreted as SQL LIKE wildcards.
            Op::AssetSeen {
                asset: b.clone(),
                volume: "card1".into(),
                path: "clips/100%_off.mov".into(),
                size: 7,
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
            db.search_by_tag("topic/drone").expect("tag query"),
            vec![a.clone()]
        );
        assert_eq!(
            db.search_by_name("sunset").expect("name query"),
            vec![a.clone()]
        );
        assert_eq!(
            db.search_by_name("SUNSET").expect("case-insensitive query"),
            vec![a.clone()]
        );
        assert_eq!(
            db.search_by_name("nothing").expect("empty query"),
            Vec::<AssetId>::new()
        );
        assert_eq!(
            db.search_by_name("100%")
                .expect("literal percent must not act as a wildcard"),
            vec![b.clone()],
            "must match only the literal '100%' instance, not clips/sunset.mov"
        );
        assert_eq!(
            db.search_by_name("100%_off")
                .expect("literal percent and underscore query"),
            vec![b],
            "instance with a literal '%' and '_' must be findable by that literal"
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
            },
            Op::AssetSeen {
                asset: b.clone(),
                volume: "card1".into(),
                path: "clips/b.mov".into(),
                size: 2,
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
    fn catalog_store_trait_object_rebuilds_and_queries() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:aa".into());
        p.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(1, 0)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: a.clone(),
                tag: "topic/drone".into(),
            },
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let mut owned = SqliteCatalog::open(&dir.path().join("catalog.db")).expect("open");
        let store: &mut dyn CatalogStore = &mut owned;
        store.rebuild(&p).expect("rebuild via trait object");
        assert_eq!(
            store
                .search_by_tag("topic/drone")
                .expect("tag query via trait object"),
            vec![a]
        );
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
            db.search_by_tag("keep-a").expect("query a"),
            Vec::<AssetId>::new(),
            "rebuild must discard the previous projection's data"
        );
        assert_eq!(db.search_by_tag("keep-b").expect("query b"), vec![b]);
    }

    /// Applies `ops` in order (each event gets an increasing `wall_ms`, so
    /// HLCs are distinct) to a fresh `Projection`, then rebuilds a fresh
    /// `SqliteCatalog` at `path` from it.
    fn rebuild_from_ops(path: &std::path::Path, ops: Vec<Op>) -> SqliteCatalog {
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
}
