use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::{GraphPosition, GraphReference, GraphReferenceKind, WorkflowGraph};

fn parse(text: &str) -> WorkflowGraph {
    WorkflowGraph::parse("workflow.fabro", text).unwrap_or_else(|error| panic!("{error}"))
}

fn describe(reference: &GraphReference<'_>) -> String {
    let where_ = reference
        .node
        .map_or_else(String::new, |node| format!("@{node}"));
    let what = match reference.kind {
        GraphReferenceKind::GoalFile { reference } => format!("goal-file:{reference}"),
        GraphReferenceKind::GoalInline { content } => format!("goal-inline:{content}"),
        GraphReferenceKind::ModelStylesheetInline { content } => {
            format!("stylesheet-inline:{content}")
        }
        GraphReferenceKind::Import { reference } => format!("import:{reference}"),
        GraphReferenceKind::ChildWorkflow { reference } => format!("child:{reference}"),
        GraphReferenceKind::FileInline { key, reference } => format!("file:{key}:{reference}"),
        GraphReferenceKind::InlinePrompt { content } => format!("inline:{content}"),
    };
    format!("{what}{where_}")
}

fn describe_all(graph: &WorkflowGraph, position: GraphPosition) -> Vec<String> {
    graph
        .references(position)
        .unwrap_or_else(|error| panic!("{error}"))
        .iter()
        .map(describe)
        .collect()
}

// The workflow text in this test is adapted from the branching example in the
// Attractor specification.
//
// Source: https://github.com/strongdm/attractor/blob/fb57a55ed97372a27ac90102f436947e29f48426/attractor-spec.md
//
// Copyright StrongDM, Inc.
//
// SPDX-License-Identifier: Apache-2.0 (https://github.com/strongdm/attractor/blob/fb57a55ed97372a27ac90102f436947e29f48426/LICENSE)
#[test]
fn reads_the_shape_with_defaults_applied_and_chains_expanded() {
    let graph = parse(
        r#"digraph Branch {
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
            gate -> exit      [label="Yes", condition="outcome=succeeded"]
            gate -> implement [label="No", condition="outcome!=succeeded"]
        }"#,
    );

    assert_eq!(graph.name(), "Branch");
    assert_eq!(graph.goal(), Some("Implement and validate a feature"));
    assert_eq!(graph.model_stylesheet(), None);
    assert_eq!(graph.node_count(), 6);
    assert_eq!(graph.edge_count(), 6);
}

#[test]
fn parse_errors_carry_petri_code_and_position() {
    let error = WorkflowGraph::parse("flows/bad.fabro", "digraph A { } extra stuff").unwrap_err();

    assert_eq!(error.code, "dot.syntax");
    assert_eq!(error.file, "flows/bad.fabro");
    assert_eq!((error.line, error.column), (1, 15));
    assert_eq!(
        error.to_string(),
        "flows/bad.fabro:1:15: unexpected `extra` after the graph"
    );

    let error = WorkflowGraph::parse("w.fabro", "not a graph").unwrap_err();
    assert_eq!(error.code, "dot.syntax");
}

#[test]
fn visits_every_reference_kind_once_in_declaration_order() {
    let graph = parse(
        r#"digraph Refs {
            graph [goal="@goal.md", model_stylesheet="{% include 'styles.partial' %}"]
            imported    [import="graphs/child.fabro"]
            child       [stack.child_workflow="children/check.fabro"]
            file_prompt [prompt="@prompts/task.md", output_schema="@schemas/out.json"]
            inline      [prompt="Do the {{ thing }}"]
            keyword     [output_schema="routing"]
        }"#,
    );

    assert_eq!(describe_all(&graph, GraphPosition::Entrypoint), [
        "goal-file:goal.md",
        "stylesheet-inline:{% include 'styles.partial' %}",
        "import:graphs/child.fabro@imported",
        "child:children/check.fabro@child",
        "file:output_schema:schemas/out.json@file_prompt",
        "file:prompt:prompts/task.md@file_prompt",
        "inline:Do the {{ thing }}@inline",
    ]);
}

#[test]
fn references_carry_the_attribute_position() {
    let graph = parse("digraph P {\n  a [label=\"A\",\n     prompt=\"@task.md\"]\n}");

    let references = graph.references(GraphPosition::Entrypoint).unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!((references[0].line, references[0].column), (3, 6));
    assert_eq!(references[0].node, Some("a"));
}

