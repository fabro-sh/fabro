//! `fabro exec`: one agentic coding session in the current directory.
//!
//! The session is pebble's coding agent over a local sandbox, run through
//! pebble's own command-line session: its event renderer, closing summary,
//! and terminal approval prompt. `--verbose` asks that renderer for each
//! tool call's arguments and result in full and for the transcript, and adds
//! fabro's own dump of every model request and response; without it the
//! renderer prints what it always has. What is fabro's here is the client
//! (model calls go either straight to the provider with the CLI's credentials
//! or through a Fabro server's completions endpoint when a server target is
//! set), the sandbox, the MCP servers, skills, search, and redaction.

use std::collections::HashMap;
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result as AnyResult};
use async_trait::async_trait;
use fabro_llm::credentials::CredentialProvider;
use fabro_llm::gateway::{GatewayAdapter, GatewayError, GatewayTransport};
use fabro_llm::lithos_catalog::{Catalog, CatalogProvider};
use fabro_llm::middleware::{Call, Middleware, Next, Output};
use fabro_llm::{Client, ClientOptions, Error as LlmError, ErrorKind};
use fabro_sandbox::{RunSandbox, SecretRedactor, local_sandbox};
use fabro_static::EnvVars;
use fabro_types::settings::cli::OutputFormat as SettingsOutputFormat;
use fabro_types::settings::run::{McpServerSettings, ResolvedMcpEntry};
use fabro_util::exit::{self, ErrorExt, ExitClass};
use fabro_util::home::Home;
use fabro_util::terminal::Styles;
use fabro_workflow::web_search::{self, SearchSecrets};
use lithos_llm::catalog::ProviderId;
use pebble_cli_core::approval::TerminalApproval;
use pebble_cli_core::render::{self, JsonStream, RenderOptions, Style};
use pebble_cli_core::session::{SessionOptions, run_prompt_with};
use pebble_coding_agent::environment::Environment;
use pebble_coding_agent::subagents::SubagentOptions;
use pebble_coding_agent::tools::{PermissionLevelPolicy, PermissionMiddleware};
use pebble_coding_agent::{CodingAgent, CodingAgentOptions, MemoryDiscovery, SkillDiscovery};
use tokio::signal;
use tokio_util::sync::CancellationToken;

use crate::args::{AgentArgs, ExecArgs, ExecOutputFormat};
use crate::command_context::CommandContext;
use crate::mcp_servers::pebble_servers;
#[cfg(feature = "sleep_inhibitor")]
use crate::sleep_inhibitor;
use crate::{server_client, user_config};

/// Posts completions to a Fabro server through the authenticated CLI client.
struct ServerCompletionTransport {
    client:   server_client::Client,
    base_url: String,
}

impl ServerCompletionTransport {
    fn new(client: server_client::Client) -> Self {
        let base_url = client.base_url();
        Self { client, base_url }
    }
}

#[async_trait]
impl GatewayTransport for ServerCompletionTransport {
    async fn post_completion(
        &self,
        body: serde_json::Value,
    ) -> Result<fabro_http::Response, GatewayError> {
        let url = format!("{}/api/v1/completions", self.base_url);
        let response = self
            .client
            .send_http_response(|http_client| {
                let body = body.clone();
                let url = url.clone();
                async move { http_client.post(url).json(&body).send().await }
            })
            .await
            .map_err(|err| GatewayError::Transport {
                auth:    exit::exit_class_for(&err) == Some(ExitClass::AuthRequired),
                message: err.to_string(),
            })?;
        response.map_err(|failure| GatewayError::Status {
            status:  failure.status.as_u16(),
            headers: failure.headers,
            body:    failure.body,
        })
    }
}

/// How a failed session is reported: a model failure by what the provider
/// said, everything else by the agent's own description.
#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error("LLM error: {0}")]
    Llm(fabro_llm::ErrorData),
    #[error(transparent)]
    Agent(pebble_coding_agent::Error),
}

impl From<pebble_coding_agent::Error> for SessionError {
    fn from(error: pebble_coding_agent::Error) -> Self {
        match error.llm_source() {
            Some(llm) => Self::Llm(llm.data()),
            None => Self::Agent(error),
        }
    }
}

