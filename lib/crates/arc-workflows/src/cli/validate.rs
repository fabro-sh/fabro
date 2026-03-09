use anyhow::bail;
use arc_util::terminal::Styles;

use crate::cli::run_config;
use crate::validation::Severity;
use crate::workflow::prepare_from_file;

use super::{print_diagnostics, ValidateArgs};

/// Parse and validate a workflow file without executing it.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or has validation errors.
pub fn validate_command(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let dot_path = if args.workflow.extension().is_some_and(|ext| ext == "toml") {
        let cfg = run_config::load_run_config(&args.workflow)?;
        run_config::resolve_graph_path(&args.workflow, &cfg.graph)
    } else {
        args.workflow.clone()
    };

    let (graph, diagnostics) = prepare_from_file(&dot_path)?;

    eprintln!(
        "{} ({} nodes, {} edges)",
        styles.bold.apply_to(format!("Workflow: {}", graph.name)),
        graph.nodes.len(),
        graph.edges.len(),
    );

    print_diagnostics(&diagnostics, styles);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        bail!("Validation failed");
    }

    eprintln!("Validation: {}", styles.green.apply_to("OK"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn validate_valid_workflow() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"digraph Simple {{
    graph [goal="Run tests and report results"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    run_tests [label="Run Tests", prompt="Run the test suite and report results"]
    report    [label="Report", prompt="Summarize the test results"]

    start -> run_tests -> report -> exit
}}"#
        )
        .unwrap();

        let args = ValidateArgs {
            workflow: tmp.path().to_path_buf(),
        };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    #[test]
    fn validate_invalid_syntax() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "not a valid dot file").unwrap();

        let args = ValidateArgs {
            workflow: tmp.path().to_path_buf(),
        };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_err(), "expected Err for invalid syntax");
    }

    #[test]
    fn validate_file_references_resolved() {
        let dir = tempfile::tempdir().unwrap();

        // Initialize a git repo so the workflow can be loaded
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Write a referenced prompt file
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("plan.md"), "Plan the work carefully.").unwrap();

        // Write a .dot file that uses @prompts/plan.md
        let dot_path = dir.path().join("workflow.dot");
        std::fs::write(
            &dot_path,
            r#"digraph FileRef {
    rankdir=LR
    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]
    plan  [label="Plan", prompt="@prompts/plan.md"]
    start -> plan -> exit
}"#,
        )
        .unwrap();

        let args = ValidateArgs { workflow: dot_path };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    #[test]
    fn validate_missing_file() {
        let args = ValidateArgs {
            workflow: PathBuf::from("/tmp/nonexistent_workflow_12345.dot"),
        };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_err(), "expected Err for missing file");
    }
}
