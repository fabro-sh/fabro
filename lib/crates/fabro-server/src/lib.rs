#![cfg_attr(
    test,
    allow(clippy::absolute_paths, clippy::await_holding_lock, clippy::float_cmp)
)]

#[allow(clippy::wildcard_imports, clippy::absolute_paths)]
mod demo;
pub mod error;
pub mod github_webhooks;
pub mod jwt_auth;
pub mod serve;
pub mod server;
pub mod server_config {
    pub use fabro_config::FabroSettings;
    pub use fabro_config::server::*;
}
pub mod sessions;
pub mod tls;
