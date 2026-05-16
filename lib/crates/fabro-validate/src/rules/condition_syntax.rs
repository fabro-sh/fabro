use fabro_graphviz::condition::parse_condition;
use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "condition_syntax"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for edge in &graph.edges {
            let Some(condition) = edge.condition() else {
                continue;
            };
            if condition.is_empty() {
                continue;
            }
            if let Err(e) = parse_condition(condition) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Error,
                    message: format!(
                        "Condition '{condition}' on edge {} -> {} failed parse: {e}",
                        edge.from, edge.to
                    ),
                    node_id: None,
                    edge: Some((edge.from.clone(), edge.to.clone())),
                    fix: Some(
                        "Use key=value, key!=value, key>value, key contains value, \
                         key matches pattern, or bare key syntax"
                            .to_string(),
                    ),

                    ..Diagnostic::default()
                });
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Edge};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn condition_syntax_rule_valid_condition() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=succeeded".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_invalid_clause() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("bad clause here".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn condition_syntax_rule_not_equals() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome!=failed".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_empty_condition() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs
            .insert("condition".to_string(), AttrValue::String(String::new()));
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_compound_and() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=succeeded && retries=0".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- Additional coverage: terminal_node two terminals ---

    #[test]
    fn condition_syntax_rule_bare_key_truthy() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.passed".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- condition_syntax: context-prefixed clause with spaces is rejected ---

    #[test]
    fn condition_syntax_rule_context_prefix_with_space() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.foo bar".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        // "context.foo bar" has an unexpected trailing word — parse error
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    // --- terminal_node: by "Exit" capitalized id ---

    #[test]
    fn condition_syntax_rule_no_condition_attr() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- freeform_edge_count: freeform=false does not count ---

    #[test]
    fn condition_syntax_rule_empty_key_fails_parse() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("=value".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        // parse_condition catches empty key
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("failed parse"));
    }

    #[test]
    fn condition_syntax_rule_valid_passes_both_checks() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=succeeded".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- condition_syntax: new operators accepted ---

    #[test]
    fn condition_syntax_rule_accepts_or() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=succeeded || outcome=failed".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_not() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("!outcome=failed".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_contains() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.x contains y".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_accepts_numeric() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.score > 80".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn condition_syntax_rule_rejects_invalid_regex() {
        let mut g = minimal_graph();
        let mut edge = Edge::new("start", "exit");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("context.x matches [bad".to_string()),
        );
        g.edges = vec![edge];
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }
}