fn classify_server_agent_auth(err: anyhow::Error) -> anyhow::Error {
    let is_auth = err.chain().any(|cause| {
        cause
            .downcast_ref::<SessionError>()
            .is_some_and(|error| {
                matches!(error, SessionError::Llm(data) if data.kind() == ErrorKind::Authentication)
            })
    });
    if is_auth {
        err.classify(ExitClass::AuthRequired)
    } else {
        err
    }
}

fn run_mcp_servers_for_exec(
    mcps: &HashMap<String, ResolvedMcpEntry>,
) -> AnyResult<Vec<McpServerSettings>> {
    mcps.iter()
        .map(|(key, entry)| match entry {
            ResolvedMcpEntry::Resolved(server) => Ok(server.clone()),
            ResolvedMcpEntry::Reference(reference) => {
                anyhow::bail!(
                    "fabro exec cannot resolve run.agent.mcps.{key} catalog reference \
                     (id `{}`); define an inline server under [cli.exec.agent.mcps.{key}] or \
                     remove the run-level reference",
                    reference.id
                );
            }
        })
        .collect()
}

pub(crate) async fn execute(mut args: ExecArgs, ctx: &CommandContext) -> AnyResult<()> {
    let cli = &ctx.user_settings().cli;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = sleep_inhibitor::guard(cli.exec.prevent_idle_sleep);
    let provider_str = cli.exec.model.provider.as_deref();
    let model_str = cli.exec.model.name.as_deref();
    let permissions = cli.exec.agent.permissions;
    let output_format = Some(match cli.output.format {
        SettingsOutputFormat::Text => ExecOutputFormat::Text,
        SettingsOutputFormat::Json => ExecOutputFormat::Json,
    });
    args.agent
        .apply_cli_defaults(provider_str, model_str, permissions, output_format);
    let server_target = user_config::exec_server_target(&args.server)?;
    // v2 MCPs live under `cli.exec.agent.mcps` (owner-specific) or
    // `run.agent.mcps`. For `fabro exec` we use the cli.exec path, falling
    // back to run.agent.mcps if unset.
    let mcp_servers: Vec<McpServerSettings> = match cli.exec.agent.mcps.as_ref() {
        Some(mcps) => mcps.values().cloned().collect(),
        None => ctx
            .run_settings()
            .ok()
            .map(|settings| run_mcp_servers_for_exec(&settings.agent.mcps))
            .transpose()?
            .unwrap_or_default(),
    };
    // Fully validate MCP transport config at the exec boundary. `fabro exec`
    // has no server vault, so secret and unsupported tokens fail instead of
    // reaching the transport.
    let mcp_servers = mcp_servers
        .into_iter()
        .map(|settings| {
            settings
                .resolve_transport_secrets(|_| None)
                .with_context(|| format!("failed to resolve MCP server {:?}", settings.name))
        })
        .collect::<AnyResult<Vec<_>>>()?;
    // Resolve color support once, leak to get 'static lifetime for use across
    // threads.
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));
    if let Some(target) = server_target {
        tracing::info!(transport = "server", "Agent session starting");
        let provider_name = args
            .agent
            .provider
            .clone()
            .unwrap_or_else(|| "anthropic".to_string());
        let catalog = ctx.catalog()?;
        let provider_id = catalog.enabled_provider(&provider_name).map_or_else(
            || ProviderId::new(provider_name.as_str()),
            |provider| provider.id().clone(),
        );
        let server_client = server_client::connect_server_target(&target).await?;
        let adapter = Arc::new(GatewayAdapter::new(Box::new(
            ServerCompletionTransport::new(server_client),
        )));
        // The server inlines attachments and is the billing authority, so the
        // local client only routes and reports diagnostics.
        let mut options = cli_client_options(&args.agent, styles);
        options.inline_attachments = false;
        let client = fabro_llm::build_offline_client(
            Catalog::clone(&catalog),
            options.with_adapter(provider_id, adapter),
        )
        .context("Failed to register fabro server adapter")?
        .client;
        run_session(args.agent, client, mcp_servers, catalog, styles)
            .await
            .map_err(classify_server_agent_auth)?;
    } else {
        tracing::info!(transport = "direct", "Agent session starting");
        let llm_source = ctx.llm_source().await?;
        let catalog = ctx.catalog()?;
        let client = build_direct_client(&args.agent, llm_source, &catalog, styles).await?;
        run_session(args.agent, client, mcp_servers, catalog, styles).await?;
    }

    Ok(())
}

