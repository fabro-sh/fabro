use fabro_graphviz::graph::{Graph, KNOWN_HANDLER_TYPES, is_known_handler_type};

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "type_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if let Some(node_type) = node.node_type() {
                if !is_known_handler_type(node_type) {
                    diagnostics.push(Diagnostic {
                        rule:     self.name().to_string(),
                        severity: Severity::Warning,
                        message:  format!("Node '{}' has unrecognized type '{node_type}'", node.id),
                        node_id:  Some(node.id.clone()),
                        edge:     None,
                        fix:      Some(format!("Use one of: {}", KNOWN_HANDLER_TYPES.join(", "))),
                    });
                }
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn type_known_rule_unknown_type() {
        let mut g = minimal_graph();
        let mut node = Node::new("custom");
        node.attrs.insert(
            "type".to_string(),
            AttrValue::String("unknown_type".to_string()),
        );
        g.nodes.insert("custom".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn type_known_rule_known_type() {
        let mut g = minimal_graph();
        let mut node = Node::new("gate");
        node.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        g.nodes.insert("gate".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn type_known_rule_no_type_attr() {
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        // Nodes without explicit type attr should not trigger warning
        assert!(d.is_empty());
    }

    // --- Additional coverage: start_no_incoming no start node ---

    #[test]
    fn type_known_rule_all_known_types_accepted() {
        let mut g = minimal_graph();

        let mut n1 = Node::new("n1");
        n1.attrs
            .insert("type".to_string(), AttrValue::String("agent".to_string()));
        g.nodes.insert("n1".to_string(), n1);

        let mut n2 = Node::new("n2");
        n2.attrs.insert(
            "type".to_string(),
            AttrValue::String("conditional".to_string()),
        );
        g.nodes.insert("n2".to_string(), n2);

        let mut n3 = Node::new("n3");
        n3.attrs.insert(
            "type".to_string(),
            AttrValue::String("parallel".to_string()),
        );
        g.nodes.insert("n3".to_string(), n3);

        let mut n4 = Node::new("n4");
        n4.attrs.insert(
            "type".to_string(),
            AttrValue::String("parallel.fan_in".to_string()),
        );
        g.nodes.insert("n4".to_string(), n4);

        let mut n5 = Node::new("n5");
        n5.attrs
            .insert("type".to_string(), AttrValue::String("command".to_string()));
        g.nodes.insert("n5".to_string(), n5);

        let mut n6 = Node::new("n6");
        n6.attrs.insert(
            "type".to_string(),
            AttrValue::String("stack.manager_loop".to_string()),
        );
        g.nodes.insert("n6".to_string(), n6);

        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- prompt_on_llm_nodes: explicit type=agent without prompt/label ---

    #[test]
    fn type_known_rule_start_exit_shapes_no_warning() {
        // The minimal_graph has start (Mdiamond) and exit (Msquare), which resolve
        // to known handler types "start" and "exit" via shape mapping, not explicit
        // type. Since they have no explicit `type` attr, the rule should not
        // flag them.
        let g = minimal_graph();
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    // --- reserved_keyword_node_id tests ---
}
