//! Persisted GUI settings: today, only the catalog path. A JSON file in
//! Tauri's app-config dir — file-based on purpose (agent-inspectable,
//! trivially portable), matching the project's file-first bias.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "config.json";

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuiConfig {
    pub catalog: Option<PathBuf>,
}

/// Reads the persisted settings, defaulting on anything unreadable: a
/// missing file is the normal first run, and a corrupt one must not stop
/// the app from opening — the user simply picks a catalog again, which
/// rewrites the file.
#[must_use]
pub fn load(config_dir: &Path) -> GuiConfig {
    let Ok(bytes) = std::fs::read(config_dir.join(FILE_NAME)) else {
        return GuiConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// # Errors
/// Returns an error if the config dir can't be created or the file written.
pub fn store(config_dir: &Path, config: &GuiConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(config_dir.join(FILE_NAME), text)?;
    Ok(())
}
