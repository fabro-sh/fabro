use std::path::Path;

use fabro_graphviz::parser::parse;

fn parse_attractor_dot(filename: &str) -> Result<fabro_graphviz::graph::Graph, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../test/attractor")
        .join(filename);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    parse(&content).map_err(|e| format!("failed to parse {filename}: {e}"))
}

// ---------------------------------------------------------------------------
// Parsing tests: every attractor DOT file must parse without error
// ---------------------------------------------------------------------------

#[test]
fn parse_attractor_simple_example() {
    let graph = parse_attractor_dot("simple_example.dot").unwrap();
    assert_eq!(graph.name, "Simple");
    assert_eq!(graph.goal(), "Run tests and report");
    assert_eq!(graph.nodes.len(), 4);
    assert_eq!(graph.edges.len(), 3);
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}

#[test]
fn parse_attractor_batch_clean() {
    let graph = parse_attractor_dot("batch_clean.dot").unwrap();
    assert_eq!(graph.name, "G");
    assert_eq!(graph.nodes.len(), 3);
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}

#[test]
fn parse_attractor_batch_has_errors() {
    // This file is intentionally missing provider on the work node.
    // It should still parse successfully — validation is separate from parsing.
    let graph = parse_attractor_dot("batch_has_errors.dot").unwrap();
    assert_eq!(graph.nodes.len(), 3);
}

#[test]
fn parse_attractor_batch_warnings_only() {
    let graph = parse_attractor_dot("batch_warnings_only.dot").unwrap();
    assert_eq!(graph.nodes.len(), 3);
}

#[test]
fn parse_attractor_solitaire_fast() {
    let graph = parse_attractor_dot("solitaire_fast.dot").unwrap();
    assert_eq!(graph.name, "solitaire");
    assert_eq!(
        graph.goal(),
        "Build a terminal-based solitaire (Klondike) game"
    );
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Large workflow: 2 control + many work nodes + diamond gates
    assert!(
        graph.nodes.len() > 15,
        "expected >15 nodes, got {}",
        graph.nodes.len()
    );
    assert!(
        graph.edges.len() > 20,
        "expected >20 edges, got {}",
        graph.edges.len()
    );
}

#[test]
fn parse_attractor_consensus_task() {
    let graph = parse_attractor_dot("consensus_task.dot").unwrap();
    assert_eq!(graph.name, "Workflow");
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Multi-model consensus: many parallel branches
    assert!(graph.nodes.len() > 10);
}

#[test]
fn parse_attractor_semport() {
    let graph = parse_attractor_dot("semport.dot").unwrap();
    assert_eq!(graph.name, "Workflow");
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Loop-based workflow with conditional routing
    assert!(graph.edges.len() > 5);
}

#[test]
fn parse_attractor_reference_template() {
    let graph = parse_attractor_dot("reference_template.dot").unwrap();
    assert_eq!(graph.name, "reference_template");
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Kitchen-sink template: subgraphs, fan-out, parallel, loops
    assert!(graph.nodes.len() > 30);
    assert!(graph.edges.len() > 30);
    // Verify subgraph-derived classes are applied
    assert!(
        graph.nodes.contains_key("implement"),
        "should contain implement node"
    );
}

#[test]
fn parse_attractor_green_test_moderate() {
    let graph = parse_attractor_dot("green_test_moderate.dot").unwrap();
    assert_eq!(graph.name, "linkcheck");
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}

#[test]
fn parse_attractor_green_test_complex() {
    let graph = parse_attractor_dot("green_test_complex.dot").unwrap();
    assert_eq!(graph.name, "dttf");
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Very large workflow (40+ stages)
    assert!(graph.nodes.len() > 40);
}

#[test]
fn parse_attractor_green_test_vague() {
    let graph = parse_attractor_dot("green_test_vague.dot").unwrap();
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}

#[test]
fn parse_attractor_refactor_test_moderate() {
    let graph = parse_attractor_dot("refactor_test_moderate.dot").unwrap();
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}

#[test]
fn parse_attractor_refactor_test_complex() {
    let graph = parse_attractor_dot("refactor_test_complex.dot").unwrap();
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
    // Large workflow
    assert!(graph.nodes.len() > 30);
}

#[test]
fn parse_attractor_refactor_test_vague() {
    let graph = parse_attractor_dot("refactor_test_vague.dot").unwrap();
    assert!(graph.find_start_node().is_some());
    assert!(graph.find_exit_node().is_some());
}
