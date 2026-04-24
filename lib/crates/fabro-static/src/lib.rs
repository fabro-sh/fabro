#![allow(
    clippy::disallowed_methods,
    reason = "This crate owns the process environment variable name registry."
)]

mod env_vars;

pub use env_vars::EnvVars;
