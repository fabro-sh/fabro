use anyhow::bail;
use fabro_config::ConfigLayer;
use fabro_config::project::resolve_workflow_path;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;
use fabro_workflows::operations::{ValidateInput, WorkflowInput, validate};

use crate::args::ValidateArgs;
use crate::shared::{print_diagnostics, relative_path};

pub(crate) fn run(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let settings = ConfigLayer::for_workflow(&args.workflow, &cwd)?
        .combine(ConfigLayer::cli()?)
        .resolve()?;
    let resolution = resolve_workflow_path(&args.workflow, &cwd)?;
    let validated = validate(ValidateInput {
        workflow: WorkflowInput::Path(args.workflow.clone()),
        settings,
        cwd,
        custom_transforms: Vec::new(),
    })?;
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
        styles.dim.apply_to(relative_path(&resolution.dot_path)),
    );

    print_diagnostics(diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    eprintln!("Validation: {}", styles.green.apply_to("OK"));
    Ok(())
}
