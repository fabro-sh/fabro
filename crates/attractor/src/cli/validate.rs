use anyhow::bail;
use terminal::Styles;

use crate::pipeline::PipelineBuilder;
use crate::validation::Severity;

use super::{print_diagnostics, read_dot_file, ValidateArgs};

/// Parse and validate a pipeline file without executing it.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or has validation errors.
pub fn validate_command(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let source = read_dot_file(&args.pipeline)?;
    let (graph, diagnostics) = PipelineBuilder::new().prepare(&source)?;

    eprintln!(
        "{bold}Parsed pipeline:{reset} {} ({dim}{} nodes, {} edges{reset})",
        graph.name,
        graph.nodes.len(),
        graph.edges.len(),
        bold = styles.bold, dim = styles.dim, reset = styles.reset,
    );

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    eprintln!(
        "Validation: {green}OK{reset}",
        green = styles.green, reset = styles.reset,
    );
    Ok(())
}
