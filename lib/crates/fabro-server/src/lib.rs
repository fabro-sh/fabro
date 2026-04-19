#![cfg_attr(
    test,
    allow(clippy::absolute_paths, clippy::await_holding_lock, clippy::float_cmp)
)]

pub mod bind;
pub mod csp;
#[allow(clippy::wildcard_imports, clippy::absolute_paths)]
mod demo;
pub mod diagnostics;
pub mod error;
pub mod github_webhooks;
pub mod install;
pub mod ip_allowlist;
pub mod jwt_auth;
mod run_files;
mod run_files_security;
mod run_manifest;
pub mod security_headers;
pub mod serve;
pub mod server;
mod server_secrets;
mod settings_view;
pub mod static_files;
pub mod web_auth;

pub use error::{ApiError, Error, Result};
