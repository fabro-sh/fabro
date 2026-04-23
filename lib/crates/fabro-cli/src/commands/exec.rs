use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result as AnyResult;
use fabro_agent::cli::{OutputFormat, run_with_args, run_with_args_and_client};
use fabro_llm::client::Client;
use fabro_llm::error::{
    Error as LlmError, ProviderErrorDetail, ProviderErrorKind, error_from_status_code,
};
use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
use fabro_llm::providers::common::{LineReader, parse_retry_after};
use fabro_llm::types::{
    FinishReason, Message, Request, Response as LlmResponse, StreamEvent, TokenCounts,
};
use fabro_mcp::config::McpServerSettings;
use fabro_types::settings::InterpString;
use fabro_types::settings::cli::OutputFormat as SettingsOutputFormat;
use fabro_util::exit::{ErrorExt, ExitClass};
use futures::stream;
use serde::Deserialize;

use crate::args::ExecArgs;
use crate::command_context::CommandContext;
use crate::{server_client, user_config};

struct AuthenticatedFabroServerAdapter {
    client:        server_client::Client,
    base_url:      String,
    provider_name: String,
}

impl AuthenticatedFabroServerAdapter {
    fn new(client: server_client::Client, provider_name: impl Into<String>) -> Self {
        let base_url = client.base_url().clone();
        Self {
            client,
            base_url,
            provider_name: provider_name.into(),
        }
    }
}

#[derive(Deserialize)]
struct ServerCompletionResponse {
    id:          String,
    model:       String,
    message:     Message,
    stop_reason: String,
    usage:       ServerUsage,
}

#[derive(Deserialize)]
struct ServerUsage {
    input_tokens:  i64,
    output_tokens: i64,
}

fn map_stop_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop" => FinishReason::Stop,
        "max_tokens" | "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        other => FinishReason::Other(other.to_string()),
    }
}

fn build_body(request: &Request, stream: bool) -> std::result::Result<serde_json::Value, LlmError> {
    let mut body = serde_json::to_value(request).map_err(|err| {
        LlmError::configuration_error(format!("failed to serialize request: {err}"), err)
    })?;
    body["stream"] = serde_json::Value::Bool(stream);
    Ok(body)
}

fn parse_server_error_body(body: &str) -> (String, Option<String>, Option<serde_json::Value>) {
    serde_json::from_str::<serde_json::Value>(body).map_or_else(
        |_| (body.to_string(), None, None),
        |value| {
            let first = value
                .get("errors")
                .and_then(serde_json::Value::as_array)
                .and_then(|errors| errors.first());
            let detail = first
                .and_then(|entry| entry.get("detail"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("detail").and_then(serde_json::Value::as_str))
                .or_else(|| {
                    value
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("Unknown error")
                .to_string();
            let code = first
                .and_then(|entry| entry.get("code"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    value
                        .get("error")
                        .and_then(|error| error.get("type"))
                        .and_then(serde_json::Value::as_str)
                })
                .map(ToOwned::to_owned);
            (detail, code, Some(value))
        },
    )
}

fn transport_error(provider: &str, err: &anyhow::Error) -> LlmError {
    let message = err.to_string();
    if message.contains("fabro auth login") || message.contains("Authentication required.") {
        return LlmError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                message,
                provider: provider.to_string(),
                status_code: Some(401),
                error_code: None,
                retry_after: None,
                raw: None,
            }),
        };
    }
    LlmError::Configuration {
        message,
        source: None,
    }
}

fn classify_server_agent_auth(err: anyhow::Error) -> anyhow::Error {
    let is_auth = err.chain().any(|cause| {
        cause
            .downcast_ref::<fabro_agent::Error>()
            .is_some_and(|error| {
                matches!(
                    error,
                    fabro_agent::Error::Llm(llm)
                        if llm.provider_kind() == Some(ProviderErrorKind::Authentication)
                )
            })
    });
    if is_auth {
        err.classify(ExitClass::AuthRequired)
    } else {
        err
    }
}

fn map_response_failure(provider: &str, failure: &fabro_client::ApiError) -> LlmError {
    let retry_after = parse_retry_after(&failure.headers);
    let (message, code, raw) = parse_server_error_body(&failure.body);
    error_from_status_code(
        failure.status.as_u16(),
        message,
        provider.to_string(),
        code,
        raw,
        retry_after,
    )
}

fn parse_sse_block(block: &str) -> Option<(String, String)> {
    let mut event_type = None;
    let mut data_lines = Vec::new();

    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim());
        }
    }

    let event_type = event_type?;
    if data_lines.is_empty() {
        return None;
    }
    Some((event_type, data_lines.join("\n")))
}

