use std::io::Write;

use super::{read_dot_file, ParseArgs};

/// Parse a DOT file and print its raw AST as JSON.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or contains trailing content.
pub fn parse_command(args: &ParseArgs) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    parse_command_to(args, stdout.lock())
}

fn parse_command_to(args: &ParseArgs, mut out: impl Write) -> anyhow::Result<()> {
    let source = read_dot_file(&args.workflow)?;
    let ast = crate::parser::parse_ast(&source)?;
    serde_json::to_writer_pretty(&mut out, &ast)?;
    writeln!(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::DotGraph;
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn parse_command_outputs_json_ast() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"digraph Hello {{
    start [shape=Mdiamond]
    exit [shape=Msquare]
    start -> exit
}}"#
        )
        .unwrap();

        let args = ParseArgs {
            workflow: tmp.path().to_path_buf(),
        };
        let mut buf = Vec::new();
        parse_command_to(&args, &mut buf).unwrap();

        let deserialized: DotGraph = serde_json::from_slice(&buf).unwrap();
        assert_eq!(deserialized.name, "Hello");
        assert_eq!(deserialized.statements.len(), 3);
    }

    #[test]
    fn parse_command_rejects_invalid_dot() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "not a valid dot file").unwrap();

        let args = ParseArgs {
            workflow: tmp.path().to_path_buf(),
        };
        let result = parse_command_to(&args, Vec::new());
        assert!(result.is_err(), "expected Err for invalid syntax");
    }

    #[test]
    fn parse_command_rejects_missing_file() {
        let args = ParseArgs {
            workflow: PathBuf::from("/tmp/nonexistent_parse_test_12345.dot"),
        };
        let result = parse_command_to(&args, Vec::new());
        assert!(result.is_err(), "expected Err for missing file");
    }
}
