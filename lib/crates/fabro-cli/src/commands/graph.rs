use std::borrow::Cow;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::bail;
use clap::{Args, ValueEnum};
use fabro_util::terminal::Styles;
use fabro_validate::Severity;
use tracing::debug;

use crate::commands::shared::{print_diagnostics, read_workflow_file, relative_path};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GraphDirection {
    /// Left to right
    Lr,
    /// Top to bottom
    Tb,
}

impl fmt::Display for GraphDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lr => write!(f, "LR"),
            Self::Tb => write!(f, "TB"),
        }
    }
}

#[derive(Args)]
pub struct GraphArgs {
    /// Path to the .fabro workflow file, .toml task config, or project workflow name
    pub workflow: PathBuf,

    /// Output format
    #[arg(
        long,
        value_enum,
        default_value_t = GraphOutputFormat::Svg
    )]
    pub format: GraphOutputFormat,

    /// Output file path (defaults to stdout)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Graph layout direction (overrides the DOT file's rankdir)
    #[arg(short = 'd', long)]
    pub direction: Option<GraphDirection>,
}

static RANKDIR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"rankdir\s*=\s*\w+").unwrap());

pub fn run(args: &GraphArgs, styles: &Styles) -> anyhow::Result<()> {
    let (dot_path, _cfg) = fabro_config::project::resolve_workflow(&args.workflow)?;

    let validated = fabro_workflows::operations::create_from_file(&dot_path)?;
    let diagnostics = validated.diagnostics();

    print_diagnostics(diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    let source = read_workflow_file(&dot_path)?;
    let source = apply_direction(&source, args.direction);
    let rendered = fabro_graphviz::render::render_dot(&source, args.format.into())?;

    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &rendered)?;
    } else {
        std::io::stdout().write_all(&rendered)?;
    }

    debug!(
        path = %relative_path(&dot_path),
        format = %args.format,
        "Rendered workflow graph"
    );

    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GraphOutputFormat {
    Svg,
    Png,
}

impl From<GraphOutputFormat> for fabro_graphviz::render::GraphFormat {
    fn from(value: GraphOutputFormat) -> Self {
        match value {
            GraphOutputFormat::Svg => Self::Svg,
            GraphOutputFormat::Png => Self::Png,
        }
    }
}

impl fmt::Display for GraphOutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Svg => write!(f, "svg"),
            Self::Png => write!(f, "png"),
        }
    }
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
