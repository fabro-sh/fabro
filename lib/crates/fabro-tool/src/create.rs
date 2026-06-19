use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_types::RunId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

use super::common::{self, FabroToolBackend, ToolError, ToolResult};
use super::manifest;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunCreateParams {
    pub runs: Vec<CreateRunSpecInput>,
}

#[derive(Debug)]
pub enum CreateRunSpecInput {
    Workflow(String),
    Spec(Box<CreateRunSpec>),
}

impl<'de> Deserialize<'de> for CreateRunSpecInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(workflow) => Ok(Self::Workflow(workflow)),
            Value::Object(_) => CreateRunSpec::deserialize(value)
                .map(Box::new)
                .map(Self::Spec)
                .map_err(de::Error::custom),
            other => Err(de::Error::custom(format!(
                "expected workflow string shorthand or create spec object, got {}",
                json_value_kind(&other)
            ))),
        }
    }
}

fn json_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

impl From<CreateRunSpec> for CreateRunSpecInput {
    fn from(spec: CreateRunSpec) -> Self {
        Self::Spec(Box::new(spec))
    }
}

impl JsonSchema for CreateRunSpecInput {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "CreateRunSpecInput".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Fabro run create specification. Use a workflow string shorthand, or an object when setting create options.",
            "anyOf": [
                {
                    "type": "string",
                    "description": "Workflow selector shorthand. Equivalent to an object with only the workflow field set."
                },
                {
                    "type": "object",
                    "description": "Full create-run specification.",
                    "required": ["workflow"],
                    "properties": {
                        "workflow": {
                            "type": "string",
                            "description": "Workflow selector, such as a workflow name or workflow file path."
                        },
                        "cwd": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Working directory used to resolve relative workflow paths."
                        },
                        "run_id": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Optional run id to use for the created run."
                        },
                        "parent_id": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Optional parent run id or selector."
                        },
                        "goal": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Optional goal override for the run."
                        },
                        "goal_file": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Read the run goal from a file. Mutually exclusive with goal. Relative paths are resolved from the run cwd."
                        },
                        "inputs": {
                            "type": "object",
                            "description": "Workflow input overrides keyed by input name.",
                            "additionalProperties": {
                                "description": "Run input override value. Inputs are TOML-compatible scalar values: string, boolean, integer, or float.",
                                "anyOf": [
                                    { "type": "string" },
                                    { "type": "boolean" },
                                    { "type": "integer" },
                                    { "type": "number" }
                                ]
                            }
                        },
                        "labels": {
                            "type": "object",
                            "description": "Labels to attach to the created run.",
                            "additionalProperties": { "type": "string" }
                        },
                        "dry_run": {
                            "anyOf": [
                                { "type": "boolean" },
                                { "type": "null" }
                            ],
                            "description": "Whether the run should use dry-run mode."
                        },
                        "auto_approve": {
                            "anyOf": [
                                { "type": "boolean" },
                                { "type": "null" }
                            ],
                            "description": "Whether agent approval prompts should be auto-approved."
                        },
                        "model": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Model override for the run."
                        },
                        "provider": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Provider override for the run."
                        },
                        "environment": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Named environment slug override for the run."
                        },
                        "preserve_sandbox": {
                            "anyOf": [
                                { "type": "boolean" },
                                { "type": "null" }
                            ],
                            "description": "Whether to preserve the sandbox after the run."
                        },
                        "start": {
                            "anyOf": [
                                { "type": "boolean" },
                                { "type": "null" }
                            ],
                            "description": "Whether to start the run immediately after creation. Defaults to true."
                        }
                    }
                }
            ]
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateRunSpec {
    pub workflow:         String,
    pub cwd:              Option<PathBuf>,
    pub run_id:           Option<String>,
    pub parent_id:        Option<String>,
    pub goal:             Option<String>,
    pub goal_file:        Option<PathBuf>,
    #[serde(default)]
    pub inputs:           HashMap<String, RunInputValue>,
    #[serde(default)]
    pub labels:           HashMap<String, String>,
    pub dry_run:          Option<bool>,
    pub auto_approve:     Option<bool>,
    pub model:            Option<String>,
    pub provider:         Option<String>,
    pub environment:      Option<String>,
    pub preserve_sandbox: Option<bool>,
    pub start:            Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct RunInputValue(Value);

impl From<Value> for RunInputValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl RunInputValue {
    pub(crate) fn into_inner(self) -> Value {
        self.0
    }
}

impl JsonSchema for RunInputValue {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "RunInputValue".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Run input override value. Inputs are TOML-compatible scalar values: string, boolean, integer, or float.",
            "anyOf": [
                { "type": "string" },
                { "type": "boolean" },
                { "type": "integer" },
                { "type": "number" }
            ]
        })
    }
}

