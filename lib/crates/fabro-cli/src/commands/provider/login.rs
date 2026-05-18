use anyhow::{Context, Result};
use fabro_api::types;
use fabro_auth::{LoginResult, OPENAI_CODEX_VAULT_SECRET_NAME};
use fabro_util::terminal::Styles;

use crate::args::ProviderLoginArgs;
use crate::command_context::CommandContext;
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
    let result = if args.api_key_stdin {
        provider_auth::authenticate_provider_with_api_key_source_and_catalog(
            args.provider,
            provider_auth::ApiKeySource::Stdin,
            &s,
            printer,
            ctx.catalog()?,
        )
        .await?
    } else {
        provider_auth::authenticate_provider_with_catalog(
            args.provider,
            &s,
            printer,
            ctx.catalog()?,
        )
        .await?
    };

    let (name, value, type_) = match result {
        LoginResult::ApiKey { provider, key } => {
            let name = ctx
                .catalog()?
                .provider_vault_secret_name(&provider)
                .with_context(|| {
                    format!("provider '{provider}' does not define a vault credential path")
                })?
                .to_string();
            (name, key, types::SecretType::Token)
        }
        LoginResult::OAuth { credential, .. } => (
            OPENAI_CODEX_VAULT_SECRET_NAME.to_string(),
            serde_json::to_string(&credential)?,
            types::SecretType::Oauth,
        ),
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
