//! Fabro's platform half of a workflow run: what Fabro does around the
//! engine.
//!
//! Petri executes every run (`fabro-petri` is the seam). This crate keeps
//! what Fabro itself owns: the create-time compile of the Fabro graph the
//! read side displays (`pipeline`, `transforms`, `operations`), the run
//! records and status vocabulary (`records`, `run_status`), the Git
//! helpers a run's platform effects use (`git`, `git_identity`,
//! `sandbox_git`), pull request creation (`pull_request`), the run tools an
//! agent session calls (`run_tools`, `services`), the built-in web search
//! backend (`web_search`).

#![cfg_attr(
    test,
    allow(
        clippy::absolute_paths,
        clippy::get_unwrap,
        clippy::large_futures,
        clippy::needless_borrows_for_generic_args,
        clippy::option_option,
        clippy::ptr_as_ptr,
        clippy::ref_as_ptr,
        clippy::cast_ptr_alignment,
        clippy::uninlined_format_args,
        clippy::unnecessary_literal_bound,
        reason = "Test-only workflow helpers favor explicit fixtures over pedantic style lints."
    )
)]

pub mod error;
pub mod file_resolver;
pub mod git;
pub mod git_identity;
pub mod operations;
pub mod outcome;
pub mod pipeline;
pub mod pull_request;
pub mod records;
pub mod run_lookup;
pub mod usage_rollup;

pub use error::{Error, FailureCategory, FailureSignature, FailureSignatureExt, Result};
pub use fabro_types::ManifestPath;
pub use usage_rollup::{
    ProjectionUsageByModel, ProjectionUsageRollup, ProjectionUsageStage,
    usage_rollup_from_projection,
};
pub mod run_materialization;
pub mod run_status;
pub mod run_tools;
pub mod sandbox_git;
pub mod services;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[doc(hidden)]
pub mod transforms;
pub mod web_search;
pub mod workflow_bundle;
