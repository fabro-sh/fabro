use crate::error::FabroError;

use super::types::{FinalizeOptions, Finalized, Retroed};

/// FINALIZE phase: classify outcome, build conclusion, persist terminal state.
///
/// # Errors
///
/// Returns `FabroError` if persisting terminal state fails.
pub async fn finalize(
    retroed: Retroed,
    _options: &FinalizeOptions,
) -> Result<Finalized, FabroError> {
    let Retroed {
        graph: _,
        outcome,
        config,
        engine: _,
        emitter: _,
        sandbox: _,
        duration_ms: _,
        retro: _,
    } = retroed;

    // TODO: Extract finalize logic from CLI run.rs in Step 5.
    // For now, return a minimal Finalized.
    let conclusion = crate::conclusion::Conclusion {
        timestamp: chrono::Utc::now(),
        status: match &outcome {
            Ok(o) => o.status.clone(),
            Err(_) => crate::outcome::StageStatus::Fail,
        },
        duration_ms: 0,
        failure_reason: outcome.as_ref().err().map(|e| e.to_string()),
        final_git_commit_sha: None,
        stages: vec![],
        total_cost: None,
        total_retries: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_write_tokens: 0,
        total_reasoning_tokens: 0,
        has_pricing: false,
    };

    Ok(Finalized {
        run_id: config.run_id,
        outcome,
        conclusion,
        pr_url: None,
    })
}
