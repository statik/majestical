//! Operation layer shared by the CLI, `maj mcp`, and the desktop app.
//! One function per verb: request in, serde-serializable outcome out.
//! Heads render outcomes; they never re-implement operations.
pub mod app;
pub mod capability;
pub mod catalog;
pub mod describer_config;
pub mod error;
pub mod index;
pub mod iso8601;
pub mod meta;
pub mod para;
pub mod query;
pub mod search;
pub mod state_dir;
pub mod sync;
pub mod tags;
pub mod volume_identity;
pub mod volumes;
