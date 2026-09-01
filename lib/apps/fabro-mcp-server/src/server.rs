use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fabro_server::run_tool_create::ServerRunCreateAdapter;
use fabro_tool::fabro_client::ClientBackend;
use fabro_tool::{self as run_tools, FabroToolBackend};
use fabro_util::version::FABRO_VERSION;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, serve_server, tool, tool_handler, tool_router};
use serde::Serialize;
use tokio::sync::OnceCell;
use tokio::time;
use tracing::warn;

use crate::executable_monitor::ExecutableMonitor;
use crate::{FabroMcpServerSettings, SERVER_NAME};

#[derive(Clone)]
pub(crate) struct FabroMcpServer {
    settings:    Arc<FabroMcpServerSettings>,
    backend:     Arc<OnceCell<Arc<dyn FabroToolBackend>>>,
    cwd:         PathBuf,
    tool_router: ToolRouter<Self>,
}

/// How long to wait for the MCP service to stop after an upgrade is detected.
/// The wait is bounded because the transport closes by writing to stdout, which
/// blocks if the host has stopped reading.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn start(settings: FabroMcpServerSettings) -> Result<()> {
    let monitor = match ExecutableMonitor::current() {
        Ok(monitor) => Some(monitor),
        Err(error) => {
            warn!(
                %error,
                "Upgrade detection is unavailable; this MCP server will keep running after an \
                 upgrade replaces it"
            );
            None
        }
    };
    let service = serve_server(FabroMcpServer::new(Arc::new(settings)), stdio()).await?;
    let Some(monitor) = monitor else {
        service.waiting().await?;
        return Ok(());
    };

    let cancellation = service.cancellation_token();
    let mut service_wait = Box::pin(service.waiting());
    tokio::select! {
        result = &mut service_wait => {
            result?;
        }
        () = monitor.wait_until_replaced() => {
            // An upgrade replaced the executable, so stop serving and let the
            // host reconnect to the new one. The CLI exits the process rather
            // than returning, because Tokio's stdin worker stays blocked on a
            // read that only the host can end.
            cancellation.cancel();
            let _ = time::timeout(SHUTDOWN_TIMEOUT, service_wait).await;
        }
    }
    Ok(())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FabroMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, FABRO_VERSION).with_title("Fabro"))
            .with_instructions("Use these tools to create, inspect, control, wait for, and read events from Fabro workflow runs.")
    }
}

