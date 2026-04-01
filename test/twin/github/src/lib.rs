#![allow(
    clippy::absolute_paths,
    clippy::manual_let_else,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_else
)]

pub mod auth;
pub mod fixtures;
pub mod handlers;
pub mod server;
pub mod state;

pub use server::TestServer;
pub use state::AppState;
