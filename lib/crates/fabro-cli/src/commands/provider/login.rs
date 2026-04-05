use anyhow::Result;
use fabro_api::types;
use fabro_config::legacy_env;
use fabro_model::Provider;
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;

use crate::args::{GlobalArgs, ProviderLoginArgs};
use crate::server_client;
use crate::shared::provider_auth;

pub(super) async fn login_command(args: ProviderLoginArgs, globals: &GlobalArgs) -> Result<()> {
    globals.require_no_json()?;
    let s = Styles::detect_stderr();
    let client = server_client::connect_server_backed_api_client(&args.target).await?;

    let use_oauth = args.provider == Provider::OpenAi
        && spawn_blocking(|| provider_auth::prompt_confirm("Log in via browser (OAuth)?", true))
            .await??;

    let env_pairs = if use_oauth {
        provider_auth::run_openai_oauth_or_api_key(&s).await?
    } else {
        let (env_var, key) = provider_auth::prompt_and_validate_key(args.provider, &s).await?;
        vec![(env_var, key)]
    };

    if let Ok(path) = legacy_env::legacy_env_file_path() {
        if path.exists() {
            eprintln!(
                "  Warning: {} is no longer read by fabro server. Re-enter credentials with `fabro provider login` or `fabro secret set`.",
                path.display()
            );
        }
    }

    for (name, value) in env_pairs {
        client
            .set_secret()
            .name(name.clone())
            .body(types::SetSecretRequest { value })
            .send()
            .await?;
        eprintln!("  {} Saved {}", s.green.apply_to("✔"), name);
    }
    Ok(())
}
