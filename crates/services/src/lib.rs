//! Operation layer shared by the CLI, `maj mcp`, and the desktop app.
//! One function per verb: request in, serde-serializable outcome out.
//! Heads render outcomes; they never re-implement operations.
pub mod app;
pub mod error;
pub mod iso8601;
pub mod state_dir;
pub mod volume_identity;
