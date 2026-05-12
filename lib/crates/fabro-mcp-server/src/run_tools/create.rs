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
    pub(crate) run_id:   String,
    pub(crate) workflow: String,
    pub(crate) started:  bool,
    pub(crate) status:   String,
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
        let manifest = manifest::build_mcp_run_manifest(&spec, &cwd, user_settings_path)?;
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
}
