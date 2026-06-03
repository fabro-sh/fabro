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
#[allow(
    dead_code,
    reason = "Automation materializer test hooks and helpers are only referenced by selected targets."
)]
mod automation_materializer;
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
mod interp;
pub mod jwt_auth;
pub mod manifest_validation;
mod migrations;
mod principal_middleware;
mod request_id;
mod run_files;
mod run_files_security;
mod run_manifest;
mod run_selector;
mod run_title_generation;
pub mod run_tool_manifest;
pub mod security_headers;
pub mod serve;
pub mod server;
mod server_secrets;
mod spawn_env;
mod startup;
pub mod static_files;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod web_auth;
mod worker_control;
mod worker_runtime;
mod worker_token;

pub use error::{ApiError, Error, Result};
pub use run_manifest::workflow_bundle_from_manifest;
pub use server_secrets::process_env_snapshot;
pub use startup::{load_startup_vault, validate_startup, validate_startup_configuration};
