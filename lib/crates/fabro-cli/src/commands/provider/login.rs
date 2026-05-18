use anyhow::Result;
use fabro_api::types;
use fabro_auth::LoginResult;
use fabro_model::{Catalog, CredentialRef, ProviderId};
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
            let name = api_key_secret_name(&provider, ctx.catalog()?.as_ref());
            (name, key, types::SecretType::Token)
        }
        LoginResult::OAuth { credential, .. } => (
            "OPENAI_CODEX".to_string(),
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

fn api_key_secret_name(provider: &ProviderId, catalog: &Catalog) -> String {
    catalog
        .provider(provider)
        .and_then(|provider| provider.auth.as_ref())
        .and_then(|auth| {
            auth.credentials
                .iter()
                .find_map(|credential_ref| match credential_ref {
                    CredentialRef::Vault(name) => Some(name.clone()),
                    CredentialRef::Env(_) => None,
                })
        })
        .unwrap_or_else(|| format!("{}_API_KEY", provider.to_string().to_uppercase()))
}
