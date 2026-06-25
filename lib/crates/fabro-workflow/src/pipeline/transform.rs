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
    let mut diagnostics = Vec::new();

    // Built-in transforms (PreambleTransform moved to engine execution time)
    let graph = if let (Some(current_dir), Some(file_resolver)) =
        (&options.current_dir, &options.file_resolver)
    {
        let (graph, transform_diagnostics) = ImportTransform::new(
            current_dir.clone(),
            Arc::clone(file_resolver),
            options.template_context.clone(),
        )
        .with_template_options(
            options.source_name.clone(),
            Some(source.clone()),
            options.render_mode,
        )
        .apply_with_diagnostics(graph)?;
        diagnostics.extend(transform_diagnostics);
        graph
    } else {
        graph
    };

    let graph = if let (Some(current_dir), Some(file_resolver)) =
        (&options.current_dir, &options.file_resolver)
    {
        let (graph, transform_diagnostics) =
            FileInliningTransform::new(current_dir.clone(), Arc::clone(file_resolver))
                .with_template_options(
                    options.template_context.clone(),
                    options.source_name.clone(),
                    Some(source.clone()),
                    options.render_mode,
                )
                .apply_with_diagnostics(graph)?;
        diagnostics.extend(transform_diagnostics);
        graph
    } else {
        graph
    };

    let (graph, transform_diagnostics) = TemplateTransform {
        context:     options.template_context.clone(),
        source_name: options.source_name.clone(),
        source_text: Some(source.clone()),
        render_mode: options.render_mode,
    }
    .apply_with_diagnostics(graph)?;
    diagnostics.extend(transform_diagnostics);
    let graph = StylesheetApplicationTransform.apply(graph)?;
    let graph = ModelResolutionTransform::new(Arc::clone(&options.catalog)).apply(graph)?;

    // Custom transforms
    let graph = options
        .custom_transforms
        .iter()
        .try_fold(graph, |graph, transform| transform.apply(graph))?;

    Ok(Transformed {
        graph,
        source,
        diagnostics,
    })
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests stage pipeline fixtures")]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use fabro_graphviz::graph::AttrValue;
    use fabro_model::Catalog;

    use super::*;
    use crate::file_resolver::FilesystemFileResolver;
    use crate::pipeline::parse::parse;
    use crate::pipeline::types::{GOAL_SELF_REFERENCE_RULE, TEMPLATE_UNDEFINED_VARIABLE_RULE};

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().unwrap())
    }

    fn transform_options() -> TransformOptions {
        TransformOptions {
            current_dir:       None,
            file_resolver:     None,
            template_context:  fabro_template::TemplateContext::new(),
            source_name:       None,
            render_mode:       crate::operations::RenderMode::Strict,
            custom_transforms: vec![],
            catalog:           test_catalog(),
        }
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
        let transformed = transform(parsed, &transform_options()).unwrap();
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
        let transformed = transform(parsed, &transform_options()).unwrap();
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
            template_context:  fabro_template::TemplateContext::new(),
            source_name:       None,
            render_mode:       crate::operations::RenderMode::Strict,
            custom_transforms: vec![],
            catalog:           test_catalog(),
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
            template_context:  fabro_template::TemplateContext::new().with_inputs(HashMap::from([
                (
                    "task".to_string(),
                    toml::Value::String("Launch".to_string()),
                ),
            ])),
            source_name:       None,
            render_mode:       crate::operations::RenderMode::Strict,
            custom_transforms: vec![],
            catalog:           test_catalog(),
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

    #[test]
    fn transform_interpolates_vars_in_node_prompt() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Service: {{ vars.SERVICE }}"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            template_context: fabro_template::TemplateContext::new().with_vars(HashMap::from([(
                "SERVICE".to_string(),
                "billing".to_string(),
            )])),
            ..transform_options()
        })
        .unwrap();
        let prompt = transformed.graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Service: billing");
    }

    #[test]
    fn transform_interpolates_vars_in_graph_goal_and_through_prompt() {
        // The goal interpolates `{{ vars.* }}`, and a prompt that embeds the
        // goal sees the vars-resolved text.
        let dot = r#"digraph Test {
            graph [goal="Ship {{ vars.SERVICE }}"]
            start [shape=Mdiamond]
            work  [prompt="Goal: {{ goal }}"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            template_context: fabro_template::TemplateContext::new().with_vars(HashMap::from([(
                "SERVICE".to_string(),
                "billing".to_string(),
            )])),
            ..transform_options()
        })
        .unwrap();
        assert_eq!(
            transformed
                .graph
                .attrs
                .get("goal")
                .and_then(AttrValue::as_str),
            Some("Ship billing")
        );
        assert_eq!(
            transformed.graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Goal: Ship billing")
        );
    }

    #[test]
    fn transform_with_empty_vars_warns_on_unknown_var() {
        // Offline / no variable store: `{{ vars.* }}` is undefined, surfacing a
        // structural-mode warning (promoted to a hard error at run-create).
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Service: {{ vars.MISSING }}"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            template_context: fabro_template::TemplateContext::new(),
            render_mode: crate::operations::RenderMode::Structural,
            ..transform_options()
        })
        .unwrap();
        let diag = transformed
            .diagnostics
            .iter()
            .find(|d| d.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
            .expect("expected a template_undefined_variable diagnostic for vars.MISSING");
        assert!(
            diag.message.contains("vars.MISSING"),
            "message: {}",
            diag.message
        );
    }

    #[test]
    fn transform_reports_goal_self_reference_once_across_passes() {
        // FileInlining renders the goal for prompt context, but TemplateTransform
        // is the only pass that should emit the self-reference diagnostic.
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse(
            r#"digraph Test {
                graph [goal="Improve on {{ goal }}"]
                start [shape=Mdiamond]
                work [prompt="Do the work"]
                exit [shape=Msquare]
                start -> work -> exit
            }"#,
        )
        .unwrap();
        let transformed = transform(parsed, &TransformOptions {
            current_dir:       Some(dir.path().to_path_buf()),
            file_resolver:     Some(Arc::new(FilesystemFileResolver::new(None))),
            template_context:  fabro_template::TemplateContext::new(),
            source_name:       None,
            render_mode:       crate::operations::RenderMode::Structural,
            custom_transforms: vec![],
            catalog:           test_catalog(),
        })
        .unwrap();

        let self_ref = transformed
            .diagnostics
            .iter()
            .filter(|d| d.rule == GOAL_SELF_REFERENCE_RULE)
            .count();
        assert_eq!(
            self_ref, 1,
            "goal self-reference should be reported exactly once across transform passes"
        );
    }
}
