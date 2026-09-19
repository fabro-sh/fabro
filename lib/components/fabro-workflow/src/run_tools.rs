//! The Fabro workflow and run tools as application tools a pebble coding
//! agent can call.

use std::sync::Arc;

use pebble_coding_agent::tools::{RegisteredTool, ToolError, ToolSource};
use serde::de::DeserializeOwned;

use crate::services::FabroRunToolServices;

/// Every Fabro run tool, bound to `services`.
#[must_use]
pub fn register_fabro_run_tools(services: &FabroRunToolServices) -> Vec<RegisteredTool> {
    fabro_tool::tool_definitions()
        .iter()
        .map(|definition| fabro_run_tool(definition, services.clone()))
        .collect()
}

/// Only the Fabro run tools whose names appear in `names`.
///
/// Unknown names are silently ignored so callers can list every tool they
/// care about without depending on the current `fabro_tool` catalog.
#[must_use]
pub fn register_named_fabro_run_tools(
    services: &FabroRunToolServices,
    names: &[&str],
) -> Vec<RegisteredTool> {
    fabro_tool::tool_definitions()
        .iter()
        .filter(|definition| names.contains(&definition.name))
        .map(|definition| fabro_run_tool(definition, services.clone()))
        .collect()
}

fn fabro_run_tool(
    definition: &fabro_tool::ToolDefinition,
    services: FabroRunToolServices,
) -> RegisteredTool {
    let name = definition.name.to_string();
    let services = Arc::new(services);
    RegisteredTool::function(
        name.clone(),
        definition.description.to_string(),
        definition.parameters.clone(),
        move |_context, arguments| {
            let name = name.clone();
            let services = Arc::clone(&services);
            async move {
                execute_fabro_run_tool(&name, arguments, &services)
                    .await
                    .map_err(|error| ToolError::execution(error.to_string()))
            }
        },
    )
    .with_source(ToolSource::Application)
    // A subagent spawned by a workflow stage does the same work under the
    // same run, so it keeps the same view of the run tree.
    .allow_in_subagents()
}

