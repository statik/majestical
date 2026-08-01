//! `maj sync`: location config plus push/pull/status orchestration over
//! `crates/sync`'s transfer engine. Locations are per-machine config
//! (mount points differ per machine) in the state dir's `sync.toml`,
//! never synced.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(crate) const NO_LOCATIONS_HINT: &str =
    "no sync locations configured — add one with `maj sync location add <name> <path>`";

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncConfig {
    /// The read-only-member switch: a machine with `readonly = true` never
    /// pushes (events already carry author identity, so this is the whole
    /// feature — a policy on the push side, not a data concept).
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
    /// Fields a newer `maj` wrote that this build doesn't know about.
    /// Flattened so `location add|rm` round-trip them unchanged instead of
    /// silently dropping them.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Location {
    pub name: String,
    pub path: PathBuf,
    /// See [`SyncConfig::extra`].
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl SyncConfig {
    /// Load config from `path`; a missing file returns `Self::default()` (a
    /// catalog that has never configured sync), never an error.
    ///
    /// # Errors
    /// Returns an error when `path` exists but can't be read, or its
    /// contents don't parse as TOML.
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

    /// Serialize and write `path`, replacing it via a same-directory
    /// temp-file-then-rename so a concurrent reader never observes a
    /// partial write. The file is always rewritten wholesale from this
    /// struct: a hand-edited known field (e.g. `readonly`) survives a
    /// later `location add|rm`, but TOML comments do not — they have no
    /// representation in the struct, so they're dropped on the next store.
    ///
    /// # Errors
    /// Returns an error when serialization fails, or the write/rename
    /// fails.
    pub(crate) fn store(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing sync config")?;
        let file_name = path.file_name().map_or_else(
            || "sync.toml".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let tmp = path.with_file_name(format!("{file_name}.tmp"));
        std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))
    }
}

/// The per-catalog `sync.toml` path in this machine's state dir.
pub(crate) fn config_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("sync.toml"))
}

/// Registers a new sync location: validates `location` is an accessible,
/// UTF-8-representable directory, canonicalizes it (locations are mount
/// points and must be absolute at rest — a relative path would resolve
/// against whatever CWD a later push/pull happens to run from), idempotently
/// creates the `events/`/`blobs/` skeleton so the first push has somewhere
/// to land, and appends it to `config`.
///
/// # Errors
/// Returns an error when `name` is empty, `location` is not an accessible
/// directory, `location` is not valid UTF-8, `name` is already configured,
/// the skeleton directories can't be created, or `config` can't be stored.
fn add_location(config: &Path, name: &str, location: &Path) -> Result<()> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "sync location name must not be empty");
    anyhow::ensure!(
        location.is_dir(),
        "{} is not an accessible directory — mount it or check the path",
        location.display()
    );
    let canonical = location
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", location.display()))?;
    anyhow::ensure!(
        canonical.to_str().is_some(),
        "{} is not valid UTF-8 — sync locations must have UTF-8 paths so they can be stored in sync.toml",
        canonical.display()
    );
    let mut cfg = SyncConfig::load(config)?;
    anyhow::ensure!(
        !cfg.locations.iter().any(|l| l.name == name),
        "sync location '{name}' is already configured — remove it first with `maj sync location rm {name}`"
    );
    // Git-init style: idempotently create the layout so the first push
    // has somewhere to land. Never touches existing files.
    for sub in ["events", "blobs"] {
        let dir = canonical.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("initializing {}", dir.display()))?;
    }
    cfg.locations.push(Location {
        name: name.to_string(),
        path: canonical,
        extra: toml::Table::new(),
    });
    cfg.store(config)
}

/// Removes the location named `name` from `config`. Never touches the
/// location's own files (its `events/`/`blobs/` directories, and anything a
/// prior sync landed there) — only the config entry is dropped.
///
/// # Errors
/// Returns an error when no location named `name` exists, or `config`
/// can't be stored.
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
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "readonly": cfg.readonly,
                "locations": &cfg.locations,
            }))?
        );
        return Ok(());
    }
    if cfg.locations.is_empty() {
        println!("{NO_LOCATIONS_HINT}");
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

        let mut location_extra = toml::Table::new();
        location_extra.insert("future-field".into(), toml::Value::String("kept".into()));
        let config = SyncConfig {
            readonly: true,
            locations: vec![Location {
                name: "nas".into(),
                path: "/Volumes/Team/sync".into(),
                extra: location_extra,
            }],
            extra: toml::Table::new(),
        };
        config.store(&path).expect("store");
        let loaded = SyncConfig::load(&path).expect("load");
        assert_eq!(loaded, config);

        // An unknown key (as a newer `maj` might have written) survives a
        // load -> store -> load round trip untouched.
        loaded.store(&path).expect("store again");
        let reloaded = SyncConfig::load(&path).expect("reload");
        assert_eq!(reloaded, loaded);
        assert_eq!(
            reloaded.locations[0].extra.get("future-field"),
            Some(&toml::Value::String("kept".into()))
        );
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

    #[test]
    fn add_rejects_an_empty_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let err = add_location(&path, "  ", dir.path()).expect_err("empty name must fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn add_stores_a_canonicalized_absolute_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "nas", &loc).expect("add");
        let cfg = SyncConfig::load(&path).expect("load");
        let stored = &cfg.locations[0].path;
        assert!(stored.is_absolute());
        assert_eq!(*stored, loc.canonicalize().expect("canonicalize"));
    }

    #[test]
    fn add_stores_a_trimmed_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync.toml");
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        add_location(&path, "  nas  ", &loc).expect("add");
        let cfg = SyncConfig::load(&path).expect("load");
        assert_eq!(cfg.locations[0].name, "nas");
    }
}