#[tool_router(router = tool_router)]
impl FabroMcpServer {
    pub(crate) fn new(settings: Arc<FabroMcpServerSettings>) -> Self {
        let cwd = settings.cwd.clone();
        Self {
            settings,
            backend: Arc::new(OnceCell::new()),
            cwd,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "fabro_run_create",
        description = "Create one or more Fabro workflow runs from a native selector, inline files, or an exact stored workflow version, optionally under a parent run and starting them by default."
    )]
    async fn fabro_run_create(
        &self,
        params: Parameters<run_tools::FabroRunCreateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedCreateRuns::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::create_runs(backend, &self.cwd, params).await {
            Ok(result) => success_result(&result, run_tools::create_runs_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_search",
        description = "Search Fabro workflow runs by id, parent, workflow, labels, status, archival state, and creation time."
    )]
    async fn fabro_run_search(
        &self,
        params: Parameters<run_tools::FabroRunSearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedSearchRuns::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::search_runs(backend, params).await {
            Ok(result) => success_result(&result, run_tools::search_runs_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_get",
        description = "Read-only inspection of a Fabro run: returns its summary, projection, and pending questions without mutating state."
    )]
    async fn fabro_run_get(
        &self,
        params: Parameters<run_tools::FabroRunGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedRunGet::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::run_get(backend, params).await {
            Ok(result) => success_result(&result, run_tools::run_get_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_interact",
        description = "Control a Fabro run: start, approve, deny, message, interrupt, cancel, archive, unarchive, link or unlink a parent, inspect or answer questions. Use fabro_run_get for read-only inspection."
    )]
    async fn fabro_run_interact(
        &self,
        params: Parameters<run_tools::FabroRunInteractParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedInteractRun::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::interact_run(backend, params).await {
            Ok(result) => success_result(&result, run_tools::interact_run_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_gather",
        description = "Wait for Fabro runs to reach terminal states, returning current state on timeout."
    )]
    async fn fabro_run_gather(
        &self,
        params: Parameters<run_tools::FabroRunGatherParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedGatherRuns::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::gather_runs(backend, params).await {
            Ok(result) => success_result(&result, run_tools::gather_runs_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_pair",
        description = "Inspect, start, message, end, or read transcript for a live Fabro run pairing session."
    )]
    async fn fabro_run_pair(
        &self,
        params: Parameters<run_tools::FabroRunPairParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedPairRun::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::pair_run(backend, params).await {
            Ok(result) => success_result(&result, run_tools::pair_run_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    #[tool(
        name = "fabro_run_events",
        description = "List, inspect, or search stored events for a Fabro workflow run."
    )]
    async fn fabro_run_events(
        &self,
        params: Parameters<run_tools::FabroRunEventsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedRunEvents::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(error_result(&err)),
        };
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(err) => return Ok(error_result(&err)),
        };
        match run_tools::run_events(backend, params).await {
            Ok(result) => success_result(&result, run_tools::run_events_text(&result)),
            Err(err) => Ok(error_result(&err)),
        }
    }

    async fn backend(&self) -> Result<Arc<dyn FabroToolBackend>, run_tools::ToolError> {
        self.backend
            .get_or_try_init(|| async {
                (self.settings.client_factory)()
                    .await
                    .map(|client| {
                        let user_workflows_root = self
                            .settings
                            .config_path
                            .parent()
                            .map(|parent| parent.join("workflows"));
                        Arc::new(
                            ClientBackend::new(Arc::new(client)).with_run_create_adapter(Arc::new(
                                ServerRunCreateAdapter::standalone(user_workflows_root),
                            )),
                        ) as Arc<dyn FabroToolBackend>
                    })
                    .map_err(|err| run_tools::ToolError::from_anyhow(&err))
            })
            .await
            .map(Arc::clone)
    }
}

fn success_result<T: Serialize>(
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

fn error_result(err: &run_tools::ToolError) -> CallToolResult {
    CallToolResult::error(vec![Content::text(err.to_string())])
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;
    use crate::FabroMcpServerSettings;

    #[test]
    fn server_info_reports_fabro_version() {
        let settings = FabroMcpServerSettings {
            cwd:            PathBuf::from("."),
            config_path:    PathBuf::from("fabro.toml"),
            client_factory: Arc::new(|| {
                Box::pin(async { panic!("client should not be constructed while reading info") })
            }),
        };
        let info = FabroMcpServer::new(Arc::new(settings)).get_info();

        assert_eq!(info.server_info.name, "fabro");
        assert_eq!(info.server_info.title.as_deref(), Some("Fabro"));
        assert_eq!(info.server_info.version, FABRO_VERSION);
    }

    #[test]
    fn fabro_run_pair_tool_is_registered_with_stage_based_schema() {
        let settings = FabroMcpServerSettings {
            cwd:            PathBuf::from("."),
            config_path:    PathBuf::from("fabro.toml"),
            client_factory: Arc::new(|| {
                Box::pin(async { panic!("client should not be constructed while listing tools") })
            }),
        };
        let server = FabroMcpServer::new(Arc::new(settings));
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "fabro_run_pair")
            .expect("fabro_run_pair should be registered");
        let schema = Value::Object(tool.input_schema.as_ref().clone());
        let schema_text = schema.to_string();

        assert!(schema_text.contains("stage_id"));
        assert!(!schema_text.contains("agent_session_id"));
        assert!(!schema_text.contains("session_id"));
        assert!(!schema_text.contains("PairTargetSelector"));
        assert!(!schema_text.contains("\"target\""));
        assert!(!schema_text.contains("provider"));
        assert!(!schema_text.contains("\"model\""));
        assert!(!schema_text.contains("\"node_id\""));
        assert!(!schema_text.contains("\"visit\""));
    }

    #[test]
    fn fabro_run_create_tool_advertises_complete_workflow_source_and_target_grammar() {
        let settings = FabroMcpServerSettings {
            cwd:            PathBuf::from("."),
            config_path:    PathBuf::from("fabro.toml"),
            client_factory: Arc::new(|| {
                Box::pin(async { panic!("client should not be constructed while listing tools") })
            }),
        };
        let server = FabroMcpServer::new(Arc::new(settings));
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "fabro_run_create")
            .expect("fabro_run_create should be registered");
        let schema = Value::Object(tool.input_schema.as_ref().clone());
        let variants = schema
            .pointer("/properties/runs/items/anyOf")
            .and_then(Value::as_array)
            .expect("runs items should advertise string and object variants");

        assert!(
            variants.iter().any(|variant| variant["type"] == "string"),
            "runs items should include workflow string shorthand: {schema}"
        );
        let object_variant = variants
            .iter()
            .find(|variant| variant["type"] == "object")
            .unwrap_or_else(|| {
                panic!("runs items should include object create spec variant: {schema}")
            });
        assert_eq!(object_variant["additionalProperties"], false);
        assert!(
            object_variant.pointer("/properties/workflow").is_some(),
            "object create spec should expose workflow property: {schema}"
        );
        assert!(
            object_variant.pointer("/properties/goal_file").is_some(),
            "object create spec should expose goal_file property: {schema}"
        );
        assert!(
            object_variant
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| required.iter().any(|name| name == "workflow")),
            "object create spec should require workflow: {schema}"
        );
        let workflow_variants = object_variant
            .pointer("/properties/workflow/anyOf")
            .and_then(Value::as_array)
            .expect("workflow should advertise selector, inline, and stored sources");
        assert!(
            workflow_variants
                .iter()
                .any(|variant| variant["type"] == "string")
        );
        for kind in ["inline", "stored"] {
            assert!(workflow_variants.iter().any(|variant| {
                variant.pointer("/properties/kind/const") == Some(&Value::from(kind))
            }));
        }
        let target_variants = object_variant
            .pointer("/properties/target/anyOf")
            .and_then(Value::as_array)
            .expect("target should advertise the canonical target union");
        for kind in ["git", "none", "folder"] {
            assert!(target_variants.iter().any(|variant| {
                variant.pointer("/properties/kind/const") == Some(&Value::from(kind))
            }));
        }
    }
}
