#![allow(
    clippy::disallowed_methods,
    reason = "This crate owns the process environment variable name registry."
)]

mod env_vars;
mod sandbox_bundle;
mod secret_registry;

pub use env_vars::EnvVars;
pub use sandbox_bundle::{
    MANAGED_VERSIONS_DIR, PLUGIN_PINS_EMBEDDED, SANDBOX_PLUGIN_BINARIES, SANDBOX_PLUGINS,
    SandboxPlugin, managed_install_root,
};
pub use secret_registry::{is_bootstrap_secret, is_optional_vault_secret, optional_vault_secrets};
