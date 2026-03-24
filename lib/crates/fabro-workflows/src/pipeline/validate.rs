use fabro_validate::LintRule;

use super::types::{Transformed, Validated};

/// VALIDATE phase: run lint rules against the transformed graph.
///
/// **Infallible.** Always returns `Validated` with diagnostics. Caller decides
/// whether to fail via `validated.raise_on_errors()`.
pub fn validate(transformed: Transformed, extra_rules: &[&dyn LintRule]) -> Validated {
    let Transformed { graph, source } = transformed;
    let diagnostics = fabro_validate::validate(&graph, extra_rules);
    Validated::new(graph, source, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::parse::parse;
    use crate::pipeline::transform;
    use crate::pipeline::types::TransformOptions;

    fn run_pipeline(dot: &str) -> Validated {
        let parsed = parse(dot).unwrap();
        let transformed = transform::transform(
            parsed,
            &TransformOptions {
                base_dir: None,
                custom_transforms: vec![],
            },
        );
        validate(transformed, &[])
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
    fn validate_missing_start_node() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = run_pipeline(dot);
        assert!(validated.has_errors());
        assert!(validated.raise_on_errors().is_err());
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
        assert!(diagnostics
            .iter()
            .all(|d| d.severity != fabro_validate::Severity::Error));
    }

    #[test]
    fn validate_diagnostics_accessible_before_raise() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = run_pipeline(dot);
        // Can read diagnostics before raising
        let diags = validated.diagnostics();
        assert!(!diags.is_empty());
        // Then raise
        assert!(validated.raise_on_errors().is_err());
    }
}
