//! `maj sync`: location config plus push/pull/status orchestration over
//! `crates/sync`'s transfer engine. Locations are per-machine config
//! (mount points differ per machine) in the state dir's `sync.toml`,
//! never synced.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncConfig {
    /// The read-only-member switch: a machine with `readonly = true` never
    /// pushes (events already carry author identity, so this is the whole
    /// feature — a policy on the push side, not a data concept).
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Location {
    pub name: String,
    pub path: PathBuf,
}

impl SyncConfig {
    /// Missing file = empty config (a catalog that has never configured
    /// sync), never an error.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub(crate) fn store(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing sync config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// The per-catalog `sync.toml` path in this machine's state dir.
pub(crate) fn config_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("sync.toml"))
}

fn add_location(config: &Path, name: &str, location: &Path) -> Result<()> {
    anyhow::ensure!(
        location.is_dir(),
        "{} is not an accessible directory — mount it or check the path",
        location.display()
    );
    let mut cfg = SyncConfig::load(config)?;
    anyhow::ensure!(
        !cfg.locations.iter().any(|l| l.name == name),
        "sync location '{name}' is already configured — remove it first with `maj sync location rm {name}`"
    );
    // Git-init style: idempotently create the layout so the first push
    // has somewhere to land. Never touches existing files.
    for sub in ["events", "blobs"] {
        let dir = location.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("initializing {}", dir.display()))?;
    }
    cfg.locations.push(Location {
        name: name.to_string(),
        path: location.to_path_buf(),
    });
    cfg.store(config)
}

fn remove_location(config: &Path, name: &str) -> Result<()> {
    let mut cfg = SyncConfig::load(config)?;
    let before = cfg.locations.len();
    cfg.locations.retain(|l| l.name != name);
    anyhow::ensure!(
        cfg.locations.len() < before,
        "no sync location named '{name}' — see `maj sync location list`"
    );
    cfg.store(config)
}

pub(crate) fn cmd_location_add(catalog: &Path, name: &str, location: &Path) -> Result<()> {
    add_location(&config_path(catalog)?, name, location)?;
    println!("added sync location '{name}' at {}", location.display());
    Ok(())
}

pub(crate) fn cmd_location_rm(catalog: &Path, name: &str) -> Result<()> {
    remove_location(&config_path(catalog)?, name)?;
    println!("removed sync location '{name}' (its files were not touched)");
    Ok(())
}

pub(crate) fn cmd_location_list(catalog: &Path, json: bool) -> Result<()> {
    let cfg = SyncConfig::load(&config_path(catalog)?)?;
    if json {
        let rows: Vec<serde_json::Value> = cfg
            .locations
            .iter()
            .map(|l| serde_json::json!({ "name": l.name, "path": l.path }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "readonly": cfg.readonly,
                "locations": rows,
            }))?
        );
        return Ok(());
    }
    if cfg.locations.is_empty() {
        println!(
            "no sync locations configured — add one with `maj sync location add <name> <path>`"
        );
        return Ok(());
    }
    for l in &cfg.locations {
        println!("{}\t{}", l.name, l.path.display());
    }
    if cfg.readonly {
        println!("readonly = true — this machine never pushes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_and_defaults_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loaded = SyncConfig::load(&path).expect("load missing");
        assert!(!loaded.readonly);
        assert!(loaded.locations.is_empty());
        let config = SyncConfig {
            readonly: true,
            locations: vec![Location {
                name: "nas".into(),
                path: "/Volumes/Team/sync".into(),
            }],
        };
        config.store(&path).expect("store");
        let loaded = SyncConfig::load(&path).expect("load");
        assert_eq!(loaded, config);
    }

    #[test]
    fn add_rejects_duplicate_names_and_rm_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "nas", &loc).expect("first add");
        assert!(
            loc.join("events").is_dir() && loc.join("blobs").is_dir(),
            "add initializes the events/ + blobs/ skeleton"
        );
        let err = add_location(&path, "nas", &loc).expect_err("dup must fail");
        assert!(err.to_string().contains("already configured"));
        let err = remove_location(&path, "ghost").expect_err("unknown rm");
        assert!(err.to_string().contains("no sync location named"));
    }

    #[test]
    fn add_rejects_an_unreachable_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let err = add_location(&path, "nas", &dir.path().join("missing"))
            .expect_err("unreachable path must fail");
        assert!(err.to_string().contains("not an accessible directory"));
    }
}
