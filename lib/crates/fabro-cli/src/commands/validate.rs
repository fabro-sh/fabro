use anyhow::bail;
use fabro_config::project::ResolveSettingsInput;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;

use crate::args::ValidateArgs;
use crate::shared::{print_diagnostics, relative_path};

pub fn run(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let cli_defaults = fabro_config::cli::load_cli_config(None)?;
    let settings = fabro_config::project::resolve_settings(ResolveSettingsInput {
        workflow_path: args.workflow.clone(),
        cwd: cwd.clone(),
        defaults: cli_defaults,
        overrides: fabro_config::FabroConfig::default(),
        apply_project_config: true,
    })?;
    let resolution = fabro_config::project::resolve_workflow_path(&args.workflow, &cwd)?;
    let validated =
        fabro_workflows::operations::validate(fabro_workflows::operations::ValidateInput {
            workflow: fabro_workflows::operations::WorkflowInput::Path(args.workflow.clone()),
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
