pub mod types;
pub mod error;
pub mod provider;
pub mod middleware;
pub mod client;
pub mod tools;
pub mod retry;
pub mod generate;
pub mod catalog;
pub mod providers;

// Re-export module-level default client helpers (Section 2.5).
pub use generate::set_default_client;
pub use provider::{ModelId, Provider};