#[allow(
    clippy::print_stderr,
    reason = "Provider build issues are diagnostics for the person running the CLI."
)]
async fn build_direct_client(
    args: &AgentArgs,
    llm_source: Arc<dyn CredentialProvider>,
    catalog: &Arc<Catalog>,
    styles: &'static Styles,
) -> AnyResult<Client> {
    let built = fabro_llm::build_client(
        Catalog::clone(catalog),
        llm_source,
        cli_client_options(args, styles),
    )
    .await
    .context("Failed to create LLM client")?;
    for issue in &built.build_issues {
        eprintln!(
            "{}",
            styles.dim.apply_to(format!(
                "[llm] provider '{}' is unavailable: {}",
                issue.provider, issue.cause
            ))
        );
    }
    Ok(built.client)
}

/// Client options for the session: standard retries plus the requested
/// diagnostic middleware.
fn cli_client_options(args: &AgentArgs, styles: &'static Styles) -> ClientOptions {
    let options = ClientOptions::standard();
    if args.verbose {
        options.with_middleware(Arc::new(VerboseMiddleware { styles }))
    } else if args.debug {
        options.with_middleware(Arc::new(DebugMiddleware { styles }))
    } else {
        options
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "fabro exec passes search process-env credentials into the agent's search tool."
)]
fn cli_search_secrets() -> SearchSecrets {
    SearchSecrets {
        brave_search_api_key: std::env::var(EnvVars::BRAVE_SEARCH_API_KEY).ok(),
        venice_api_key:       std::env::var(EnvVars::VENICE_API_KEY).ok(),
    }
}

/// The provider the session runs on: the `--provider` flag, else the
/// highest-priority available provider offering `--model`, else the default.
fn resolve_provider_id(
    catalog: &Catalog,
    args: &AgentArgs,
    available: &std::collections::HashSet<ProviderId>,
) -> ProviderId {
    let requested = ProviderId::new(args.provider.as_deref().unwrap_or("anthropic"));
    if args.provider.is_some() {
        return canonical_provider_id(catalog, &requested);
    }
    if let Some(model_id) = args.model.as_deref() {
        let matches = catalog.offerings_matching(model_id);
        if let Some(entry) = matches
            .iter()
            .find(|entry| available.contains(entry.provider.id()))
            .or_else(|| matches.first())
        {
            return entry.provider.id().clone();
        }
    }
    canonical_provider_id(catalog, &requested)
}

/// The catalog id for `requested`, resolving aliases; the request itself when
/// the catalog does not know it, so the error names what the caller typed.
fn canonical_provider_id(catalog: &Catalog, requested: &ProviderId) -> ProviderId {
    catalog
        .enabled_provider(requested.as_str())
        .map_or_else(|| requested.clone(), |provider| provider.id().clone())
}

/// The model that summarizes fetched web pages: the provider's small default,
/// else its default model, else the session's own model.
fn summarizer_model(catalog: &Catalog, provider_id: &ProviderId, selected_model: &str) -> String {
    let model = catalog
        .small_default_for([provider_id])
        .filter(|entry| entry.provider.id() == provider_id)
        .or_else(|| {
            catalog
                .enabled_provider(provider_id.as_str())?
                .default_offering()
        })
        .map_or_else(
            || selected_model.to_string(),
            |entry| entry.model.id().to_string(),
        );
    format!("{provider_id}/{model}")
}

/// Middleware that logs LLM request/response summaries to stderr.
struct DebugMiddleware {
    styles: &'static Styles,
}

#[async_trait]
impl Middleware for DebugMiddleware {
    #[allow(
        clippy::print_stderr,
        reason = "Debug middleware logs request and response summaries to stderr."
    )]
    async fn handle(&self, call: Call, next: Next) -> Result<Output, LlmError> {
        let s = self.styles;
        eprintln!(
            "{}",
            s.dim.apply_to(format!(
                "[debug] request: model={} messages={} tools={}",
                call.route().handle(),
                call.request().messages().len(),
                call.request().tools().len(),
            )),
        );
        let output = next.run(call).await?;
        if let Output::Complete(response) = &output {
            eprintln!(
                "{}",
                s.dim.apply_to(format!(
                    "[debug] response: model={} finish={:?} usage=({}/{}/{})",
                    response.model,
                    response.finish_reason,
                    response.usage.input,
                    response.usage.output,
                    response.usage.total(),
                )),
            );
        }
        Ok(output)
    }
}

