//! Opens the ad hoc sqlite view of the catalog shared by every read path
//! (`search`, `volumes list`, `para list`) — the open+sync pair lives in
//! exactly one place. Moved verbatim from `crates/cli/src/commands.rs`.
use crate::app::{FsApp, warn_skipped_corrupt_lines};
use anyhow::{Context, Result};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::projection::Projection;
use std::path::Path;

/// Opens the sqlite catalog from the per-machine local state dir (see
/// `state_dir`), applying only the events past its last-saved cursor (or
/// rebuilding from scratch if there's no usable saved state).
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or the sqlite
/// catalog fails to open or sync.
pub fn open_catalog(app: &FsApp, catalog_dir: &Path) -> Result<(SqliteCatalog, Projection)> {
    let paths = crate::state_dir::catalog_paths(catalog_dir)?;
    let mut skipped = 0usize;
    let (db, projection, _mode) =
        SqliteCatalog::open_synced(&paths.db_path, app.log(), &mut |_line| skipped += 1)
            .context("opening sqlite catalog")?;
    warn_skipped_corrupt_lines(skipped, catalog_dir);
    Ok((db, projection))
}
