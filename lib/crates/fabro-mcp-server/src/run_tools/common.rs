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
    pub(crate) run_id:              String,
    pub(crate) parent_id:           Option<String>,
    pub(crate) children_count:      u64,
    pub(crate) workflow_name:       Option<String>,
    pub(crate) workflow_graph_name: Option<String>,
    pub(crate) workflow_slug:       Option<String>,
    pub(crate) status:              String,
    pub(crate) archived:            bool,
    pub(crate) created_at:          String,
    pub(crate) started_at:          Option<String>,
    pub(crate) completed_at:        Option<String>,
    pub(crate) labels:              HashMap<String, String>,
    pub(crate) source_directory:    Option<String>,
    pub(crate) repo_origin_url:     Option<String>,
    pub(crate) goal:                String,
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
        run_id:              run.id.to_string(),
        parent_id:           run.parent_id.map(|parent_id| parent_id.to_string()),
        children_count:      run.children_count,
        workflow_name:       run.workflow.name.clone(),
        workflow_graph_name: run.workflow.graph_name.clone(),
        workflow_slug:       run.workflow.slug.clone(),
        status:              run_status_kind(run.lifecycle.status).to_string(),
        archived:            run.lifecycle.archived,
        created_at:          run.timestamps.created_at.to_rfc3339(),
        started_at:          run
            .timestamps
            .started_at
            .map(|timestamp| timestamp.to_rfc3339()),
        completed_at:        run
            .timestamps
            .completed_at
            .map(|timestamp| timestamp.to_rfc3339()),
        labels:              run.labels.clone(),
        source_directory:    run.source_directory.clone(),
        repo_origin_url:     run
            .repository
            .as_ref()
            .and_then(|repository| repository.origin_url.clone()),
        goal:                run.goal.clone(),
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::{RunLifecycle, RunLinks, RunOrigin, RunTimestamps, WorkflowRef};

    use super::*;

    #[test]
    fn run_summary_result_includes_parent_metadata() {
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let run = Run {
            id:               run_id("01KRBZW5C00000000000000001"),
            parent_id:        Some(parent_id),
            children_count:   3,
            title:            "test".to_string(),
            goal:             "test".to_string(),
            workflow:         WorkflowRef {
                slug:       Some("simple".to_string()),
                name:       Some("Simple".to_string()),
                graph_name: Some("GraphName".to_string()),
            },
            automation:       None,
            repository:       None,
            created_by:       None,
            origin:           RunOrigin::default(),
            labels:           HashMap::new(),
            lifecycle:        RunLifecycle {
                status:          RunStatus::Submitted,
                pending_control: None,
                queue_position:  None,
                error:           None,
                archived:        false,
                archived_at:     None,
            },
            sandbox:          None,
            models:           Vec::new(),
            source_directory: None,
            timestamps:       RunTimestamps {
                created_at:    Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap(),
                started_at:    None,
                last_event_at: None,
                completed_at:  None,
                duration_ms:   None,
                elapsed_secs:  None,
            },
            billing:          None,
            diff:             None,
            pull_request:     None,
            current_question: None,
            superseded_by:    None,
            links:            RunLinks { web: None },
        };

        let summary = run_summary_result(&run);

        assert_eq!(summary.parent_id, Some(parent_id.to_string()));
        assert_eq!(summary.children_count, 3);
        assert_eq!(summary.workflow_name.as_deref(), Some("Simple"));
        assert_eq!(summary.workflow_graph_name.as_deref(), Some("GraphName"));
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }
}
