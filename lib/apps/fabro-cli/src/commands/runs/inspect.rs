use std::collections::BTreeMap;

use anyhow::Result;
use fabro_types::{StageHandler, StageState};
use fabro_workflow::run_status::RunStatus;
use serde::Serialize;

use crate::args::InspectArgs;
use crate::command_context::CommandContext;
use crate::server_client::RunProjection;
use crate::server_runs::ServerRunInfo;

#[derive(Debug, Serialize)]
pub(crate) struct InspectOutput {
    pub run_id:       String,
    pub parent_id:    Option<String>,
    pub status:       RunStatus,
    pub run_spec:     Option<serde_json::Value>,
    pub start_record: Option<serde_json::Value>,
    pub conclusion:   Option<serde_json::Value>,
    pub checkpoint:   Option<serde_json::Value>,
    pub sandbox:      Option<serde_json::Value>,
    /// The stages by their id, with what each produced. A large output or
    /// response is a `blob://sha256/<hex>` reference into the run's blob
    /// table, as the projection holds it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub stages:       BTreeMap<String, InspectStage>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectStage {
    pub handler:  Option<StageHandler>,
    pub state:    StageState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output:   Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff:     Option<String>,
}

pub(crate) async fn run(args: &InspectArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let client = ctx.server().await?;
    let run = ServerRunInfo::from_run(client.resolve_run(&args.run).await?);
    let run_id = run.run_id();
    let state = client.get_run_state(&run_id).await?;
    let output = inspect_run_state(&run, state);
    let json = serde_json::to_string_pretty(&[output])?;
    fabro_util::printout!(printer, "{json}");
    Ok(())
}

fn inspect_run_state(run: &ServerRunInfo, state: RunProjection) -> InspectOutput {
    let checkpoint = state
        .current_checkpoint()
        .and_then(|record| serde_json::to_value(record).ok());
    let stages = state
        .iter_stages()
        .map(|(stage_id, stage)| {
            (stage_id.to_string(), InspectStage {
                handler:  stage.handler,
                state:    stage.state,
                output:   stage.output.clone(),
                response: stage.response.clone(),
                diff:     stage.diff.clone(),
            })
        })
        .collect();
    InspectOutput {
        run_id: run.run_id().to_string(),
        parent_id: state.parent_id.map(|parent_id| parent_id.to_string()),
        status: state.status,
        run_spec: serde_json::to_value(state.spec).ok(),
        start_record: state
            .start
            .and_then(|record| serde_json::to_value(record).ok()),
        conclusion: state
            .conclusion
            .and_then(|record| serde_json::to_value(record).ok()),
        checkpoint,
        sandbox: state
            .sandbox
            .and_then(|record| serde_json::to_value(record).ok()),
        stages,
    }
}
