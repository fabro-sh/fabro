use std::path::Path;

use super::start::{StartServices, Started, execute_persisted_run};
use crate::error::Error;
use crate::event::{Event, append_event_to_sink};
use crate::outcome::StageStatus;
use crate::run_status::RunStatus;

/// Resume a workflow run from its checkpoint. Errors if no checkpoint is found.
pub async fn resume(run_dir: &Path, services: StartServices) -> Result<Started, Error> {
    let state = services
        .run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    if let Some(record) = state.status {
        if record.status == RunStatus::Succeeded {
            return Err(Error::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }
    if let Some(conclusion) = state.conclusion {
        if matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped
        ) {
            return Err(Error::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }

    let checkpoint = state
        .checkpoint
        .ok_or_else(|| Error::Precondition("no checkpoint to resume from".to_string()))?;
    let definition_blob = state.run.as_ref().and_then(|run| run.definition_blob);

    cleanup_resume_artifacts(run_dir);
    append_event_to_sink(
        &services.event_sink,
        &services.run_id,
        &Event::RunSubmitted {
            reason: None,
            definition_blob,
        },
    )
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    Box::pin(execute_persisted_run(run_dir, Some(checkpoint), services)).await
}

fn cleanup_resume_artifacts(run_dir: &Path) {
    let _ = run_dir;
}
