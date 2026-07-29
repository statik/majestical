//! `SQLite` projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection` (incremental apply and
//! FTS5/sqlite-vec arrive in later phases).
use majestical_core::event::AssetId;
use majestical_core::ports::{CatalogStore, PortError};
use majestical_core::projection::Projection;
use rusqlite::Connection;
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
        tx.execute_batch(
            "DROP TABLE IF EXISTS tags;
             DROP TABLE IF EXISTS instances;
             DROP TABLE IF EXISTS assets;
             DROP TABLE IF EXISTS volumes;
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
             );",
        )?;

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
        }
        for (id, state) in projection.volumes() {
            let label = state.label().unwrap_or("");
            let last_seen_ms = state.last_seen().map_or(0, |hlc| hlc.wall_ms);
            let last_seen_ms = i64::try_from(last_seen_ms).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO volumes (id, label, last_seen_ms) VALUES (?1, ?2, ?3)",
                (id, label, last_seen_ms),
            )?;
        }
        tx.commit()?;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};
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
}
