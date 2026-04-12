use anyhow::Result;
use fabro_util::printer::Printer;

use crate::args::{ArtifactListArgs, GlobalArgs};

pub(super) async fn list_command(
    args: &ArtifactListArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let (_run_id, _client, entries) = super::resolve_artifacts(
        &args.server,
        &args.run_id,
        args.node.as_deref(),
        args.retry,
        printer,
    )
    .await?;

    if globals.json {
        fabro_util::printout!(printer, "{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        fabro_util::printout!(printer, "No artifacts found for this run.");
        return Ok(());
    }

    let node_width = entries
        .iter()
        .map(|entry| entry.node_slug.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let retry_width = entries
        .iter()
        .map(|entry| entry.retry.to_string().len())
        .max()
        .unwrap_or(5)
        .max(5);

    fabro_util::printout!(
        printer,
        "{:<node_width$}  {:>retry_width$}  PATH",
        "NODE",
        "RETRY"
    );
    for entry in &entries {
        fabro_util::printout!(
            printer,
            "{:<node_width$}  {:>retry_width$}  {}",
            entry.node_slug,
            entry.retry,
            entry.relative_path
        );
    }
    fabro_util::printout!(printer, "");
    fabro_util::printout!(printer, "{} artifact(s)", entries.len());

    Ok(())
}
