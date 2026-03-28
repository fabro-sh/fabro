pub mod ast;
pub mod grammar;
pub mod lexer;
pub mod semantic;

use crate::error::GraphvizError;
use crate::graph::types::Graph;

use self::ast::DotGraph;

/// Parse a DOT source string into a raw `DotGraph` AST.
///
/// Strips comments, parses the grammar, and validates there is no
/// trailing content. Does NOT perform semantic transformation.
///
/// # Errors
///
/// Returns an error if the input is not valid DOT syntax or contains
/// trailing content after the graph definition.
pub fn parse_ast(input: &str) -> Result<DotGraph, GraphvizError> {
    let stripped = lexer::strip_comments(input);
    let (rest, dot_graph) = grammar::parse_dot_graph(&stripped)
        .map_err(|e| GraphvizError::Parse(format!("grammar error: {e}")))?;

    let remaining = rest.trim();
    if !remaining.is_empty() {
        return Err(GraphvizError::Parse(format!(
            "unexpected trailing content: {:?}",
            &remaining[..remaining.len().min(50)]
        )));
    }

    Ok(dot_graph)
}

/// Parse a DOT source string into a semantic `Graph`.
///
/// Strips comments, parses the grammar, and performs semantic transformation
/// (expanding chained edges, applying defaults, flattening subgraphs).
///
/// # Errors
///
/// Returns an error if the input is not valid DOT syntax or contains
/// trailing content after the graph definition.
pub fn parse(input: &str) -> Result<Graph, GraphvizError> {
    let dot_graph = parse_ast(input)?;
    semantic::ast_to_graph(&dot_graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_linear() {
        let input = r#"digraph Simple {
            graph [goal="Run tests and report"]
            rankdir=LR

            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]

            run_tests [label="Run Tests", prompt="Run the test suite and report results"]
            report    [label="Report", prompt="Summarize the test results"]

            start -> run_tests -> report -> exit
        }"#;
        let graph = parse(input).unwrap();
        assert_eq!(graph.name, "Simple");
        assert_eq!(graph.goal(), "Run tests and report");
        assert_eq!(graph.nodes.len(), 4);
        // start->run_tests, run_tests->report, report->exit
        assert_eq!(graph.edges.len(), 3);
        assert!(graph.find_start_node().is_some());
        assert!(graph.find_exit_node().is_some());
    }

    #[test]
    fn parse_branching_with_conditions() {
        let input = r#"digraph Branch {
            graph [goal="Implement and validate a feature"]
            rankdir=LR
            node [shape=box, timeout="900s"]

            start     [shape=Mdiamond, label="Start"]
            exit      [shape=Msquare, label="Exit"]
            plan      [label="Plan", prompt="Plan the implementation"]
            implement [label="Implement", prompt="Implement the plan"]
            validate  [label="Validate", prompt="Run tests"]
            gate      [shape=diamond, label="Tests passing?"]

            start -> plan -> implement -> validate -> gate
            gate -> exit      [label="Yes", condition="outcome=success"]
            gate -> implement [label="No", condition="outcome!=success"]
        }"#;
        let graph = parse(input).unwrap();
        assert_eq!(graph.name, "Branch");
        assert_eq!(graph.nodes.len(), 6);
        // chain: 4 edges + 2 conditional = 6
        assert_eq!(graph.edges.len(), 6);

        // Check condition on gate -> exit edge
        let gate_exit = graph
            .edges
            .iter()
            .find(|e| e.from == "gate" && e.to == "exit")
            .unwrap();
        assert_eq!(gate_exit.condition(), Some("outcome=success"));
    }

    #[test]
    fn parse_human_gate() {
        let input = r#"digraph Review {
            rankdir=LR

            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]

            review_gate [
                shape=hexagon,
                label="Review Changes",
                type="human"
            ]

            start -> review_gate
            review_gate -> ship_it [label="[A] Approve"]
            review_gate -> fixes   [label="[F] Fix"]
            ship_it -> exit
            fixes -> review_gate
        }"#;
        let graph = parse(input).unwrap();
        assert_eq!(graph.name, "Review");
        let gate = &graph.nodes["review_gate"];
        assert_eq!(gate.node_type(), Some("human"));
        assert_eq!(gate.shape(), "hexagon");
    }

    #[test]
    fn parse_with_comments() {
        let input = r"// This is a comment
        digraph Test {
            /* block comment */
            start [shape=Mdiamond] // inline comment
            exit [shape=Msquare]
            start -> exit
        }";
        let graph = parse(input).unwrap();
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn parse_error_on_invalid_input() {
        let result = parse("not a graph");
        assert!(result.is_err());
    }

    #[test]
    fn parse_error_on_trailing_content() {
        let input = "digraph A { } extra stuff";
        let result = parse(input);
        assert!(result.is_err());
    }

    #[test]
    fn parse_subgraph_derives_class_on_contained_nodes() {
        let input = r#"digraph SubgraphClassTest {
            start [shape=Mdiamond]
            exit  [shape=Msquare]

            subgraph cluster_loop {
                label = "Loop A"
                plan      [label="Plan"]
                implement [label="Implement"]
                plan -> implement
            }

            start -> plan
            implement -> exit
        }"#;
        let graph = parse(input).unwrap();
        assert!(graph.nodes["plan"].classes.contains(&"loop-a".to_string()));
        assert!(
            graph.nodes["implement"]
                .classes
                .contains(&"loop-a".to_string())
        );
    }

    #[test]
    fn parse_legacy_codergen_mode_attribute() {
        let input = r#"digraph Legacy {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            classify [codergen_mode="one_shot", prompt="Classify this"]
            start -> classify -> exit
        }"#;
        let graph = parse(input).unwrap();
        assert_eq!(graph.nodes["classify"].handler_type(), Some("prompt"));
    }
}
