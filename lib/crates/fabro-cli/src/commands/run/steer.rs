use anyhow::{Result, bail};
use tokio::io::{AsyncReadExt as _, stdin};
use tracing::info;

use crate::args::SteerArgs;
use crate::command_context::CommandContext;

pub(crate) async fn run(args: SteerArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.run_id;

    let text = match (args.text_stdin, args.text.clone()) {
        (true, _) => {
            let mut buf = String::new();
            stdin().read_to_string(&mut buf).await?;
            buf
        }
        (false, Some(text)) => text,
        (false, None) => {
            bail!("missing steer text — pass it as a positional argument or use --text-stdin")
        }
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("steer text must not be empty");
    }

    info!(run_id = %run_id, interrupt = args.interrupt, "Sending steer");
    client.steer_run(&run_id, text, args.interrupt).await?;
    Ok(())
}
