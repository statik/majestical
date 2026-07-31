//! Derived-data production for the catalog: content-addressed blobs
//! (thumbnails, embeddings) in the sync root, and (in later tasks) the work
//! planner that diffs required derivations against what exists. Everything
//! here is disposable and regenerable; the event log stays the only truth.
pub mod blob;
pub mod encoder;
pub mod error;
pub mod model;
pub mod preprocess;
pub mod resize;
pub mod thumbs;
pub mod vector_store;
pub mod video;
pub mod work;

pub use error::IndexError;
