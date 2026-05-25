#![allow(
    clippy::disallowed_methods,
    reason = "This crate owns the process environment variable name registry."
)]

mod env_vars;
mod secret_registry;

pub use env_vars::EnvVars;
pub use secret_registry::{is_bootstrap_secret, is_optional_vault_secret, optional_vault_secrets};
