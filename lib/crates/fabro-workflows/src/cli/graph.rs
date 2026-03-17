use std::borrow::Cow;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

use anyhow::bail;
use clap::{Args, ValueEnum};
use fabro_util::terminal::Styles;
use tracing::debug;

use crate::validation::Severity;
use crate::workflow::prepare_from_file;

use super::{print_diagnostics, read_workflow_file, relative_path};

/// Output format for graph rendering.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum GraphFormat {
    /// Scalable Vector Graphics
    #[default]
    Svg,
    /// Portable Network Graphics
    Png,
}

impl fmt::Display for GraphFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Svg => write!(f, "svg"),
            Self::Png => write!(f, "png"),
        }
    }
}

/// Graph layout direction.
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
    #[arg(long, value_enum, default_value_t = GraphFormat::Svg)]
    pub format: GraphFormat,

    /// Output file path (defaults to stdout)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Graph layout direction (overrides the DOT file's rankdir)
    #[arg(short = 'd', long)]
    pub direction: Option<GraphDirection>,
}

/// Render a workflow graph to SVG or PNG.
pub fn graph_command(args: &GraphArgs, styles: &Styles) -> anyhow::Result<()> {
    let (dot_path, _cfg) = super::project_config::resolve_workflow(&args.workflow)?;

    let (_graph, diagnostics) = prepare_from_file(&dot_path)?;

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    let source = read_workflow_file(&dot_path)?;
    let source = apply_direction(&source, args.direction);
    let rendered = render_dot(&source, args.format)?;

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

/// Dark mode CSS injected into SVG output (leading newline included for insertion).
const DARK_MODE_STYLE: &str = r##"
<style>
  @media (prefers-color-scheme: dark) {
    text { fill: #e0e0e0 !important; }
    [stroke="#357f9e"] { stroke: #5bb8d8; }
    [stroke="#666666"] { stroke: #999999; }
    polygon[fill="#357f9e"] { fill: #5bb8d8; }
    polygon[fill="#666666"] { fill: #999999; }
  }
</style>"##;

/// DOT graph-level defaults injected after the first `{`.
const DOT_STYLE_DEFAULTS: &str = r##"
    bgcolor="transparent"
    node [color="#357f9e", fontname="Helvetica", fontsize=12, fontcolor="#1a1a1a"]
    edge [color="#666666", fontname="Helvetica", fontsize=10, fontcolor="#666666"]
"##;

static RANKDIR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"rankdir\s*=\s*\w+").unwrap());

/// If a direction override is given, rewrite `rankdir=…` in the DOT source.
fn apply_direction<'a>(source: &'a str, direction: Option<GraphDirection>) -> Cow<'a, str> {
    match direction {
        Some(dir) => {
            let replacement = format!("rankdir={dir}");
            RANKDIR_RE.replace(source, replacement.as_str())
        }
        None => Cow::Borrowed(source),
    }
}

/// Inject DOT graph-level style defaults (transparent background, teal nodes,
/// gray edges, Helvetica font) right after the first `{` in the DOT source.
/// Per-node/edge attributes override these defaults.
fn inject_dot_style_defaults(source: &str) -> String {
    let Some(pos) = source.find('{') else {
        return source.to_string();
    };
    let (before, after) = source.split_at(pos + 1);
    format!("{before}{DOT_STYLE_DEFAULTS}{after}")
}

/// Post-process raw SVG output from Graphviz:
/// 1. Remove the white background `<polygon>` element
/// 2. Insert a dark-mode `<style>` block after the opening `<svg ...>` tag
fn postprocess_svg(raw: Vec<u8>) -> Vec<u8> {
    let mut svg = String::from_utf8(raw)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

    // Remove white background polygon (single line containing it)
    svg = svg
        .lines()
        .filter(|line| {
            !(line.contains("<polygon")
                && line.contains("fill=\"white\"")
                && line.contains("stroke=\"none\""))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Insert dark mode style block after the opening <svg ...> tag
    if let Some(svg_close) = svg
        .find("<svg")
        .and_then(|start| svg[start..].find('>').map(|end| start + end))
    {
        svg.insert_str(svg_close + 1, DARK_MODE_STYLE);
    }

    svg.into_bytes()
}

/// Render styled DOT source into the given format via the `dot` command.
///
/// Injects style defaults (colors, fonts, transparent background) into the DOT
/// source, then post-processes SVG output with dark-mode CSS and background removal.
pub fn render_dot(source: &str, format: GraphFormat) -> anyhow::Result<Vec<u8>> {
    let styled_source = inject_dot_style_defaults(source);
    let mut child = match Command::new("dot")
        .arg(format!("-T{format}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("Graphviz is not installed. Install it with: brew install graphviz");
        }
        Err(err) => {
            bail!("Failed to run dot: {err}");
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(styled_source.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("dot failed: {stderr}");
    }

    let raw = output.stdout;
    if matches!(format, GraphFormat::Svg) {
        Ok(postprocess_svg(raw))
    } else {
        Ok(raw)
    }
}

/// Check whether the `dot` command is available on PATH.
#[cfg(test)]
fn dot_is_available() -> bool {
    Command::new("dot")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const VALID_DOT: &str = r#"digraph Simple {
    graph [goal="Run tests and report results"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    run_tests [label="Run Tests", prompt="Run the test suite and report results"]
    report    [label="Report", prompt="Summarize the test results"]

    start -> run_tests -> report -> exit
}"#;

    #[test]
    fn graph_missing_file() {
        let args = GraphArgs {
            workflow: PathBuf::from("/tmp/nonexistent_workflow_99999.fabro"),
            format: GraphFormat::Svg,
            output: None,
            direction: None,
        };
        let styles = Styles::new(false);
        let result = graph_command(&args, &styles);
        assert!(result.is_err(), "expected Err for missing file");
    }

    #[test]
    fn graph_invalid_syntax() {
        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
        write!(tmp, "not a valid dot file").unwrap();

        let args = GraphArgs {
            workflow: tmp.path().to_path_buf(),
            format: GraphFormat::Svg,
            output: None,
            direction: None,
        };
        let styles = Styles::new(false);
        let result = graph_command(&args, &styles);
        assert!(result.is_err(), "expected Err for invalid syntax");
    }

    #[test]
    fn graph_valid_workflow_svg() {
        if !dot_is_available() {
            eprintln!("skipping: graphviz not installed");
            return;
        }

        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
        write!(tmp, "{VALID_DOT}").unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("out.svg");

        let args = GraphArgs {
            workflow: tmp.path().to_path_buf(),
            format: GraphFormat::Svg,
            output: Some(output_path.clone()),
            direction: None,
        };
        let styles = Styles::new(false);
        let result = graph_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("<svg"), "expected SVG content");
        assert!(
            content.contains("prefers-color-scheme: dark"),
            "expected dark mode style block"
        );
        assert!(
            !content.contains("fill=\"white\""),
            "white background should be removed"
        );
    }

    #[test]
    fn graph_valid_workflow_png() {
        if !dot_is_available() {
            eprintln!("skipping: graphviz not installed");
            return;
        }

        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
        write!(tmp, "{VALID_DOT}").unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("out.png");

        let args = GraphArgs {
            workflow: tmp.path().to_path_buf(),
            format: GraphFormat::Png,
            output: Some(output_path.clone()),
            direction: None,
        };
        let styles = Styles::new(false);
        let result = graph_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");

        let bytes = std::fs::read(&output_path).unwrap();
        // PNG magic bytes: 0x89 P N G
        assert!(
            bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
            "expected PNG magic bytes"
        );
    }

    #[test]
    fn graph_output_to_file() {
        if !dot_is_available() {
            eprintln!("skipping: graphviz not installed");
            return;
        }

        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
        write!(tmp, "{VALID_DOT}").unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("result.svg");

        let args = GraphArgs {
            workflow: tmp.path().to_path_buf(),
            format: GraphFormat::Svg,
            output: Some(output_path.clone()),
            direction: None,
        };
        let styles = Styles::new(false);
        graph_command(&args, &styles).unwrap();

        assert!(output_path.exists(), "output file should exist");
        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(!content.is_empty(), "output file should not be empty");
    }

    #[test]
    fn inject_dot_style_defaults_inserts_attrs() {
        let source = "digraph G {\n    a -> b\n}";
        let styled = inject_dot_style_defaults(source);
        assert!(styled.contains("bgcolor=\"transparent\""));
        assert!(styled.contains("node [color=\"#357f9e\""));
        assert!(styled.contains("fontname=\"Helvetica\""));
        assert!(styled.contains("edge [color=\"#666666\""));
        // Original content preserved
        assert!(styled.contains("a -> b"));
    }

    #[test]
    fn inject_dot_style_defaults_no_brace() {
        let source = "no brace here";
        let result = inject_dot_style_defaults(source);
        assert_eq!(result, source);
    }

    #[test]
    fn postprocess_svg_removes_white_bg() {
        let svg = b"<svg xmlns=\"...\" width=\"100\">\n<polygon fill=\"white\" stroke=\"none\" points=\"0,0 100,0 100,100 0,100\"/>\n<g>content</g>\n</svg>";
        let result = postprocess_svg(svg.to_vec());
        let result_str = String::from_utf8(result).unwrap();
        assert!(
            !result_str.contains("fill=\"white\""),
            "white background polygon should be removed"
        );
        assert!(result_str.contains("<g>content</g>"));
    }

    #[test]
    fn postprocess_svg_injects_dark_mode() {
        let svg = b"<svg xmlns=\"...\" width=\"100\">\n<g>content</g>\n</svg>";
        let result = postprocess_svg(svg.to_vec());
        let result_str = String::from_utf8(result).unwrap();
        assert!(
            result_str.contains("prefers-color-scheme: dark"),
            "dark mode style block should be present"
        );
        // Style block should come after <svg ...>
        let svg_tag_end = result_str.find('>').unwrap();
        let style_pos = result_str.find("<style>").unwrap();
        assert!(style_pos > svg_tag_end);
    }

    #[test]
    fn graph_toml_path() {
        if !dot_is_available() {
            eprintln!("skipping: graphviz not installed");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join("workflows").join("hello");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(wf_dir.join("workflow.fabro"), VALID_DOT).unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("out.svg");

        let args = GraphArgs {
            workflow: wf_dir.join("workflow.toml"),
            format: GraphFormat::Svg,
            output: Some(output_path.clone()),
            direction: None,
        };
        let styles = Styles::new(false);
        let result = graph_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("<svg"), "expected SVG content");
    }

    #[test]
    fn apply_direction_rewrites_rankdir() {
        let source = "digraph G {\n    rankdir=LR\n    a -> b\n}";
        let result = super::apply_direction(source, Some(GraphDirection::Tb));
        assert!(
            result.contains("rankdir=TB"),
            "expected rankdir=TB but got: {result}"
        );
        assert!(
            !result.contains("rankdir=LR"),
            "should not contain original rankdir=LR"
        );
    }

    #[test]
    fn apply_direction_none_preserves_source() {
        let source = "digraph G {\n    rankdir=LR\n    a -> b\n}";
        let result = super::apply_direction(source, None);
        assert_eq!(result, source);
    }
}
