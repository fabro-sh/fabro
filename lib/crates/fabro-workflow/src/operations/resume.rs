use std::path::Path;

use fabro_store::RuntimeState;

use crate::error::FabroError;
use crate::outcome::StageStatus;
use crate::run_status::{self, RunStatus};

use super::start::{StartServices, Started, execute_persisted_run};

/// Resume a workflow run from its checkpoint. Errors if no checkpoint is found.
pub async fn resume(run_dir: &Path, services: StartServices) -> Result<Started, FabroError> {
    if let Some(record) = services
        .run_store
        .get_status()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?
    {
        if record.status == RunStatus::Succeeded {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }
    if let Some(conclusion) = services
        .run_store
        .get_conclusion()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?
    {
        if matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped
        ) {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }

    let checkpoint = services
        .run_store
        .get_checkpoint()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?
        .ok_or_else(|| FabroError::Precondition("no checkpoint to resume from".to_string()))?;

    cleanup_resume_artifacts(run_dir);
    services
        .run_store
        .put_status(&run_status::RunStatusRecord::new(
            RunStatus::Submitted,
            None,
        ))
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?;

    Box::pin(execute_persisted_run(run_dir, Some(checkpoint), services)).await
}

fn cleanup_resume_artifacts(run_dir: &Path) {
    let runtime_state = RuntimeState::new(run_dir);
    for path in [
        runtime_state.interview_request_path(),
        runtime_state.interview_response_path(),
        runtime_state.interview_claim_path(),
    ] {
        let _ = std::fs::remove_file(path);
    }

    for name in ["detached_failure.json"] {
        let _ = std::fs::remove_file(run_dir.join(name));
    }
}
