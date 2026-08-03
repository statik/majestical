//! Service-level error type. Carries operation + input + suggested fix so
//! every head (CLI exit message, MCP tool error, GUI dialog) can render the
//! same remedy without re-deriving it.
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The single source of truth for the "no catalog here" message — every
    /// guard that checks for `<root>/events` (`FsApp::open`, `sync::status`'s
    /// catalog check) constructs this variant rather than formatting its own
    /// copy of the string.
    #[error("no catalog at {root} — run `maj catalog init` first")]
    NoCatalog { root: PathBuf },
    /// `para archive`'s partial-failure carrier: a multi-root run that fails
    /// partway through has already moved (or classified as
    /// already-archived/planned) the roots processed before the one that
    /// failed — real, completed filesystem facts that must reach the head
    /// rather than being silently dropped by an early `Err` return. A head
    /// renders `moves` (e.g. as `moved X -> Y` lines) before surfacing
    /// `source`; a confirm-gated caller (e.g. an MCP tool) can act on
    /// `moves` as data instead of parsing the rendered text.
    #[error("{source}")]
    ParaArchivePartial {
        moves: Vec<crate::para::ArchiveMove>,
        source: anyhow::Error,
    },
    /// Escape hatch while extraction is in flight: wraps the anyhow chains
    /// the cmd_* bodies already produce. Individual verbs migrate to typed
    /// variants only when a head needs to match on them (YAGNI).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
