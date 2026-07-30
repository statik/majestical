//! Per-machine local state for a catalog: the sqlite projection, ingest run
//! journals, and (later) the vector index. Keyed by the canonicalized sync-root
//! path so distinct catalogs never collide; `MAJ_STATE_DIR` overrides the base
//! (tests, portable setups).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use xxhash_rust::xxh3::xxh3_128;

pub(crate) struct CatalogPaths {
    #[expect(
        dead_code,
        reason = "consumed by the vector index location added in a later PR"
    )]
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub runs_dir: PathBuf,
}

fn state_base() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MAJ_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let data =
        dirs::data_dir().context("no platform data directory; set MAJ_STATE_DIR explicitly")?;
    Ok(data.join("majestical"))
}

fn state_dir_with_base(base: &Path, catalog_root: &Path) -> Result<PathBuf> {
    let canonical = catalog_root
        .canonicalize()
        .with_context(|| format!("canonicalizing catalog root {}", catalog_root.display()))?;
    let key = format!(
        "{:032x}",
        xxh3_128(canonical.as_os_str().as_encoded_bytes())
    );
    Ok(base.join("catalogs").join(key))
}

/// Resolve (and create) the state dir for a catalog, migrating any legacy
/// derived files out of the sync root: a pre-phase-4 `catalog.db` is deleted
/// (disposable by invariant; it is rebuilt locally), and `runs/*.jsonl`
/// journals are moved so `--resume` keeps working.
pub(crate) fn catalog_paths(catalog_root: &Path) -> Result<CatalogPaths> {
    let state_dir = state_dir_with_base(&state_base()?, catalog_root)?;
    let runs_dir = state_dir.join("runs");
    std::fs::create_dir_all(&runs_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    migrate_legacy(catalog_root, &runs_dir)?;
    Ok(CatalogPaths {
        db_path: state_dir.join("catalog.db"),
        state_dir,
        runs_dir,
    })
}

fn migrate_legacy(catalog_root: &Path, state_runs: &Path) -> Result<()> {
    let legacy_db = catalog_root.join("catalog.db");
    if legacy_db.is_file() {
        std::fs::remove_file(&legacy_db)
            .with_context(|| format!("removing legacy catalog.db at {}", legacy_db.display()))?;
        eprintln!("note: removed legacy catalog.db from the sync root (rebuilt locally)");
    }
    let legacy_runs = catalog_root.join("runs");
    if legacy_runs.is_dir() {
        for entry in std::fs::read_dir(&legacy_runs)
            .with_context(|| format!("reading {}", legacy_runs.display()))?
        {
            let entry = entry.with_context(|| format!("reading {}", legacy_runs.display()))?;
            let from = entry.path();
            let Some(name) = from.file_name() else {
                continue;
            };
            let to = state_runs.join(name);
            // Sync root and state dir may be different filesystems: copy + delete.
            std::fs::copy(&from, &to)
                .with_context(|| format!("moving journal {}", from.display()))?;
            std::fs::remove_file(&from)
                .with_context(|| format!("removing migrated journal {}", from.display()))?;
        }
        std::fs::remove_dir(&legacy_runs)
            .with_context(|| format!("removing legacy runs dir {}", legacy_runs.display()))?;
        eprintln!("note: moved legacy run journals into the local state dir");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_root_same_dir_different_roots_differ() {
        let base = tempfile::tempdir().expect("tempdir");
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        let d1 = state_dir_with_base(base.path(), a.path()).expect("state dir");
        let d2 = state_dir_with_base(base.path(), a.path()).expect("state dir");
        let d3 = state_dir_with_base(base.path(), b.path()).expect("state dir");
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
    }

    #[test]
    fn missing_catalog_root_is_a_clear_error() {
        let base = tempfile::tempdir().expect("tempdir");
        let err = state_dir_with_base(base.path(), Path::new("/nonexistent-maj-root"))
            .expect_err("must fail");
        assert!(err.to_string().contains("canonicalizing catalog root"));
    }
}
