use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use fabro_client::Client;
use fabro_types::{Run, RunId, RunStatus};
use fabro_util::exit::{self, ExitClass};
use rmcp::model::{CallToolResult, Content};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug)]
pub(crate) struct ToolError {
    message: String,
}

impl ToolError {
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn from_anyhow(err: &anyhow::Error) -> Self {
        Self::message(format_tool_error(err))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.message
    }
}

pub(super) type ToolResult<T> = Result<T, ToolError>;

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RunSummaryResult {
    pub(crate) run_id:           String,
    pub(crate) workflow_name:    String,
    pub(crate) workflow_slug:    Option<String>,
    pub(crate) status:           String,
    pub(crate) archived:         bool,
    pub(crate) created_at:       String,
    pub(crate) started_at:       Option<String>,
    pub(crate) completed_at:     Option<String>,
    pub(crate) labels:           HashMap<String, String>,
    pub(crate) source_directory: Option<String>,
    pub(crate) repo_origin_url:  Option<String>,
    pub(crate) goal:             String,
}

pub(crate) fn success_result<T: Serialize>(
    value: &T,
    text: impl Into<String>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    let structured_content = serde_json::to_value(value).map_err(|err| {
        rmcp::ErrorData::internal_error(
            format!("failed to serialize Fabro MCP tool result: {err}"),
            None,
        )
    })?;
    let mut result = CallToolResult::structured(structured_content);
    result.content = vec![Content::text(text.into())];
    Ok(result)
}

pub(crate) fn error_result(err: ToolError) -> CallToolResult {
    CallToolResult::error(vec![Content::text(err.message)])
}

pub(super) fn validate_len(name: &str, len: usize, min: usize, max: usize) -> ToolResult<()> {
    if len < min {
        return Err(ToolError::message(format!(
            "{name} must contain at least {min} item(s)"
        )));
    }
    if len > max {
        return Err(ToolError::message(format!(
            "{name} must contain no more than {max} item(s)"
        )));
    }
    Ok(())
}

pub(super) async fn retrieve_run(client: &Client, run_id: &RunId) -> ToolResult<Run> {
    client
        .retrieve_run(run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))
}

pub(super) fn run_summary_result(run: &Run) -> RunSummaryResult {
    RunSummaryResult {
        run_id:           run.id.to_string(),
        workflow_name:    run.workflow.name.clone(),
        workflow_slug:    run.workflow.slug.clone(),
        status:           run_status_kind(run.lifecycle.status).to_string(),
        archived:         run.lifecycle.archived,
        created_at:       run.timestamps.created_at.to_rfc3339(),
        started_at:       run
            .timestamps
            .started_at
            .map(|timestamp| timestamp.to_rfc3339()),
        completed_at:     run
            .timestamps
            .completed_at
            .map(|timestamp| timestamp.to_rfc3339()),
        labels:           run.labels.clone(),
        source_directory: run.source_directory.clone(),
        repo_origin_url:  run
            .repository
            .as_ref()
            .and_then(|repository| repository.origin_url.clone()),
        goal:             run.goal.clone(),
    }
}

pub(super) fn parse_datetime_filter(name: &str, raw: &str) -> ToolResult<DateTime<Utc>> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|err| {
        ToolError::message(format!("{name} must be RFC3339 or YYYY-MM-DD: {err}"))
    })?;
    let datetime = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| ToolError::message(format!("{name} contains an invalid date")))?;
    Ok(DateTime::from_naive_utc_and_offset(datetime, Utc))
}

pub(super) fn run_status_kind(status: RunStatus) -> &'static str {
    status.kind().into()
}

fn format_tool_error(err: &anyhow::Error) -> String {
    let mut rendered = format!("{err:#}");
    if exit::exit_class_for(err) == Some(ExitClass::AuthRequired)
        && !rendered.contains("fabro auth login")
    {
        rendered.push_str("\nRun `fabro auth login` to authenticate.");
    }
    rendered
}
