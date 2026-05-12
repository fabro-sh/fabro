use std::sync::Arc;
use std::time::{Duration, Instant};

use fabro_client::Client;
use futures::future::try_join_all;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time;

use super::common;
use super::common::{RunSummaryResult, ToolError, ToolResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct FabroRunGatherParams {
    pub(crate) run_ids:               Vec<String>,
    pub(crate) timeout_seconds:       Option<u64>,
    pub(crate) poll_interval_seconds: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ValidatedGatherRuns {
    pub(crate) run_ids:               Vec<String>,
    pub(crate) timeout_seconds:       u64,
    pub(crate) poll_interval_seconds: u64,
}

impl TryFrom<FabroRunGatherParams> for ValidatedGatherRuns {
    type Error = ToolError;

    fn try_from(params: FabroRunGatherParams) -> Result<Self, Self::Error> {
        common::validate_len("run_ids", params.run_ids.len(), 1, 50)?;
        if params.timeout_seconds.is_some_and(|timeout| timeout > 600) {
            return Err(ToolError::message("timeout_seconds must be <= 600"));
        }
        if params
            .poll_interval_seconds
            .is_some_and(|interval| interval < 5)
        {
            return Err(ToolError::message("poll_interval_seconds must be >= 5"));
        }
        Ok(Self {
            run_ids:               params.run_ids,
            timeout_seconds:       params.timeout_seconds.unwrap_or(300),
            poll_interval_seconds: params.poll_interval_seconds.unwrap_or(15),
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct GatherRunsResult {
    pub(crate) runs:            Vec<RunSummaryResult>,
    pub(crate) timed_out:       bool,
    pub(crate) elapsed_seconds: u64,
}

pub(crate) async fn gather_runs(
    client: Arc<Client>,
    params: ValidatedGatherRuns,
) -> ToolResult<GatherRunsResult> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(params.timeout_seconds);
    let run_ids = try_join_all(params.run_ids.into_iter().map(|selector| {
        let client = Arc::clone(&client);
        async move {
            client
                .resolve_run(&selector)
                .await
                .map(|run| run.id)
                .map_err(|err| ToolError::from_anyhow(&err))
        }
    }))
    .await?;

    loop {
        let summaries = try_join_all(run_ids.iter().map(|run_id| {
            let client = Arc::clone(&client);
            async move { common::retrieve_run(&client, run_id).await }
        }))
        .await?;
        if summaries
            .iter()
            .all(|run| run.lifecycle.status.is_terminal())
        {
            return Ok(GatherRunsResult {
                runs:            summaries.iter().map(common::run_summary_result).collect(),
                timed_out:       false,
                elapsed_seconds: start.elapsed().as_secs(),
            });
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(GatherRunsResult {
                runs:            summaries.iter().map(common::run_summary_result).collect(),
                timed_out:       true,
                elapsed_seconds: start.elapsed().as_secs(),
            });
        }
        let sleep_for = Duration::from_secs(params.poll_interval_seconds).min(deadline - now);
        time::sleep(sleep_for).await;
    }
}

pub(crate) fn gather_runs_text(result: &GatherRunsResult) -> String {
    format!(
        "gathered {} Fabro run(s), timed_out={}",
        result.runs.len(),
        result.timed_out
    )
}
