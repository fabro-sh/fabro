use std::sync::Arc;

use anyhow::Context as _;
use fabro_types::{RunId, RunIntent, RunIntentArgs, RunProjection, RunTarget, WorkflowVersionId};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};

use super::common::{self, FabroToolBackend, ToolError, ToolResult};
use super::manifest;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FabroRunCreateParams {
    #[serde(deserialize_with = "deserialize_runs")]
    #[schemars(length(min = 1, max = 50))]
    pub runs: Vec<CreateRunSpec>,
}

fn deserialize_runs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<CreateRunSpec>, D::Error> {
    Vec::<CreateRunSpec>::deserialize(deserializer).map_err(|error| {
        de::Error::custom(format!(
            "fabro_run_create requires workflow_version_id and canonical RunIntent fields; register file contents with fabro_workflow_version_create first: {error}"
        ))
    })
}

/// RunIntent fields plus tool-only parent/target defaults and a separate start
/// request.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRunSpec {
    #[schemars(with = "String")]
    pub workflow_version_id: WorkflowVersionId,
    #[serde(default)]
    #[schemars(schema_with = "run_target_schema")]
    pub target:              Option<RunTarget>,
    #[serde(default)]
    #[schemars(schema_with = "run_args_schema")]
    pub args:                RunIntentArgs,
    pub environment_id:      Option<String>,
    #[schemars(with = "Option<String>")]
    pub parent_id:           Option<RunId>,
    pub title:               Option<String>,
    pub goal:                Option<String>,
    pub start:               Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CreateRunOptions {
    /// Set only by trusted native worker dispatch, never from tool JSON.
    pub forced_parent_id: Option<RunId>,
}

