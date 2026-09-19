mod create;
mod fork;
mod retry;
mod rewind;
mod source;
mod timeline;
mod validate;

pub use create::{
    CompiledRun, CreateRunCompileInput, CreateRunPersistenceInput, CreateRunPersistenceMetadata,
    CreatedRun, MaterializedRun, assemble_create_run_persistence_input, compile_admitted_run,
    make_run_dir, materialize_admitted_run, persist_create_run,
};
use fabro_types::RunId;
pub use fork::{
    ForkedRunInput, ResolvedForkTarget, ensure_forkable, ensure_terminal, forked_run_record,
    persist_forked_run,
};
pub use retry::{ensure_retryable, reruns_last};
pub use rewind::{ensure_rewindable, superseded_record};
pub use source::WorkflowInput;
pub use timeline::{
    ForkTarget, RunTimeline, StageLabel, StageLabels, TimelineEntry, TimelinePosition,
};
pub use validate::{ValidateInput, validate};

pub use crate::error::Error;
pub use crate::transforms::RenderMode;

/// The canonical "run is archived — mutation rejected" error message. Shared
/// by the server's HTTP guards and the CLI so the user sees the same
/// actionable guidance everywhere.
#[must_use]
pub fn archived_rejection_message(run_id: &RunId) -> String {
    format!("run {run_id} is archived; run `fabro unarchive {run_id}` to restore it and try again")
}

/// Returns `Err(Error::Precondition)` when the given status represents an
/// archived run. Use this at any mutation entry point that would otherwise
/// transition the run.
pub fn ensure_not_archived(archived: bool, run_id: &RunId) -> Result<(), Error> {
    if archived {
        Err(Error::Precondition(archived_rejection_message(run_id)))
    } else {
        Ok(())
    }
}
