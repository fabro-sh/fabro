use anyhow::bail;
use fabro_util::terminal::Styles;

use crate::validation::Severity;
use crate::workflow::prepare_from_file;

use super::{print_diagnostics, relative_path, ValidateArgs};

/// Parse and validate a workflow file without executing it.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or has validation errors.
pub fn validate_command(args: &ValidateArgs, styles: &Styles) -> anyhow::Result<()> {
    let (dot_path, _cfg) = super::project_config::resolve_workflow(&args.workflow)?;

    let (graph, diagnostics) = prepare_from_file(&dot_path)?;

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
        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
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
        let mut tmp = tempfile::Builder::new()
            .suffix(".fabro")
            .tempfile()
            .unwrap();
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

        // Write a .fabro file that uses @prompts/plan.md
        let dot_path = dir.path().join("workflow.fabro");
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
    fn validate_toml_path() {
        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join("workflows").join("hello");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("workflow.fabro"),
            r#"digraph Hello {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    run [label="Run", prompt="Do it"]
    start -> run -> exit
}"#,
        )
        .unwrap();

        let args = ValidateArgs {
            workflow: wf_dir.join("workflow.toml"),
        };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    #[test]
    fn validate_missing_file() {
        let args = ValidateArgs {
            workflow: PathBuf::from("/tmp/nonexistent_workflow_12345.fabro"),
        };
        let styles = Styles::new(false);
        let result = validate_command(&args, &styles);
        assert!(result.is_err(), "expected Err for missing file");
    }
}
