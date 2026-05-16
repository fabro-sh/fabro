use fabro_graphviz::graph::Graph;
use fabro_graphviz::stylesheet::parse_stylesheet;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "stylesheet_syntax"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let stylesheet = graph.model_stylesheet();
        if stylesheet.is_empty() {
            return Vec::new();
        }
        match parse_stylesheet(stylesheet) {
            Ok(_) => Vec::new(),
            Err(e) => vec![Diagnostic {
                rule: self.name().to_string(),
                severity: Severity::Error,
                message: format!("Model stylesheet parse error: {e}"),
                node_id: None,
                edge: None,
                fix: Some("Fix the model_stylesheet syntax".to_string()),

                ..Diagnostic::default()
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::AttrValue;

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn stylesheet_syntax_rule_unbalanced() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: foo;".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn stylesheet_syntax_rule_balanced() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: foo; }".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn stylesheet_syntax_rule_malformed_selector() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { garbage garbage }".to_string()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    // --- Additional coverage: condition_syntax invalid case ---

    #[test]
    fn stylesheet_syntax_rule_no_stylesheet() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: type_known no type attr ---

    #[test]
    fn stylesheet_syntax_rule_multi_rule_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String(
                "* { model: gpt-4; } .fast { model: gpt-3.5; reasoning_effort: low; }".to_string(),
            ),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- fidelity_valid: multiple simultaneous violations ---

    #[test]
    fn stylesheet_syntax_rule_empty_string() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String(String::new()),
        );
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- type_known: start and exit types from shape are not flagged ---
}