#[test]
fn node_defaults_reach_every_node_declared_under_them() {
    let graph = parse(
        r#"digraph Defaults {
            node [prompt="@shared.md"]
            a
            b [prompt="own prompt"]
            subgraph cluster_x {
                node [output_schema="@x.json"]
                c
            }
            d
        }"#,
    );

    assert_eq!(describe_all(&graph, GraphPosition::Entrypoint), [
        "file:prompt:shared.md@a",
        "inline:own prompt@b",
        "file:output_schema:x.json@c",
        "file:prompt:shared.md@c",
        "file:prompt:shared.md@d",
    ]);
}

#[test]
fn imported_graphs_do_not_emit_model_stylesheet() {
    let graph = parse(
        r#"digraph Imported {
            graph [model_stylesheet="* { reasoning_effort: low; }"]
        }"#,
    );

    assert!(describe_all(&graph, GraphPosition::Imported).is_empty());
    assert_eq!(describe_all(&graph, GraphPosition::Entrypoint), [
        "stylesheet-inline:* { reasoning_effort: low; }"
    ]);
}

#[test]
fn empty_goal_and_non_string_attributes_are_not_references() {
    let graph = parse(
        r#"digraph Quiet {
            graph [goal=""]
            a [prompt=5, import=true]
        }"#,
    );

    assert!(describe_all(&graph, GraphPosition::Entrypoint).is_empty());
}

#[test]
fn rejects_template_syntax_in_references_before_visiting() {
    for (source, kind) in [
        (
            r#"digraph T { imported [import="graphs/{{ name }}.fabro"] }"#,
            "import reference",
        ),
        (
            r#"digraph T { child [stack.child_workflow="{{ inputs.child }}"] }"#,
            "child workflow reference",
        ),
        (
            r#"digraph T { work [prompt="@prompts/{{ lang }}.md"] }"#,
            "file inline reference",
        ),
        (
            r#"digraph T { graph [goal="@{{ goal_file }}"] }"#,
            "graph goal file reference",
        ),
    ] {
        let error = parse(source)
            .references(GraphPosition::Entrypoint)
            .expect_err("template syntax in a file reference must be refused");
        assert_eq!(error.kind().to_string(), kind, "source: {source}");
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root should resolve")
}

fn checked_in_workflows(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for directory in [
        ".fabro/workflows",
        "test/dot-compatibility",
        "lib/apps/fabro-cli/tests/it/workflow/fixtures",
    ] {
        for entry in walkdir::WalkDir::new(root.join(directory)).sort_by_file_name() {
            let entry = entry.expect("workflow directory entries should be readable");
            if entry.path().extension().and_then(|ext| ext.to_str()) == Some("fabro") {
                files.push(entry.into_path());
            }
        }
    }
    files
}

/// The walker's output over every checked-in workflow bundle. A change here
/// means the bundler will see a different version closure for one of Fabro's
/// own workflows: read the diff as that.
#[expect(
    clippy::disallowed_methods,
    reason = "unit test reads the checked-in workflow bundles synchronously"
)]
#[test]
fn checked_in_workflows_keep_their_shape_and_references() {
    let root = repository_root();
    let mut report = String::new();
    for path in checked_in_workflows(&root) {
        let relative = path
            .strip_prefix(&root)
            .expect("workflow paths sit under the repository root");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let graph = WorkflowGraph::parse(&relative.display().to_string(), &text)
            .unwrap_or_else(|error| panic!("{error}"));
        writeln!(
            report,
            "{}: {} nodes={} edges={}",
            relative.display(),
            graph.name(),
            graph.node_count(),
            graph.edge_count()
        )
        .expect("writing to a String cannot fail");
        for reference in graph.references(GraphPosition::Entrypoint).unwrap() {
            let description = match reference.kind {
                GraphReferenceKind::GoalInline { .. } => "goal-inline".to_string(),
                GraphReferenceKind::ModelStylesheetInline { .. } => "stylesheet-inline".to_string(),
                GraphReferenceKind::InlinePrompt { .. } => {
                    format!("inline@{}", reference.node.unwrap_or_default())
                }
                _ => describe(&reference),
            };
            writeln!(report, "  {description}").expect("writing to a String cannot fail");
        }
    }
    insta::assert_snapshot!(report);
}
