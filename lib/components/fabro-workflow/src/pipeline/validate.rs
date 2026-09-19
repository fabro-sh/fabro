use super::types::{Transformed, Validated};

/// VALIDATE phase: the transformed graph with the transforms' diagnostics.
/// Fabro's lint rules went with the legacy executor; the workflow's rules
/// and its models are Petri's to judge at admission.
///
/// **Infallible.** Always returns `Validated` with diagnostics. Caller decides
/// whether to fail via `validated.raise_on_errors()`.
#[must_use]
pub fn validate(transformed: Transformed) -> Validated {
    let Transformed {
        graph,
        source,
        diagnostics,
    } = transformed;
    Validated::new(graph, source, diagnostics)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::diagnostic::Severity;

    use super::*;
    use crate::pipeline::parse::parse;
    use crate::pipeline::transform;
    use crate::pipeline::types::TransformOptions;

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

    fn run_pipeline(dot: &str) -> Validated {
        let parsed = parse(dot).unwrap();
        let transformed = transform::transform(parsed, &transform_options()).unwrap();
        validate(transformed)
    }

    #[test]
    fn validate_valid_graph() {
        let dot = r#"digraph Test {
            graph [goal="Build feature"]
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            start -> exit
        }"#;
        let validated = run_pipeline(dot);
        assert!(!validated.has_errors());
        assert!(validated.raise_on_errors().is_ok());
    }

    #[test]
    fn validate_into_parts() {
        let dot = r#"digraph Test {
            graph [goal="Build feature"]
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            start -> exit
        }"#;
        let validated = run_pipeline(dot);
        let (graph, source, diagnostics) = validated.into_parts();
        assert_eq!(graph.name, "Test");
        assert_eq!(source, dot);
        assert!(diagnostics.iter().all(|d| d.severity != Severity::Error));
    }

    #[test]
    fn unresolved_template_variables_are_the_transforms_diagnostics() {
        let dot = r#"digraph Test {
            graph [model_stylesheet="* { model: {{ vars.MODEL }}; }"]
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }"#;
        let transformed = transform::transform(parse(dot).unwrap(), &TransformOptions {
            template_context: fabro_template::TemplateContext::new().with_inputs(HashMap::new()),
            source_name: Some("workflow.fabro".to_string()),
            render_mode: crate::operations::RenderMode::Structural,
            ..transform_options()
        })
        .unwrap();
        let validated = validate(transformed);

        assert!(validated.diagnostics().iter().any(|diagnostic| {
            diagnostic.rule == "template_undefined_variable"
                && diagnostic.message.contains("vars.MODEL")
        }));
    }
}
