use std::path::Path;

use crate::error::FabroError;
use crate::graph::Graph;
use crate::transform::{
    FileInliningTransform, ProviderInferenceTransform, StylesheetApplicationTransform, Transform,
    VariableExpansionTransform,
};
use crate::validation::{self, Diagnostic};

/// Builder for configuring and executing a workflow preparation.
/// Collects custom transforms that run after the built-in ones.
pub struct WorkflowBuilder {
    transforms: Vec<Box<dyn Transform>>,
}

impl WorkflowBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            transforms: Vec::new(),
        }
    }

    /// Register a custom transform. Custom transforms run after built-in transforms,
    /// in registration order.
    pub fn register_transform(&mut self, transform: Box<dyn Transform>) {
        self.transforms.push(transform);
    }

    /// Prepare a workflow: parse DOT, apply built-in and custom transforms, validate.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing or validation fails.
    pub fn prepare(&self, dot_source: &str) -> Result<(Graph, Vec<Diagnostic>), FabroError> {
        self.prepare_inner(dot_source, None)
    }

    /// Prepare a workflow with file inlining: parse DOT, apply built-in transforms
    /// including `FileInliningTransform`, then custom transforms, then validate.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing or validation fails.
    pub fn prepare_with_file_inlining(
        &self,
        dot_source: &str,
        base_dir: &Path,
    ) -> Result<(Graph, Vec<Diagnostic>), FabroError> {
        self.prepare_inner(dot_source, Some(base_dir))
    }

    fn prepare_inner(
        &self,
        dot_source: &str,
        base_dir: Option<&Path>,
    ) -> Result<(Graph, Vec<Diagnostic>), FabroError> {
        let mut graph = crate::parser::parse(dot_source)?;

        // Built-in transforms (PreambleTransform moved to engine execution time)
        VariableExpansionTransform.apply(&mut graph);
        StylesheetApplicationTransform.apply(&mut graph);
        ProviderInferenceTransform.apply(&mut graph);

        // File inlining when base_dir is provided
        if let Some(dir) = base_dir {
            let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
            FileInliningTransform::new(dir.to_path_buf(), fallback).apply(&mut graph);
        }

        // Custom transforms
        for transform in &self.transforms {
            transform.apply(&mut graph);
        }

        let diagnostics = validation::validate(&graph, &[]);
        Ok((graph, diagnostics))
    }
}

impl Default for WorkflowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: read a DOT file, apply built-in transforms including file inlining, validate.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or validated.
pub fn prepare_from_file(path: &Path) -> Result<(Graph, Vec<Diagnostic>), FabroError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| FabroError::Parse(format!("Failed to read {}: {e}", path.display())))?;
    let dot_dir = path.parent().unwrap_or(Path::new("."));
    WorkflowBuilder::new().prepare_with_file_inlining(&source, dot_dir)
}

/// Convenience: parse DOT source (no file inlining), apply built-in transforms, validate.
/// Returns the graph or an error if validation produces Error-severity diagnostics.
///
/// # Errors
///
/// Returns an error if parsing fails or if validation produces Error-severity diagnostics.
pub fn prepare_from_source(dot_source: &str) -> Result<Graph, FabroError> {
    let builder = WorkflowBuilder::new();
    let (graph, diagnostics) = builder.prepare(dot_source)?;
    validation::raise_on_errors(&diagnostics)?;
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AttrValue;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    #[test]
    fn prepare_from_source_minimal() {
        let graph = prepare_from_source(MINIMAL_DOT).unwrap();
        assert_eq!(graph.name, "Test");
        assert!(graph.find_start_node().is_some());
        assert!(graph.find_exit_node().is_some());
    }

    #[test]
    fn prepare_from_source_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: $goal"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let graph = prepare_from_source(dot).unwrap();
        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
    }

    #[test]
    fn prepare_from_source_applies_stylesheet() {
        let dot = r#"digraph Test {
            graph [goal="Test", model_stylesheet="* { model: sonnet; }"]
            start [shape=Mdiamond]
            work  [label="Work"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let graph = prepare_from_source(dot).unwrap();
        assert_eq!(
            graph.nodes["work"].attrs.get("model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn prepare_from_source_returns_error_on_invalid_dot() {
        let result = prepare_from_source("not a graph");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_from_source_returns_error_on_validation_failure() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let result = prepare_from_source(dot);
        assert!(result.is_err());
    }

    #[test]
    fn pipeline_builder_custom_transform() {
        struct TagTransform;
        impl Transform for TagTransform {
            fn apply(&self, graph: &mut crate::graph::Graph) {
                for node in graph.nodes.values_mut() {
                    node.attrs
                        .insert("tagged".to_string(), AttrValue::Boolean(true));
                }
            }
        }

        let mut builder = WorkflowBuilder::new();
        builder.register_transform(Box::new(TagTransform));
        let (graph, _) = builder.prepare(MINIMAL_DOT).unwrap();
        assert_eq!(
            graph.nodes["start"].attrs.get("tagged"),
            Some(&AttrValue::Boolean(true))
        );
    }

    #[test]
    fn pipeline_builder_default() {
        let builder = WorkflowBuilder::default();
        let (graph, _) = builder.prepare(MINIMAL_DOT).unwrap();
        assert_eq!(graph.name, "Test");
    }
}
