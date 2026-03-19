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
            eprintln!(
                "  {}",
                s.dim.apply_to("Opening browser for OpenAI login...")
            );
            match fabro_openai_oauth::run_browser_flow(
                fabro_openai_oauth::DEFAULT_ISSUER,
                fabro_openai_oauth::DEFAULT_CLIENT_ID,
            )
            .await
            {
                Ok(tokens) => {
                    tracing::info!("OpenAI OAuth browser flow completed");
                    let account_id = fabro_openai_oauth::extract_account_id(&tokens);
                    let pairs = provider_auth::openai_oauth_env_pairs(
                        &tokens.access_token,
                        &tokens.refresh_token,
                        account_id.as_deref(),
                    );
                    eprintln!(
                        "  {} OpenAI configured via browser login",
                        s.green.apply_to("✔")
                    );
                    pairs
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OpenAI OAuth browser flow failed");
                    eprintln!("  Browser login failed: {e}");
                    eprintln!(
                        "  {}",
                        s.dim.apply_to("Falling back to manual API key entry.")
                    );
                    let (env_var, key) =
                        provider_auth::prompt_and_validate_key(Provider::OpenAi, &s).await?;
                    vec![(env_var, key)]
                }
            }
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
