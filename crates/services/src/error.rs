//! Service-level error type. Carries operation + input + suggested fix so
//! every head (CLI exit message, MCP tool error, GUI dialog) can render the
//! same remedy without re-deriving it.
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("no catalog at {root} — run `maj catalog init` first")]
    NoCatalog { root: PathBuf },
    /// Escape hatch while extraction is in flight: wraps the anyhow chains
    /// the cmd_* bodies already produce. Individual verbs migrate to typed
    /// variants only when a head needs to match on them (YAGNI).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
