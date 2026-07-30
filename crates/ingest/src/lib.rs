//! Ingest engine: planning and layout templates (verified copy, journal, and
//! ASC MHL arrive in later tasks).
pub mod plan;
pub mod template;

/// Errors from the ingest engine. Every variant names the operation and the
/// path so a failure is actionable without a debugger.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("walking source {path}: {source}")]
    Walk {
        path: std::path::PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("reading {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "non-UTF-8 file name at {} — ASC MHL cannot represent it; rename the file to ingest it",
        path.display()
    )]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("template: {0}")]
    Template(String),
}
