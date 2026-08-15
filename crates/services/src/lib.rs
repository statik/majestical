//! Operation layer shared by the CLI, `maj mcp`, and the desktop app.
//! One function per verb: request in, serde-serializable outcome out.
//! Heads render outcomes; they never re-implement operations.
pub mod app;
pub mod browse;
pub mod capability;
pub mod catalog;
pub mod describer_config;
pub mod error;
pub mod inbox;
pub mod inbox_manifest;
pub mod index;
pub mod ingest;
pub mod iso8601;
pub mod meta;
pub mod notices;
pub mod para;
pub mod query;
pub mod runtime;
pub mod scan;
pub mod search;
pub mod state_dir;
pub mod sync;
pub mod tags;
pub mod verify;
pub mod volume_identity;
pub mod volumes;
