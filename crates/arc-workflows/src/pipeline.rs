use crate::error::ArcError;
use crate::graph::Graph;
use crate::transform::{StylesheetApplicationTransform, Transform, VariableExpansionTransform};
use crate::validation::{self, Diagnostic};

/// Builder for configuring and executing a pipeline preparation.
/// Collects custom transforms that run after the built-in ones.
pub struct PipelineBuilder {
    transforms: Vec<Box<dyn Transform>>,
}

impl PipelineBuilder {
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

    /// Prepare a pipeline: parse DOT, apply built-in and custom transforms, validate.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing or validation fails.
    pub fn prepare(&self, dot_source: &str) -> Result<(Graph, Vec<Diagnostic>), ArcError> {
        let mut graph = crate::parser::parse(dot_source)?;

        // Built-in transforms (PreambleTransform moved to engine execution time)
        VariableExpansionTransform.apply(&mut graph);
        StylesheetApplicationTransform.apply(&mut graph);

        // Custom transforms
        for transform in &self.transforms {
            transform.apply(&mut graph);
        }

        let diagnostics = validation::validate(&graph, &[]);
        Ok((graph, diagnostics))
    }
}

impl Default for PipelineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function: parse DOT, apply built-in transforms, validate, return graph.
///
/// # Errors
///
/// Returns an error if parsing fails or if validation produces Error-severity diagnostics.
pub fn prepare_pipeline(dot_source: &str) -> Result<Graph, ArcError> {
    let builder = PipelineBuilder::new();
    let (graph, diagnostics) = builder.prepare(dot_source)?;

    let errors: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == validation::Severity::Error)
        .collect();
    if !errors.is_empty() {
        let messages: Vec<String> = errors.iter().map(|d| d.message.clone()).collect();
        return Err(ArcError::Validation(messages.join("; ")));
    }

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
    fn prepare_pipeline_minimal() {
        let graph = prepare_pipeline(MINIMAL_DOT).unwrap();
        assert_eq!(graph.name, "Test");
        assert!(graph.find_start_node().is_some());
        assert!(graph.find_exit_node().is_some());
    }

    #[test]
    fn prepare_pipeline_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: $goal"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let graph = prepare_pipeline(dot).unwrap();
        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
    }

    #[test]
    fn prepare_pipeline_applies_stylesheet() {
        let dot = r#"digraph Test {
            graph [goal="Test", model_stylesheet="* { llm_model: sonnet; }"]
            start [shape=Mdiamond]
            work  [label="Work"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let graph = prepare_pipeline(dot).unwrap();
        assert_eq!(
            graph.nodes["work"].attrs.get("llm_model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn prepare_pipeline_returns_error_on_invalid_dot() {
        let result = prepare_pipeline("not a graph");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_pipeline_returns_error_on_validation_failure() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let result = prepare_pipeline(dot);
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

        let mut builder = PipelineBuilder::new();
        builder.register_transform(Box::new(TagTransform));
        let (graph, _) = builder.prepare(MINIMAL_DOT).unwrap();
        assert_eq!(
            graph.nodes["start"].attrs.get("tagged"),
            Some(&AttrValue::Boolean(true))
        );
    }

    #[test]
    fn pipeline_builder_default() {
        let builder = PipelineBuilder::default();
        let (graph, _) = builder.prepare(MINIMAL_DOT).unwrap();
        assert_eq!(graph.name, "Test");
    }
}
