//! The configured describer backend (`describer.toml`), read by
//! `maj describer show|test`, `index run`/`index status`, and `search`'s
//! caption coverage remedy. Moved verbatim from
//! `crates/cli/src/describer_cmd.rs`; writing the config (`maj describer
//! set`) stays a CLI concern.
use anyhow::{Context as _, Result};
use majestical_describe::DescriberConfig;
use std::path::{Path, PathBuf};

/// # Errors
/// Returns an error if the local state dir can't be resolved.
pub fn config_path(catalog_root: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog_root)?.join("describer.toml"))
}

/// Loads the configured describer, if any.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved or an existing
/// config file can't be read/parsed.
pub fn load_config(catalog_root: &Path) -> Result<Option<DescriberConfig>> {
    let path = config_path(catalog_root)?;
    DescriberConfig::load(&path).with_context(|| format!("load {}", path.display()))
}
