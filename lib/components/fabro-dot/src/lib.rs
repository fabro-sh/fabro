//! The workflow graph as written, read through Petri's DOT parser.
//!
//! Petri admits and runs every workflow. Fabro's platform reads the DOT only
//! to learn a workflow's shape (its name, goal, node and edge counts) and the
//! files it names through a fixed attribute vocabulary: `import`,
//! `stack.child_workflow`, and `@`-prefixed `prompt`, `output_schema` and
//! `goal` values. The bundler also scans the inline templates for includes: a
//! non-`@` goal or prompt, and the entrypoint's `model_stylesheet`.
//!
//! File references are static. They may not contain template syntax, because
//! they resolve before any template renders. [`WorkflowGraph::references`] is
//! the one walker over that vocabulary: the manifest bundler and the
//! workflow-version store both consume it, so a new reference-bearing
//! attribute is added here once.
//!
//! This crate and `fabro-petri` are the two places Fabro imports Petri. It
//! stays small so the bundler and the version store read a graph without
//! pulling the engine in, and so `fabro-graphviz` can re-emit Fabro DOT for
//! Graphviz without a parser of its own.

mod graphviz;

use std::fmt;

use fabro_template::{StaticReferenceError, validate_static_reference};
use fabro_types::ReferenceKind;
pub use graphviz::normalize_for_graphviz;
use petri_frontend_attractor::dot;
use petri_frontend_attractor::model::{self, AttrValue, NodeDecl, Workflow};

/// A DOT text Petri's parser refused: the first problem it found, with the
/// position Petri reported.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct ParseError {
    /// Petri's diagnostic code, such as `dot.syntax` or
    /// `unsupported.dot.html_string`.
    pub code:    String,
    pub message: String,
    /// What to write instead, when Petri offers one.
    pub hint:    Option<String>,
    /// The file the text was parsed as.
    pub file:    String,
    /// One-based; zero when the problem has no position.
    pub line:    u32,
    /// One-based; zero when the problem has no position.
    pub column:  u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if self.line > 0 {
            write!(f, ":{}:{}", self.line, self.column)?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

/// Whether the walked graph is the workflow's entrypoint or was reached
/// through an `import` or `stack.child_workflow` reference.
///
/// Position-dependent reference semantics (today: `model_stylesheet` is a
/// template root only on the entrypoint, because an imported stylesheet is
/// ignored at run time) live in the walker, so every consumer applies the
/// same rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphPosition {
    Entrypoint,
    Imported,
}

