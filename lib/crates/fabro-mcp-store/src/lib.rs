//! Durable storage for server-managed MCP server definitions.
//!
//! Concrete [`McpServerStore`] modeled on `fabro-automation`'s
//! `AutomationStore`: per-file TOML under `{config}/mcps/{id}.toml`, in-memory
//! cache, SHA-256 revision for optimistic concurrency, and async
//! storage-agnostic methods. The domain model lives in `fabro-types`; this
//! crate owns persistence.

mod error;
mod model;
mod store;

pub use error::McpServerStoreError;
pub use store::McpServerStore;
