use std::path::PathBuf;
use std::sync::Arc;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::TemplateContext;
use fabro_types::diagnostic::Diagnostic;

use super::file_inlining::template_render_store;
use super::variable_expansion::{
    RenderMode, TemplateRenderOutcome, TemplateRenderTarget, render_template_for_target_outcome,
};
use crate::error::Error;
use crate::file_resolver::FileResolver;

/// Renders the root graph's `model_stylesheet` with its restricted template
/// context after imports are expanded and before stylesheet parsing.
pub(crate) struct ModelStylesheetTemplateTransform {
    pub context:         TemplateContext,
    pub source_name:     Option<String>,
    pub source_text:     Option<String>,
    pub render_mode:     RenderMode,
    /// Enables `{% include %}` resolution; without it the stylesheet renders
    /// from its inline text alone.
    pub file_resolution: Option<(PathBuf, Arc<dyn FileResolver>)>,
}

impl ModelStylesheetTemplateTransform {
    pub(crate) fn apply_with_diagnostics(
        &self,
        mut graph: Graph,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        let stylesheet = graph.model_stylesheet();
        if stylesheet.is_empty() {
            return Ok((graph, Vec::new()));
        }

        let mut target =
            TemplateRenderTarget::graph_attr(self.source_name.clone(), "model_stylesheet")
                .with_source_origin(self.source_text.as_deref(), stylesheet)
                .with_restricted_namespace_fix(
                    "`model_stylesheet` templates expose only `inputs` and `vars`; use one of \
                     those values or a MiniJinja local value",
                );
        if let Some((current_dir, resolver)) = &self.file_resolution {
            target = target.with_template_store(template_render_store(
                current_dir,
                Arc::clone(resolver),
                self.source_name.as_deref(),
            )?);
        }

        let mut diagnostics = Vec::new();
        let outcome = render_template_for_target_outcome(
            stylesheet,
            &self.context.for_model_stylesheet(),
            self.render_mode,
            &target,
            &mut diagnostics,
        )?;
        let rendered = match outcome {
            TemplateRenderOutcome::Rendered(rendered) => rendered,
            // Do not feed raw or partly rendered MiniJinja source to the
            // stylesheet parser during structural validation.
            TemplateRenderOutcome::Unresolved => String::new(),
        };

        graph
            .attrs
            .insert("model_stylesheet".to_string(), AttrValue::String(rendered));
        Ok((graph, diagnostics))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_graphviz::graph::Graph;
    use fabro_util::error::collect_chain;

    use super::*;

    fn graph_with_stylesheet(stylesheet: &str) -> Graph {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String(stylesheet.to_string()),
        );
        graph
    }

    fn transform(
        context: TemplateContext,
        stylesheet: &str,
        render_mode: RenderMode,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        ModelStylesheetTemplateTransform {
            context,
            source_name: Some("workflow.fabro".to_string()),
            source_text: Some(format!(
                "digraph Test {{ graph [model_stylesheet=\"{stylesheet}\"] }}"
            )),
            render_mode,
            file_resolution: None,
        }
        .apply_with_diagnostics(graph_with_stylesheet(stylesheet))
    }

    #[test]
    fn preserves_static_stylesheet_bytes() {
        let stylesheet = "\n  * { reasoning_effort: low; }\n";

        let (graph, diagnostics) =
            transform(TemplateContext::new(), stylesheet, RenderMode::Strict).unwrap();

        assert!(diagnostics.is_empty());
        assert_eq!(graph.model_stylesheet(), stylesheet);
    }

    #[test]
    fn renders_inputs_vars_control_flow_and_locals_once() {
        let stylesheet = r"{% set prefix = '.tier-' %}
{% for effort in inputs.efforts %}
{{ prefix }}{{ loop.index }} { reasoning_effort: {{ effort }}; }
{% endfor %}
.selected { model: {{ vars.MODEL }}; }
.literal { model: {{ inputs.literal }}; }";
        let context = TemplateContext::new()
            .with_goal("must stay unavailable")
            .with_inputs(HashMap::from([
                (
                    "efforts".to_string(),
                    toml::Value::Array(vec![
                        toml::Value::String("low".to_string()),
                        toml::Value::String("high".to_string()),
                    ]),
                ),
                (
                    "literal".to_string(),
                    toml::Value::String("{{ vars.MODEL }}".to_string()),
                ),
            ]))
            .with_vars(HashMap::from([("MODEL".to_string(), "sonnet".to_string())]));

        let (graph, diagnostics) = transform(context, stylesheet, RenderMode::Strict).unwrap();

        assert!(diagnostics.is_empty());
        assert!(
            graph
                .model_stylesheet()
                .contains(".tier-1 { reasoning_effort: low; }")
        );
        assert!(
            graph
                .model_stylesheet()
                .contains(".tier-2 { reasoning_effort: high; }")
        );
        assert!(
            graph
                .model_stylesheet()
                .contains(".selected { model: sonnet; }")
        );
        assert!(
            graph
                .model_stylesheet()
                .contains(".literal { model: {{ vars.MODEL }}; }")
        );
    }

    #[test]
    fn structural_undefined_value_clears_stylesheet_and_reports_context() {
        for (expression, expected_fix) in [
            ("inputs.effort", "[run.inputs]"),
            ("vars.MODEL", "fabro variable set MODEL"),
            ("goal", "expose only `inputs` and `vars`"),
            ("env.MODEL", "expose only `inputs` and `vars`"),
            ("secrets.MODEL", "expose only `inputs` and `vars`"),
        ] {
            let stylesheet = format!("* {{ model: {{{{ {expression} }}}}; }}");
            let (graph, diagnostics) = transform(
                TemplateContext::new().with_goal("hidden"),
                &stylesheet,
                RenderMode::Structural,
            )
            .unwrap();

            assert_eq!(graph.model_stylesheet(), "", "expression: {expression}");
            assert_eq!(diagnostics.len(), 1, "expression: {expression}");
            assert_eq!(diagnostics[0].rule, "template_undefined_variable");
            assert!(
                diagnostics[0]
                    .message
                    .contains("graph attribute `model_stylesheet`"),
                "{:?}",
                diagnostics[0]
            );
            assert!(
                diagnostics[0]
                    .fix
                    .as_deref()
                    .is_some_and(|fix| fix.contains(expected_fix)),
                "{:?}",
                diagnostics[0]
            );
        }
    }

    #[test]
    fn syntax_error_preserves_owner_and_source_chain() {
        let error = transform(
            TemplateContext::new(),
            "* { model: {% if %}; }",
            RenderMode::Strict,
        )
        .unwrap_err();
        let chain = collect_chain(&error).join(": ");

        assert!(
            chain.contains("graph attribute `model_stylesheet`"),
            "{chain}"
        );
        assert!(chain.contains("template syntax error"), "{chain}");
        assert!(chain.contains("workflow.fabro"), "{chain}");
    }
}
