pub mod rules;

use fabro_graphviz::graph::Graph;
use fabro_model::Catalog;
use serde::{Deserialize, Serialize};

/// Severity level for validation diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A validation diagnostic produced by a lint rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub rule:        String,
    pub severity:    Severity,
    pub message:     String,
    pub node_id:     Option<String>,
    pub edge:        Option<(String, String)>,
    pub fix:         Option<String>,
    pub source_path: Option<String>,
    pub line:        Option<u32>,
    pub column:      Option<u32>,
    pub span_start:  Option<usize>,
    pub span_len:    Option<usize>,
    #[serde(default)]
    pub related:     Vec<RelatedDiagnostic>,
}

impl Default for Diagnostic {
    fn default() -> Self {
        Self {
            rule:        String::new(),
            severity:    Severity::Info,
            message:     String::new(),
            node_id:     None,
            edge:        None,
            fix:         None,
            source_path: None,
            line:        None,
            column:      None,
            span_start:  None,
            span_len:    None,
            related:     Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedDiagnostic {
    pub message:     String,
    pub source_path: Option<String>,
    pub line:        Option<u32>,
    pub column:      Option<u32>,
}

/// A lint rule that validates a graph.
pub trait LintRule {
    fn name(&self) -> &'static str;
    fn apply(&self, graph: &Graph) -> Vec<Diagnostic>;
}

/// Validation error returned when error-severity diagnostics are present.
#[derive(Debug, thiserror::Error)]
#[error("Validation error: {0}")]
pub struct ValidationError(pub String);

/// Run all built-in lint rules (and any extra rules) against the graph.
#[must_use]
pub fn validate(graph: &Graph, extra_rules: &[&dyn LintRule]) -> Vec<Diagnostic> {
    let built_in = rules::built_in_rules();
    let mut diagnostics = Vec::new();
    for rule in &built_in {
        diagnostics.extend(rule.apply(graph));
    }
    for rule in extra_rules {
        diagnostics.extend(rule.apply(graph));
    }
    diagnostics
}

/// Run all built-in catalog-free lint rules, caller-supplied model catalog
/// rules, and any extra rules against the graph.
#[must_use]
pub fn validate_with_catalog(
    graph: &Graph,
    catalog: &Catalog,
    extra_rules: &[&dyn LintRule],
) -> Vec<Diagnostic> {
    let mut diagnostics = validate(graph, &[]);
    let catalog_rules = rules::catalog_rules(catalog);
    for rule in &catalog_rules {
        diagnostics.extend(rule.apply(graph));
    }
    for rule in extra_rules {
        diagnostics.extend(rule.apply(graph));
    }
    diagnostics
}

/// If any Error-severity diagnostics are present, return `ValidationError`.
///
/// # Errors
/// Returns `ValidationError` with joined error messages.
pub fn raise_on_errors(diagnostics: &[Diagnostic]) -> Result<(), ValidationError> {
    let mut errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .peekable();
    if errors.peek().is_some() {
        let message = errors
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ValidationError(message));
    }
    Ok(())
}

/// Run all built-in lint rules (and any extra rules). Returns Err if any
/// Error-severity diagnostics are found.
///
/// # Errors
/// Returns `ValidationError` if any Error-severity diagnostics are found.
pub fn validate_or_raise(
    graph: &Graph,
    extra_rules: &[&dyn LintRule],
) -> Result<Vec<Diagnostic>, ValidationError> {
    let diagnostics = validate(graph, extra_rules);
    raise_on_errors(&diagnostics)?;
    Ok(diagnostics)
}

/// Run catalog-aware validation and return `ValidationError` if any
/// Error-severity diagnostics are found.
///
/// # Errors
/// Returns `ValidationError` if any Error-severity diagnostics are found.
pub fn validate_with_catalog_or_raise(
    graph: &Graph,
    catalog: &Catalog,
    extra_rules: &[&dyn LintRule],
) -> Result<Vec<Diagnostic>, ValidationError> {
    let diagnostics = validate_with_catalog(graph, catalog, extra_rules);
    raise_on_errors(&diagnostics)?;
    Ok(diagnostics)
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_model::catalog::LlmCatalogSettings;
    use fabro_model::{Catalog, ProviderId};

    use super::*;

    fn minimal_valid_graph() -> Graph {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "exit"));
        g
    }

