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
pub mod secret_store;
pub mod serve;
pub mod server;
mod settings_view;
pub mod static_files;
pub mod server_config {
    pub use fabro_types::Settings;
    pub use fabro_types::settings::server::*;
}
pub mod tls;
pub mod web_auth;
