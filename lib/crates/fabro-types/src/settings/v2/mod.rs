//! Namespaced settings schema (v2).
//!
//! This module is the hard-cut replacement for the flat [`super::Settings`]
//! shape. The top-level schema is strictly namespaced with `_version`,
//! `[project]`, `[workflow]`, `[run]`, `[cli]`, `[server]`, and `[features]`.
//! Value-language helpers live alongside the tree: durations, byte sizes,
//! model references, env interpolation, and splice-capable arrays.

pub mod accessors;
pub mod bridge;
pub mod cli;
pub mod duration;
pub mod features;
pub mod interp;
pub mod model_ref;
pub mod project;
pub mod run;
pub mod server;
pub mod size;
pub mod splice_array;
pub mod tree;
pub mod version;
pub mod workflow;

pub use bridge::bridge_to_old;

pub use cli::CliLayer;
pub use duration::{Duration, ParseDurationError};
pub use features::FeaturesLayer;
pub use interp::{InterpString, Provenance, ResolveEnvError, Resolved};
pub use model_ref::{
    AmbiguousModelRef, ModelRef, ModelRegistry, ParseModelRefError, ResolvedModelRef,
};
pub use project::ProjectLayer;
pub use run::RunLayer;
pub use server::ServerLayer;
pub use size::{ParseSizeError, Size};
pub use splice_array::{SPLICE_MARKER, SpliceArray, SpliceArrayError};
pub use tree::{ParseError, SettingsFile, parse_settings_file};
pub use version::{CURRENT_VERSION, SchemaVersion, VersionError, validate_version};
pub use workflow::WorkflowLayer;
