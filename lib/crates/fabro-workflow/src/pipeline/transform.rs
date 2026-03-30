use crate::transforms::{
    FileInliningTransform, ImportTransform, ModelResolutionTransform,
    StylesheetApplicationTransform, Transform, VariableExpansionTransform,
};

use super::types::{Parsed, TransformOptions, Transformed};

/// TRANSFORM phase: apply built-in and custom transforms to a parsed graph.
///
/// Infallible. Returns `Transformed` with a graph for post-transform
/// adjustments (e.g. goal override) before validation.
pub fn transform(parsed: Parsed, options: &TransformOptions) -> Transformed {
    let Parsed { graph, source } = parsed;

    // Built-in transforms (PreambleTransform moved to engine execution time)
    let graph = if let Some(ref dir) = options.base_dir {
        let fallback = dirs::home_dir().map(|home| home.join(".fabro"));
        ImportTransform::new(dir.clone(), fallback).apply(graph)
    } else {
        graph
    };

    let graph = if let Some(ref dir) = options.base_dir {
        let fallback = dirs::home_dir().map(|home| home.join(".fabro"));
        FileInliningTransform::new(dir.clone(), fallback).apply(graph)
    } else {
        graph
    };

    let graph = VariableExpansionTransform.apply(graph);
    let graph = StylesheetApplicationTransform.apply(graph);
    let graph = ModelResolutionTransform.apply(graph);

    // Custom transforms
    let graph = options
        .custom_transforms
        .iter()
        .fold(graph, |graph, transform| transform.apply(graph));

    Transformed { graph, source }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::pipeline::parse::parse;
    use fabro_graphviz::graph::AttrValue;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

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

    #[test]
    fn transform_inlines_files_before_variable_expansion() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("goal.md"), "Expand $goal");

        let parsed = parse(
            r#"digraph Test {
                graph [goal="Ship it"]
                start [shape=Mdiamond]
                work [prompt="@goal.md"]
                exit [shape=Msquare]
                start -> work -> exit
            }"#,
        )
        .unwrap();
        let transformed = transform(
            parsed,
            &TransformOptions {
                base_dir: Some(dir.path().to_path_buf()),
                custom_transforms: vec![],
            },
        );

        assert_eq!(
            transformed.graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Expand Ship it")
        );
    }

    #[test]
    fn transform_imports_before_variable_expansion_and_stylesheet() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("prompts/lint.md"), "Run checks for $goal");
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="@prompts/lint.md"]
                exit [shape=Msquare]
                start -> lint -> exit
            }"#,
        );

        let parsed = parse(
            r#"digraph Test {
                graph [goal="Launch", model_stylesheet=".validate { model: sonnet; }"]
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
        )
        .unwrap();
        let transformed = transform(
            parsed,
            &TransformOptions {
                base_dir: Some(dir.path().to_path_buf()),
                custom_transforms: vec![],
            },
        );

        let lint = &transformed.graph.nodes["validate.lint"];
        assert_eq!(
            lint.attrs.get("prompt").and_then(AttrValue::as_str),
            Some("Run checks for Launch")
        );
        assert_eq!(
            lint.attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-6".into()))
        );
    }
}
