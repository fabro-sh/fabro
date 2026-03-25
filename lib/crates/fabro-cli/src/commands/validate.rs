use std::path::PathBuf;

use anyhow::bail;
use clap::Args;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;

use crate::commands::shared::{print_diagnostics, relative_path};

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the .fabro workflow file
    pub workflow: PathBuf,
}

pub fn run(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let (dot_path, _cfg) = fabro_config::project::resolve_workflow(&args.workflow)?;

    let validated = fabro_workflows::operations::create_from_file(&dot_path)?;
    let graph = validated.graph();
    let diagnostics = validated.diagnostics();

    eprintln!(
        "{} ({} nodes, {} edges)",
        styles.bold.apply_to(format!("Workflow: {}", graph.name)),
        graph.nodes.len(),
        graph.edges.len(),
    );
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(relative_path(&dot_path)),
    );

    print_diagnostics(diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    eprintln!("Validation: {}", styles.green.apply_to("OK"));
    Ok(())
}
