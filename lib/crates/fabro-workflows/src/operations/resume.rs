use std::path::Path;

use fabro_store::RuntimeState;

use crate::error::FabroError;
use crate::outcome::StageStatus;
use crate::records::{Checkpoint, CheckpointExt, Conclusion, ConclusionExt};
use crate::run_status::{self, RunStatus, RunStatusRecordExt};

use super::start::{StartServices, Started, execute_persisted_run};

/// Resume a workflow run from its checkpoint. Errors if no checkpoint is found.
pub async fn resume(run_dir: &Path, services: StartServices) -> Result<Started, FabroError> {
    if let Ok(record) = run_status::RunStatusRecord::load(&run_dir.join("status.json")) {
        if record.status == RunStatus::Succeeded {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }
    if let Ok(conclusion) = Conclusion::load(&run_dir.join("conclusion.json")) {
        if matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped
        ) {
            return Err(FabroError::Precondition(
                "run already finished successfully — nothing to resume".to_string(),
            ));
        }
    }

    let cp_path = run_dir.join("checkpoint.json");
    let checkpoint = Checkpoint::load(&cp_path)
        .map_err(|e| FabroError::Precondition(format!("no checkpoint to resume from: {e}")))?;

    cleanup_resume_artifacts(run_dir);
    run_status::write_run_status(run_dir, RunStatus::Submitted, None);

    execute_persisted_run(run_dir, Some(checkpoint), services).await
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

    for name in [
        "conclusion.json",
        "pull_request.json",
        "detached_failure.json",
        "interview_request.json",
        "interview_response.json",
        "interview_request.claim",
        "progress.jsonl",
    ] {
        let _ = std::fs::remove_file(run_dir.join(name));
    }
}