/// Middleware that logs full LLM request/response JSON to stderr.
struct VerboseMiddleware {
    styles: &'static Styles,
}

#[async_trait]
impl Middleware for VerboseMiddleware {
    #[allow(
        clippy::print_stderr,
        reason = "Verbose middleware dumps full request and response JSON to stderr."
    )]
    async fn handle(&self, call: Call, next: Next) -> Result<Output, LlmError> {
        let s = self.styles;
        eprintln!(
            "{}\n{}",
            s.dim.apply_to("[verbose] request:"),
            serde_json::to_string_pretty(call.request())
                .unwrap_or_else(|e| format!("<serialize error: {e}>"))
        );
        let output = next.run(call).await?;
        if let Output::Complete(response) = &output {
            eprintln!(
                "{}\n{}",
                s.dim.apply_to("[verbose] response:"),
                serde_json::to_string_pretty(response)
                    .unwrap_or_else(|e| format!("<serialize error: {e}>"))
            );
        }
        Ok(output)
    }
}

#[allow(
    clippy::print_stderr,
    reason = "The model line is a diagnostic for the person running the CLI."
)]
async fn run_session(
    args: AgentArgs,
    client: Client,
    mcp_servers: Vec<McpServerSettings>,
    catalog: Arc<Catalog>,
    styles: &'static Styles,
) -> AnyResult<()> {
    let available: std::collections::HashSet<ProviderId> =
        client.available_providers().iter().cloned().collect();
    let provider_id = resolve_provider_id(&catalog, &args, &available);
    if !available.contains(&provider_id) {
        anyhow::bail!("LLM credentials not configured for provider '{provider_id}'");
    }
    let model = if let Some(model) = args.model.clone() {
        model
    } else {
        catalog
            .enabled_provider(provider_id.as_str())
            .and_then(CatalogProvider::default_offering)
            .map(|entry| entry.model.id().to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "provider '{provider_id}' has no default model in the catalog; pass --model explicitly"
                )
            })?
    };
    eprintln!("{}", styles.dim.apply_to(format!("Using model: {model}")));

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sandbox: Arc<RunSandbox> = Arc::new(
        local_sandbox(cwd)
            .await
            .context("failed to create the local sandbox")?,
    );

    let permissions = args.permission_level();
    #[expect(
        clippy::disallowed_methods,
        reason = "is_terminal() on stdin is a non-blocking fstat; no actual I/O performed"
    )]
    let is_interactive = std::io::stdin().is_terminal() && !args.auto_approve;
    let permission_middleware =
        PermissionMiddleware::new(Arc::new(PermissionLevelPolicy::new(permissions)))
            .with_approval(Arc::new(TerminalApproval::new(permissions, is_interactive)));

    // The profile's own instruction files from the repository root down, and
    // fabro's skill directories: pebble knows the files and does the walk.
    let mut options = CodingAgentOptions::default()
        .with_memory_discovery(MemoryDiscovery::from_git_root())
        .with_recorded_permission_level(permissions);
    options = match &args.skills_dir {
        Some(skills_dir) => options.with_skill_dirs([skills_dir.clone()]),
        None => options.with_skill_discovery(
            SkillDiscovery::new()
                .search(Home::from_env().skills_dir().to_string_lossy().into_owned())
                .search_under_git_root(".fabro/skills")
                .search_under_git_root("skills"),
        ),
    };

    let environment: Arc<dyn Environment> = Arc::clone(&sandbox) as Arc<dyn Environment>;
    let mut builder = CodingAgent::builder(client, environment)
        .model(format!("{provider_id}/{model}"))
        .options(options)
        .tool_middleware(Arc::new(permission_middleware))
        .redactor(Arc::new(SecretRedactor))
        .web_fetch_summarizer(summarizer_model(&catalog, &provider_id, &model))
        .mcp_servers(pebble_servers(&mcp_servers))
        .subagents(SubagentOptions::enabled());
    if let Some(routes) = sandbox.port_routes() {
        builder = builder.port_routes(routes);
    }
    if let Some(search) = web_search::search_provider(&cli_search_secrets()) {
        builder = builder.search_provider(search);
    }
    let agent = builder
        .build()
        .await
        .context("failed to start the agent session")?;

    // Text puts progress on stderr and the answer on stdout; JSON puts the
    // event stream itself on stdout, as scripts that read it expect.
    let session = match args.output_format.unwrap_or(ExecOutputFormat::Text) {
        ExecOutputFormat::Text => SessionOptions::default(),
        ExecOutputFormat::Json => SessionOptions {
            style:        Style::Json,
            json_to:      JsonStream::Stdout,
            write_answer: false,
        },
    };
    render::report_mcp_servers(&agent, session.style);

    // SIGINT ends the prompt; the session shuts down as cancelled.
    let cancel_token = CancellationToken::new();
    let sigint_token = cancel_token.clone();
    tokio::spawn(async move {
        signal::ctrl_c().await.ok();
        sigint_token.cancel();
    });

    // `--verbose` asks the renderer for everything it can say: each tool
    // call's arguments and result in full, and the transcript. The answer on
    // stdout is the session's and is not changed by it.
    let render = if args.verbose {
        RenderOptions::verbose()
    } else {
        RenderOptions::default()
    };
    let report =
        run_prompt_with(agent, args.prompt.as_str(), &cancel_token, session, render).await?;
    report
        .result
        .map(|_| ())
        .map_err(|error| anyhow::Error::new(SessionError::from(error)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_llm::test_support::{test_catalog, test_catalog_with_overlay};
    use fabro_types::settings::run::{McpServerRef, McpServerSettings, ResolvedMcpEntry};
    use lithos_llm::catalog::builtin;

    use super::{AgentArgs, resolve_provider_id, run_mcp_servers_for_exec, summarizer_model};
    use crate::args::{ExecOutputFormat, PermissionsArg};

    fn args(provider: Option<&str>, model: Option<&str>) -> AgentArgs {
        AgentArgs {
            prompt:        "task".to_string(),
            provider:      provider.map(str::to_string),
            model:         model.map(str::to_string),
            permissions:   Some(PermissionsArg::Full),
            auto_approve:  true,
            debug:         false,
            verbose:       false,
            skills_dir:    None,
            output_format: Some(ExecOutputFormat::Text),
        }
    }

    #[test]
    fn run_mcp_servers_for_exec_rejects_catalog_references() {
        let err = run_mcp_servers_for_exec(&HashMap::from([(
            "sentry".to_string(),
            ResolvedMcpEntry::Reference(McpServerRef {
                id:      "catalog/sentry".to_string(),
                enabled: None,
            }),
        )]))
        .expect_err("fabro exec should reject unresolved run-level MCP references");

        assert!(
            err.to_string()
                .contains("fabro exec cannot resolve run.agent.mcps.sentry catalog reference"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn run_mcp_servers_for_exec_keeps_resolved_servers() {
        let servers = run_mcp_servers_for_exec(&HashMap::from([(
            "inline".to_string(),
            ResolvedMcpEntry::Resolved(McpServerSettings {
                name: "inline".to_string(),
                ..McpServerSettings::default()
            }),
        )]))
        .expect("resolved inline server should be usable by fabro exec");

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "inline");
    }

    #[test]
    fn explicit_provider_wins_over_model_matching() {
        let catalog = test_catalog_with_overlay("[providers.openrouter]\nenabled = true\n");
        let available = [builtin::openai()].into_iter().collect();

        let provider = resolve_provider_id(&catalog, &args(Some("openrouter"), None), &available);

        assert_eq!(provider.as_str(), "openrouter");
    }

    #[test]
    fn a_bare_model_picks_an_available_provider_offering_it() {
        let catalog = test_catalog();
        let available = [builtin::openai()].into_iter().collect();

        let provider = resolve_provider_id(&catalog, &args(None, Some("gpt-5.4")), &available);

        assert_eq!(provider, builtin::openai());
    }

    #[test]
    fn summarizer_uses_the_providers_small_default() {
        let catalog = test_catalog();

        let selector = summarizer_model(&catalog, &builtin::anthropic(), "claude-opus-4-6");

        assert!(selector.starts_with("anthropic/"), "{selector}");
        assert_ne!(selector, "anthropic/claude-opus-4-6");
    }
}
