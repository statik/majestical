//! Projection table schema: `create_tables` drops and recreates every table
//! this crate owns, used by both `rebuild`'s full-rebuild path and as the
//! one-time setup step before an incremental apply.
use crate::SqliteCatalog;
use rusqlite::Transaction;

impl SqliteCatalog {
    pub(crate) fn create_tables(tx: &Transaction) -> rusqlite::Result<()> {
        tx.execute_batch(
            "DROP TABLE IF EXISTS tags;
             DROP TABLE IF EXISTS instances;
             DROP TABLE IF EXISTS assets;
             DROP TABLE IF EXISTS volumes;
             DROP TABLE IF EXISTS para_nodes;
             DROP TABLE IF EXISTS asset_para;
             DROP TABLE IF EXISTS verifications;
             DROP TABLE IF EXISTS manifests;
             DROP TABLE IF EXISTS apply_cursors;
             DROP TABLE IF EXISTS apply_snapshot;
             DROP TABLE IF EXISTS names_fts;
             CREATE TABLE assets (id TEXT PRIMARY KEY);
             CREATE TABLE instances (
               asset TEXT NOT NULL REFERENCES assets(id),
               volume TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL,
               mtime_ms INTEGER NOT NULL, kind TEXT NOT NULL,
               -- (asset, volume, path) is unique: the projection's instances
               -- are an HLC-LWW map keyed on (volume, path), so a rescan
               -- updates in place rather than producing a second row.
               PRIMARY KEY (asset, volume, path)
             );
             CREATE VIRTUAL TABLE names_fts USING fts5(
               name, asset UNINDEXED, tokenize = 'unicode61 remove_diacritics 2'
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
               mhl_path TEXT NOT NULL, roothash TEXT NOT NULL
             );
             CREATE TABLE apply_cursors (
               machine TEXT NOT NULL, segment TEXT NOT NULL, offset INTEGER NOT NULL,
               PRIMARY KEY (machine, segment)
             );
             CREATE TABLE apply_snapshot (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               version INTEGER NOT NULL,
               projection TEXT NOT NULL
             );",
        )
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn bundled_sqlite_has_fts5() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute("CREATE VIRTUAL TABLE t USING fts5(x)", [])
            .expect("bundled sqlite must include FTS5");
    }
}