/// What one reference in a workflow graph points at.
///
/// `@` prefixes are already stripped from file references. Inline variants
/// carry template content the consumer feeds to template-dependency
/// discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphReferenceKind<'graph> {
    /// `graph [goal="@<reference>"]`.
    GoalFile { reference: &'graph str },
    /// A non-`@` graph `goal`: inline template content.
    GoalInline { content: &'graph str },
    /// The entrypoint graph's inline `model_stylesheet` template content.
    ModelStylesheetInline { content: &'graph str },
    /// `node [import="<reference>"]`: another graph file to walk.
    Import { reference: &'graph str },
    /// `node [stack.child_workflow="<reference>"]`.
    ChildWorkflow { reference: &'graph str },
    /// `node [<key>="@<reference>"]` for the file-inlined attributes
    /// `prompt` and `output_schema`.
    FileInline {
        key:       &'graph str,
        reference: &'graph str,
    },
    /// A non-`@` node prompt: inline template content.
    InlinePrompt { content: &'graph str },
}

impl<'graph> GraphReferenceKind<'graph> {
    /// The file this reference names and the kind it is validated as;
    /// `None` for inline template content.
    #[must_use]
    pub fn file_reference(&self) -> Option<(&'graph str, ReferenceKind)> {
        match *self {
            Self::GoalFile { reference } => Some((reference, ReferenceKind::GraphGoalFile)),
            Self::Import { reference } => Some((reference, ReferenceKind::Import)),
            Self::ChildWorkflow { reference } => Some((reference, ReferenceKind::ChildWorkflow)),
            Self::FileInline { reference, .. } => Some((reference, ReferenceKind::FileInline)),
            Self::GoalInline { .. }
            | Self::ModelStylesheetInline { .. }
            | Self::InlinePrompt { .. } => None,
        }
    }
}

/// One file reference or inline template found in a workflow graph, with
/// where it was written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphReference<'graph> {
    pub kind:   GraphReferenceKind<'graph>,
    /// The node the reference sits on; `None` for a graph attribute.
    pub node:   Option<&'graph str>,
    /// The position of the attribute key, one-based.
    pub line:   u32,
    pub column: u32,
}

/// A parsed workflow graph: Petri's semantic model of the DOT, with node and
/// edge defaults applied, subgraphs flattened and edge chains expanded.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowGraph {
    workflow: Workflow,
}

impl WorkflowGraph {
    /// Parse `text` as the workflow file `file`, which positions and the
    /// error name.
    pub fn parse(file: &str, text: &str) -> Result<Self, ParseError> {
        let dot = dot::parse(file, text).map_err(|diagnostic| ParseError {
            code:    diagnostic.code.to_string(),
            message: diagnostic.message,
            hint:    diagnostic.hint,
            file:    diagnostic.span.file.to_string(),
            line:    diagnostic.span.line,
            column:  diagnostic.span.column,
        })?;
        Ok(Self {
            workflow: model::build(&dot),
        })
    }

    /// The `digraph` name; empty when the graph has none.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.workflow.name
    }

    /// The graph `goal` attribute as written, `@` prefix included, when it
    /// is a string.
    #[must_use]
    pub fn goal(&self) -> Option<&str> {
        self.graph_str("goal")
    }

    /// The graph `model_stylesheet` attribute, when it is a string.
    #[must_use]
    pub fn model_stylesheet(&self) -> Option<&str> {
        self.graph_str("model_stylesheet")
    }

    /// The graph `rankdir` attribute, written bare or quoted.
    #[must_use]
    pub fn rankdir(&self) -> Option<&str> {
        self.graph_str("rankdir")
    }

    /// Every node, declared or named only by an edge.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.workflow.nodes.len()
    }

    /// Every edge, with chains expanded.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.workflow.edges.len()
    }

    fn graph_str(&self, key: &str) -> Option<&str> {
        self.workflow
            .attrs
            .get(key)
            .and_then(|attr| attr.value.as_str())
    }

    /// Every static file reference and inline template in this graph, in
    /// declaration order, each file reference checked to be template-free.
    ///
    /// The walk covers this graph alone; recursing into `Import` targets and
    /// resolving references against a file source are the consumer's job.
    /// `position` says whether this graph is the workflow entrypoint, which
    /// gates the position-dependent references.
    pub fn references(
        &self,
        position: GraphPosition,
    ) -> Result<Vec<GraphReference<'_>>, StaticReferenceError> {
        let mut found = Vec::new();

        if let Some(goal) = self.workflow.attrs.get("goal") {
            if let Some(goal_text) = goal.value.as_str().filter(|goal| !goal.is_empty()) {
                let kind = if let Some(reference) = goal_text.strip_prefix('@') {
                    validate_static_reference(reference, ReferenceKind::GraphGoalFile)?;
                    GraphReferenceKind::GoalFile { reference }
                } else {
                    GraphReferenceKind::GoalInline { content: goal_text }
                };
                found.push(GraphReference {
                    kind,
                    node: None,
                    line: goal.span.line,
                    column: goal.span.column,
                });
            }
        }

        if position == GraphPosition::Entrypoint {
            if let Some(stylesheet) = self.workflow.attrs.get("model_stylesheet") {
                if let Some(content) = stylesheet.value.as_str().filter(|css| !css.is_empty()) {
                    found.push(GraphReference {
                        kind:   GraphReferenceKind::ModelStylesheetInline { content },
                        node:   None,
                        line:   stylesheet.span.line,
                        column: stylesheet.span.column,
                    });
                }
            }
        }

        for node in &self.workflow.nodes {
            node_references(node, &mut found)?;
        }
        Ok(found)
    }
}

fn node_references<'graph>(
    node: &'graph NodeDecl,
    found: &mut Vec<GraphReference<'graph>>,
) -> Result<(), StaticReferenceError> {
    for (key, attr) in node.attrs.iter() {
        let AttrValue::Str(value) = &attr.value else {
            continue;
        };
        let kind = match key {
            "import" => GraphReferenceKind::Import { reference: value },
            "stack.child_workflow" => GraphReferenceKind::ChildWorkflow { reference: value },
            "prompt" | "output_schema" => match value.strip_prefix('@') {
                Some(reference) => GraphReferenceKind::FileInline { key, reference },
                None => continue,
            },
            _ => continue,
        };
        if let Some((reference, reference_kind)) = kind.file_reference() {
            validate_static_reference(reference, reference_kind)?;
        }
        found.push(GraphReference {
            kind,
            node: Some(&node.id),
            line: attr.span.line,
            column: attr.span.column,
        });
    }

    if let Some(prompt) = node.attrs.get("prompt") {
        if let Some(content) = prompt
            .value
            .as_str()
            .filter(|prompt| !prompt.starts_with('@'))
        {
            found.push(GraphReference {
                kind:   GraphReferenceKind::InlinePrompt { content },
                node:   Some(&node.id),
                line:   prompt.span.line,
                column: prompt.span.column,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
