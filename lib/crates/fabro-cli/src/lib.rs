#![expect(
    dead_code,
    reason = "the reference-facing library compiles CLI args without the binary dispatch modules"
)]

mod args;
mod manifest_builder;

use clap::{Command, CommandFactory};
pub use manifest_builder::{BuiltManifest, ManifestBuildInput, build_run_manifest};

pub fn command_for_reference() -> Command {
    args::Cli::command()
}
