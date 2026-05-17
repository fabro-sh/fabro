use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_client::Client;
use fabro_types::RunId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{ToolError, ToolResult};
use super::{common, manifest};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct FabroRunCreateParams {
    pub(crate) runs: Vec<CreateRunSpec>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CreateRunSpec {
    pub(crate) workflow:         String,
    pub(crate) cwd:              Option<PathBuf>,
    pub(crate) run_id:           Option<String>,
    pub(crate) parent_id:        Option<String>,
    pub(crate) goal:             Option<String>,
    #[serde(default)]
    pub(crate) inputs:           HashMap<String, RunInputValue>,
    #[serde(default)]
    pub(crate) labels:           HashMap<String, String>,
    pub(crate) dry_run:          Option<bool>,
    pub(crate) auto_approve:     Option<bool>,
    pub(crate) model:            Option<String>,
    pub(crate) provider:         Option<String>,
    pub(crate) sandbox:          Option<String>,
    pub(crate) preserve_sandbox: Option<bool>,
    pub(crate) start:            Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct RunInputValue(Value);

impl From<Value> for RunInputValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl RunInputValue {
    fn into_inner(self) -> Value {
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
pub(crate) struct ValidatedCreateRuns {
    pub(crate) runs: Vec<ValidatedCreateRunSpec>,
}

#[derive(Debug)]
pub(crate) struct ValidatedCreateRunSpec {
    pub(crate) workflow:         String,
    pub(crate) cwd:              Option<PathBuf>,
    pub(crate) run_id:           Option<RunId>,
    pub(crate) parent_id:        Option<String>,
    pub(crate) goal:             Option<String>,
    pub(crate) inputs:           HashMap<String, toml::Value>,
    pub(crate) labels:           HashMap<String, String>,
    pub(crate) dry_run:          Option<bool>,
    pub(crate) auto_approve:     Option<bool>,
    pub(crate) model:            Option<String>,
    pub(crate) provider:         Option<String>,
    pub(crate) sandbox:          Option<String>,
    pub(crate) preserve_sandbox: Option<bool>,
    pub(crate) start:            Option<bool>,
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
            inputs,
            labels: spec.labels,
            dry_run: spec.dry_run,
            auto_approve: spec.auto_approve,
            model: spec.model,
            provider: spec.provider,
            sandbox: spec.sandbox,
            preserve_sandbox: spec.preserve_sandbox,
            start: spec.start,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CreateRunsResult {
    pub(crate) runs: Vec<CreatedRunResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CreatedRunResult {
    pub(crate) run_id:         String,
    pub(crate) parent_id:      Option<String>,
    pub(crate) children_count: u64,
    pub(crate) workflow:       String,
    pub(crate) started:        bool,
    pub(crate) status:         String,
}

pub(crate) async fn create_runs(
    client: Arc<Client>,
    base_cwd: &Path,
    user_settings_path: &Path,
    params: ValidatedCreateRuns,
) -> ToolResult<CreateRunsResult> {
    let mut created = Vec::with_capacity(params.runs.len());
    for spec in params.runs {
        let cwd = spec.cwd.clone().unwrap_or_else(|| base_cwd.to_path_buf());
        let mut manifest = manifest::build_mcp_run_manifest(&spec, &cwd, user_settings_path)?;
        if let Some(parent_selector) = spec.parent_id.as_deref() {
            let parent_id = client
                .resolve_run(parent_selector)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
                .id;
            manifest.parent_id = Some(parent_id.to_string());
        }
        let run_id = client
            .create_run_from_manifest(manifest)
            .await
            .map_err(|err| ToolError::from_anyhow(&err))?;
        let started = spec.start.unwrap_or(true);
        let summary = if started {
            client
                .start_run(&run_id, false)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        } else {
            client
                .retrieve_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        };
        created.push(CreatedRunResult {
            run_id: summary.id.to_string(),
            parent_id: summary.parent_id.map(|parent_id| parent_id.to_string()),
            children_count: summary.children_count,
            workflow: spec.workflow,
            started,
            status: common::run_status_kind(summary.lifecycle.status).to_string(),
        });
    }
    Ok(CreateRunsResult { runs: created })
}

pub(crate) fn create_runs_text(result: &CreateRunsResult) -> String {
    let started = result.runs.iter().filter(|run| run.started).count();
    format!(
        "created {} Fabro run(s), started {started}",
        result.runs.len()
    )
}

#[cfg(test)]
mod tests {
    use schemars::SchemaGenerator;
    use serde_json::json;
    use tokio::fs;

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
            inputs:           HashMap::new(),
            labels:           HashMap::new(),
            dry_run:          None,
            auto_approve:     None,
            model:            None,
            provider:         None,
            sandbox:          None,
            preserve_sandbox: None,
            start:            None,
        })
        .expect("parent selectors should validate without requiring exact run ids");

        assert_eq!(spec.parent_id.as_deref(), Some("nightly-parent"));
    }

    #[tokio::test]
    async fn create_runs_resolves_parent_selector_and_sends_parent_id_in_manifest() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let workflow = temp.path().join("simple.fabro");
        fs::write(
            &workflow,
            r#"digraph Simple {
    graph [goal="Run tests and report results"]
    start [shape=Mdiamond, label="Start"]
    exit [shape=Msquare, label="Exit"]
    start -> exit
}
"#,
        )
        .await
        .expect("workflow should be written");
        let settings = temp.path().join("settings.toml");
        fs::write(&settings, "")
            .await
            .expect("settings should be written");

        let server = httpmock::MockServer::start();
        let child_id = run_id("01KRBZW5C00000000000000001");
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let resolve_parent = server.mock(|when, then| {
            when.method("GET")
                .path("/api/v1/runs/resolve")
                .query_param("selector", "nightly-parent");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(run_summary_json(parent_id, None, 1));
        });
        let create = server.mock(|when, then| {
            when.method("POST")
                .path("/api/v1/runs")
                .json_body_includes(format!(r#"{{"parent_id":"{parent_id}"}}"#));
            then.status(201)
                .header("Content-Type", "application/json")
                .json_body(run_summary_json(child_id, Some(parent_id), 0));
        });
        let retrieve = server.mock(|when, then| {
            when.method("GET").path(format!("/api/v1/runs/{child_id}"));
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(run_summary_json(child_id, Some(parent_id), 0));
        });
        let client =
            Arc::new(Client::new_no_proxy(&server.base_url()).expect("client should build"));
        let params = ValidatedCreateRuns::try_from(FabroRunCreateParams {
            runs: vec![CreateRunSpec {
                workflow:         workflow.display().to_string(),
                cwd:              None,
                run_id:           None,
                parent_id:        Some("nightly-parent".to_string()),
                goal:             None,
                inputs:           HashMap::new(),
                labels:           HashMap::new(),
                dry_run:          Some(true),
                auto_approve:     Some(true),
                model:            None,
                provider:         None,
                sandbox:          None,
                preserve_sandbox: None,
                start:            Some(false),
            }],
        })
        .expect("create params should validate");

        let result = create_runs(client, temp.path(), &settings, params)
            .await
            .expect("run should be created");

        assert_eq!(result.runs[0].parent_id, Some(parent_id.to_string()));
        assert_eq!(result.runs[0].children_count, 0);
        resolve_parent.assert();
        create.assert();
        retrieve.assert();
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }

    fn run_summary_json(
        run_id: RunId,
        parent_id: Option<RunId>,
        children_count: u64,
    ) -> serde_json::Value {
        json!({
            "id": run_id,
            "parent_id": parent_id,
            "children_count": children_count,
            "title": "Test run",
            "goal": "Test run",
            "workflow": {
                "slug": "simple",
                "name": "Simple"
            },
            "repository": null,
            "origin": {
                "kind": "api"
            },
            "labels": {},
            "lifecycle": {
                "status": { "kind": "submitted" },
                "pending_control": null,
                "queue_position": null,
                "error": null,
                "archived": false,
                "archived_at": null
            },
            "models": [],
            "source_directory": "/srv/repo",
            "timestamps": {
                "created_at": "2026-04-05T12:00:00Z",
                "started_at": null,
                "last_event_at": null,
                "completed_at": null,
                "duration_ms": null,
                "elapsed_secs": null
            },
            "billing": null,
            "diff": null,
            "pull_request": null,
            "current_question": null,
            "superseded_by": null,
            "links": {
                "web": null
            }
        })
    }
}
