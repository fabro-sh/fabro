#![cfg_attr(
    test,
    allow(
        clippy::absolute_paths,
        clippy::await_holding_lock,
        clippy::float_cmp,
        reason = "Test-only server modules favor explicit assertions and fixture code."
    )
)]

pub mod auth;
mod canonical_host;
mod canonical_origin;
pub mod csp;
#[allow(
    clippy::wildcard_imports,
    clippy::absolute_paths,
    reason = "The demo module is isolated fixture-style code."
)]
mod demo;
pub mod diagnostics;
pub mod error;
pub mod github_webhooks;
pub mod install;
pub mod ip_allowlist;
pub mod jwt_auth;
pub mod manifest_validation;
mod request_id;
mod run_files;
mod run_files_security;
mod run_manifest;
mod run_selector;
pub mod security_headers;
pub mod serve;
pub mod server;
mod server_secrets;
mod spawn_env;
mod startup;
pub mod static_files;
pub mod web_auth;
mod worker_token;

pub use error::{ApiError, Error, Result};
pub use server_secrets::process_env_snapshot;
pub use startup::validate_startup;
