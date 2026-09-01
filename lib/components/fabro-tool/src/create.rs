use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_types::{RunId, RunTarget, WorkflowPath, WorkflowVersionId};
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
                    "additionalProperties": false,
                    "properties": {
                        "workflow": {
                            "description": "Workflow content source. Selector strings require a proven shared filesystem; inline files and exact stored IDs are portable.",
                            "anyOf": [
                                {
                                    "type": "string",
                                    "description": "Workflow selector, such as a workflow name or workflow file path."
                                },
                                {
                                    "type": "object",
                                    "required": ["kind", "entrypoint", "files"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "inline" },
                                        "entrypoint": { "type": "string" },
                                        "files": {
                                            "type": "object",
                                            "additionalProperties": { "type": "string" }
                                        }
                                    }
                                },
                                {
                                    "type": "object",
                                    "required": ["kind", "workflow_version_id"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "stored" },
                                        "workflow_version_id": { "type": "string" }
                                    }
                                }
                            ]
                        },
                        "cwd": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Working directory used to resolve relative workflow paths."
                        },
                        "parent_id": {
                            "anyOf": [
                                { "type": "string" },
                                { "type": "null" }
                            ],
                            "description": "Optional parent run id or selector."
                        },
                        "target": {
                            "description": "Canonical run workspace target. Worker calls inherit the parent target when omitted; standalone calls require an observable Git checkout when omitted.",
                            "anyOf": [
                                { "type": "null" },
                                {
                                    "type": "object",
                                    "required": ["kind", "repo", "branch"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "git" },
                                        "repo": { "type": "string" },
                                        "branch": { "type": "string" },
                                        "tag": {
                                            "anyOf": [
                                                { "type": "string" },
                                                { "type": "null" }
                                            ]
                                        },
                                        "sha": {
                                            "anyOf": [
                                                { "type": "string" },
                                                { "type": "null" }
                                            ]
                                        }
                                    }
                                },
                                {
                                    "type": "object",
                                    "required": ["kind"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "none" }
                                    }
                                },
                                {
                                    "type": "object",
                                    "required": ["kind", "path"],
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "folder" },
                                        "path": { "type": "string" }
                                    }
                                }
                            ]
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

#[derive(Debug, Clone)]
pub enum CreateRunWorkflowSource {
    Selector(String),
    Inline(InlineWorkflowSource),
    Stored {
        workflow_version_id: WorkflowVersionId,
    },
}

impl CreateRunWorkflowSource {
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Selector(selector) => selector.clone(),
            Self::Inline(source) => source.entrypoint.to_string(),
            Self::Stored {
                workflow_version_id,
            } => workflow_version_id.to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for CreateRunWorkflowSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum TaggedSource {
            Inline {
                entrypoint: WorkflowPath,
                files:      BTreeMap<WorkflowPath, String>,
            },
            Stored {
                workflow_version_id: WorkflowVersionId,
            },
        }