pub(crate) async fn execute_fabro_run_tool(
    name: &str,
    args: serde_json::Value,
    services: &FabroRunToolServices,
) -> fabro_tool::ToolResult<String> {
    match name {
        fabro_tool::FABRO_WORKFLOW_VERSION_CREATE_TOOL_NAME => {
            let params =
                parse_fabro_tool_args::<fabro_tool::FabroWorkflowVersionCreateParams>(name, args)?;
            let source = fabro_tool::ValidatedWorkflowVersionCreate::try_from(params)?;
            let result =
                fabro_tool::create_workflow_version(Arc::clone(&services.backend), source).await?;
            let summary = fabro_tool::workflow_version_create_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_CREATE_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunCreateParams>(name, args)?;
            let result = fabro_tool::create_runs_with_options(
                Arc::clone(&services.backend),
                params,
                fabro_tool::CreateRunOptions {
                    forced_parent_id: Some(services.current_run_id),
                },
            )
            .await?;
            let summary = fabro_tool::create_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_SEARCH_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunSearchParams>(name, args)?;
            let result = fabro_tool::search_runs(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedSearchRuns::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::search_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_GET_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunGetParams>(name, args)?;
            let result = fabro_tool::run_get(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedRunGet::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::run_get_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_INTERACT_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunInteractParams>(name, args)?;
            let validated = fabro_tool::ValidatedInteractRun::try_from(params)?;
            if validated.action.requires_user() {
                return Err(fabro_tool::ToolError::message(
                    "Run approval must be performed by a user through the API, CLI, web UI, or human MCP server.",
                ));
            }
            let result = fabro_tool::interact_run(Arc::clone(&services.backend), validated).await?;
            let summary = fabro_tool::interact_run_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_GATHER_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunGatherParams>(name, args)?;
            let result = fabro_tool::gather_runs(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedGatherRuns::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::gather_runs_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunEventsParams>(name, args)?;
            let result = fabro_tool::run_events(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedRunEvents::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::run_events_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        fabro_tool::FABRO_RUN_PAIR_TOOL_NAME => {
            let params = parse_fabro_tool_args::<fabro_tool::FabroRunPairParams>(name, args)?;
            let result = fabro_tool::pair_run(
                Arc::clone(&services.backend),
                fabro_tool::ValidatedPairRun::try_from(params)?,
            )
            .await?;
            let summary = fabro_tool::pair_run_text(&result);
            render_fabro_tool_result(&summary, &result)
        }
        _ => Err(fabro_tool::ToolError::message(format!(
            "unknown Fabro run tool `{name}`"
        ))),
    }
}

fn parse_fabro_tool_args<T>(name: &str, args: serde_json::Value) -> fabro_tool::ToolResult<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args)
        .map_err(|err| fabro_tool::ToolError::message(format!("invalid {name} arguments: {err}")))
}

fn render_fabro_tool_result<T>(summary: &str, result: &T) -> fabro_tool::ToolResult<String>
where
    T: serde::Serialize,
{
    let json = serde_json::to_string_pretty(result).map_err(|err| {
        fabro_tool::ToolError::message(format!("failed to serialize tool result: {err}"))
    })?;
    Ok(format!("{summary}\n{json}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use fabro_tool::fabro_client::ClientBackend;
    use fabro_tool::{ValidatedWorkflowVersionCreate, WorkflowVersionPackager};
    use fabro_types::WorkflowVersion;
    use fabro_workflow_version::{CollectedWorkflowClosure, ValidatedWorkflowVersion};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn native_run_create_submits_intent_and_enforces_current_parent() {
        let server = httpmock::MockServer::start_async().await;
        let parent_id = fabro_types::RunId::new();
        let version_id: fabro_types::WorkflowVersionId =
            fabro_types::BlobHash::new(b"registered workflow").into();
        let create = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/v1/runs")
                    .json_body(json!({
                        "workflow_version_id": version_id,
                        "target": {"kind":"none"},
                        "parent_id": parent_id,
                        "args": {"auto_approve":false}
                    }));
                // Admission rejection proves the native dispatcher reached the
                // canonical API without registering or looking up a workflow.
                then.status(422).body("native admission rejection");
            })
            .await;
        let state = server
            .mock_async(|when, then| {
                when.path(format!("/api/v1/runs/{parent_id}/state"));
                then.status(500);
            })
            .await;
        let client = fabro_client::Client::new_no_proxy(&server.url("")).unwrap();
        let services = FabroRunToolServices {
            backend:        Arc::new(ClientBackend::new(Arc::new(client))),
            current_run_id: parent_id,
        };
        let name = fabro_tool::FABRO_RUN_CREATE_TOOL_NAME;
        let mut args = json!({"runs":[{
            "workflow_version_id":version_id,
            "target":{"kind":"none"},
            "args":{"auto_approve":false}
        }]});
        let error = execute_fabro_run_tool(name, args.clone(), &services)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("native admission rejection"));
        args["runs"][0]["parent_id"] = json!(fabro_types::RunId::new());
        let error = execute_fabro_run_tool(name, args, &services)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("match the current run"));
        create.assert_calls_async(1).await;
        state.assert_calls_async(0).await;
    }

    struct SingleGraphPackager;

    #[async_trait]
    impl WorkflowVersionPackager for SingleGraphPackager {
        async fn package(
            &self,
            source: ValidatedWorkflowVersionCreate,
        ) -> anyhow::Result<CollectedWorkflowClosure> {
            let version = WorkflowVersion::new(source.entrypoint, source.files, BTreeMap::new())?;
            let id = version.id()?;
            Ok(CollectedWorkflowClosure::from_dependency_order(id, vec![(
                id,
                ValidatedWorkflowVersion::new(version)?,
            )]))
        }
    }

    #[tokio::test]
    async fn workflow_version_native_dispatch_registers_and_returns_version() {
        let server = httpmock::MockServer::start_async().await;
        let version = WorkflowVersion::new(
            "workflow".parse().unwrap(),
            BTreeMap::from([("workflow".parse().unwrap(), "digraph W {}".into())]),
            BTreeMap::new(),
        )
        .unwrap();
        let id = version.id().unwrap();
        let upload = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/v1/workflow-versions")
                    .json_body_obj(&version);
                then.status(201)
                    .json_body(json!({"workflow_version_id": id}));
            })
            .await;
        let client = fabro_client::Client::new_no_proxy(&server.url("")).unwrap();
        let services = FabroRunToolServices {
            backend:        Arc::new(
                ClientBackend::new(Arc::new(client))
                    .with_workflow_version_packager(Arc::new(SingleGraphPackager)),
            ),
            current_run_id: "01KRBZW4DW0000000000000002".parse().unwrap(),
        };
        let name = fabro_tool::FABRO_WORKFLOW_VERSION_CREATE_TOOL_NAME;
        assert_eq!(register_named_fabro_run_tools(&services, &[name]).len(), 1);
        let output = execute_fabro_run_tool(
            name,
            json!({"entrypoint":"workflow", "files":{"workflow":"digraph W {}"}}),
            &services,
        )
        .await
        .unwrap();
        let (summary, body) = output.split_once('\n').unwrap();
        assert_eq!(summary, format!("Registered workflow version {id}"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            json!({"workflow_version_id": id})
        );
        let error = execute_fabro_run_tool(
            name,
            json!({"entrypoint":"missing", "files":{"workflow":"digraph W {}"}}),
            &services,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not present"));
        upload.assert_calls_async(1).await;
    }
}
