use anyhow::Result;
use fabro_api::types;
use fabro_config::legacy_env;
use fabro_model::Provider;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;

use crate::args::{GlobalArgs, ProviderLoginArgs};
use crate::command_context::CommandContext;
use crate::shared::provider_auth;

pub(super) async fn login_command(
    args: ProviderLoginArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    globals.require_no_json()?;
    let s = Styles::detect_stderr();
    let ctx = CommandContext::for_target(&args.target, printer)?;
    let server = ctx.server().await?;

    let use_oauth = args.provider == Provider::OpenAi
        && spawn_blocking(|| provider_auth::prompt_confirm("Log in via browser (OAuth)?", true))
            .await??;

    let env_pairs = if use_oauth {
        provider_auth::run_openai_oauth_or_api_key(&s, printer).await?
    } else {
        let (env_var, key) =
            provider_auth::prompt_and_validate_key(args.provider, &s, printer).await?;
        vec![(env_var, key)]
    };

    {
        let path = legacy_env::legacy_env_file_path();
        if path.exists() {
            fabro_util::printerr!(
                printer,
                "  Warning: {} is no longer read by fabro server. Re-enter credentials with `fabro provider login` or `fabro secret set`.",
                path.display()
            );
        }
    }

    for (name, value) in env_pairs {
        server
            .api()
            .set_secret()
            .name(name.clone())
            .body(types::SetSecretRequest { value })
            .send()
            .await?;
        fabro_util::printerr!(printer, "  {} Saved {}", s.green.apply_to("✔"), name);
    }
    Ok(())
}
