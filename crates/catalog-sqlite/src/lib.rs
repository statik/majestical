//! `SQLite` projection of the catalog. Disposable by design: `rebuild`
//! recreates it wholesale from a `Projection`; `open_synced` instead applies
//! only the log events past a stored cursor, falling back to a full rebuild
//! when there's no usable snapshot to resume from. Includes an FTS5 name
//! index (`names_fts`); the vector index for semantic search lives outside
//! this crate per the phase 4 design.
use majestical_core::event::AssetId;
use majestical_core::ports::{AssetSummary, CatalogStore, Filter, PortError};
use majestical_core::projection::Projection;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;

mod apply;
mod query;
mod schema;

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
///
/// Bumped to 4: `Projection` gained the `saved_searches` field.
///
/// Bumped to 5: `create_tables` gained the `saved_searches` table.
pub(crate) const SNAPSHOT_VERSION: i64 = 5;

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
    use majestical_core::event::{Event, EventId, Op};

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
}
