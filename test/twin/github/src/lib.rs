#![allow(
    clippy::absolute_paths,
    clippy::manual_let_else,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_else,
    reason = "This twin GitHub harness prefers explicit fixture modules over pedantic style lints."
)]

pub mod auth;
pub mod fixtures;
pub mod handlers;
pub mod server;
pub mod state;
#[cfg(test)]
pub mod test_support;

pub use server::TestServer;
pub use state::AppState;