impl FabroRunCreateParams {
    pub fn validate(&self, options: CreateRunOptions) -> ToolResult<()> {
        common::validate_len("runs", self.runs.len(), 1, 50)?;
        for spec in &self.runs {
            if let Some(parent) = options.forced_parent_id {
                if spec.parent_id.is_some_and(|id| id != parent) {
                    return Err(ToolError::message(format!(
                        "parent_id must be omitted or match the current run {parent}"
                    )));
                }
            } else if spec.target.is_none() {
                return Err(ToolError::message(
                    "standalone fabro_run_create requires an explicit target; use kind: none for an empty workspace",
                ));
            }
            if let Some(target) = &spec.target {
                target
                    .clone()
                    .canonical()
                    .map_err(|error| ToolError::from_anyhow(&error.into()))?;
            }
            for (key, value) in &spec.args.inputs {
                manifest::json_to_toml_value(key, value)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreateRunsResult {
    pub runs: Vec<CreatedRunResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreatedRunResult {
    pub run_id:              String,
    pub parent_id:           Option<String>,
    pub children_count:      u64,
    #[schemars(with = "String")]
    pub workflow_version_id: WorkflowVersionId,
    pub start_requested:     bool,
    pub status:              String,
}

pub async fn create_runs(
    backend: Arc<dyn FabroToolBackend>,
    params: FabroRunCreateParams,
) -> ToolResult<CreateRunsResult> {
    create_runs_with_options(backend, params, CreateRunOptions::default()).await
}

pub async fn create_runs_with_options(
    backend: Arc<dyn FabroToolBackend>,
    params: FabroRunCreateParams,
    options: CreateRunOptions,
) -> ToolResult<CreateRunsResult> {
    params.validate(options)?;
    let mut runs = Vec::with_capacity(params.runs.len());
    let mut created_ids = Vec::new();
    for spec in params.runs {
        let result: anyhow::Result<CreatedRunResult> = async {
            let parent_id = options.forced_parent_id.or(spec.parent_id);
            let target = if let Some(target) = spec.target {
                target
            } else {
                let parent = options
                    .forced_parent_id
                    .context("an explicit target is required")?;
                inherit_parent_target(&backend.get_run_state(&parent).await?)?
            };
            let intent = RunIntent {
                workflow_version_id: spec.workflow_version_id,
                target,
                args: spec.args,
                environment_id: spec.environment_id,
                parent_id,
                title: spec.title,
                goal: spec.goal,
            };
            let run_id = backend.create_run_from_intent(intent).await?;
            created_ids.push(run_id);
            let start_requested = spec.start.unwrap_or(true);
            let summary = if start_requested {
                backend.start_run(&run_id, false).await.with_context(|| {
                    format!("run {run_id} was created but its start request failed")
                })?
            } else {
                backend.retrieve_run(&run_id).await.with_context(|| {
                    format!("run {run_id} was created but retrieving its summary failed")
                })?
            };
            Ok(CreatedRunResult {
                run_id: run_id.to_string(),
                parent_id: summary.parent_id.map(|id| id.to_string()),
                children_count: summary.children_count,
                workflow_version_id: spec.workflow_version_id,
                start_requested,
                status: summary.lifecycle.status.kind().to_string(),
            })
        }
        .await;
        match result {
            Ok(run) => runs.push(run),
            Err(error) => {
                let error = if created_ids.is_empty() {
                    error
                } else {
                    error.context(format!(
                        "already created run IDs: {}; inspect these runs before retrying",
                        created_ids
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                };
                return Err(ToolError::from_anyhow(&error));
            }
        }
    }
    Ok(CreateRunsResult { runs })
}

fn inherit_parent_target(parent: &RunProjection) -> anyhow::Result<RunTarget> {
    let target = parent.spec.target.as_ref().context(
        "the parent run has no canonical target; send an explicit target for this child run",
    )?;
    Ok(match target {
        RunTarget::Git(git) => {
            let branch = if parent.spec.settings.run.run_branch.enabled {
                parent.start.as_ref().and_then(|start| start.run_branch.as_ref())
                    .filter(|branch| !branch.trim().is_empty()).context(
                    "the parent run has no execution branch yet; send an explicit target for this child run"
                )?
            } else {
                &git.branch
            };
            RunTarget::Git(fabro_types::GitRunTarget {
                repo:   git.repo.clone(),
                branch: branch.clone(),
                tag:    None,
                sha:    None,
            })
        }
        RunTarget::None {} | RunTarget::Folder { .. } => target.clone(),
    })
}

pub fn create_runs_text(result: &CreateRunsResult) -> String {
    let start_requested = result.runs.iter().filter(|run| run.start_requested).count();
    format!(
        "created {} Fabro run(s), start requested for {start_requested}",
        result.runs.len()
    )
}

// Schema metadata for canonical types owned by fabro-types. Runtime values use
// those types directly; parity tests below prevent tool-schema drift.
fn run_args_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "model": {"type": ["string", "null"]},
            "provider": {"type": ["string", "null"]},
            "inputs": {"type": "object", "additionalProperties": {"type": ["string", "boolean", "number"]}},
            "labels": {"type": "object", "additionalProperties": {"type": "string"}},
            "dry_run": {"type": ["boolean", "null"]},
            "auto_approve": {"type": ["boolean", "null"]},
            "preserve_sandbox": {"type": ["boolean", "null"]}
        }
    })
}
fn run_target_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "description": "Canonical workspace target. Required for standalone calls; native workers inherit the parent execution target when omitted.",
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
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use fabro_api::types::Usage;
    use fabro_types::{
        GitRunTarget, Run, RunLifecycle, RunLinks, RunOrigin, RunStatus, RunTimestamps,
        WorkflowRef, test_support,
    };
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use serde_json::{Value, json};

    use super::*;
    use crate::fabro_client::ClientBackend;

    fn version_id() -> WorkflowVersionId {
        fabro_types::BlobHash::new(b"workflow").into()
    }
    fn params(value: Value) -> FabroRunCreateParams {
        FabroRunCreateParams {
            runs: vec![serde_json::from_value(value).unwrap()],
        }
    }
    fn spec() -> Value {
        json!({"workflow_version_id": version_id(), "target": {"kind": "none"}, "start": false})
    }
    fn backend(server: &MockServer) -> Arc<dyn FabroToolBackend> {
        Arc::new(ClientBackend::new(Arc::new(
            fabro_client::Client::new_no_proxy(&server.url("")).unwrap(),
        )))
    }
    fn parent(target: RunTarget) -> RunProjection {
        let mut spec = test_support::test_run_spec();
        spec.target = Some(target);
        RunProjection::new(String::new(), spec, Utc::now())
    }

    #[test]
    fn run_create_accepts_canonical_id_target_and_args() {
        let mut value = spec();
        value["args"] = json!({"model":"model", "provider":"provider", "inputs":{"s":"x", "b":true,"i":4,"f":1.25}, "labels":{"x":"y"}, "dry_run":false, "auto_approve":true, "preserve_sandbox":false});
        let p = params(value.clone());
        p.validate(CreateRunOptions::default()).unwrap();
        assert_eq!(
            serde_json::to_value(&p.runs[0].args).unwrap(),
            value["args"]
        );
        let p = params(spec());
        assert_eq!(serde_json::to_value(&p.runs[0].args).unwrap(), json!({}));
        assert_eq!(p.runs[0].environment_id, None);
    }

    #[test]
    fn run_create_schema_and_serde_reject_old_sources_and_accept_canonical_variants() {
        let schema = serde_json::to_value(schemars::schema_for!(FabroRunCreateParams)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for target in [
            json!({"kind":"none"}),
            json!({"kind":"folder","path":"/workspace"}),
            json!({"kind":"git","repo":"acme/repo","branch":"main","sha":"a".repeat(40),"tag":"v1"}),
        ] {
            let value = json!({"runs":[{"workflow_version_id": version_id(), "target":target, "args":{"dry_run":false}}]});
            assert!(validator.is_valid(&value));
            serde_json::from_value::<FabroRunCreateParams>(value)
                .unwrap()
                .validate(CreateRunOptions::default())
                .unwrap();
        }
        for old in [
            json!("workflow"),
            json!({"workflow":"workflow"}),
            json!({"workflow":{"kind":"inline","entrypoint":"x","files":{"x":"content"}}}),
        ] {
            let value = json!({"runs":[old]});
            assert!(!validator.is_valid(&value));
            let error = serde_json::from_value::<FabroRunCreateParams>(value)
                .unwrap_err()
                .to_string();
            assert!(error.contains("fabro_workflow_version_create"));
        }
        for (key, value) in [
            ("cwd", json!("/tmp")),
            ("goal_file", json!("goal.md")),
            ("inputs", json!({})),
            ("environment", json!("local")),
        ] {
            let mut item = spec();
            item[key] = value;
            let value = json!({"runs":[item]});
            assert!(!validator.is_valid(&value));
            assert!(serde_json::from_value::<FabroRunCreateParams>(value).is_err());
        }
        for value in [json!(null), json!([]), json!({})] {
            let mut item = spec();
            item["args"] = json!({"inputs":{"bad":value}});
            assert!(params(item).validate(CreateRunOptions::default()).is_err());
        }
    }

    #[test]
    fn run_create_context_validation_requires_standalone_target_and_current_worker_parent() {
        let mut item = spec();
        item.as_object_mut().unwrap().remove("target");
        item["parent_id"] = json!(RunId::new());
        assert!(
            params(item.clone())
                .validate(CreateRunOptions::default())
                .unwrap_err()
                .as_str()
                .contains("explicit target")
        );
        let options = CreateRunOptions {
            forced_parent_id: Some(RunId::new()),
        };
        assert!(
            params(item.clone())
                .validate(options)
                .unwrap_err()
                .as_str()
                .contains("current run")
        );
        item["parent_id"] = json!(options.forced_parent_id);
        params(item).validate(options).unwrap();
        assert!(
            FabroRunCreateParams { runs: vec![] }
                .validate(options)
                .is_err()
        );
    }

    #[test]
    fn inherited_target_uses_execution_branch_and_requires_execution_state() {
        let mut p = parent(RunTarget::Git(GitRunTarget {
            repo:   "acme/repo".into(),
            branch: "main".into(),
            sha:    Some("a".repeat(40)),
            tag:    Some("v1".into()),
        }));
        assert!(
            inherit_parent_target(&p)
                .unwrap_err()
                .to_string()
                .contains("no execution branch")
        );
        p.start = Some(fabro_types::StartRecord {
            start_time: Utc::now(),
            run_branch: Some("fabro/run/parent".into()),
            base_sha:   None,
        });
        let RunTarget::Git(target) = inherit_parent_target(&p).unwrap() else {
            panic!("expected git")
        };
        assert_eq!(target.branch, "fabro/run/parent");
        assert_eq!(target.sha, None);
        assert_eq!(target.tag, None);
        p.spec.settings.run.run_branch.enabled = false;
        let RunTarget::Git(target) = inherit_parent_target(&p).unwrap() else {
            panic!("expected git")
        };
        assert_eq!(target.branch, "main");
        assert_eq!(target.sha, None);
        assert_eq!(target.tag, None);
        for target in [RunTarget::None {}, RunTarget::Folder {
            path: "/workspace".into(),
        }] {
            p.spec.target = Some(target.clone());
            assert_eq!(inherit_parent_target(&p).unwrap(), target);
        }
        p.spec.target = None;
        assert!(inherit_parent_target(&p).is_err());
    }

    #[tokio::test]
    async fn run_create_submits_canonical_intent_without_registration_or_parent_lookup() {
        let server = MockServer::start_async().await;
        let id = RunId::new();
        let parent_id = RunId::new();
        let mut item = spec();
        item["parent_id"] = json!(parent_id);
        item["title"] = json!("Title");
        item["goal"] = json!("Literal goal");
        item["environment_id"] = json!("environment");
        item["args"] = json!({"dry_run":false});
        let mut intent = item.clone();
        intent.as_object_mut().unwrap().remove("start");
        let create = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/runs")
                    .json_body_obj(&intent);
                then.status(201).json_body_obj(&run(id, None, 0));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path(format!("/api/v1/runs/{id}"));
                then.status(200).json_body_obj(&run(id, Some(parent_id), 0));
            })
            .await;
        let registration = server
            .mock_async(|when, then| {
                when.path("/api/v1/workflow-versions");
                then.status(500);
            })
            .await;
        let state = server
            .mock_async(|when, then| {
                when.path(format!("/api/v1/runs/{parent_id}/state"));
                then.status(500);
            })
            .await;
        let result = create_runs(backend(&server), params(item)).await.unwrap();
        assert_eq!(result.runs[0].run_id, id.to_string());
        assert!(!result.runs[0].start_requested);
        create.assert_calls_async(1).await;
        registration.assert_calls_async(0).await;
        state.assert_calls_async(0).await;
    }

    #[tokio::test]
    async fn run_create_worker_inherits_fresh_parent_state() {
        let server = MockServer::start_async().await;
        let id = RunId::new();
        let p = parent(RunTarget::None {});
        let parent_id = p.spec.id();
        let state = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(format!("/api/v1/runs/{parent_id}/state"));
                then.status(200).json_body_obj(&p);
            })
            .await;
        let create = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/v1/runs").json_body(json!({
                    "workflow_version_id": version_id(),
                    "target": {"kind": "none"},
                    "args": {},
                    "parent_id": parent_id
                }));
                then.status(201).json_body_obj(&run(id, Some(parent_id), 0));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path(format!("/api/v1/runs/{id}"));
                then.status(200).json_body_obj(&run(id, Some(parent_id), 0));
            })
            .await;
        let mut item = spec();
        item.as_object_mut().unwrap().remove("target");
        create_runs_with_options(backend(&server), params(item), CreateRunOptions {
            forced_parent_id: Some(parent_id),
        })
        .await
        .unwrap();
        state.assert_calls_async(1).await;
        create.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn run_create_start_and_later_batch_failures_report_already_created_ids() {
        let server = MockServer::start_async().await;
        let id = RunId::new();
        server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/runs")
                    .json_body_includes(json!({"goal":"first"}).to_string());
                then.status(201).json_body_obj(&run(id, None, 0));
            })
            .await;
        let start = server
            .mock_async(|when, then| {
                when.method(POST).path(format!("/api/v1/runs/{id}/start"));
                then.status(500);
            })
            .await;
        let mut item = spec();
        item["goal"] = json!("first");
        item.as_object_mut().unwrap().remove("start");
        let error = create_runs(backend(&server), params(item.clone()))
            .await
            .unwrap_err();
        assert!(error.as_str().contains(&id.to_string()));
        assert!(error.as_str().contains("start request failed"));
        start.assert_calls_async(1).await;
        item["start"] = json!(false);
        server
            .mock_async(|when, then| {
                when.method(GET).path(format!("/api/v1/runs/{id}"));
                then.status(200).json_body_obj(&run(id, None, 0));
            })
            .await;
        let failed = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/v1/runs")
                    .json_body_includes(json!({"goal":"second"}).to_string());
                then.status(400);
            })
            .await;
        let mut second = item.clone();
        second["goal"] = json!("second");
        let p = serde_json::from_value(json!({"runs":[item,second]})).unwrap();
        let error = create_runs(backend(&server), p).await.unwrap_err();
        assert!(error.as_str().contains(&id.to_string()));
        failed.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn run_create_same_run_backend_denies_before_network() {
        let client = fabro_client::Client::new_no_proxy("http://127.0.0.1:1").unwrap();
        let backend = Arc::new(ClientBackend::new(Arc::new(client)).with_run_scope(RunId::new()));
        let error = create_runs(backend, params(spec())).await.unwrap_err();
        assert!(error.as_str().contains("outside this tool session"));
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
            usage: Usage::default(),
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
}
