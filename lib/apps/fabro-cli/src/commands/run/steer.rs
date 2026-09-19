use anyhow::{Result, bail};
use fabro_api::types::{RunControlAcknowledgement, RunControlOutcome};
use tokio::io::{AsyncReadExt as _, stdin};
use tracing::info;

use crate::args::SteerArgs;
use crate::command_context::CommandContext;

/// Send a steer, or with `--interrupt` an interrupt carrying the text, and
/// say what the worker made of it: delivered to its stage, or pending when
/// the worker gave no answer in time. A refusal is the command's error,
/// with the reason the worker gave.
pub(crate) async fn run(args: SteerArgs, base_ctx: &CommandContext) -> Result<()> {
    let printer = base_ctx.printer();
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.id;

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

    let stage = args
        .stage
        .as_deref()
        .map(str::trim)
        .filter(|stage| !stage.is_empty())
        .map(str::to_owned);
    info!(run_id = %run_id, interrupt = args.interrupt, stage = ?stage, "Sending steer");
    let acknowledgement = client
        .steer_run(&run_id, text, args.interrupt, stage)
        .await?;
    let control = if args.interrupt { "Interrupt" } else { "Steer" };
    fabro_util::printerr!(printer, "{}", describe(control, &acknowledgement));
    Ok(())
}

/// One line on what became of the control.
fn describe(control: &str, acknowledgement: &RunControlAcknowledgement) -> String {
    match (&acknowledgement.outcome, acknowledgement.stage.as_deref()) {
        (RunControlOutcome::Delivered, Some(stage)) => {
            format!("{control} delivered to stage {stage}.")
        }
        (RunControlOutcome::Delivered, None) => format!("{control} delivered."),
        (RunControlOutcome::Pending, _) => format!(
            "{control} forwarded; the worker has not answered yet. The run's events say what \
             became of it."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outcome_is_described_in_one_line() {
        assert_eq!(
            describe("Steer", &RunControlAcknowledgement {
                outcome: RunControlOutcome::Delivered,
                stage:   Some("work@1".to_string()),
            }),
            "Steer delivered to stage work@1."
        );
        assert_eq!(
            describe("Interrupt", &RunControlAcknowledgement {
                outcome: RunControlOutcome::Pending,
                stage:   None,
            }),
            "Interrupt forwarded; the worker has not answered yet. The run's events say what \
             became of it."
        );
    }
}
