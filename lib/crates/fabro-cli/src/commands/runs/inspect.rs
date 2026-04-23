use anyhow::Result;
use fabro_workflow::run_status::RunStatus;
use serde::Serialize;

use crate::args::InspectArgs;
use crate::command_context::CommandContext;
use crate::server_client::RunProjection;
use crate::server_runs::ServerRunSummaryInfo;

#[derive(Debug, Serialize)]
pub(crate) struct InspectOutput {
    pub run_id:       String,
    pub status:       RunStatus,
    pub run_spec:     Option<serde_json::Value>,
    pub start_record: Option<serde_json::Value>,
    pub conclusion:   Option<serde_json::Value>,
    pub checkpoint:   Option<serde_json::Value>,
    pub sandbox:      Option<serde_json::Value>,
}

pub(crate) async fn run(args: &InspectArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let client = ctx.server().await?;
    let run = ServerRunSummaryInfo::from_summary(client.resolve_run(&args.run).await?);
    let run_id = run.run_id();
    let state = client.get_run_state(&run_id).await?;
    let output = inspect_run_state(&run, state);
    let json = serde_json::to_string_pretty(&[output])?;
    fabro_util::printout!(printer, "{json}");
    Ok(())
}

fn inspect_run_state(run: &ServerRunSummaryInfo, state: RunProjection) -> InspectOutput {
    InspectOutput {
        run_id:       run.run_id().to_string(),
        status:       state.status.unwrap_or(run.status()),
        run_spec:     state
            .spec
            .and_then(|record| serde_json::to_value(record).ok()),
        start_record: state
            .start
            .and_then(|record| serde_json::to_value(record).ok()),
        conclusion:   state
            .conclusion
            .and_then(|record| serde_json::to_value(record).ok()),
        checkpoint:   state
            .checkpoint
            .and_then(|record| serde_json::to_value(record).ok()),
        sandbox:      state
            .sandbox
            .and_then(|record| serde_json::to_value(record).ok()),
    }
}
