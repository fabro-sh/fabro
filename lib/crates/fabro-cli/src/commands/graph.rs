use std::borrow::Cow;
use std::io::Write;
use std::sync::LazyLock;

use anyhow::bail;
use fabro_config::cli::load_cli_config;
use fabro_config::project::{resolve_settings, resolve_workflow_path, ResolveSettingsInput};
use fabro_graphviz::render::render_dot;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;
use fabro_workflows::operations::{validate, ValidateInput, WorkflowInput};
use tracing::debug;

use crate::args::{GraphArgs, GraphDirection};
use crate::shared::{print_diagnostics, read_workflow_file, relative_path};

static RANKDIR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"rankdir\s*=\s*\w+").unwrap());

pub fn run(args: &GraphArgs, styles: &Styles) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let cli_defaults = load_cli_config(None)?;
    let settings = resolve_settings(ResolveSettingsInput {
        workflow_path: args.workflow.clone(),
        cwd: cwd.clone(),
        defaults: cli_defaults,
        overrides: fabro_config::FabroConfig::default(),
        apply_project_config: true,
    })?;
    let resolution = resolve_workflow_path(&args.workflow, &cwd)?;
    let validated = validate(ValidateInput {
        workflow: WorkflowInput::Path(args.workflow.clone()),
        settings,
        cwd,
        custom_transforms: Vec::new(),
    })?;
    let diagnostics = validated.diagnostics();

    print_diagnostics(diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    let source = read_workflow_file(&resolution.dot_path)?;
    let source = apply_direction(&source, args.direction);
    let rendered = render_dot(&source, args.format.into())?;

    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &rendered)?;
    } else {
        std::io::stdout().write_all(&rendered)?;
    }

    debug!(
        path = %relative_path(&resolution.dot_path),
        format = %args.format,
        "Rendered workflow graph"
    );

    Ok(())
}

fn apply_direction<'a>(source: &'a str, direction: Option<GraphDirection>) -> Cow<'a, str> {
    match direction {
        Some(dir) => {
            let replacement = format!("rankdir={dir}");
            RANKDIR_RE.replace(source, replacement.as_str())
        }
        None => Cow::Borrowed(source),
    }
}
