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
    #[error("model fetch: {0}")]
    Model(String),
    #[error("encoder: {0}")]
    Encoder(String),
    #[error("vector store: {0}")]
    VectorStore(String),
}
