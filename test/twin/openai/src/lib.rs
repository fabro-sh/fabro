#![allow(clippy::result_large_err)]

pub mod admin;
pub mod app;
pub mod config;
pub mod debug_ui;
pub mod engine;
pub mod logs;
pub mod openai;
pub mod sse;
pub mod state;

use axum::Router;
use config::Config;
use state::AppState;

pub fn build_app() -> Router {
    build_app_with_config(Config::from_env().unwrap_or_default())
}

pub fn build_app_with_config(config: Config) -> Router {
    app::router(AppState::new(config))
}
