use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use fabro_api::types;
use fabro_types::{
    PairId, PairMessageRecord, PairMessageRequest, PairRecord, PairTranscriptResponse, Run, RunId,
    RunPairStatusResponse, StageId,
};
use fabro_util::exit::{self, ExitClass};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn from_anyhow(err: &anyhow::Error) -> Self {
        Self::message(format_tool_error(err))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

pub type ToolResult<T> = Result<T, ToolError>;

#[async_trait]
pub trait FabroToolBackend: Send + Sync {
    async fn create_run_from_spec(
        &self,
        spec: &crate::ValidatedCreateRunSpec,
        cwd: &Path,
        user_settings_path: &Path,
        parent_id: Option<RunId>,
    ) -> anyhow::Result<RunId>;

    async fn resolve_run(&self, selector: &str) -> anyhow::Result<Run>;
    async fn retrieve_run(&self, run_id: &RunId) -> anyhow::Result<Run>;
    async fn start_run(&self, run_id: &RunId, resume: bool) -> anyhow::Result<Run>;
    async fn approve_run(&self, run_id: &RunId) -> anyhow::Result<Run>;
    async fn deny_run(&self, run_id: &RunId, reason: Option<String>) -> anyhow::Result<Run>;
    async fn cancel_run(&self, run_id: &RunId) -> anyhow::Result<Run>;
    async fn interrupt_run(&self, run_id: &RunId) -> anyhow::Result<()>;
    async fn steer_run(&self, run_id: &RunId, text: String, interrupt: bool) -> anyhow::Result<()>;
    async fn archive_run(&self, run_id: &RunId) -> anyhow::Result<Run>;
    async fn unarchive_run(&self, run_id: &RunId) -> anyhow::Result<Run>;
    async fn list_store_runs(&self) -> anyhow::Result<Vec<Run>>;
    async fn list_store_runs_by_parent(&self, parent_id: RunId) -> anyhow::Result<Vec<Run>>;
    async fn link_run_parent(&self, child_id: &RunId, parent_id: &RunId) -> anyhow::Result<Run>;
    async fn unlink_run_parent(&self, child_id: &RunId) -> anyhow::Result<Run>;
    async fn get_run_state(&self, run_id: &RunId) -> anyhow::Result<fabro_types::RunProjection>;
    async fn list_run_events(
        &self,
        run_id: &RunId,
        after: Option<u32>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<fabro_types::EventEnvelope>>;
    async fn list_run_events_until(
        &self,
        run_id: &RunId,
        after: Option<u32>,
        limit: usize,
    ) -> anyhow::Result<Vec<fabro_types::EventEnvelope>>;
    async fn list_run_questions(&self, run_id: &RunId) -> anyhow::Result<Vec<types::ApiQuestion>>;
    async fn submit_run_answer(
        &self,
        run_id: &RunId,
        question_id: &str,
        body: types::SubmitAnswerRequest,
    ) -> anyhow::Result<()>;

    async fn get_run_pair_status(&self, _run_id: &RunId) -> anyhow::Result<RunPairStatusResponse> {
        Err(pair_tool_unavailable_error())
    }

    async fn start_run_pair(
        &self,
        _run_id: &RunId,
        _stage_id: StageId,
    ) -> anyhow::Result<PairRecord> {
        Err(pair_tool_unavailable_error())
    }

    async fn get_run_pair(&self, _run_id: &RunId, _pair_id: &PairId) -> anyhow::Result<PairRecord> {
        Err(pair_tool_unavailable_error())
    }

    async fn end_run_pair(&self, _run_id: &RunId, _pair_id: &PairId) -> anyhow::Result<PairRecord> {
        Err(pair_tool_unavailable_error())
    }

    async fn send_run_pair_message(
        &self,
        _run_id: &RunId,
        _pair_id: &PairId,
        _request: PairMessageRequest,
    ) -> anyhow::Result<PairMessageRecord> {
        Err(pair_tool_unavailable_error())
    }

    async fn get_run_pair_transcript(
        &self,
        _run_id: &RunId,
        _pair_id: &PairId,
        _since_seq: Option<u32>,
        _limit: Option<u32>,
    ) -> anyhow::Result<PairTranscriptResponse> {
        Err(pair_tool_unavailable_error())
    }
}

fn pair_tool_unavailable_error() -> anyhow::Error {
    ToolError::message(format!("{FABRO_RUN_PAIR_TOOL_NAME} is not available")).into()
}

pub trait RunManifestBuilder: Send + Sync {
    fn build_run_manifest(
        &self,
        spec: &crate::ValidatedCreateRunSpec,
        cwd: &Path,
        user_settings_path: &Path,
    ) -> ToolResult<types::RunManifest>;
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunSummaryResult {
    pub run_id:              String,
    pub parent_id:           Option<String>,
    pub children_count:      u64,
    pub workflow_name:       Option<String>,
    pub workflow_graph_name: Option<String>,
    pub workflow_slug:       Option<String>,
    pub status:              String,
    pub archived:            bool,
    pub created_at:          String,
    pub started_at:          Option<String>,
    pub completed_at:        Option<String>,
    pub labels:              HashMap<String, String>,
    pub source_directory:    Option<String>,
    pub repo_origin_url:     Option<String>,
    pub goal:                String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name:        &'static str,
    pub description: &'static str,
    pub parameters:  Value,
}

pub const FABRO_RUN_CREATE_TOOL_NAME: &str = "fabro_run_create";
pub const FABRO_RUN_SEARCH_TOOL_NAME: &str = "fabro_run_search";
pub const FABRO_RUN_GET_TOOL_NAME: &str = "fabro_run_get";
pub const FABRO_RUN_INTERACT_TOOL_NAME: &str = "fabro_run_interact";
pub const FABRO_RUN_GATHER_TOOL_NAME: &str = "fabro_run_gather";
pub const FABRO_RUN_EVENTS_TOOL_NAME: &str = "fabro_run_events";
pub const FABRO_RUN_PAIR_TOOL_NAME: &str = "fabro_run_pair";

static TOOL_DEFINITIONS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    vec![
        tool_definition::<crate::FabroRunCreateParams>(
            FABRO_RUN_CREATE_TOOL_NAME,
            "Create one or more Fabro workflow runs, optionally under a parent run, starting them by default.",
        ),
        tool_definition::<crate::FabroRunSearchParams>(
            FABRO_RUN_SEARCH_TOOL_NAME,
            "Search Fabro workflow runs by id, parent, workflow, labels, status, archival state, and creation time.",
        ),
        tool_definition::<crate::FabroRunGetParams>(
            FABRO_RUN_GET_TOOL_NAME,
            "Read-only inspection of a Fabro run: returns its summary, projection, and pending questions without mutating state.",
        ),
        tool_definition::<crate::FabroRunInteractParams>(
            FABRO_RUN_INTERACT_TOOL_NAME,
            "Control a Fabro run: start, approve, deny, message, interrupt, cancel, archive, unarchive, link or unlink a parent, inspect or answer questions. Use fabro_run_get for read-only inspection.",
        ),
        tool_definition::<crate::FabroRunGatherParams>(
            FABRO_RUN_GATHER_TOOL_NAME,
            "Wait for Fabro runs to reach terminal states, returning current state on timeout.",
        ),
        tool_definition::<crate::FabroRunPairParams>(
            FABRO_RUN_PAIR_TOOL_NAME,
            "Inspect, start, message, end, or read transcript for a live Fabro run pairing session.",
        ),
        tool_definition::<crate::FabroRunEventsParams>(
            FABRO_RUN_EVENTS_TOOL_NAME,
            "List, inspect, or search stored events for a Fabro workflow run.",
        ),
    ]
});

#[must_use]
pub fn tool_definitions() -> &'static [ToolDefinition] {
    TOOL_DEFINITIONS.as_slice()
}

