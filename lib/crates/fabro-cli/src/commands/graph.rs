use std::borrow::Cow;
use std::io::Write;
use std::sync::LazyLock;

use anyhow::bail;
use fabro_config::ConfigLayer;
use fabro_config::project::resolve_workflow_path;
use fabro_graphviz::render::render_dot;
use fabro_util::terminal::Styles;
use fabro_validate::Severity;
use fabro_workflow::operations::{ValidateInput, WorkflowInput, validate};
use tracing::debug;

use crate::args::{GlobalArgs, GraphArgs, GraphDirection};
use crate::shared::{
    absolute_or_current, print_diagnostics, print_json_pretty, read_workflow_file, relative_path,
};

static RANKDIR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"rankdir\s*=\s*\w+").unwrap());

pub(crate) fn run(args: &GraphArgs, styles: &Styles, globals: &GlobalArgs) -> anyhow::Result<()> {
    if globals.json && args.output.is_none() {
        globals.require_no_json()?;
    }

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
        if globals.json {
            print_json_pretty(&serde_json::json!({
                "path": absolute_or_current(output_path),
                "format": args.format.to_string(),
            }))?;
        }
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

fn apply_direction(source: &str, direction: Option<GraphDirection>) -> Cow<'_, str> {
    match direction {
        Some(dir) => {
            let replacement = format!("rankdir={dir}");
            RANKDIR_RE.replace(source, replacement.as_str())
        }
        None => Cow::Borrowed(source),
    }
}
