#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `variable set` command: reads variable value from stdin via blocking std::io::Read"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `variable set` command: reads variable value from std::io::stdin"
)]

use std::io::Read as _;

use anyhow::{Context as _, Result, bail};
use fabro_api::types;
use tokio::task::spawn_blocking;

use crate::args::VariableSetArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

async fn resolve_value(args: &VariableSetArgs) -> Result<String> {
    if let Some(value) = &args.value {
        return Ok(value.clone());
    }

    if args.value_stdin {
        let value = spawn_blocking(|| {
            let mut raw = String::new();
            std::io::stdin()
                .read_to_string(&mut raw)
                .context("failed to read variable value from stdin")?;
            Ok::<String, anyhow::Error>(raw.trim_end_matches(['\r', '\n']).to_string())
        })
        .await??;
        return Ok(value);
    }

    bail!("variable value required: pass <VALUE> or use --value-stdin")
}

pub(super) async fn set_command(args: &VariableSetArgs, ctx: &CommandContext) -> Result<()> {
    let value = resolve_value(args).await?;
    let client = ctx.server().await?;
    let variable = client
        .create_variable(types::CreateVariableRequest {
            name: args.name.clone(),
            value,
            description: args.description.clone(),
        })
        .await?;
    if ctx.json_output() {
        print_json_pretty(&variable)?;
    } else {
        fabro_util::printerr!(ctx.printer(), "Set {}", variable.name);
    }
    Ok(())
}
