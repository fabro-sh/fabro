use anyhow::bail;
use fabro_config::ConfigLayer;
use fabro_config::project::resolve_workflow_path;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;
use fabro_workflow::operations::{ValidateInput, WorkflowInput, validate};

use crate::args::{GlobalArgs, ValidateArgs};
use crate::shared::{print_diagnostics, print_json_pretty, relative_path};

pub(crate) fn run(
    args: &ValidateArgs,
    styles: &Styles,
    globals: &GlobalArgs,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let settings = ConfigLayer::for_workflow(&args.workflow, &cwd)?
        .combine(ConfigLayer::user()?)
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

    if globals.json {
        print_json_pretty(&serde_json::json!({
            "workflow_name": graph.name,
            "nodes": graph.nodes.len(),
            "edges": graph.edges.len(),
            "valid": !diagnostics.iter().any(|d| d.severity == Severity::Error),
            "diagnostics": diagnostics,
        }))?;

        if diagnostics.iter().any(|d| d.severity == Severity::Error) {
            bail!("Validation failed");
        }
        return Ok(());
    }

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
