use std::collections::HashMap;
use std::sync::Arc;

use fabro_agent::{Sandbox, ToolEnvProvider};
use fabro_auth::{CliAgentKind, CredentialResolver, CredentialUsage, ResolvedCredential};
use fabro_model::Provider;
use tokio_util::sync::CancellationToken;

use super::cli::{AgentCli, process_env_var};
use crate::error::Error;
use crate::event::{Emitter, RunNoticeCode, RunNoticeLevel};

pub(crate) struct AgentLaunchEnvRequest<'a> {
    pub provider: Provider,
    pub cli: AgentCli,
    pub resolver: Option<&'a CredentialResolver>,
    pub tool_env: Option<&'a Arc<dyn ToolEnvProvider>>,
    pub github_token_refresh_managed: bool,
    pub stage_label: &'static str,
    pub emitter: &'a Arc<Emitter>,
    pub sandbox: &'a Arc<dyn Sandbox>,
    pub cancel_token: &'a CancellationToken,
}

pub(crate) async fn resolve_agent_launch_env(
    request: AgentLaunchEnvRequest<'_>,
) -> Result<HashMap<String, String>, Error> {
    let cli_agent = match request.cli {
        AgentCli::Claude => CliAgentKind::Claude,
        AgentCli::Codex => CliAgentKind::Codex,
        AgentCli::Gemini => CliAgentKind::Gemini,
    };

    let mut launch_env = if let Some(resolver) = request.resolver {
        let resolved = resolver
            .resolve(request.provider, CredentialUsage::CliAgent(cli_agent))
            .await
            .map_err(|err| {
                Error::handler_with_source(
                    format!("Failed to resolve {} credential", request.stage_label),
                    &err,
                )
            })?;
        let ResolvedCredential::Cli(cli_credential) = resolved else {
            return Err(Error::handler("Expected CLI credential".to_string()));
        };
        if let Some(login_cmd) = &cli_credential.login_command {
            let login_result = request
                .sandbox
                .exec_command(
                    login_cmd,
                    30_000,
                    None,
                    None,
                    Some(request.cancel_token.child_token()),
                )
                .await
                .map_err(|err| {
                    Error::handler_with_source(
                        format!("{} credential login failed", request.stage_label),
                        &err,
                    )
                })?;
            if !login_result.is_success() {
                tracing::warn!(
                    exit_code = login_result.display_exit_code(),
                    stage = request.stage_label,
                    "{} credential login failed: {}",
                    request.stage_label,
                    login_result.stderr
                );
            }
        }
        cli_credential.env_vars
    } else {
        let mut env = HashMap::new();
        for name in request.provider.api_key_env_vars() {
            if let Some(value) = process_env_var(name) {
                env.insert((*name).to_string(), value);
            }
        }
        env
    };

    if let Some(provider) = request.tool_env {
        if request.github_token_refresh_managed {
            request.emitter.notice(
                RunNoticeLevel::Info,
                RunNoticeCode::GithubTokenRefreshLimited,
                format!(
                    "{} agent stages receive GitHub tokens at process launch; stages running \
                     beyond token expiry may need to be retried.",
                    request.stage_label
                ),
            );
        }
        let tool_env = provider.resolve().await.map_err(|err| {
            Error::handler_with_anyhow(
                format!("Failed to resolve {} agent env", request.stage_label),
                &err,
            )
        })?;
        launch_env.extend(tool_env);
    }

    Ok(launch_env)
}
