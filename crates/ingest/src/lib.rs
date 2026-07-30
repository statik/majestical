//! Ingest engine: planning, layout templates, the resumable transfer
//! journal, and the verified copy engine (ASC MHL arrives in a later task).
pub mod engine;
pub mod journal;
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
    #[error("journal {path}: {source}")]
    Journal {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding journal record: {0}")]
    JournalEncode(#[source] serde_json::Error),
    #[error(
        "all destinations must share one subdir this phase: {first:?} != {other:?} \
         — Task 7's per-destination layout is not wired up yet"
    )]
    MismatchedSubdirs { first: String, other: String },
    #[error("at least one destination required — nothing to copy to")]
    NoDestinations,
}
