//! Fabro's platform half of a workflow run: what Fabro does around the
//! engine.
//!
//! Petri compiles and executes every run (`fabro-petri` is the seam). This
//! crate keeps what Fabro itself owns: the run's creation around Petri's
//! admission and the other run operations (`operations`), the bundle a run
//! is created from (`workflow_bundle`), the Git helpers a run's platform
//! effects use (`git`), pull request creation
//! (`pull_request`), the run tools an agent session calls (`run_tools`,
//! `services`), the built-in web search backend (`web_search`). The run
//! records and status vocabulary are `fabro_types`'.

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
pub mod git;
pub mod operations;
pub mod pull_request;
pub mod run_lookup;

pub use error::{Error, Result};
pub use fabro_types::ManifestPath;
pub mod run_tools;
pub mod services;
pub mod web_search;
pub mod workflow_bundle;
