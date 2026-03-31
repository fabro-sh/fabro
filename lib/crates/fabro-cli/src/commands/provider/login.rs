use anyhow::{Context, Result};
use fabro_model::Provider;
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;

use crate::args::{GlobalArgs, ProviderLoginArgs};
use crate::shared::provider_auth;

pub(super) async fn login_command(args: ProviderLoginArgs, globals: &GlobalArgs) -> Result<()> {
    globals.require_no_json()?;
    let s = Styles::detect_stderr();
    let arc_dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join(".fabro");
    std::fs::create_dir_all(&arc_dir)?;

    let use_oauth = args.provider == Provider::OpenAi
        && spawn_blocking(|| provider_auth::prompt_confirm("Log in via browser (OAuth)?", true))
            .await??;

    let env_pairs = if use_oauth {
        provider_auth::run_openai_oauth_or_api_key(&s).await?
    } else {
        let (env_var, key) = provider_auth::prompt_and_validate_key(args.provider, &s).await?;
        vec![(env_var, key)]
    };

    provider_auth::write_env_file(&arc_dir, &env_pairs, &s)?;
    Ok(())
}
