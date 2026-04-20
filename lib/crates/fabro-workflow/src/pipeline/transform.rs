use std::sync::Arc;

use super::types::{Parsed, TransformOptions, Transformed};
use crate::error::Error;
use crate::transforms::{
    FileInliningTransform, ImportTransform, ModelResolutionTransform,
    StylesheetApplicationTransform, TemplateTransform, Transform,
};

/// TRANSFORM phase: apply built-in and custom transforms to a parsed graph.
///
/// Returns `Transformed` with a graph for post-transform adjustments
/// (e.g. goal override) before validation.
pub fn transform(parsed: Parsed, options: &TransformOptions) -> Result<Transformed, Error> {
    let Parsed { graph, source } = parsed;

    // Built-in transforms (PreambleTransform moved to engine execution time)
    let graph = if let (Some(current_dir), Some(file_resolver)) =
        (&options.current_dir, &options.file_resolver)
    {
        ImportTransform::new(
            current_dir.clone(),
            Arc::clone(file_resolver),
            options.inputs.clone(),
        )
        .apply(graph)?
    } else {
        graph
    };

    let graph = if let (Some(current_dir), Some(file_resolver)) =
        (&options.current_dir, &options.file_resolver)
    {
        FileInliningTransform::new(current_dir.clone(), Arc::clone(file_resolver)).apply(graph)?
    } else {
        graph
    };

    let graph = TemplateTransform {
        inputs: options.inputs.clone(),
    }
    .apply(graph)?;
    let graph = StylesheetApplicationTransform.apply(graph)?;
    let graph = ModelResolutionTransform.apply(graph)?;

    // Custom transforms
    let graph = options
        .custom_transforms
        .iter()
        .try_fold(graph, |graph, transform| transform.apply(graph))?;

    Ok(Transformed { graph, source })
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests stage pipeline fixtures")]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use fabro_graphviz::graph::AttrValue;

    use super::*;
    use crate::file_resolver::FilesystemFileResolver;
    use crate::pipeline::parse::parse;

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
            work  [prompt="Goal: {{ goal }}"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            current_dir:       None,
            file_resolver:     None,
            inputs:            HashMap::new(),
            custom_transforms: vec![],
        })
        .unwrap();
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
        let transformed = transform(parsed, &TransformOptions {
            current_dir:       None,
            file_resolver:     None,
            inputs:            HashMap::new(),
            custom_transforms: vec![],
        })
        .unwrap();
        assert_eq!(
            transformed.graph.nodes["work"].attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-6".into()))
        );
    }

    #[test]
    fn transform_inlines_files_before_variable_expansion() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("goal.md"), "Expand {{ goal }}");

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
        let transformed = transform(parsed, &TransformOptions {
            current_dir:       Some(dir.path().to_path_buf()),
            file_resolver:     Some(Arc::new(FilesystemFileResolver::new(None))),
            inputs:            HashMap::new(),
            custom_transforms: vec![],
        })
        .unwrap();

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
        write_file(
            &dir.path().join("prompts/lint.md"),
            "Run checks for {{ inputs.task }}",
        );
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
        let transformed = transform(parsed, &TransformOptions {
            current_dir:       Some(dir.path().to_path_buf()),
            file_resolver:     Some(Arc::new(FilesystemFileResolver::new(None))),
            inputs:            HashMap::from([(
                "task".to_string(),
                toml::Value::String("Launch".to_string()),
            )]),
            custom_transforms: vec![],
        })
        .unwrap();

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
