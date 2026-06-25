use anyhow::{Context, Result};
use fabro_api::types;
use fabro_auth::{AuthContextRequest, AuthMethod, LoginResult, OPENAI_CODEX_VAULT_SECRET_NAME};
use fabro_model::ProviderId;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;

use crate::args::ProviderLoginArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::provider_auth;

pub(super) async fn login_command(
    args: ProviderLoginArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    base_ctx.require_no_json_override()?;
    let printer = base_ctx.printer();
    let s = Styles::detect_stderr();
    let ctx = base_ctx.with_target(&args.target)?;
    let server = ctx.server().await?;
    let (name, value, type_) = if args.api_key_stdin {
        let (name, key) = login_with_server_api_key(
            &server,
            args.provider,
            provider_auth::ApiKeySource::Stdin,
            &s,
            printer,
        )
        .await?;
        (name, key, types::SecretType::Token)
    } else {
        match provider_auth::pick_auth_method(&args.provider).await? {
            AuthMethod::ApiKey => {
                let (name, key) = login_with_server_api_key(
                    &server,
                    args.provider,
                    provider_auth::ApiKeySource::Prompt,
                    &s,
                    printer,
                )
                .await?;
                (name, key, types::SecretType::Token)
            }
            method @ AuthMethod::CodexDevice(_) => {
                let result = provider_auth::authenticate_provider_with_method(
                    args.provider,
                    method,
                    &s,
                    printer,
                )
                .await?;
                match result {
                    LoginResult::OAuth { credential, .. } => (
                        OPENAI_CODEX_VAULT_SECRET_NAME.to_string(),
                        serde_json::to_string(&credential)?,
                        types::SecretType::Oauth,
                    ),
                    LoginResult::ApiKey { .. } => {
                        unreachable!("Codex device authentication cannot produce an API key result")
                    }
                }
            }
        }
    };

    server
        .create_secret(types::CreateSecretRequest {
            name: name.clone(),
            value,
            type_,
            description: None,
        })
        .await?;
    fabro_util::printerr!(printer, "  {} Saved {}", s.green.apply_to("✔"), name);
    Ok(())
}

async fn login_with_server_api_key(
    server: &server_client::Client,
    requested_provider: ProviderId,
    source: provider_auth::ApiKeySource,
    s: &Styles,
    printer: Printer,
) -> Result<(String, String)> {
    let provider = server_provider(server, &requested_provider).await?;
    let secret_name = provider.expected_secret_name.clone().with_context(|| {
        format!(
            "provider '{}' does not define a vault credential path",
            provider.id
        )
    })?;
    let request = AuthContextRequest::ApiKey {
        provider_id:   provider.id.clone(),
        display_name:  provider.display_name.clone(),
        env_var_names: vec![secret_name.clone()],
        api_key_url:   provider.api_key_url.clone(),
    };
    provider_auth::present_to_user(&request, s, printer);

    loop {
        let key = provider_auth::read_api_key_from_source(&source, &secret_name).await?;
        fabro_util::printerr!(printer, "  {}", s.dim.apply_to("Validating API key..."));
        match server.test_provider_credentials(&provider.id, &key).await {
            Ok(()) => {
                fabro_util::printerr!(printer, "  {} API key is valid", s.green.apply_to("✔"));
                return Ok((secret_name, key));
            }
            Err(err) => {
                fabro_util::printerr!(printer, "  [error] API key validation failed: {err}");
                if matches!(source, provider_auth::ApiKeySource::Prompt) {
                    let retry = spawn_blocking(|| {
                        provider_auth::prompt_confirm("Try again with a different key?", true)
                    })
                    .await??;
                    if !retry {
                        return Ok((secret_name, key));
                    }
                } else {
                    return Err(err).context("API key validation failed");
                }
            }
        }
    }
}

async fn server_provider(
    server: &server_client::Client,
    requested_provider: &ProviderId,
) -> Result<types::Provider> {
    server
        .list_providers()
        .await?
        .into_iter()
        .find(|provider| provider_matches(provider, requested_provider))
        .with_context(|| {
            format!("provider '{requested_provider}' is not configured in the server model catalog")
        })
}

fn provider_matches(provider: &types::Provider, requested_provider: &ProviderId) -> bool {
    provider.id == *requested_provider
        || provider
            .aliases
            .iter()
            .any(|alias| alias == requested_provider.as_str())
}
