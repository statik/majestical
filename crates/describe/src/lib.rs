//! HTTP describer adapter: Ollama / LM Studio / `OpenRouter` behind one
//! OpenAI-compatible client, implementing `majestical_core::ports::Describer`.

pub mod client;
pub mod config;

pub use client::{HttpDescriber, ProbeReport};
pub use config::{BackendKind, DescriberConfig};
