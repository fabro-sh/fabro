/// Convert a Duration's milliseconds to u64, saturating on overflow.
pub(crate) fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Save a value as pretty-printed JSON to a file.
pub(crate) fn save_json<T: serde::Serialize>(
    value: &T,
    path: &std::path::Path,
    label: &str,
) -> error::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| error::FabroError::Checkpoint(format!("{label} serialize failed: {e}")))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a value from a JSON file.
pub(crate) fn load_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    label: &str,
) -> error::Result<T> {
    let data = std::fs::read_to_string(path)?;
    serde_json::from_str(&data)
        .map_err(|e| error::FabroError::Checkpoint(format!("{label} deserialize failed: {e}")))
}

/// Build `Vec<CompletedStage>` from a `Checkpoint`, mapping workflow-engine
/// types into the flat struct expected by `fabro_retro::retro::derive_retro`.
pub fn build_completed_stages(
    cp: &checkpoint::Checkpoint,
    run_failed: bool,
) -> Vec<fabro_retro::retro::CompletedStage> {
    use outcome::{OutcomeExt, StageStatus};

    let mut stages = Vec::new();
    let mut any_stage_failed = false;

    for node_id in &cp.completed_nodes {
        let outcome = cp.node_outcomes.get(node_id);
        let retries = cp.node_retries.get(node_id).copied().unwrap_or(0);

        let status = outcome
            .map(|o| o.status.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let succeeded = matches!(
            outcome.map(|o| &o.status),
            Some(StageStatus::Success | StageStatus::PartialSuccess)
        );
        let failed = matches!(outcome.map(|o| &o.status), Some(StageStatus::Fail));
        if failed {
            any_stage_failed = true;
        }

        stages.push(fabro_retro::retro::CompletedStage {
            node_id: node_id.clone(),
            status,
            succeeded,
            failed,
            retries,
            cost: outcome.and_then(|o| o.usage.as_ref()).and_then(|u| u.cost),
            notes: outcome.and_then(|o| o.notes.clone()),
            failure_reason: outcome.and_then(|o| o.failure_reason().map(String::from)),
            files_touched: outcome.map(|o| o.files_touched.clone()).unwrap_or_default(),
        });
    }

    // If run failed with an error not captured in stages, mark the last stage
    if run_failed && !any_stage_failed {
        if let Some(last) = stages.last_mut() {
            last.failed = true;
        } else {
            stages.push(fabro_retro::retro::CompletedStage {
                node_id: "unknown".to_string(),
                status: "fail".to_string(),
                succeeded: false,
                failed: true,
                retries: 0,
                cost: None,
                notes: None,
                failure_reason: None,
                files_touched: vec![],
            });
        }
    }

    stages
}

pub mod artifact;
pub mod asset_snapshot;
pub mod assets;
pub mod backend;
pub mod checkpoint;
pub mod conclusion;
pub mod condition;
pub mod context;
pub mod core_adapter;
pub mod cost;
pub mod devcontainer_bridge;
pub mod error;
pub mod event;
pub mod git;
pub mod graph_ops;
pub mod graph_render;
pub mod handler;
pub mod operations;
pub mod outcome;
pub mod pipeline;
pub mod preamble;
pub mod pull_request;
pub mod run_dir;
pub mod run_lookup;
pub mod run_record;
pub mod run_settings;
pub mod run_status;
pub mod sandbox_git;
pub mod sandbox_provider;
pub mod sandbox_reconnect;
pub mod sandbox_record;
pub mod start_record;
pub mod stylesheet;
#[doc(hidden)]
pub mod test_support;
pub mod transform;
pub mod vars;