    fn graph_with_model_and_provider(model: &str, provider: &str) -> Graph {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);

        let mut work = Node::new("work");
        work.attrs
            .insert("model".to_string(), AttrValue::String(model.to_string()));
        work.attrs.insert(
            "provider".to_string(),
            AttrValue::String(provider.to_string()),
        );
        g.nodes.insert("work".to_string(), work);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);

        g.edges.push(Edge::new("start", "work"));
        g.edges.push(Edge::new("work", "exit"));
        g
    }

    fn custom_catalog() -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.venice]
display_name = "Venice"
adapter = "openai_compatible"
base_url = "https://api.venice.ai/api/v1"
credentials = ["env:VENICE_API_KEY"]

[models."venice-large"]
provider = "venice"
display_name = "Venice Large"
family = "venice"
default = true

[models."venice-large".limits]
context_window = 128000

[models."venice-large".features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    #[test]
    fn validate_minimal_valid_graph_has_no_errors() {
        let g = minimal_valid_graph();
        let diagnostics = validate(&g, &[]);
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_or_raise_passes_for_valid_graph() {
        let g = minimal_valid_graph();
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_or_raise_fails_for_missing_start() {
        let mut g = Graph::new("test");
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        g.nodes.insert("exit".to_string(), exit);
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn validate_or_raise_fails_for_missing_exit() {
        let mut g = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        g.nodes.insert("start".to_string(), start);
        let result = validate_or_raise(&g, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn validate_runs_extra_rules() {
        struct AlwaysWarnRule;
        impl LintRule for AlwaysWarnRule {
            fn name(&self) -> &'static str {
                "always_warn"
            }
            fn apply(&self, _graph: &Graph) -> Vec<Diagnostic> {
                vec![Diagnostic {
                    rule: "always_warn".to_string(),
                    severity: Severity::Warning,
                    message: "custom warning".to_string(),
                    node_id: None,
                    edge: None,
                    fix: None,

                    ..Diagnostic::default()
                }]
            }
        }
        let g = minimal_valid_graph();
        let extra = AlwaysWarnRule;
        let diagnostics = validate(&g, &[&extra]);
        let custom: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.rule == "always_warn")
            .collect();
        assert_eq!(custom.len(), 1);
    }

    #[test]
    fn validate_does_not_run_catalog_aware_model_rules() {
        let g = graph_with_model_and_provider("not-in-any-catalog", "not-a-provider");

        let diagnostics = validate(&g, &[]);

        assert!(
            diagnostics
                .iter()
                .all(|d| d.rule != "node_model_known" && d.rule != "stylesheet_model_known"),
            "catalog-free validation should not emit model/provider diagnostics: {diagnostics:?}"
        );
    }

    #[test]
    fn validate_with_catalog_accepts_custom_catalog_entries() {
        let g = graph_with_model_and_provider("venice-large", "venice");
        let catalog = custom_catalog();

        let diagnostics = validate_with_catalog(&g, &catalog, &[]);

        assert!(
            diagnostics
                .iter()
                .all(|d| d.rule != "node_model_known" && d.rule != "stylesheet_model_known"),
            "custom catalog entries should validate cleanly: {diagnostics:?}"
        );
    }

    #[test]
    fn validate_with_catalog_warns_for_unknown_model_and_provider() {
        let g = graph_with_model_and_provider("missing-model", "missing-provider");
        let catalog = custom_catalog();

        let diagnostics = validate_with_catalog(&g, &catalog, &[]);

        assert!(
            diagnostics
                .iter()
                .any(|d| d.rule == "node_model_known" && d.message.contains("missing-model")),
            "missing model diagnostic not found: {diagnostics:?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.rule == "node_model_known"
                && d.message.contains("missing-provider")
                && d.message.contains(ProviderId::new("venice").as_str())),
            "missing provider diagnostic not found: {diagnostics:?}"
        );
    }

    #[test]
    fn diagnostic_severity_eq() {
        assert_eq!(Severity::Error, Severity::Error);
        assert_ne!(Severity::Error, Severity::Warning);
        assert_ne!(Severity::Warning, Severity::Info);
    }
}
