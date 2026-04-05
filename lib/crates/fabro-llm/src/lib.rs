pub mod cli;
pub mod client;
pub mod error;
pub mod generate;
pub mod middleware;
pub mod model_test;
pub mod provider;
pub mod providers;
pub mod retry;
pub mod tools;
pub mod types;

// Re-export module-level default client helpers (Section 2.5).
pub use fabro_model::{ModelRef, Provider};
pub use generate::set_default_client;