#[derive(Debug)]
pub struct ValidatedCreateRuns {
    pub runs: Vec<ValidatedCreateRunSpec>,
}

#[derive(Debug)]
pub struct ValidatedCreateRunSpec {
    pub workflow:         String,
    pub cwd:              Option<PathBuf>,
    pub run_id:           Option<RunId>,
    pub parent_id:        Option<String>,
    pub goal:             Option<String>,
    pub goal_file:        Option<PathBuf>,
    pub inputs:           HashMap<String, toml::Value>,
    pub labels:           HashMap<String, String>,
    pub dry_run:          Option<bool>,
    pub auto_approve:     Option<bool>,
    pub model:            Option<String>,
    pub provider:         Option<String>,
    pub environment:      Option<String>,
    pub preserve_sandbox: Option<bool>,
    pub start:            Option<bool>,
}

impl TryFrom<FabroRunCreateParams> for ValidatedCreateRuns {
    type Error = ToolError;

    fn try_from(params: FabroRunCreateParams) -> Result<Self, Self::Error> {
        common::validate_len("runs", params.runs.len(), 1, 50)?;
        let runs = params
            .runs
            .into_iter()
            .map(ValidatedCreateRunSpec::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { runs })
    }
}

impl TryFrom<CreateRunSpecInput> for ValidatedCreateRunSpec {
    type Error = ToolError;

    fn try_from(spec: CreateRunSpecInput) -> Result<Self, Self::Error> {
        match spec {
            CreateRunSpecInput::Workflow(workflow) => {
                let workflow = workflow.trim();
                if workflow.is_empty() {
                    return Err(ToolError::message("workflow must not be blank"));
                }
                Self::try_from(CreateRunSpec {
                    workflow:         workflow.to_string(),
                    cwd:              None,
                    run_id:           None,
                    parent_id:        None,
                    goal:             None,
                    goal_file:        None,
                    inputs:           HashMap::new(),
                    labels:           HashMap::new(),
                    dry_run:          None,
                    auto_approve:     None,
                    model:            None,
                    provider:         None,
                    environment:      None,
                    preserve_sandbox: None,
                    start:            None,
                })
            }
            CreateRunSpecInput::Spec(spec) => Self::try_from(*spec),
        }
    }
}

impl TryFrom<CreateRunSpec> for ValidatedCreateRunSpec {
    type Error = ToolError;

