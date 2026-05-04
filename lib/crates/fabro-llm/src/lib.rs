pub mod adapter_registry;
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

pub use error::{Error, ProviderErrorDetail, ProviderErrorKind, Result};
pub use fabro_model::{ModelHandle, Provider};