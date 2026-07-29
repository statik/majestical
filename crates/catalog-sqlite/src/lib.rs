//! `SQLite` projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection` (incremental apply and
//! FTS5/sqlite-vec arrive in later phases).
use majestical_core::event::AssetId;
use majestical_core::projection::Projection;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog db: {0} — delete the file and rebuild from the event log")]
    Sqlite(#[from] rusqlite::Error),
    #[error("removing stale catalog at {}: {source} — close other apps using it and retry", path.display())]
    RemoveStale {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub struct SqliteCatalog {
    conn: Connection,
}

impl SqliteCatalog {
    /// Recreates the catalog database at `path` from `projection`, wholesale.
    ///
    /// Any existing file at `path` is discarded first — the database is a
    /// disposable projection, not a source of truth.
    ///
    /// # Errors
    /// Returns an error if the database file can't be opened or a write fails.
    pub fn rebuild(path: &Path, projection: &Projection) -> Result<Self, CatalogError> {
        // Projection files are disposable; the event log is the truth. A
        // missing file is fine (nothing to discard); any other failure
        // (permissions, the file being open elsewhere) must propagate rather
        // than silently rebuilding on top of stale data.
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CatalogError::RemoveStale {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE assets (id TEXT PRIMARY KEY);
             CREATE TABLE instances (
               asset TEXT NOT NULL REFERENCES assets(id),
               volume TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
               PRIMARY KEY (asset, volume, path)
             );
             CREATE TABLE tags (
               asset TEXT NOT NULL REFERENCES assets(id),
               tag TEXT NOT NULL, PRIMARY KEY (asset, tag)
             );
             CREATE INDEX tags_by_tag ON tags (tag);",
        )?;

        let tx = conn.transaction()?;
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
        tx.commit()?;

        Ok(Self { conn })
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
        let db = SqliteCatalog::rebuild(&dir.path().join("catalog.db"), &p).expect("rebuild");
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
    fn rebuild_discards_previous_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");

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
        SqliteCatalog::rebuild(&path, &p1).expect("rebuild a");

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
        let db = SqliteCatalog::rebuild(&path, &p2).expect("rebuild b");

        assert_eq!(
            db.search_by_tag("keep-a").expect("query a"),
            Vec::<AssetId>::new(),
            "rebuild must discard the previous projection's data"
        );
        assert_eq!(db.search_by_tag("keep-b").expect("query b"), vec![b]);
    }
}
