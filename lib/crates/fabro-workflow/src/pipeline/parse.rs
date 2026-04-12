use fabro_graphviz::parser;

use super::types::Parsed;
use crate::error::Error;

/// PARSE phase: parse DOT source into a `Parsed` graph.
///
/// # Errors
///
/// Returns `Error::Parse` if the DOT source is invalid.
pub fn parse(dot_source: &str) -> Result<Parsed, Error> {
    let graph = parser::parse(dot_source)?;
    Ok(Parsed {
        graph,
        source: dot_source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_dot() {
        let dot = r#"digraph Test {
            graph [goal="Build feature"]
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            start -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        assert_eq!(parsed.graph.name, "Test");
        assert!(parsed.graph.find_start_node().is_some());
        assert!(parsed.graph.find_exit_node().is_some());
        assert_eq!(parsed.source, dot);
    }

    #[test]
    fn parse_invalid_dot() {
        let result = parse("not a graph");
        assert!(result.is_err());
    }
}