        match Value::deserialize(deserializer)? {
            Value::String(selector) => Ok(Self::Selector(selector)),
            value @ Value::Object(_) => {
                match serde_json::from_value::<TaggedSource>(value).map_err(de::Error::custom)? {
                    TaggedSource::Inline { entrypoint, files } => {
                        Ok(Self::Inline(InlineWorkflowSource { entrypoint, files }))
                    }
                    TaggedSource::Stored {
                        workflow_version_id,
                    } => Ok(Self::Stored {
                        workflow_version_id,
                    }),
                }
            }
            other => Err(de::Error::custom(format!(
                "expected workflow selector string or tagged source object, got {}",
                json_value_kind(&other)
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InlineWorkflowSource {
    pub entrypoint: WorkflowPath,
    pub files:      BTreeMap<WorkflowPath, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunSpec {
    pub workflow:         CreateRunWorkflowSource,
    pub cwd:              Option<PathBuf>,
    pub parent_id:        Option<String>,
    pub target:           Option<RunTarget>,
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
    pub workflow:         CreateRunWorkflowSource,
    pub cwd:              Option<PathBuf>,
    pub parent_id:        Option<String>,
    pub target:           Option<RunTarget>,
    pub goal:             Option<String>,
    pub goal_file:        Option<PathBuf>,
    pub inputs:           HashMap<String, ValidatedRunInputValue>,
    pub labels:           HashMap<String, String>,
    pub dry_run:          Option<bool>,
    pub auto_approve:     Option<bool>,
    pub model:            Option<String>,
    pub provider:         Option<String>,
    pub environment:      Option<String>,
    pub preserve_sandbox: Option<bool>,
    pub start:            Option<bool>,
}

#[derive(Debug)]
pub struct ValidatedRunInputValue {
    json: Value,
    toml: toml::Value,
}

impl ValidatedRunInputValue {
    #[must_use]
    pub fn json(&self) -> &Value {
        &self.json
    }

    #[must_use]
    pub fn toml(&self) -> &toml::Value {
        &self.toml
    }
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
            CreateRunSpecInput::Workflow(workflow) => Self::try_from(CreateRunSpec {
                workflow:         CreateRunWorkflowSource::Selector(workflow),
                cwd:              None,
                parent_id:        None,
                target:           None,
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
            }),
            CreateRunSpecInput::Spec(spec) => Self::try_from(*spec),
        }
    }
}

impl TryFrom<CreateRunSpec> for ValidatedCreateRunSpec {
    type Error = ToolError;

    fn try_from(spec: CreateRunSpec) -> Result<Self, Self::Error> {
        let workflow = validate_workflow_source(spec.workflow)?;
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
                let json = value.into_inner();
                manifest::json_to_toml_value(&key, &json)
                    .map(|toml| (key, ValidatedRunInputValue { json, toml }))
            })
            .collect::<ToolResult<HashMap<_, _>>>()?;
        let target = spec
            .target
            .map(|target| {
                target
                    .validate()
                    .map(|validated| validated.target)
                    .map_err(|err| ToolError::message(format!("invalid run target: {err}")))
            })
            .transpose()?;
        Ok(Self {
            workflow,
            cwd: spec.cwd,
            parent_id,
            target,
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

fn validate_workflow_source(
    source: CreateRunWorkflowSource,
) -> ToolResult<CreateRunWorkflowSource> {
    match source {
        CreateRunWorkflowSource::Selector(selector) => {
            let selector = selector.trim();
            if selector.is_empty() {
                return Err(ToolError::message("workflow selector must not be blank"));
            }
            Ok(CreateRunWorkflowSource::Selector(selector.to_string()))
        }
        CreateRunWorkflowSource::Inline(source) => {
            if source.files.len() > fabro_types::MAX_WORKFLOW_VERSION_FILES {
                return Err(ToolError::message(format!(
                    "inline workflow contains more than {} files",
                    fabro_types::MAX_WORKFLOW_VERSION_FILES
                )));
            }
            if !source.files.contains_key(&source.entrypoint) {
                return Err(ToolError::message(format!(
                    "inline workflow entrypoint `{}` is missing from files",
                    source.entrypoint
                )));
            }
            let mut total_bytes = 0usize;
            for (path, content) in &source.files {
                let bytes = content.len();
                if bytes > fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES {
                    return Err(ToolError::message(format!(
                        "inline workflow file `{path}` exceeds {} KiB",
                        fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES / 1024
                    )));
                }
                total_bytes = total_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| ToolError::message("inline workflow content size overflowed"))?;
                if total_bytes > fabro_types::MAX_WORKFLOW_VERSION_BYTES {
                    return Err(ToolError::message(format!(
                        "inline workflow content exceeds {} MiB in aggregate",
                        fabro_types::MAX_WORKFLOW_VERSION_BYTES / (1024 * 1024)
                    )));
                }
            }
            Ok(CreateRunWorkflowSource::Inline(source))
        }
        stored @ CreateRunWorkflowSource::Stored { .. } => Ok(stored),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings:        Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CreateRunOptions {
    pub forced_parent_id: Option<RunId>,
}

pub async fn create_runs(
    backend: Arc<dyn FabroToolBackend>,
    base_cwd: &Path,
    params: ValidatedCreateRuns,
) -> ToolResult<CreateRunsResult> {
    create_runs_with_options(backend, base_cwd, params, CreateRunOptions::default()).await
}

pub async fn create_runs_with_options(
    backend: Arc<dyn FabroToolBackend>,
    base_cwd: &Path,
    params: ValidatedCreateRuns,
    options: CreateRunOptions,
) -> ToolResult<CreateRunsResult> {
    let mut created = Vec::with_capacity(params.runs.len());
    let mut parent_id_cache = HashMap::<String, RunId>::new();
    for spec in params.runs {
        let workflow = spec.workflow.display();
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
        let submission = backend
            .create_run_from_spec(&spec, &cwd, parent_id)
            .await
            .map_err(|err| ToolError::from_anyhow(&err))?;
        let run_id = submission.run_id;
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
            workflow,
            start_requested,
            status: summary.lifecycle.status.kind().to_string(),
            warnings: submission.warnings,
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
    let mut text = format!(
        "created {} Fabro run(s), start requested for {start_requested}",
        result.runs.len()
    );
    for warning in result.runs.iter().flat_map(|run| &run.warnings) {
        text.push_str("\nwarning: ");
        text.push_str(warning);
    }
    text
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
    fn create_spec_schema_advertises_the_accepted_workflow_and_target_grammar() {
        let mut generator = SchemaGenerator::default();
        let schema = CreateRunSpecInput::json_schema(&mut generator);
        let schema = serde_json::to_value(schema).expect("schema should serialize");
        let properties = schema["anyOf"][1]["properties"]
            .as_object()
            .expect("object form should have properties");

        assert!(!properties.contains_key("run_id"));
        assert_eq!(schema["anyOf"][1]["additionalProperties"], false);
        let workflow_variants = properties["workflow"]["anyOf"]
            .as_array()
            .expect("workflow should advertise all source variants");
        assert!(
            workflow_variants
                .iter()
                .any(|variant| variant["type"] == "string")
        );
        for kind in ["inline", "stored"] {
            assert!(workflow_variants.iter().any(|variant| {
                variant.pointer("/properties/kind/const") == Some(&json!(kind))
            }));
        }
        let target_variants = properties["target"]["anyOf"]
            .as_array()
            .expect("target should advertise the canonical target variants");
        for kind in ["git", "none", "folder"] {
            assert!(target_variants.iter().any(|variant| {
                variant.pointer("/properties/kind/const") == Some(&json!(kind))
            }));
        }
    }

    #[test]
    fn create_spec_schema_stays_in_parity_with_run_target_serde() {
        let mut generator = SchemaGenerator::default();
        let schema = CreateRunSpecInput::json_schema(&mut generator);
        let schema = serde_json::to_value(schema).expect("schema should serialize");
        let validator = jsonschema::validator_for(&schema).expect("advertised schema must compile");

        // Every serde-produced target shape must satisfy the hand-written
        // schema literal; a field added to a target variant without updating
        // the literal fails here because the schema denies unknown fields.
        let targets = [
            RunTarget::Git(fabro_types::GitRunTarget {
                repo:   "fabro-sh/fabro".to_string(),
                branch: "main".to_string(),
                tag:    Some("v1.0.0".to_string()),
                sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            }),
            RunTarget::None {},
            RunTarget::Folder {
                path: "/srv/workspace".to_string(),
            },
        ];
        for target in targets {
            let target = serde_json::to_value(&target).expect("target should serialize");
            let spec = json!({ "workflow": "demo", "target": target });
            assert!(
                validator.is_valid(&spec),
                "advertised schema rejects serde-produced target {target}"
            );
        }

        // The schema must actually enforce the field lists, so the parity
        // assertions above have teeth.
        let unknown_field = json!({
            "workflow": "demo",
            "target": {
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "main",
                "unknown_field": true
            }
        });
        assert!(!validator.is_valid(&unknown_field));
    }

    #[test]
    fn create_spec_accepts_parent_selector() {
        let spec = ValidatedCreateRunSpec::try_from(CreateRunSpec {
            workflow:         selector("simple.fabro"),
            cwd:              None,
            parent_id:        Some(" nightly-parent ".to_string()),
            target:           None,
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
        assert_eq!(spec.workflow.display(), "simple.fabro");
        assert_eq!(spec.cwd, None);
        assert_eq!(spec.parent_id, None);
        assert!(spec.inputs.is_empty());
        assert!(spec.labels.is_empty());
        assert_eq!(spec.start, None);
    }

    #[test]
    fn create_params_accept_inline_and_stored_workflow_sources() {
        let stored_id = fabro_types::BlobHash::new(b"stored workflow").to_string();
        let inline: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "inline",
                    "entrypoint": "flows/main.fabro",
                    "files": {
                        "flows/main.fabro": "digraph Main {}",
                        "prompts/goal.md": "Ship it"
                    }
                },
                "target": { "kind": "none" },
                "start": false
            }]
        }))
        .expect("inline workflow source should deserialize");
        let inline =
            ValidatedCreateRuns::try_from(inline).expect("inline workflow source should validate");
        assert_eq!(inline.runs[0].workflow.display(), "flows/main.fabro");

        let stored: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "stored",
                    "workflow_version_id": stored_id
                },
                "target": { "kind": "git", "repo": "fabro-sh/fabro", "branch": "main" }
            }]
        }))
        .expect("stored workflow source should deserialize");
        let stored =
            ValidatedCreateRuns::try_from(stored).expect("stored workflow source should validate");
        assert_eq!(stored.runs[0].workflow.display(), stored_id);
    }

    #[test]
    fn create_params_reject_invalid_workflow_sources_and_inputs_before_backend_work() {
        for workflow in [
            json!({
                "kind": "inline",
                "entrypoint": "../escape.fabro",
                "files": { "../escape.fabro": "digraph W {}" }
            }),
            json!({
                "kind": "stored",
                "workflow_version_id": "not-an-id"
            }),
            json!({
                "kind": "stored",
                "workflow_version_id": fabro_types::BlobHash::new(b"stored").to_string(),
                "extra": true
            }),
        ] {
            serde_json::from_value::<FabroRunCreateParams>(json!({
                "runs": [{ "workflow": workflow }]
            }))
            .expect_err("invalid workflow source should fail deserialization");
        }

        let missing_entrypoint: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "inline",
                    "entrypoint": "main.fabro",
                    "files": { "other.fabro": "digraph Other {}" }
                }
            }]
        }))
        .unwrap();
        assert!(
            ValidatedCreateRuns::try_from(missing_entrypoint)
                .unwrap_err()
                .to_string()
                .contains("entrypoint")
        );

        let too_many = (0..=fabro_types::MAX_WORKFLOW_VERSION_FILES)
            .map(|index| (format!("files/{index}.md"), json!("x")))
            .collect::<serde_json::Map<_, _>>();
        let too_many: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "inline",
                    "entrypoint": "files/0.md",
                    "files": too_many
                }
            }]
        }))
        .unwrap();
        assert!(ValidatedCreateRuns::try_from(too_many).is_err());

        let oversized: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "inline",
                    "entrypoint": "main.fabro",
                    "files": { "main.fabro": "x".repeat(fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES + 1) }
                }
            }]
        }))
        .unwrap();
        assert!(ValidatedCreateRuns::try_from(oversized).is_err());

        let aggregate: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": {
                    "kind": "inline",
                    "entrypoint": "0.fabro",
                    "files": {
                        "0.fabro": "x".repeat(fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES),
                        "1.md": "x".repeat(fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES),
                        "2.md": "x".repeat(fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES),
                        "3.md": "x".repeat(fabro_types::MAX_WORKFLOW_VERSION_FILE_BYTES),
                        "4.md": "x"
                    }
                }
            }]
        }))
        .unwrap();
        assert!(ValidatedCreateRuns::try_from(aggregate).is_err());

        let invalid_target: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": "main.fabro",
                "target": { "kind": "git", "repo": "not-a-slug", "branch": "HEAD" }
            }]
        }))
        .unwrap();
        assert!(ValidatedCreateRuns::try_from(invalid_target).is_err());

        let nonscalar: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": [{
                "workflow": "main.fabro",
                "inputs": { "nested": { "no": "objects" } }
            }]
        }))
        .unwrap();
        assert!(ValidatedCreateRuns::try_from(nonscalar).is_err());
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
        assert_eq!(spec.workflow.display(), "simple.fabro");
        assert_eq!(spec.dry_run, Some(true));
        assert_eq!(spec.auto_approve, Some(true));
        assert_eq!(
            spec.labels.get("source").map(String::as_str),
            Some("mcp-test")
        );
        assert_eq!(spec.start, Some(false));
    }

    #[test]
    fn create_params_reject_unknown_object_fields() {
        let err = serde_json::from_value::<FabroRunCreateParams>(json!({
            "runs": [{
                "workflow": "simple.fabro",
                "run_id": "not-a-valid-run-id",
                "start": false
            }]
        }))
        .expect_err("unknown create fields should be rejected");

        assert!(err.to_string().contains("unknown field `run_id`"), "{err}");
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
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
            warnings: Vec::new(),
            create_error: false,
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         selector("simple.fabro"),
                    cwd:              None,
                    parent_id:        Some("nightly-parent".to_string()),
                    target:           None,
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

        let result = create_runs(backend.clone(), temp.path(), params)
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
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
            warnings: Vec::new(),
            create_error: false,
        });
        let runs: Vec<CreateRunSpecInput> = (0..2)
            .map(|_| {
                CreateRunSpecInput::from(CreateRunSpec {
                    workflow:         selector("simple.fabro"),
                    cwd:              None,
                    parent_id:        Some("nightly-parent".to_string()),
                    target:           None,
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

        create_runs(backend.clone(), temp.path(), params)
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
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
            warnings: Vec::new(),
            create_error: false,
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         selector("simple.fabro"),
                    cwd:              None,
                    parent_id:        Some(parent_id.to_string()),
                    target:           None,
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

        create_runs_with_options(backend.clone(), temp.path(), params, CreateRunOptions {
            forced_parent_id: Some(parent_id),
        })
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
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
            warnings: vec!["uncommitted changes are excluded".to_string()],
            create_error: false,
        });
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![
                CreateRunSpec {
                    workflow:         selector("simple.fabro"),
                    cwd:              None,
                    parent_id:        Some(parent_id.to_string()),
                    target:           None,
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

        let result = create_runs(backend.clone(), temp.path(), params)
            .await
            .expect("run should be created and start requested");

        assert!(result.runs[0].start_requested);
        assert_eq!(result.runs[0].status, "pending");
        assert_eq!(backend.started_run_ids.lock().unwrap().as_slice(), &[
            child_id
        ]);
        assert_eq!(
            create_runs_text(&result),
            "created 1 Fabro run(s), start requested for 1\nwarning: uncommitted changes are excluded"
        );
        assert_eq!(result.runs[0].warnings, [
            "uncommitted changes are excluded"
        ]);
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(
            wire["runs"][0]["warnings"],
            json!(["uncommitted changes are excluded"])
        );
    }

    #[tokio::test]
    async fn create_runs_create_failure_does_not_request_start() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let backend = Arc::new(MockCreateBackend {
            child_id,
            parent_id,
            created_parent_ids: Mutex::new(Vec::new()),
            resolved_selectors: Mutex::new(Vec::new()),
            started_run_ids: Mutex::new(Vec::new()),
            warnings: Vec::new(),
            create_error: true,
        });
        let params: FabroRunCreateParams = serde_json::from_value(json!({
            "runs": ["simple.fabro"]
        }))
        .unwrap();

        create_runs(
            backend.clone(),
            temp.path(),
            ValidatedCreateRuns::try_from(params).unwrap(),
        )
        .await
        .expect_err("create failure should be returned");

        assert!(backend.started_run_ids.lock().unwrap().is_empty());
    }

    #[test]
    fn create_run_result_omits_empty_warnings() {
        let result = CreateRunsResult {
            runs: vec![CreatedRunResult {
                run_id:          RunId::new().to_string(),
                parent_id:       None,
                children_count:  0,
                workflow:        "simple".to_string(),
                start_requested: false,
                status:          "submitted".to_string(),
                warnings:        Vec::new(),
            }],
        };

        assert!(
            serde_json::to_value(result).unwrap()["runs"][0]
                .get("warnings")
                .is_none()
        );
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }

    fn selector(value: &str) -> CreateRunWorkflowSource {
        CreateRunWorkflowSource::Selector(value.to_string())
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
        warnings:           Vec<String>,
        create_error:       bool,
    }

    #[async_trait]
    impl FabroToolBackend for MockCreateBackend {
        async fn create_run_from_spec(
            &self,
            _spec: &ValidatedCreateRunSpec,
            _cwd: &Path,
            parent_id: Option<RunId>,
        ) -> anyhow::Result<crate::CreateRunSubmission> {
            self.created_parent_ids.lock().unwrap().push(parent_id);
            if self.create_error {
                anyhow::bail!("create failed");
            }
            Ok(crate::CreateRunSubmission {
                run_id:   self.child_id,
                warnings: self.warnings.clone(),
            })
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
