#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `config` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `config` command: blocking std::io::stdout is the intended output mechanism"
)]

use std::io::Write;

use fabro_api::types::ServerSettings;
use fabro_config::UserSettingsBuilder;
use fabro_types::UserSettings;
use serde::Serialize;

use crate::args::SettingsArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

#[derive(Serialize)]
struct RenderedConfig {
    user:   UserSettings,
    server: ServerSettings,
}

pub(crate) async fn execute(args: &SettingsArgs, base_ctx: &CommandContext) -> anyhow::Result<()> {
    let ctx = base_ctx.with_target(&args.target)?;
    let user = UserSettingsBuilder::load_default()?;
    let server = ctx
        .server()
        .await?
        .retrieve_resolved_server_settings()
        .await?;
    let config = serde_json::to_value(RenderedConfig { user, server })?;

    if base_ctx.json_output() {
        print_json_pretty(&config)?;
        return Ok(());
    }

    let mut yaml = serde_yaml::to_string(&config)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(yaml.as_bytes())?;

    Ok(())
}
