use fabro_graphviz::graph::{AttrValue, Graph};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

const VALID_DIRECTIONS: &[&str] = &["TB", "LR", "BT", "RL"];

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "direction_valid"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let Some(rankdir) = graph.attrs.get("rankdir").and_then(AttrValue::as_str) else {
            return Vec::new();
        };
        if VALID_DIRECTIONS.contains(&rankdir) {
            return Vec::new();
        }
        vec![Diagnostic {
            rule: self.name().to_string(),
            severity: Severity::Warning,
            message: format!("Graph has invalid rankdir '{rankdir}'"),
            node_id: None,
            edge: None,
            fix: Some(format!("Use one of: {}", VALID_DIRECTIONS.join(", "))),

            ..Diagnostic::default()
        }]
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::AttrValue;

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn direction_valid_rule_no_rankdir() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn direction_valid_rule_valid_directions() {
        let rule = Rule;
        let mut g = minimal_graph();
        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("LR".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("TB".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("BT".to_string()));
        assert!(rule.apply(&g).is_empty());

        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("RL".to_string()));
        assert!(rule.apply(&g).is_empty());
    }

    #[test]
    fn direction_valid_rule_invalid_direction() {
        let mut g = minimal_graph();
        g.attrs
            .insert("rankdir".to_string(), AttrValue::String("XY".to_string()));
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }
}
