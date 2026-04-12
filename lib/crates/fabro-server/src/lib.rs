#![cfg_attr(
    test,
    allow(clippy::absolute_paths, clippy::await_holding_lock, clippy::float_cmp)
)]

pub mod bind;
#[allow(clippy::wildcard_imports, clippy::absolute_paths)]
mod demo;
pub mod diagnostics;
pub mod error;
pub mod github_webhooks;
pub mod jwt_auth;
mod run_manifest;
pub mod serve;
pub mod server;
mod server_secrets;
mod settings_view;
pub mod static_files;
pub mod tls;
pub mod web_auth;

pub use error::{ApiError, Error, Result};
