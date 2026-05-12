#![allow(
    dead_code,
    reason = "MCP DTO fields are consumed by serde and schema generation even when not read directly."
)]

mod common;
mod create;
mod events;
mod gather;
mod interact;
mod manifest;
mod search;

pub(crate) use common::{ToolError, error_result, success_result};
pub(crate) use create::{FabroRunCreateParams, ValidatedCreateRuns, create_runs, create_runs_text};
pub(crate) use events::{FabroRunEventsParams, ValidatedRunEvents, run_events, run_events_text};
pub(crate) use gather::{FabroRunGatherParams, ValidatedGatherRuns, gather_runs, gather_runs_text};
pub(crate) use interact::{
    FabroRunInteractParams, ValidatedInteractRun, interact_run, interact_run_text,
};
pub(crate) use search::{FabroRunSearchParams, ValidatedSearchRuns, search_runs, search_runs_text};
