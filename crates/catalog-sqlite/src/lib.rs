//! `SQLite` projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection`; `open_synced` instead applies
//! only the log events past a stored cursor, falling back to a full rebuild
//! when there's no usable snapshot to resume from. Includes an FTS5 name
//! index (`names_fts`); the vector index for semantic search lives outside
//! this crate per the phase 4 design.
use majestical_core::event::{AssetId, ParaKind, VerifyOutcome};
use majestical_core::media_kind::media_kind;
use majestical_core::ports::{AssetSummary, CatalogStore, EventLog, Filter, LogCursor, PortError};
use majestical_core::projection::{
    AssetState, ManifestRecord, ParaNodeState, Projection, Touched, VolumeState,
};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params_from_iter};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog db: {0} — delete the file and re-run")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event log: {0}")]
    Port(#[from] PortError),
    #[error("apply snapshot: {0} — delete catalog.db and re-run")]
    Snapshot(#[from] serde_json::Error),
}

/// On-disk format version for the `apply_snapshot` row. Bumped whenever the
/// serialized shape changes in a way old rows can't deserialize into;
/// `load_apply_state` treats a mismatch the same as a missing snapshot.
///
/// This has never shipped, so an in-place shape change (e.g. serializing
/// `Projection` directly instead of wrapped in a struct) doesn't need a bump
/// on its own — an old-format row just fails to deserialize as the new
/// shape, `load_apply_state` returns `None`, and `open_synced` falls back to
/// a full rebuild. Self-healing by construction; bump this only when a
/// future change needs two different versions to coexist across a real
/// upgrade.
///
/// Bumped to 2: `AssetState::instances` changed from a `BTreeSet<(volume,
/// path, size)>` to an HLC-LWW `BTreeMap<(volume, path), InstanceInfo>` (see
/// `majestical_core::projection`), forcing a full rebuild for any snapshot
/// written under the old shape.
///
/// This constant doubles as the schema version: any change to the tables
/// `create_tables` builds must bump it, so a pre-change db file takes the
/// full-rebuild path (which drops and recreates every table) instead of
/// running incremental applies against a schema it predates.
///
/// Bumped to 3: `instances` gained a `kind` column and `names_fts` (the FTS5
/// name index) was added.
const SNAPSHOT_VERSION: i64 = 3;

/// How `open_synced` populated the catalog: from a stored cursor plus new
/// events, or from scratch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyMode {
    /// Applied `applied` newly-read events on top of the saved snapshot;
    /// `applied: 0` means the open was a no-op — nothing new was in the log.
    Incremental {
        applied: usize,
    },
    FullRebuild,
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

    /// Assets whose basename matches any of `terms` by word-prefix, ranked
    /// best-first (FTS5's `rank`), capped at `limit` rows — one row per
    /// asset, at its best-matching basename's rank, even when several of its
    /// instances have basenames that all match. Unicode-aware and
    /// case-insensitive via the `unicode61 remove_diacritics 2` tokenizer.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn search_names_ranked(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<(AssetId, f64)>, CatalogError> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // Each term quoted (embedded quotes doubled) with a prefix star,
        // OR-joined: beach -> "beach"* — FTS5 string syntax, immune to
        // operator injection.
        let match_expr = terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        // GROUP BY asset + min(rank): an asset with several matching
        // basenames (e.g. beach.mov and beach_copy.mov) otherwise comes back
        // as one row per matching basename. bm25 ranks are negative with
        // more-negative meaning better, so min() picks the best match.
        let mut stmt = self.conn.prepare(
            "SELECT asset, min(rank) FROM names_fts WHERE names_fts MATCH ?1
             GROUP BY asset ORDER BY min(rank) LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            (&match_expr, i64::try_from(limit).unwrap_or(i64::MAX)),
            |r| Ok((AssetId(r.get(0)?), r.get::<_, f64>(1)?)),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Assets satisfying every filter in `filters` (conjunction).
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn assets_matching(&self, filters: &[Filter]) -> Result<BTreeSet<AssetId>, CatalogError> {
        let mut sql = String::from("SELECT a.id FROM assets a WHERE 1=1 ");
        let mut params: Vec<Value> = Vec::new();
        for filter in filters {
            let next_param = params.len() + 1;
            sql.push_str(&Self::filter_clause(filter, next_param, &mut params));
            sql.push(' ');
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(0))?;
        let mut out = BTreeSet::new();
        for row in rows {
            out.insert(AssetId(row?));
        }
        Ok(out)
    }

    /// Builds one filter's SQL clause (starting with `AND`), pushing its
    /// bind values onto `params` starting at 1-based index `next_param`.
    fn filter_clause(filter: &Filter, next_param: usize, params: &mut Vec<Value>) -> String {
        match filter {
            Filter::Tag { value, negated } => {
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM tags t \
                     WHERE t.asset = a.id AND t.tag = ?{next_param})"
                )
            }
            Filter::Volume { value, negated } => {
                params.push(Value::Text(value.clone()));
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                let p2 = next_param + 1;
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM instances i \
                     LEFT JOIN volumes v ON v.id = i.volume \
                     WHERE i.asset = a.id AND (v.label = ?{next_param} OR i.volume = ?{p2}))"
                )
            }
            Filter::Para { node, negated } => {
                params.push(Value::Text(node.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM asset_para ap \
                     WHERE ap.asset = a.id AND ap.node = ?{next_param})"
                )
            }
            Filter::Kind { value, negated } => {
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM instances i \
                     WHERE i.asset = a.id AND i.kind = ?{next_param})"
                )
            }
            Filter::Online { ids, want } => Self::online_clause(ids, *want, next_param, params),
            Filter::Before(ms) => {
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
                format!(
                    "AND EXISTS (SELECT 1 FROM instances i \
                     WHERE i.asset = a.id AND i.mtime_ms < ?{next_param})"
                )
            }
            Filter::After(ms) => {
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
                format!(
                    "AND EXISTS (SELECT 1 FROM instances i \
                     WHERE i.asset = a.id AND i.mtime_ms > ?{next_param})"
                )
            }
        }
    }

    /// `Online` clause: empty `ids` short-circuits (no volume can ever be
    /// "one of zero mounted volumes"), non-empty binds one placeholder per
    /// id.
    fn online_clause(
        ids: &[String],
        want: bool,
        next_param: usize,
        params: &mut Vec<Value>,
    ) -> String {
        if ids.is_empty() {
            return if want {
                "AND 0=1".to_string()
            } else {
                "AND EXISTS (SELECT 1 FROM instances i WHERE i.asset = a.id)".to_string()
            };
        }
        let placeholders: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                params.push(Value::Text(id.clone()));
                format!("?{}", next_param + i)
            })
            .collect();
        let not = if want { "" } else { "NOT " };
        format!(
            "AND {not}EXISTS (SELECT 1 FROM instances i \
             WHERE i.asset = a.id AND i.volume IN ({}))",
            placeholders.join(", ")
        )
    }

    /// Presentation rows for exactly the given asset ids: name (the first
    /// instance's basename, ordered by volume then path), the distinct
    /// volumes holding an instance (label falling back to the volume id),
    /// tags in alphabetical order, and the current PARA assignment.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn asset_summaries(&self, ids: &[AssetId]) -> Result<Vec<AssetSummary>, CatalogError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(AssetSummary {
                asset: id.clone(),
                name: self.first_instance_name(id)?,
                volumes: self.instance_volumes(id)?,
                tags: self.query_asset_tags(id)?,
                para: self.query_asset_para(id)?,
            });
        }
        Ok(out)
    }

    fn first_instance_name(&self, id: &AssetId) -> Result<String, CatalogError> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM instances WHERE asset = ?1 ORDER BY volume, path LIMIT 1",
                [&id.0],
                |r| r.get(0),
            )
            .optional()?;
        Ok(path
            .and_then(|p| p.rsplit('/').next().map(str::to_string))
            .unwrap_or_default())
    }

    fn instance_volumes(&self, id: &AssetId) -> Result<Vec<(String, String)>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT i.volume, COALESCE(v.label, i.volume) FROM instances i \
             LEFT JOIN volumes v ON v.id = i.volume WHERE i.asset = ?1 ORDER BY i.volume",
        )?;
        let rows = stmt.query_map([&id.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn query_asset_tags(&self, id: &AssetId) -> Result<Vec<String>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE asset = ?1 ORDER BY tag")?;
        let rows = stmt.query_map([&id.0], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn query_asset_para(&self, id: &AssetId) -> Result<Option<String>, CatalogError> {
        Ok(self
            .conn
            .query_row(
                "SELECT node FROM asset_para WHERE asset = ?1",
                [&id.0],
                |r| r.get(0),
            )
            .optional()?)
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
        let tables: [(&str, &str); 9] = [
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

impl CatalogStore for SqliteCatalog {
    fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError> {
        Self::rebuild(self, projection).map_err(|e| PortError::new("catalog store", e))
    }

    fn assets_matching(&self, filters: &[Filter]) -> Result<BTreeSet<AssetId>, PortError> {
        Self::assets_matching(self, filters).map_err(|e| PortError::new("catalog store", e))
    }

    fn search_names_ranked(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<(AssetId, f64)>, PortError> {
        Self::search_names_ranked(self, terms, limit)
            .map_err(|e| PortError::new("catalog store", e))
    }

    fn asset_summaries(&self, ids: &[AssetId]) -> Result<Vec<AssetSummary>, PortError> {
        Self::asset_summaries(self, ids).map_err(|e| PortError::new("catalog store", e))
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

    /// Canary: the bundled rusqlite build must include FTS5. If this ever
    /// fails, the `bundled` feature needs an explicit FTS5 flag — every
    /// other test in this module depends on it.
    #[test]
    fn bundled_sqlite_has_fts5() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute("CREATE VIRTUAL TABLE t USING fts5(x)", [])
            .expect("bundled sqlite must include FTS5");
    }

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
                .assets_matching(&[Filter::Tag {
                    value: "topic/drone".into(),
                    negated: false,
                }])
                .expect("tag query via trait object"),
            BTreeSet::from([a])
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
    fn fts_name_search_is_unicode_case_insensitive_and_ranked() {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v".into(),
                    path: "clips/Café-sunset.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: b.clone(),
                    volume: "v".into(),
                    path: "docs/readme.txt".into(),
                    size: 1,
                    mtime_ms: 0,
                },
            ],
        );
        let hits = db
            .search_names_ranked(&["cafe".to_string()], 10)
            .expect("name search");
        let ids: Vec<AssetId> = hits.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![a],
            "unicode61 remove_diacritics must fold Café to cafe"
        );
    }

    /// Pins `remove_diacritics 2` specifically (not the default level 1):
    /// U+1EC6 (Ệ) carries two combining marks a circumflex and a dot below)
    /// that level 1's smaller diacritic table doesn't fold away, so this
    /// only matches under level 2.
    #[test]
    fn diacritics_level_2_folds_marks_level_1_does_not() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "clips/Ệlan.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );
        let hits = db
            .search_names_ranked(&["elan".to_string()], 10)
            .expect("name search");
        let ids: Vec<AssetId> = hits.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![a],
            "remove_diacritics 2 must fold Ệ (circumflex + dot below) to e"
        );
    }

    #[test]
    fn prefix_terms_match() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "clips/beach_day.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );
        let hits = db
            .search_names_ranked(&["beach".to_string()], 10)
            .expect("prefix search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, a);
    }

    /// An asset with two instances whose basenames both match the search
    /// term must come back as one row, not one per matching basename — a
    /// naive `SELECT asset, rank FROM names_fts WHERE ... MATCH` returns one
    /// row per matching (name, asset) pair in the FTS index.
    #[test]
    fn ranked_search_returns_one_row_per_asset_not_per_matching_basename() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v".into(),
                    path: "clips/beach.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v2".into(),
                    path: "clips/beach_copy.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
            ],
        );
        let hits = db
            .search_names_ranked(&["beach".to_string()], 10)
            .expect("search");
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one row for one asset, got {hits:?}"
        );
        assert_eq!(hits[0].0, a);
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

    /// Shared fixture for the `filters_*` tests below: asset `a` is tagged
    /// "keep", lives on volume v1 ("Card A") as a video, mtime 1000, and is
    /// assigned to PARA node N1; asset `b` is tagged "rejected", lives on
    /// volume v2 ("Card B") as a non-media file, mtime 2000, and has no PARA
    /// assignment.
    fn filters_fixture(dir: &std::path::Path) -> (SqliteCatalog, AssetId, AssetId) {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let db = rebuild_from_ops(
            &dir.join("catalog.db"),
            vec![
                Op::VolumeSeen {
                    volume: "v1".into(),
                    label: "Card A".into(),
                },
                Op::VolumeSeen {
                    volume: "v2".into(),
                    label: "Card B".into(),
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v1".into(),
                    path: "beach.mov".into(),
                    size: 1,
                    mtime_ms: 1000,
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "keep".into(),
                },
                Op::ParaNodeCreate {
                    node: "N1".into(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                Op::AssetParaSet {
                    asset: a.clone(),
                    node: "N1".into(),
                },
                Op::AssetSeen {
                    asset: b.clone(),
                    volume: "v2".into(),
                    path: "notes.txt".into(),
                    size: 1,
                    mtime_ms: 2000,
                },
                Op::TagAdd {
                    asset: b.clone(),
                    tag: "rejected".into(),
                },
            ],
        );
        (db, a, b)
    }

    #[test]
    fn tag_filters_match_and_negate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "keep".into(),
                negated: false,
            }])
            .expect("tag"),
            BTreeSet::from([a.clone()])
        );
        let rejected_negated = db
            .assets_matching(&[Filter::Tag {
                value: "rejected".into(),
                negated: true,
            }])
            .expect("negated tag");
        assert!(rejected_negated.contains(&a));
        assert!(!rejected_negated.contains(&b));
    }

    #[test]
    fn time_filters_bound_by_mtime_and_combine_with_other_filters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[
                Filter::Tag {
                    value: "keep".into(),
                    negated: false,
                },
                Filter::Before(1500),
            ])
            .expect("tag and before"),
            BTreeSet::from([a])
        );
        assert!(
            db.assets_matching(&[Filter::After(1500)])
                .expect("after")
                .contains(&b)
        );
    }

    /// `Before`/`After` are strict inequalities: an instance whose `mtime_ms`
    /// exactly equals the bound matches neither.
    #[test]
    fn before_and_after_are_strict_at_the_exact_boundary() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a,
                volume: "v".into(),
                path: "a.mov".into(),
                size: 1,
                mtime_ms: 1000,
            }],
        );
        assert_eq!(
            db.assets_matching(&[Filter::Before(1000)]).expect("before"),
            BTreeSet::new(),
            "Before(1000) must not match an instance with mtime_ms == 1000"
        );
        assert_eq!(
            db.assets_matching(&[Filter::After(1000)]).expect("after"),
            BTreeSet::new(),
            "After(1000) must not match an instance with mtime_ms == 1000"
        );
    }

    #[test]
    fn kind_and_volume_filters_match_by_id_or_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Kind {
                value: "video".into(),
                negated: false,
            }])
            .expect("kind video"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Kind {
                value: "other".into(),
                negated: false,
            }])
            .expect("kind other"),
            BTreeSet::from([b])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "v1".into(),
                negated: false,
            }])
            .expect("volume by id"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "Card A".into(),
                negated: false,
            }])
            .expect("volume by label"),
            BTreeSet::from([a])
        );
    }

    #[test]
    fn para_filter_matches_assigned_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, _b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Para {
                node: "N1".into(),
                negated: false,
            }])
            .expect("para"),
            BTreeSet::from([a])
        );
    }

    #[test]
    fn online_filter_handles_mounted_ids_and_the_empty_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec!["v1".into()],
                want: true,
            }])
            .expect("online true"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec!["v1".into()],
                want: false,
            }])
            .expect("online false"),
            BTreeSet::from([b.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec![],
                want: true,
            }])
            .expect("online empty want true"),
            BTreeSet::new()
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec![],
                want: false,
            }])
            .expect("online empty want false"),
            BTreeSet::from([a, b])
        );
    }

    #[test]
    fn asset_summaries_carry_name_volumes_tags_para() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::VolumeSeen {
                    volume: "v1".into(),
                    label: "Card A".into(),
                },
                Op::VolumeSeen {
                    volume: "v2".into(),
                    label: "Card B".into(),
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v1".into(),
                    path: "clips/beach.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v2".into(),
                    path: "clips/beach_copy.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "keep".into(),
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "topic/drone".into(),
                },
                Op::ParaNodeCreate {
                    node: "N1".into(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                Op::AssetParaSet {
                    asset: a.clone(),
                    node: "N1".into(),
                },
            ],
        );

        let summaries = db
            .asset_summaries(std::slice::from_ref(&a))
            .expect("summaries");
        assert_eq!(summaries.len(), 1);
        let s = &summaries[0];
        assert_eq!(s.asset, a);
        assert_eq!(
            s.name, "beach.mov",
            "name must be the first instance's basename, ordered by volume then path"
        );
        assert_eq!(
            s.volumes,
            vec![
                ("v1".to_string(), "Card A".to_string()),
                ("v2".to_string(), "Card B".to_string()),
            ]
        );
        assert_eq!(s.tags, vec!["keep".to_string(), "topic/drone".to_string()]);
        assert_eq!(s.para.as_deref(), Some("N1"));
    }

    /// A "ghost" volume — an instance whose volume id has no `VolumeSeen`
    /// row — is reachable via partial cross-machine sync (one machine's log
    /// carries an `AssetSeen` for a volume the reader never observed a
    /// `VolumeSeen` for). `Filter::Volume` must still match it by instance
    /// volume id, and `asset_summaries` must fall back to the id as the
    /// label rather than erroring on a NULL join.
    #[test]
    fn ghost_volume_is_findable_and_summarizable_without_a_volume_seen_row() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "ghost".into(),
                path: "a.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );

        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "ghost".into(),
                negated: false,
            }])
            .expect("volume filter on a ghost volume"),
            BTreeSet::from([a.clone()])
        );

        let summaries = db
            .asset_summaries(std::slice::from_ref(&a))
            .expect("summaries must not error on a ghost volume");
        assert_eq!(
            summaries[0].volumes,
            vec![("ghost".to_string(), "ghost".to_string())],
            "label must fall back to the volume id when there's no VolumeSeen row"
        );
    }
}
