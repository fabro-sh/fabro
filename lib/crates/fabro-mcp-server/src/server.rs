use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fabro_client::Client;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, serve_server, tool, tool_handler, tool_router};
use tokio::sync::OnceCell;

use crate::{FabroMcpServerSettings, run_tools};

#[derive(Clone)]
pub(crate) struct FabroMcpServer {
    settings:    Arc<FabroMcpServerSettings>,
    client:      Arc<OnceCell<Arc<Client>>>,
    cwd:         PathBuf,
    tool_router: ToolRouter<Self>,
}

pub async fn start(settings: FabroMcpServerSettings) -> Result<()> {
    let server = FabroMcpServer::new(Arc::new(settings));
    let service = serve_server(server, stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FabroMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Use these tools to create, inspect, control, wait for, and read events from Fabro workflow runs.")
    }
}

#[tool_router(router = tool_router)]
impl FabroMcpServer {
    pub(crate) fn new(settings: Arc<FabroMcpServerSettings>) -> Self {
        let cwd = settings.cwd.clone();
        Self {
            settings,
            client: Arc::new(OnceCell::new()),
            cwd,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "fabro_run_create",
        description = "Create one or more Fabro workflow runs, starting them by default."
    )]
    async fn fabro_run_create(
        &self,
        params: Parameters<run_tools::FabroRunCreateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedCreateRuns::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        let client = match self.client().await {
            Ok(client) => client,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        match run_tools::create_runs(client, &self.cwd, &self.settings.config_path, params).await {
            Ok(result) => run_tools::success_result(&result, run_tools::create_runs_text(&result)),
            Err(err) => Ok(run_tools::error_result(err)),
        }
    }

    #[tool(
        name = "fabro_run_search",
        description = "Search Fabro workflow runs by id, workflow, labels, status, archival state, and creation time."
    )]
    async fn fabro_run_search(
        &self,
        params: Parameters<run_tools::FabroRunSearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedSearchRuns::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        let client = match self.client().await {
            Ok(client) => client,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        match run_tools::search_runs(client, params).await {
            Ok(result) => run_tools::success_result(&result, run_tools::search_runs_text(&result)),
            Err(err) => Ok(run_tools::error_result(err)),
        }
    }

    #[tool(
        name = "fabro_run_interact",
        description = "Get, start, message, cancel, archive, unarchive, inspect questions, or answer a Fabro run."
    )]
    async fn fabro_run_interact(
        &self,
        params: Parameters<run_tools::FabroRunInteractParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let params = match run_tools::ValidatedInteractRun::try_from(params.0) {
            Ok(params) => params,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        let client = match self.client().await {
            Ok(client) => client,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        match run_tools::interact_run(client, params).await {
            Ok(result) => run_tools::success_result(&result, run_tools::interact_run_text(&result)),
            Err(err) => Ok(run_tools::error_result(err)),
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
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        let client = match self.client().await {
            Ok(client) => client,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        match run_tools::gather_runs(client, params).await {
            Ok(result) => run_tools::success_result(&result, run_tools::gather_runs_text(&result)),
            Err(err) => Ok(run_tools::error_result(err)),
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
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        let client = match self.client().await {
            Ok(client) => client,
            Err(err) => return Ok(run_tools::error_result(err)),
        };
        match run_tools::run_events(client, params).await {
            Ok(result) => run_tools::success_result(&result, run_tools::run_events_text(&result)),
            Err(err) => Ok(run_tools::error_result(err)),
        }
    }

    async fn client(&self) -> Result<Arc<Client>, run_tools::ToolError> {
        self.client
            .get_or_try_init(|| async {
                (self.settings.client_factory)()
                    .await
                    .map(Arc::new)
                    .map_err(|err| run_tools::ToolError::from_anyhow(&err))
            })
            .await
            .map(Arc::clone)
    }
}