fn tool_definition<T>(name: &'static str, description: &'static str) -> ToolDefinition
where
    T: JsonSchema,
{
    ToolDefinition {
        name,
        description,
        parameters: serde_json::to_value(schemars::schema_for!(T))
            .expect("tool parameter schema should serialize"),
    }
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

pub(super) async fn retrieve_run(
    backend: &dyn FabroToolBackend,
    run_id: &RunId,
) -> ToolResult<Run> {
    backend
        .retrieve_run(run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))
}

pub(crate) fn run_summary_result(run: &Run) -> RunSummaryResult {
    RunSummaryResult {
        run_id:              run.id.to_string(),
        parent_id:           run.parent_id.map(|parent_id| parent_id.to_string()),
        children_count:      run.children_count,
        workflow_name:       run.workflow.name.clone(),
        workflow_graph_name: run.workflow.graph_name.clone(),
        workflow_slug:       run.workflow.slug.clone(),
        status:              run.lifecycle.status.kind().to_string(),
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

pub(crate) fn parse_datetime_filter(name: &str, raw: &str) -> ToolResult<DateTime<Utc>> {
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
    use fabro_types::{
        RunLifecycle, RunLinks, RunOrigin, RunStatus, RunTimestamps, WorkflowRef, test_support,
    };

    use super::*;

    fn shared_tool_names() -> Vec<&'static str> {
        tool_definitions()
            .iter()
            .map(|definition| definition.name)
            .collect()
    }

    #[test]
    fn shared_tool_definitions_include_run_management_catalog() {
        assert_eq!(shared_tool_names(), vec![
            FABRO_RUN_CREATE_TOOL_NAME,
            FABRO_RUN_SEARCH_TOOL_NAME,
            FABRO_RUN_GET_TOOL_NAME,
            FABRO_RUN_INTERACT_TOOL_NAME,
            FABRO_RUN_GATHER_TOOL_NAME,
            FABRO_RUN_PAIR_TOOL_NAME,
            FABRO_RUN_EVENTS_TOOL_NAME,
        ]);
    }

    #[test]
    fn pair_tool_definition_exposes_pair_schema() {
        let definition = tool_definitions()
            .iter()
            .find(|definition| definition.name == FABRO_RUN_PAIR_TOOL_NAME)
            .expect("pair tool should be in the shared catalog");
        let schema = &definition.parameters;
        let schema_text = schema.to_string();

        assert_eq!(
            definition.description,
            "Inspect, start, message, end, or read transcript for a live Fabro run pairing session."
        );
        for field in [
            "action",
            "run_id",
            "pair_id",
            "stage_id",
            "text",
            "client_message_id",
            "since_seq",
            "limit",
        ] {
            assert!(
                schema.pointer(&format!("/properties/{field}")).is_some(),
                "pair schema should expose {field}: {schema}"
            );
        }
        for action in ["status", "start", "get", "message", "end", "transcript"] {
            assert!(
                schema_text.contains(&format!("\"{action}\"")),
                "pair schema should expose action {action}: {schema}"
            );
        }
    }

    #[test]
    fn interact_tool_definition_exposes_approval_schema() {
        let definition = tool_definitions()
            .iter()
            .find(|definition| definition.name == FABRO_RUN_INTERACT_TOOL_NAME)
            .expect("interact tool should be in the shared catalog");
        let schema = &definition.parameters;
        let schema_text = schema.to_string();

        assert!(
            definition.description.contains("approve") && definition.description.contains("deny"),
            "interact description should include approval actions: {}",
            definition.description
        );
        for field in ["action", "run_id", "reason"] {
            assert!(
                schema.pointer(&format!("/properties/{field}")).is_some(),
                "interact schema should expose {field}: {schema}"
            );
        }
        for action in ["approve", "deny"] {
            assert!(
                schema_text.contains(&format!("\"{action}\"")),
                "interact schema should expose action {action}: {schema}"
            );
        }
    }

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
                node_count: 0,
                edge_count: 0,
            },
            automation:       None,
            repository:       None,
            created_by:       test_support::test_principal(),
            origin:           RunOrigin::default(),
            labels:           HashMap::new(),
            lifecycle:        RunLifecycle {
                status:          RunStatus::Submitted,
                approval:        None,
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
            },
            timing:           None,
            billing:          None,
            size:             fabro_types::RunSize::default(),
            ask_fabro:        fabro_types::AskFabro::default(),
            diff:             None,
            pull_request:     None,
            current_question: None,
            superseded_by:    None,
            retried_from:     None,
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
