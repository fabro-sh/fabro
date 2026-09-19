//! Rewinding a run: a fork that replaces its source.
//!
//! A rewind forks a terminal run at a checkpoint (`fork`), then archives the
//! source and records `run.superseded` on it, naming the new run and the
//! checkpoint it continues from. The archive is the server's own archive
//! operation; what this module holds is the precondition and the record.

use fabro_store::platform_records::{PlatformRecord, RunSupersededRecord};
use fabro_types::{RunId, RunProjection};

use super::fork::{ResolvedForkTarget, ensure_forkable, ensure_terminal};
use crate::error::Error;

/// A run can be rewound when it is terminal and not archived.
pub fn ensure_rewindable(source: &RunProjection, run_id: &RunId) -> Result<(), Error> {
    ensure_forkable(source, run_id)?;
    ensure_terminal(source, run_id, "rewind")
}

/// The record a rewound source carries: which run replaced it, from which
/// checkpoint.
#[must_use]
pub fn superseded_record(new_run_id: RunId, target: &ResolvedForkTarget) -> PlatformRecord {
    PlatformRecord::RunSuperseded(RunSupersededRecord {
        new_run_id,
        target_checkpoint_ordinal: target.checkpoint_ordinal,
        target_node_id: target.node_id.clone(),
        target_visit: target.visit,
    })
}
