use anyhow::{Context, Result};
use clap::Args;
use fabro_llm::provider::Provider;
use fabro_util::terminal::Styles;

use crate::provider_auth;

#[derive(Args)]
pub struct ProviderLoginArgs {
    /// LLM provider to authenticate with
    #[arg(long)]
    pub provider: Provider,
}

pub async fn login_command(args: ProviderLoginArgs) -> Result<()> {
    let s = Styles::detect_stderr();
    let arc_dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join(".fabro");
    std::fs::create_dir_all(&arc_dir)?;

    let env_pairs = if args.provider == Provider::OpenAi {
        // Offer OAuth browser flow for OpenAI
        let use_oauth = tokio::task::spawn_blocking(|| {
            provider_auth::prompt_confirm("Log in via browser (OAuth)?", true)
        })
        .await??;

        if use_oauth {
            provider_auth::run_openai_oauth_or_api_key(&s).await?
        } else {
            let (env_var, key) =
                provider_auth::prompt_and_validate_key(Provider::OpenAi, &s).await?;
            vec![(env_var, key)]
        }
    } else {
        let (env_var, key) = provider_auth::prompt_and_validate_key(args.provider, &s).await?;
        vec![(env_var, key)]
    };

    provider_auth::write_env_file(&arc_dir, &env_pairs, &s)?;
    Ok(())
}