#[async_trait::async_trait]
impl ProviderAdapter for AuthenticatedFabroServerAdapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, request: &Request) -> std::result::Result<LlmResponse, LlmError> {
        let url = format!("{}/api/v1/completions", self.base_url);
        let body = build_body(request, false)?;
        let response = self
            .client
            .send_http_response(|http_client| {
                let body = body.clone();
                let url = url.clone();
                async move { http_client.post(url).json(&body).send().await }
            })
            .await
            .map_err(|err| transport_error(&self.provider_name, &err))?;
        let response =
            response.map_err(|failure| map_response_failure(&self.provider_name, &failure))?;
        let response_body = response
            .text()
            .await
            .map_err(|err| LlmError::network(err.to_string(), err))?;
        let server_response: ServerCompletionResponse = serde_json::from_str(&response_body)
            .map_err(|err| {
                LlmError::stream_error(format!("failed to parse completion response: {err}"), err)
            })?;

        Ok(LlmResponse {
            id:            server_response.id,
            model:         server_response.model,
            provider:      self.provider_name.clone(),
            message:       server_response.message,
            finish_reason: map_stop_reason(&server_response.stop_reason),
            usage:         TokenCounts {
                input_tokens: server_response.usage.input_tokens,
                output_tokens: server_response.usage.output_tokens,
                ..Default::default()
            },
            raw:           None,
            warnings:      vec![],
            rate_limit:    None,
        })
    }

    async fn stream(&self, request: &Request) -> std::result::Result<StreamEventStream, LlmError> {
        let url = format!("{}/api/v1/completions", self.base_url);
        let body = build_body(request, true)?;
        let response = self
            .client
            .send_http_response(|http_client| {
                let body = body.clone();
                let url = url.clone();
                async move { http_client.post(url).json(&body).send().await }
            })
            .await
            .map_err(|err| transport_error(&self.provider_name, &err))?;
        let response =
            response.map_err(|failure| map_response_failure(&self.provider_name, &failure))?;

        let stream = stream::unfold(LineReader::new(response, None), |mut reader| async move {
            loop {
                match reader.read_next_chunk("\n\n").await {
                    Ok(Some(block)) => {
                        if let Some((event_type, data)) = parse_sse_block(&block) {
                            if event_type == "stream_event" {
                                match serde_json::from_str::<StreamEvent>(&data) {
                                    Ok(event) => return Some((Ok(event), reader)),
                                    Err(err) => {
                                        return Some((
                                            Err(LlmError::stream_error(
                                                format!("failed to parse stream event: {err}"),
                                                err,
                                            )),
                                            reader,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => return None,
                    Err(err) => return Some((Err(err), reader)),
                }
            }
        });

        Ok(Box::pin(stream))
    }
}

pub(crate) async fn execute(mut args: ExecArgs, ctx: &CommandContext) -> AnyResult<()> {
    use fabro_agent::cli::PermissionLevel as AgentPermissionLevel;
    use fabro_types::settings::run::AgentPermissions;

    let cli = &ctx.user_settings().cli;
    #[cfg(feature = "sleep_inhibitor")]
    let _sleep_guard = crate::sleep_inhibitor::guard(cli.exec.prevent_idle_sleep);
    let provider_str = cli
        .exec
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source);
    let model_str = cli.exec.model.name.as_ref().map(InterpString::as_source);
    let permissions = cli.exec.agent.permissions.map(|p| match p {
        AgentPermissions::ReadOnly => AgentPermissionLevel::ReadOnly,
        AgentPermissions::ReadWrite => AgentPermissionLevel::ReadWrite,
        AgentPermissions::Full => AgentPermissionLevel::Full,
    });
    let output_format = Some(match cli.output.format {
        SettingsOutputFormat::Text => OutputFormat::Text,
        SettingsOutputFormat::Json => OutputFormat::Json,
    });
    args.agent.apply_cli_defaults(
        provider_str.as_deref(),
        model_str.as_deref(),
        permissions,
        output_format,
    );
    let server_target = user_config::exec_server_target(&args.server)?;
    // v2 MCPs live under `cli.exec.agent.mcps` (owner-specific) or
    // `run.agent.mcps`. For `fabro exec` we use the cli.exec path, falling
    // back to run.agent.mcps if unset.
    let mcp_servers: Vec<McpServerSettings> = if !cli.exec.agent.mcps.is_empty() {
        cli.exec.agent.mcps.values().cloned().collect()
    } else {
        ctx.run_settings()
            .map(|settings| settings.agent.mcps.values().cloned().collect())
            .unwrap_or_default()
    };
    if let Some(target) = server_target {
        tracing::info!(transport = "server", "Agent session starting");
        let provider_name = args
            .agent
            .provider
            .clone()
            .unwrap_or_else(|| "anthropic".to_string());
        let server_client = server_client::connect_server_target(&target).await?;
        let adapter = Arc::new(AuthenticatedFabroServerAdapter::new(
            server_client,
            &provider_name,
        ));
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(adapter)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to register fabro server adapter: {e}"))?;
        run_with_args_and_client(args.agent, Some(client), mcp_servers)
            .await
            .map_err(classify_server_agent_auth)?;
    } else {
        tracing::info!(transport = "direct", "Agent session starting");
        run_with_args(args.agent, mcp_servers).await?;
    }

    Ok(())
}
