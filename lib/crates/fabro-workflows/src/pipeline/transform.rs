use crate::transform::{
    FileInliningTransform, ModelResolutionTransform, StylesheetApplicationTransform, Transform,
    VariableExpansionTransform,
};

use super::types::{Parsed, TransformOptions, Transformed};

/// TRANSFORM phase: apply built-in and custom transforms to a parsed graph.
///
/// Infallible. Returns `Transformed` with a mutable `graph` for post-transform
/// adjustments (e.g. goal override) before validation.
pub fn transform(parsed: Parsed, options: &TransformOptions) -> Transformed {
    let Parsed { mut graph, source } = parsed;

    // Built-in transforms (PreambleTransform moved to engine execution time)
    VariableExpansionTransform.apply(&mut graph);
    StylesheetApplicationTransform.apply(&mut graph);
    ModelResolutionTransform.apply(&mut graph);

    // File inlining when base_dir is provided
    if let Some(ref dir) = options.base_dir {
        let fallback = dirs::home_dir().map(|h| h.join(".fabro"));
        FileInliningTransform::new(dir.clone(), fallback).apply(&mut graph);
    }

    // Custom transforms
    for t in &options.custom_transforms {
        t.apply(&mut graph);
    }

    Transformed { graph, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::parse::parse;
    use fabro_graphviz::graph::AttrValue;

    #[test]
    fn transform_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: $goal"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(
            parsed,
            &TransformOptions {
                base_dir: None,
                custom_transforms: vec![],
            },
        );
        let prompt = transformed.graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
    }

    #[test]
    fn transform_applies_stylesheet() {
        let dot = r#"digraph Test {
            graph [goal="Test", model_stylesheet="* { model: sonnet; }"]
            start [shape=Mdiamond]
            work  [label="Work"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(
            parsed,
            &TransformOptions {
                base_dir: None,
                custom_transforms: vec![],
            },
        );
        assert_eq!(
            transformed.graph.nodes["work"].attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-6".into()))
        );
    }
}