    fn try_from(spec: CreateRunSpec) -> Result<Self, Self::Error> {
        let run_id = spec
            .run_id
            .as_deref()
            .map(str::parse::<RunId>)
            .transpose()
            .map_err(|err| {
                ToolError::message(format!("run_id must be a valid Fabro run id: {err}"))
            })?;
        let parent_id = spec
            .parent_id
            .as_deref()
            .map(str::trim)
            .filter(|parent_id| !parent_id.is_empty())
            .map(ToOwned::to_owned);
        if spec.parent_id.is_some() && parent_id.is_none() {
            return Err(ToolError::message("parent_id must not be blank"));
        }
        if spec.goal.is_some() && spec.goal_file.is_some() {
            return Err(ToolError::message(
                "goal and goal_file are mutually exclusive; use exactly one",
            ));
        }
        if spec
            .goal_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ToolError::message("goal_file must not be blank"));
        }
        let inputs = spec
            .inputs
            .into_iter()
            .map(|(key, value)| {
                let value = value.into_inner();
                manifest::json_to_toml_value(&key, &value).map(|value| (key, value))
            })
            .collect::<ToolResult<HashMap<_, _>>>()?;
        Ok(Self {
            workflow: spec.workflow,
            cwd: spec.cwd,
            run_id,
            parent_id,
            goal: spec.goal,
            goal_file: spec.goal_file,
            inputs,
            labels: spec.labels,
            dry_run: spec.dry_run,
            auto_approve: spec.auto_approve,
            model: spec.model,
            provider: spec.provider,
            environment: spec.environment,
            preserve_sandbox: spec.preserve_sandbox,
            start: spec.start,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreateRunsResult {
    pub runs: Vec<CreatedRunResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreatedRunResult {
    pub run_id:          String,
    pub parent_id:       Option<String>,
    pub children_count:  u64,
    pub workflow:        String,
    pub start_requested: bool,
    pub status:          String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CreateRunOptions {
    pub forced_parent_id: Option<RunId>,
}

pub async fn create_runs(
    backend: Arc<dyn FabroToolBackend>,
    base_cwd: &Path,
    user_settings_path: &Path,
    params: ValidatedCreateRuns,
) -> ToolResult<CreateRunsResult> {
    create_runs_with_options(
        backend,
        base_cwd,
        user_settings_path,
        params,
        CreateRunOptions::default(),
    )
    .await
}

pub async fn create_runs_with_options(
    backend: Arc<dyn FabroToolBackend>,
    base_cwd: &Path,
    user_settings_path: &Path,
    params: ValidatedCreateRuns,
    options: CreateRunOptions,
) -> ToolResult<CreateRunsResult> {
    let mut created = Vec::with_capacity(params.runs.len());
    let mut parent_id_cache = HashMap::<String, RunId>::new();
    for spec in params.runs {
        let cwd = spec.cwd.clone().unwrap_or_else(|| base_cwd.to_path_buf());
        let parent_id = if let Some(forced_parent_id) = options.forced_parent_id {
            Some(forced_parent_id)
        } else if let Some(parent_selector) = spec.parent_id.as_deref() {
            Some(
                resolve_parent_run_id(backend.as_ref(), &mut parent_id_cache, parent_selector)
                    .await?,
            )
        } else {
            None
        };
        let run_id = backend
            .create_run_from_spec(&spec, &cwd, user_settings_path, parent_id)
            .await
            .map_err(|err| ToolError::from_anyhow(&err))?;
        let start_requested = spec.start.unwrap_or(true);
        let summary = if start_requested {
            backend
                .start_run(&run_id, false)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        } else {
            backend
                .retrieve_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        };
        created.push(CreatedRunResult {
            run_id: summary.id.to_string(),
            parent_id: summary.parent_id.map(|parent_id| parent_id.to_string()),
            children_count: summary.children_count,
            workflow: spec.workflow,
            start_requested,
            status: summary.lifecycle.status.kind().to_string(),
        });
    }
    Ok(CreateRunsResult { runs: created })
}

async fn resolve_parent_run_id(
    backend: &dyn FabroToolBackend,
    parent_id_cache: &mut HashMap<String, RunId>,
    parent_selector: &str,
) -> ToolResult<RunId> {
    if let Ok(parent_id) = parent_selector.parse::<RunId>() {
        return Ok(parent_id);
    }
    if let Some(parent_id) = parent_id_cache.get(parent_selector) {
        return Ok(*parent_id);
    }

    let parent_id = backend
        .resolve_run(parent_selector)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;
    parent_id_cache.insert(parent_selector.to_string(), parent_id);
    Ok(parent_id)
}

pub fn create_runs_text(result: &CreateRunsResult) -> String {
    let start_requested = result.runs.iter().filter(|run| run.start_requested).count();
    format!(
        "created {} Fabro run(s), start requested for {start_requested}",
        result.runs.len()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use fabro_api::types;
    use fabro_types::{
        EventEnvelope, Run, RunLifecycle, RunLinks, RunOrigin, RunProjection, RunStatus,
        RunTimestamps, WorkflowRef, test_support,
    };
    use schemars::SchemaGenerator;
    use serde_json::json;

    use super::*;

    #[test]
    fn run_input_value_schema_allows_only_json_scalars() {
        let mut generator = SchemaGenerator::default();
        let schema = RunInputValue::json_schema(&mut generator);
        let schema = serde_json::to_value(schema).expect("schema should serialize");

        assert_eq!(
            schema["anyOf"],
            json!([
                { "type": "string" },
                { "type": "boolean" },
                { "type": "integer" },
                { "type": "number" },
            ])
        );
    }

    #[test]
    fn create_spec_accepts_parent_selector() {
        let spec = ValidatedCreateRunSpec::try_from(CreateRunSpec {
            workflow:         "simple.fabro".to_string(),
            cwd:              None,
            run_id:           None,
            parent_id:        Some(" nightly-parent ".to_string()),
            goal:             None,
            goal_file:        None,
            inputs:           HashMap::new(),
            labels:           HashMap::new(),
            dry_run:          None,
            auto_approve:     None,
            model:            None,
            provider:         None,
            environment:      None,
            preserve_sandbox: None,
            start:            None,
        })
        .expect("parent selectors should validate without requiring exact run ids");

        assert_eq!(spec.parent_id.as_deref(), Some("nightly-parent"));
    }

    #[test]
    fn create_params_accept_string_shorthand() {
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": ["simple.fabro"]
        }))
        .expect("string shorthand should deserialize");

        let params = ValidatedCreateRuns::try_from(params)
            .expect("string shorthand should validate as workflow selector");
        let spec = &params.runs[0];
        assert_eq!(spec.workflow, "simple.fabro");
        assert_eq!(spec.cwd, None);
        assert_eq!(spec.run_id, None);
        assert_eq!(spec.parent_id, None);
        assert!(spec.inputs.is_empty());
        assert!(spec.labels.is_empty());
        assert_eq!(spec.start, None);
    }

    #[test]
    fn create_params_preserve_object_form_options() {
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": "simple.fabro",
                "dry_run": true,
                "auto_approve": true,
                "labels": { "source": "mcp-test" },
                "start": false
            }]
        }))
        .expect("object form should deserialize");

        let params =
            ValidatedCreateRuns::try_from(params).expect("object form should still validate");
        let spec = &params.runs[0];
        assert_eq!(spec.workflow, "simple.fabro");
        assert_eq!(spec.dry_run, Some(true));
        assert_eq!(spec.auto_approve, Some(true));
        assert_eq!(
            spec.labels.get("source").map(String::as_str),
            Some("mcp-test")
        );
        assert_eq!(spec.start, Some(false));
    }

    #[test]
    fn create_params_preserve_goal_file_option() {
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": "implement-plan",
                "goal_file": "plans/ship-it.md",
                "start": false
            }]
        }))
        .expect("object form with goal_file should deserialize");

        let params = ValidatedCreateRuns::try_from(params).expect("goal_file should validate");
        let spec = &params.runs[0];
        assert_eq!(spec.goal, None);
        assert_eq!(
            spec.goal_file.as_deref(),
            Some(Path::new("plans/ship-it.md"))
        );
    }

    #[test]
    fn create_params_reject_goal_and_goal_file_together() {
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": "implement-plan",
                "goal": "inline goal",
                "goal_file": "plans/ship-it.md"
            }]
        }))
        .expect("object form with both goal forms should deserialize before validation");

        let err = ValidatedCreateRuns::try_from(params)
            .expect_err("goal and goal_file should be mutually exclusive");
        assert!(
            err.to_string()
                .contains("goal and goal_file are mutually exclusive"),
            "{err}"
        );
    }

    #[test]
    fn create_params_reject_blank_string_shorthand_workflow() {
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": ["  "]
        }))
        .expect("blank shorthand should deserialize before validation");

        let err = ValidatedCreateRuns::try_from(params).expect_err("blank workflow should fail");
        assert!(err.to_string().contains("workflow"), "{err}");
    }

    #[test]
    fn create_params_missing_object_workflow_keeps_field_error() {
        let err = serde_json::from_value::<FabroRunCreateParams>(json!({
            "runs": [{ "dry_run": true }]
        }))
        .expect_err("object form without workflow should fail deserialization");

        assert!(
            err.to_string().contains("missing field `workflow`"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn create_runs_resolves_parent_selector_and_sends_parent_id_to_backend() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let settings = temp.path().join("settings.toml");
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         "simple.fabro".to_string(),
                    cwd:              None,
                    run_id:           None,
                    parent_id:        Some("nightly-parent".to_string()),
                    goal:             None,
                    goal_file:        None,
                    inputs:           HashMap::new(),
                    labels:           HashMap::new(),
                    dry_run:          Some(true),
                    auto_approve:     Some(true),
                    model:            None,
                    provider:         None,
                    environment:      None,
                    preserve_sandbox: None,
                    start:            Some(false),
                }
                .into(),
            ],
        })
        .expect("create params should validate");

        let result = create_runs(backend.clone(), temp.path(), &settings, params)
            .await
            .expect("run should be created");

        assert_eq!(result.runs[0].parent_id, Some(parent_id.to_string()));
        assert_eq!(result.runs[0].children_count, 0);
        assert_eq!(backend.created_parent_ids.lock().unwrap().as_slice(), &[
            Some(parent_id)
        ]);
        assert_eq!(backend.resolved_selectors.lock().unwrap().as_slice(), &[
            "nightly-parent".to_string()
        ]);
    }

    #[tokio::test]
    async fn create_runs_reuses_parent_selector_resolution_within_batch() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let settings = temp.path().join("settings.toml");
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
        });
        let runs: Vec<CreateRunSpecInput> = (0..2)
            .map(|_| {
                CreateRunSpecInput::from(CreateRunSpec {
                    workflow:         "simple.fabro".to_string(),
                    cwd:              None,
                    run_id:           None,
                    parent_id:        Some("nightly-parent".to_string()),
                    goal:             None,
                    goal_file:        None,
                    inputs:           HashMap::new(),
                    labels:           HashMap::new(),
                    dry_run:          Some(true),
                    auto_approve:     Some(true),
                    model:            None,
                    provider:         None,
                    environment:      None,
                    preserve_sandbox: None,
                    start:            Some(false),
                })
            })
            .collect();
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams { runs })
            .expect("create params should validate");

        create_runs(backend.clone(), temp.path(), &settings, params)
            .await
            .expect("runs should be created");

        assert_eq!(backend.created_parent_ids.lock().unwrap().as_slice(), &[
            Some(parent_id),
            Some(parent_id),
        ]);
        assert_eq!(backend.resolved_selectors.lock().unwrap().as_slice(), &[
            "nightly-parent".to_string()
        ]);
    }

    #[tokio::test]
    async fn create_runs_forced_parent_id_skips_selector_resolution() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let settings = temp.path().join("settings.toml");
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         "simple.fabro".to_string(),
                    cwd:              None,
                    run_id:           None,
                    parent_id:        Some(parent_id.to_string()),
                    goal:             None,
                    goal_file:        None,
                    inputs:           HashMap::new(),
                    labels:           HashMap::new(),
                    dry_run:          Some(true),
                    auto_approve:     Some(true),
                    model:            None,
                    provider:         None,
                    environment:      None,
                    preserve_sandbox: None,
                    start:            Some(false),
                }
                .into(),
            ],
        })
        .expect("create params should validate");

        create_runs_with_options(
            backend.clone(),
            temp.path(),
            &settings,
            params,
            CreateRunOptions {
                forced_parent_id: Some(parent_id),
            },
        )
        .await
        .expect("run should be created");

        assert_eq!(backend.created_parent_ids.lock().unwrap().as_slice(), &[
            Some(parent_id)
        ]);
        assert!(backend.resolved_selectors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_runs_defaults_to_start_request_and_reports_pending_child_status() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let settings = temp.path().join("settings.toml");
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         "simple.fabro".to_string(),
                    cwd:              None,
                    run_id:           None,
                    parent_id:        Some(parent_id.to_string()),
                    goal:             None,
                    goal_file:        None,
                    inputs:           HashMap::new(),
                    labels:           HashMap::new(),
                    dry_run:          Some(true),
                    auto_approve:     Some(true),
                    model:            None,
                    provider:         None,
                    environment:      None,
                    preserve_sandbox: None,
                    start:            None,
                }
                .into(),
            ],
        })
        .expect("create params should validate");

        let result = create_runs(backend.clone(), temp.path(), &settings, params)
            .await
            .expect("run should be created and start requested");

        assert!(result.runs[0].start_requested);
        assert_eq!(result.runs[0].status, "pending");
        assert_eq!(backend.started_run_ids.lock().unwrap().as_slice(), &[
            child_id
        ]);
        assert_eq!(
            create_runs_text(&result),
            "created 1 Fabro run(s), start requested for 1"
        );
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }

    fn run(run_id: RunId, parent_id: Option<RunId>, children_count: u64) -> Run {
        run_with_status(run_id, parent_id, children_count, RunStatus::Submitted)
    }

    fn run_with_status(
        run_id: RunId,
        parent_id: Option<RunId>,
        children_count: u64,
        status: RunStatus,
    ) -> Run {
        Run {
            id: run_id,
            parent_id,
            children_count,
            title: "Test run".to_string(),
            goal: "Test run".to_string(),
            workflow: WorkflowRef {
                slug:       Some("simple".to_string()),
                name:       Some("Simple".to_string()),
                graph_name: None,
                node_count: 0,
                edge_count: 0,
            },
            automation: None,
            repository: None,
            created_by: test_support::test_principal(),
            origin: RunOrigin::default(),
            labels: HashMap::new(),
            lifecycle: RunLifecycle {
                status,
                approval: None,
                pending_control: None,
                queue_position: None,
                error: None,
                archived: false,
                archived_at: None,
            },
            sandbox: None,
            models: Vec::new(),
            source_directory: Some("/srv/repo".to_string()),
            timestamps: RunTimestamps {
                created_at:    Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap(),
                started_at:    None,
                last_event_at: None,
                completed_at:  None,
            },
            timing: None,
            billing: None,
            size: fabro_types::RunSize::default(),
            ask_fabro: fabro_types::AskFabro::default(),
            diff: None,
            pull_request: None,
            current_question: None,
            superseded_by: None,
            retried_from: None,
            links: RunLinks { web: None },
        }
    }

    struct MockCreateBackend {
        child_id:           RunId,
        parent_id:          RunId,
        created_parent_ids: Mutex<Vec<Option<RunId>>>,
        resolved_selectors: Mutex<Vec<String>>,
        started_run_ids:    Mutex<Vec<RunId>>,
    }

    #[async_trait]
    impl FabroToolBackend for MockCreateBackend {
        async fn create_run_from_spec(
            &self,
            _spec: &ValidatedCreateRunSpec,
            _cwd: &Path,
            _user_settings_path: &Path,
            parent_id: Option<RunId>,
        ) -> anyhow::Result<RunId> {
            self.created_parent_ids.lock().unwrap().push(parent_id);
            Ok(self.child_id)
        }

        async fn resolve_run(&self, selector: &str) -> anyhow::Result<Run> {
            assert_eq!(selector, "nightly-parent");
            self.resolved_selectors
                .lock()
                .unwrap()
                .push(selector.to_string());
            Ok(run(self.parent_id, None, 1))
        }

        async fn retrieve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
            assert_eq!(*run_id, self.child_id);
            Ok(run(self.child_id, Some(self.parent_id), 0))
        }

        async fn start_run(&self, run_id: &RunId, resume: bool) -> anyhow::Result<Run> {
            assert_eq!(*run_id, self.child_id);
            assert!(!resume);
            self.started_run_ids.lock().unwrap().push(*run_id);
            Ok(run_with_status(
                self.child_id,
                Some(self.parent_id),
                0,
                RunStatus::Pending {
                    reason: fabro_types::PendingReason::ApprovalRequired,
                },
            ))
        }

        async fn approve_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn deny_run(&self, _run_id: &RunId, _reason: Option<String>) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn cancel_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn interrupt_run(&self, _run_id: &RunId) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn steer_run(
            &self,
            _run_id: &RunId,
            _text: String,
            _interrupt: bool,
        ) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn archive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unarchive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn list_store_runs(&self) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn list_store_runs_by_parent(&self, _parent_id: RunId) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn link_run_parent(
            &self,
            _child_id: &RunId,
            _parent_id: &RunId,
        ) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unlink_run_parent(&self, _child_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn get_run_state(&self, _run_id: &RunId) -> anyhow::Result<RunProjection> {
            unreachable!()
        }

        async fn list_run_events(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: Option<usize>,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            unreachable!()
        }

        async fn list_run_events_until(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: usize,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            unreachable!()
        }

        async fn list_run_questions(
            &self,
            _run_id: &RunId,
        ) -> anyhow::Result<Vec<types::ApiQuestion>> {
            unreachable!()
        }

        async fn submit_run_answer(
            &self,
            _run_id: &RunId,
            _question_id: &str,
            _body: types::SubmitAnswerRequest,
        ) -> anyhow::Result<()> {
            unreachable!()
        }
    }
}
