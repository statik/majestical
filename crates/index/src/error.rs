use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("blob {path}: {source}")]
    Blob {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("decoding {path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("vector blob {path}: invalid length {len}")]
    VectorShape { path: PathBuf, len: usize },
    #[error("image resize: {0}")]
    Resize(String),
    #[error("model: {0}")]
    Model(String),
    #[error("encoder: {0}")]
    Encoder(String),
    #[error("vector store: {0}")]
    VectorStore(String),
    #[error("video {path}: {message}")]
    Video { path: PathBuf, message: String },
    /// A derivation whose native backend exists only on macOS, requested
    /// from a build without it. Names the capability and the framework so
    /// degradation is never a silent zero (the never-lie rule).
    #[error(
        "{capability} is unavailable in this build: it requires {framework}, which exists only on macOS"
    )]
    PlatformUnavailable {
        capability: &'static str,
        framework: &'static str,
    },
}
