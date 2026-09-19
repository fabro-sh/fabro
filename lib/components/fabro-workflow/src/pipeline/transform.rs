use std::sync::Arc;

use super::types::{Parsed, TransformOptions, Transformed};
use crate::error::Error;
use crate::transforms::{
    FileInliningTransform, ImportTransform, ModelStylesheetTemplateTransform,
    ScriptInterpolationTransform, StylesheetApplicationTransform, TemplateTransform, Transform,
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
    let graph = if graph.model_stylesheet().is_empty() {
        graph
    } else {
        let (graph, transform_diagnostics) = ModelStylesheetTemplateTransform {
            context:         options.template_context.clone(),
            source_name:     options.source_name.clone(),
            source_text:     Some(source.clone()),
            render_mode:     options.render_mode,
            file_resolution: options
                .current_dir
                .clone()
                .zip(options.file_resolver.clone()),
        }
        .apply_with_diagnostics(graph)?;
        diagnostics.extend(transform_diagnostics);
        graph
    };
    let (graph, transform_diagnostics) = ScriptInterpolationTransform {
        context:     options.template_context.clone(),
        source_name: options.source_name.clone(),
        render_mode: options.render_mode,
    }
    .apply_with_diagnostics(graph)?;
    diagnostics.extend(transform_diagnostics);
    let graph = StylesheetApplicationTransform.apply(graph)?;

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

    fn transform_options() -> TransformOptions {
        TransformOptions {
            current_dir:       None,
            file_resolver:     None,
            template_context:  fabro_template::TemplateContext::new(),
            source_name:       None,
            render_mode:       crate::operations::RenderMode::Strict,
            custom_transforms: vec![],
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
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn transform_renders_model_stylesheet_before_applying_and_resolving_it() {
        let dot = r#"digraph Test {
            graph [
                goal="Test",
                model_stylesheet="
                    * { reasoning_effort: low; }
                    {# MiniJinja comments can sit beside CSS braces. #}
                    {% if inputs.effort == 'deep' %}
                    .variable { model: sonnet; reasoning_effort: high; }
                    {% endif %}
                "
            ]
            start [shape=Mdiamond]
            baseline [prompt="Baseline"]
            selected [prompt="Selected", class="variable"]
            explicit [prompt="Explicit", class="variable", reasoning_effort="medium"]
            exit [shape=Msquare]
            start -> baseline -> selected -> explicit -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            template_context: fabro_template::TemplateContext::new().with_inputs(HashMap::from([
                (
                    "effort".to_string(),
                    toml::Value::String("deep".to_string()),
                ),
            ])),
            ..transform_options()
        })
        .unwrap();

        assert_eq!(
            transformed.graph.nodes["baseline"]
                .attrs
                .get("reasoning_effort")
                .and_then(AttrValue::as_str),
            Some("low")
        );
        assert_eq!(
            transformed.graph.nodes["selected"]
                .attrs
                .get("reasoning_effort")
                .and_then(AttrValue::as_str),
            Some("high")
        );
        assert_eq!(
            transformed.graph.nodes["selected"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("sonnet")
        );
        assert_eq!(
            transformed.graph.nodes["explicit"]
                .attrs
                .get("reasoning_effort")
                .and_then(AttrValue::as_str),
            Some("medium")
        );
        assert!(
            transformed
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.rule != "detemplated_attribute"),
            "{:?}",
            transformed.diagnostics
        );
    }

    #[test]
    fn transform_renders_model_stylesheet_static_include() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("styles.partial"),
            ".selected { model: sonnet; }",
        );
        let source_name = dir.path().join("workflow.fabro");
        let dot = r#"digraph Test {
            graph [model_stylesheet="{% include 'styles.partial' %}"]
            start [shape=Mdiamond]
            selected [prompt="Selected", class="selected"]
            exit [shape=Msquare]
            start -> selected -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            source_name: Some(source_name.display().to_string()),
            ..transform_options()
        })
        .unwrap();

        assert_eq!(
            transformed.graph.nodes["selected"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("sonnet")
        );
    }

    #[test]
    fn structural_model_stylesheet_undefined_value_skips_stylesheet_parsing() {
        let dot = r#"digraph Test {
            graph [model_stylesheet="* { reasoning_effort: {{ inputs.effort }}; }"]
            start [shape=Mdiamond]
            work [prompt="Work"]
            exit [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            render_mode: crate::operations::RenderMode::Structural,
            ..transform_options()
        })
        .unwrap();

        assert_eq!(transformed.graph.model_stylesheet(), "");
        assert_eq!(
            transformed
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
                .count(),
            1
        );
        assert!(
            transformed
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.rule != "detemplated_attribute")
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
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            ..transform_options()
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
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            template_context: fabro_template::TemplateContext::new().with_inputs(HashMap::from([
                (
                    "task".to_string(),
                    toml::Value::String("Launch".to_string()),
                ),
            ])),
            ..transform_options()
        })
        .unwrap();

        let lint = &transformed.graph.nodes["validate.lint"];
        assert_eq!(
            lint.attrs.get("prompt").and_then(AttrValue::as_str),
            Some("Run checks for Launch")
        );
        assert_eq!(
            lint.attrs.get("model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn imported_script_does_not_rescan_substituted_token_syntax() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("child.fabro"),
            r#"digraph child {
                start [shape=Mdiamond]
                run [shape=parallelogram, script="printf '%s' {{ inputs.payload }}"]
                exit [shape=Msquare]
                start -> run -> exit
            }"#,
        );
        let parsed = parse(
            r#"digraph Test {
                graph [goal="Test"]
                start [shape=Mdiamond]
                child [import="./child.fabro"]
                exit [shape=Msquare]
                start -> child -> exit
            }"#,
        )
        .unwrap();
        let transformed = transform(parsed, &TransformOptions {
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            template_context: fabro_template::TemplateContext::new().with_inputs(HashMap::from([
                (
                    "payload".to_string(),
                    toml::Value::String("{{ secrets.API_KEY }}".to_string()),
                ),
            ])),
            ..transform_options()
        })
        .unwrap();

        assert_eq!(
            transformed.graph.nodes["child.run"]
                .attrs
                .get("script")
                .and_then(AttrValue::as_str),
            Some("printf '%s' '{{ secrets.API_KEY }}'")
        );
    }

    #[test]
    fn imported_script_reports_a_missing_value_once() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("child.fabro"),
            r#"digraph child {
                start [shape=Mdiamond]
                run [shape=parallelogram, script="echo {{ inputs.missing }}"]
                exit [shape=Msquare]
                start -> run -> exit
            }"#,
        );
        let parsed = parse(
            r#"digraph Test {
                graph [goal="Test"]
                start [shape=Mdiamond]
                child [import="./child.fabro"]
                exit [shape=Msquare]
                start -> child -> exit
            }"#,
        )
        .unwrap();
        let transformed = transform(parsed, &TransformOptions {
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            render_mode: crate::operations::RenderMode::Structural,
            ..transform_options()
        })
        .unwrap();

        let missing_value_diagnostics = transformed
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
            .count();
        assert_eq!(
            missing_value_diagnostics, 1,
            "{:?}",
            transformed.diagnostics
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
    fn structural_transform_preserves_catalog_owned_model_selection() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            start [shape=Mdiamond]
            work [prompt="Do work", model="private-model", provider="server-only"]
            exit [shape=Msquare]
            start -> work -> exit
        }"#;
        let parsed = parse(dot).unwrap();
        let transformed = transform(parsed, &TransformOptions {
            ..transform_options()
        })
        .unwrap();
        let work = &transformed.graph.nodes["work"];

        assert_eq!(
            work.attrs.get("model").and_then(AttrValue::as_str),
            Some("private-model")
        );
        assert_eq!(
            work.attrs.get("provider").and_then(AttrValue::as_str),
            Some("server-only")
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
            current_dir: Some(dir.path().to_path_buf()),
            file_resolver: Some(Arc::new(FilesystemFileResolver::new(None))),
            render_mode: crate::operations::RenderMode::Structural,
            ..transform_options()
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
